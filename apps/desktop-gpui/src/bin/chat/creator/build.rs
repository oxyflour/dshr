use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dshr_agent_core::{ToolCallRequest, ToolOutcome};
use dshr_plugin_api::manifest::RuntimeKind;
use dshr_sidecar_guard::{GuardLimits, spawn_guarded};
use dshr_tool_runtime::{ApprovalBroker, ApprovalOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    BUILD_PLUGIN_TOOL, MAX_PACKAGE_BYTES, MAX_PACKAGE_ENTRIES, Session, SnapshotBudget,
    checked_metadata, json_outcome, read_manifest, snapshot_directory, source_directory,
};

const BUILD_TIMEOUT: Duration = Duration::from_secs(180);
const SDK_FILES: [(&str, &str); 5] = [
    (
        "Cargo.toml",
        include_str!("../../../../../../crates/plugin-sdk/Cargo.toml"),
    ),
    (
        "src/lib.rs",
        include_str!("../../../../../../crates/plugin-sdk/src/lib.rs"),
    ),
    (
        "src/protocol.rs",
        include_str!("../../../../../../crates/plugin-sdk/src/protocol.rs"),
    ),
    (
        "src/session.rs",
        include_str!("../../../../../../crates/plugin-sdk/src/session.rs"),
    ),
    (
        "src/tool_result.rs",
        include_str!("../../../../../../crates/plugin-sdk/src/tool_result.rs"),
    ),
];

const TOOLCHAIN_CONFIG: &str = "creator-toolchain.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Toolchain {
    cargo: PathBuf,
    cargo_home: Option<PathBuf>,
    #[serde(skip)]
    managed: Option<PathBuf>,
}

impl Toolchain {
    fn validate(self) -> Result<Self, String> {
        if !self.cargo.is_absolute() || !self.cargo.is_file() {
            return Err("Cargo 路径必须是存在的可执行文件的绝对路径".to_owned());
        }
        if self
            .cargo_home
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !path.is_dir())
        {
            return Err("cargo_home 必须是已存在的缓存目录的绝对路径".to_owned());
        }
        Ok(self)
    }
}

fn read_toolchain(path: &Path) -> Result<Option<Toolchain>, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取 {}。{error}", path.display())),
    };
    let parsed = serde_json::from_str::<Toolchain>(raw.trim_start_matches('\u{feff}'))
        .map_err(|error| format!("{} 格式无效。{error}", path.display()))?;
    parsed
        .validate()
        .map(Some)
        .map_err(|error| format!("{} 设置无效。{error}", path.display()))
}

pub(super) fn toolchain() -> Result<Toolchain, String> {
    if let Some(path) = crate::non_empty_env_path("DSHR_CREATOR_CARGO") {
        return Toolchain {
            cargo: path,
            cargo_home: None,
            managed: None,
        }
        .validate()
        .map_err(|error| format!("DSHR_CREATOR_CARGO 设置无效。{error}"));
    }
    let config = crate::data_root().join(TOOLCHAIN_CONFIG);
    if let Some(toolchain) = read_toolchain(&config)? {
        return Ok(toolchain);
    }
    #[cfg(windows)]
    if crate::non_empty_env_path("DSHR_HOME").is_none() {
        if let Some(local) = crate::non_empty_env_path("LOCALAPPDATA") {
            let user_config = local.join("dshr").join(TOOLCHAIN_CONFIG);
            if user_config != config {
                if let Some(toolchain) = read_toolchain(&user_config)? {
                    return Ok(toolchain);
                }
            }
        }
    }
    if let Some(root) =
        crate::runtime_components::installed(crate::runtime_components::Component::CreatorBuild)
    {
        return Ok(Toolchain {
            cargo: root.join("toolchain/bin/cargo.exe"),
            cargo_home: None,
            managed: Some(root),
        });
    }
    let program = format!("cargo{}", env::consts::EXE_SUFFIX);
    let cargo = env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(&program))
        .chain(env::var_os("CARGO_HOME").map(|home| PathBuf::from(home).join("bin").join(&program)))
        .chain(env::home_dir().map(|home| home.join(".cargo/bin").join(&program)))
        .find(|path| path.is_absolute() && path.is_file())
        .ok_or(
            "未找到 Cargo。可在设置 → 运行组件安装创造模式构建组件，或在 creator-toolchain.json 指定已有的 Cargo 和缓存。",
        )?;
    Ok(Toolchain {
        cargo,
        cargo_home: None,
        managed: None,
    })
}

pub(super) fn run(
    session: &Session<'_>,
    relative: &str,
    call: &ToolCallRequest,
    approvals: &ApprovalBroker,
    cancel: &AtomicBool,
) -> Result<ToolOutcome, String> {
    if session.read_only || cancel.load(Ordering::Acquire) {
        return Err("当前权限或取消状态不允许构建插件".to_owned());
    }
    let toolchain = toolchain()?;
    let source = source_directory(session.workspace, relative)?;
    let scratch = tempfile::tempdir_in(session.sessions_dir).map_err(|error| error.to_string())?;
    let project = scratch.path().join("package");
    copy_sources(&source, &project)?;
    let manifest = read_manifest(&project, relative)?;
    if manifest.runtime.kind != RuntimeKind::ExternalProcess {
        return Err("当前构建工具接受 Rust external-process 包".to_owned());
    }
    let entry = manifest.runtime.entry.as_deref().unwrap_or_default();
    let binary = binary_name(entry)?;
    let manifest_before =
        fs::read(project.join("plugin.toml")).map_err(|error| error.to_string())?;
    let target = format!("{relative}/{entry}");
    let preview = json!({
        "path": relative,
        "entry": target,
        "operation": "使用本机 Cargo 离线构建。构建依赖可用当前用户权限执行代码。"
    });
    if cancel.load(Ordering::Acquire)
        || approvals.request_user_cancellable(
            &call.call_id,
            BUILD_PLUGIN_TOOL,
            &preview,
            Duration::from_secs(300),
            cancel,
        ) != ApprovalOutcome::AllowOnce
        || cancel.load(Ordering::Acquire)
    {
        return Err("插件构建未获批准".to_owned());
    }
    let sdk = project.join("sdk");
    fs::create_dir_all(sdk.join("src")).map_err(|error| error.to_string())?;
    for (path, text) in SDK_FILES {
        fs::write(sdk.join(path), text).map_err(|error| error.to_string())?;
    }
    let output = compile(&toolchain, &project, scratch.path(), binary, cancel)?;
    if cancel.load(Ordering::Acquire) {
        return Err("插件构建已取消".to_owned());
    }
    if source_directory(session.workspace, relative)? != source
        || fs::read(source.join("plugin.toml")).map_err(|error| error.to_string())?
            != manifest_before
    {
        return Err("构建期间包路径或清单已变化。请按当前清单重新构建。".to_owned());
    }
    publish(&source, entry, &output).map_err(|error| format!("发布构建产物失败。{error}"))?;
    Ok(json_outcome(
        json!({ "path": relative, "entry": target, "next": super::TRY_PLUGIN_TOOL }),
    ))
}

fn copy_sources(source: &Path, project: &Path) -> Result<(), String> {
    fs::create_dir(project).map_err(|error| error.to_string())?;
    let mut budget = SnapshotBudget {
        bytes: MAX_PACKAGE_BYTES,
        entries: MAX_PACKAGE_ENTRIES,
    };
    for file in ["plugin.toml", "Cargo.toml", "Cargo.lock"] {
        let path = source.join(file);
        if file == "Cargo.lock" && !path.exists() {
            continue;
        }
        let metadata = checked_metadata(&path).map_err(|error| format!("{file}：{error}"))?;
        if !metadata.is_file() {
            return Err(format!("{file} 必须是普通文件"));
        }
        super::copy_snapshot_file(&path, &project.join(file), &metadata, &mut budget)?;
    }
    let src = source.join("src");
    if !checked_metadata(&src)?.is_dir() {
        return Err("Rust 源码应写入 src/".to_owned());
    }
    fs::create_dir(project.join("src")).map_err(|error| error.to_string())?;
    snapshot_directory(&src, &project.join("src"), 1, &mut budget)
}

fn binary_name(entry: &str) -> Result<&str, String> {
    entry
        .strip_prefix("bin/")
        .and_then(|name| name.strip_suffix(env::consts::EXE_SUFFIX))
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| {
            format!(
                "将 runtime.entry 设置为 bin/<Cargo 二进制名>{}",
                env::consts::EXE_SUFFIX
            )
        })
}

fn compile(
    toolchain: &Toolchain,
    project: &Path,
    scratch: &Path,
    bin: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let cargo = &toolchain.cargo;
    let target = scratch.join("target");
    let stdout = scratch.join("cargo.jsonl");
    let stderr = scratch.join("cargo.stderr");
    let mut command = Command::new(cargo);
    command
        .args([
            "build",
            "--offline",
            "--message-format=json",
            "--bin",
            bin,
            "--target-dir",
        ])
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).map_err(|error| error.to_string())?)
        .stderr(File::create(&stderr).map_err(|error| error.to_string())?);
    let path = env::join_paths(
        cargo
            .parent()
            .into_iter()
            .map(Path::to_path_buf)
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .map_err(|error| error.to_string())?;
    let mut env = vec![
        ("PATH".to_owned(), path.to_string_lossy().into_owned()),
        ("TMP".to_owned(), scratch.display().to_string()),
        ("TEMP".to_owned(), scratch.display().to_string()),
        ("TMPDIR".to_owned(), scratch.display().to_string()),
    ];
    if let Some(home) = &toolchain.cargo_home {
        env.push(("CARGO_HOME".to_owned(), home.to_string_lossy().into_owned()));
    }
    if let Some(root) = &toolchain.managed {
        let home = scratch.join("cargo-home");
        fs::create_dir(&home).map_err(|error| error.to_string())?;
        let vendor = root.join("vendor").to_string_lossy().replace('\\', "/");
        let vendor = serde_json::to_string(&vendor).map_err(|error| error.to_string())?;
        fs::write(
            home.join("config.toml"),
            format!(
                "[source.crates-io]\nreplace-with = \"dshr-vendor\"\n[source.dshr-vendor]\ndirectory = {vendor}\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        env.extend([
            ("CARGO_HOME".to_owned(), home.to_string_lossy().into_owned()),
            (
                "RUSTC".to_owned(),
                root.join("toolchain/bin/rustc.exe").to_string_lossy().into_owned(),
            ),
        ]);
        command.args(["--target", "x86_64-pc-windows-gnu"]);
    }
    let mut child = spawn_guarded(command, project, &env, &GuardLimits::unlimited())
        .map_err(|error| format!("无法启动本机 Cargo。{error}"))?;
    let deadline = Instant::now() + BUILD_TIMEOUT;
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            return Err("插件构建已取消".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("Cargo 构建超时".to_owned());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let mut artifact = None;
    let mut diagnostics = String::new();
    let reader = BufReader::new(File::open(stdout).map_err(|error| error.to_string())?);
    for line in reader.take(16 * 1024 * 1024).lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["reason"] == "compiler-artifact" && message["target"]["name"] == bin {
            artifact = message["executable"]
                .as_str()
                .map(PathBuf::from)
                .or(artifact);
        } else if message["message"]["level"] == "error" && diagnostics.len() < 6000 {
            if let Some(detail) = message["message"]["rendered"].as_str() {
                diagnostics.extend(detail.chars().take(3000));
            }
        }
    }
    if !status.success() {
        if let Ok(mut log) = File::open(stderr) {
            let start = log
                .metadata()
                .map_or(0, |meta| meta.len().saturating_sub(4000));
            let _ = log.seek(SeekFrom::Start(start));
            let mut tail = Vec::new();
            let _ = log.take(4000).read_to_end(&mut tail);
            diagnostics.push_str(&String::from_utf8_lossy(&tail));
        }
        return Err(format!(
            "Cargo 构建失败。修正具体错误后再调用 creator_build_plugin。\n{diagnostics}"
        ));
    }
    let artifact = artifact.ok_or("Cargo 未返回可执行产物")?;
    let path = artifact.canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(target.canonicalize().map_err(|error| error.to_string())?)
        || !checked_metadata(&artifact)?.is_file()
    {
        return Err("Cargo 产物不在临时构建目录内".to_owned());
    }
    Ok(path)
}

fn publish(package: &Path, entry: &str, artifact: &Path) -> Result<(), String> {
    let bin = package.join("bin");
    if !bin.exists() {
        match fs::create_dir(&bin) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    if !checked_metadata(&bin)?.is_dir()
        || !bin
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(package)
    {
        return Err("bin 必须是插件包内的目录".to_owned());
    }
    let destination = package.join(entry);
    match fs::symlink_metadata(&destination) {
        Ok(_) if !checked_metadata(&destination)?.is_file() => {
            return Err("入口必须是普通文件".to_owned());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let meta = checked_metadata(artifact)?;
    let mut staged = tempfile::NamedTempFile::new_in(&bin).map_err(|error| error.to_string())?;
    let file = File::open(artifact).map_err(|error| error.to_string())?;
    let copied = io::copy(&mut file.take(MAX_PACKAGE_BYTES + 1), staged.as_file_mut())
        .map_err(|error| error.to_string())?;
    if copied > MAX_PACKAGE_BYTES {
        return Err("构建产物超过 64 MiB".to_owned());
    }
    staged
        .as_file()
        .set_permissions(meta.permissions())
        .map_err(|error| error.to_string())?;
    staged
        .persist(destination)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // CPP-CREATOR-012
    #[test]
    fn local_config_recovers_cargo_and_cache_paths_and_rejects_bad_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(TOOLCHAIN_CONFIG);
        assert!(read_toolchain(&path).unwrap().is_none());
        let cargo = root
            .path()
            .join(format!("cargo{}", env::consts::EXE_SUFFIX));
        let cargo_home = root.path().join("cargo cache");
        fs::write(&cargo, []).unwrap();
        fs::create_dir(&cargo_home).unwrap();
        let set = |value: Value| fs::write(&path, format!("\u{feff}{value}")).unwrap();
        set(json!({ "cargo": &cargo, "cargo_home": &cargo_home }));
        let configured = read_toolchain(&path).unwrap().unwrap();
        assert_eq!(configured.cargo, cargo);
        assert_eq!(configured.cargo_home, Some(cargo_home));
        set(json!({ "cargo": "cargo" }));
        assert!(read_toolchain(&path).unwrap_err().contains("Cargo 路径"));
        set(json!({ "cargo": cargo, "cargo_home": "missing" }));
        assert!(read_toolchain(&path).unwrap_err().contains("cargo_home"));
    }

    // CPP-CREATOR-011
    #[test]
    fn build_entry_is_scoped_and_failed_publish_preserves_the_executable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let entry = format!("bin/hello-world{}", env::consts::EXE_SUFFIX);
        assert_eq!(binary_name(&entry).unwrap(), "hello-world");
        for bad in [
            "../hello-world.exe",
            "bin/../hello-world.exe",
            "bin/other/hello-world.exe",
        ] {
            assert!(binary_name(bad).is_err());
        }
        let artifact = root.join("artifact");
        fs::write(&artifact, b"old").unwrap();
        publish(&root, &entry, &artifact).unwrap();
        assert!(publish(&root, &entry, &root.join("missing")).is_err());
        assert_eq!(fs::read(root.join(&entry)).unwrap(), b"old");
        fs::write(&artifact, b"rebuilt").unwrap();
        publish(&root, &entry, &artifact).unwrap();
        assert_eq!(fs::read(root.join(entry)).unwrap(), b"rebuilt");
    }
}
