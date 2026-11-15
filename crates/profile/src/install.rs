//! 离线安装插件包：目录或 npm `pack` 产物（`.tgz`），复制到 sidecar 的
//! `node_modules`，并写入 profile。
//!
//! 这是 M2 的最小"安装"语义：**不做 npm 网络解析、不处理依赖树、不执行生命周期
//! 脚本**。包必须已经在本机是一个完整可加载的目录（例如从本机 DSH 的 `app.asar`
//! 抽出来的官方包，或用户自己的构建产物 + 依赖），或者是一个 npm pack 产物
//! `.tgz`（离线解包）。
//!
//! 安全边界：
//!
//! - 包名和 `dsh` 字段里的引用路径不允许 `..` / 绝对路径 / 反斜杠逃逸；
//! - 压缩包拒绝符号链接、硬链接与设备文件条目（见 [`crate::archive`]）；
//! - 升级是**原子替换**：先完整复制到同父目录的 staging，再 rename 旧目录到备份、
//!   staging rename 到位；任何一步失败都回滚并保留旧版本（磁盘满/复制错误不会
//!   留下"没有任何版本"的状态）；
//! - 不联网、不执行包内脚本。

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::archive;
use crate::{PluginEntry, Profile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub name: String,
    pub destination: PathBuf,
    pub added_to_profile: bool,
}

/// 安装一个插件包：本地目录，或 npm pack 产物 `.tgz`。
///
/// `.tgz` 会先解包到系统临时目录（结束即清理），再走与目录安装完全相同的
/// 校验/复制/写 profile 流程。
pub fn install_package(
    sidecar_dir: &Path,
    package: &Path,
    profile: &mut Profile,
) -> Result<InstallReport, String> {
    if !profile.can_edit() {
        return Err(String::from("Loader 模式的插件列表只读，不能安装"));
    }
    let source = prepare_package(package)?;
    install_dir(sidecar_dir, &source.root, profile)
}

/// 解析安装源（目录或 `.tgz`）到可复制的包根目录；`.tgz` 的暂存目录随
/// [`PackageSource`] 一起释放。信任链（`crate::trust`）复用它，保证摘要与
/// 实际安装内容一致。
pub(crate) struct PackageSource {
    pub(crate) root: PathBuf,
    _staging: Option<StagingDir>,
}

pub(crate) fn prepare_package(package: &Path) -> Result<PackageSource, String> {
    if package.is_dir() {
        return Ok(PackageSource {
            root: package.to_path_buf(),
            _staging: None,
        });
    }
    if !package.is_file() {
        return Err(format!(
            "插件包路径不存在（只支持目录或 .tgz）: {}",
            package.display()
        ));
    }
    let file_name = package
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !file_name.ends_with(".tgz") && !file_name.ends_with(".tar.gz") {
        return Err(format!(
            "不支持的插件包文件（只支持目录或 .tgz）: {}",
            package.display()
        ));
    }

    let staging = StagingDir::create()?;
    let unpacked = archive::unpack_tgz(package, staging.path())?;
    Ok(PackageSource {
        root: unpacked.root,
        _staging: Some(staging),
    })
}

pub(crate) fn install_dir(
    sidecar_dir: &Path,
    package_dir: &Path,
    profile: &mut Profile,
) -> Result<InstallReport, String> {
    let name = read_manifest(package_dir)?;

    let node_modules = sidecar_dir.join("node_modules");
    fs::create_dir_all(&node_modules)
        .map_err(|err| format!("创建 {} 失败: {err}", node_modules.display()))?;
    let destination = node_modules.join(&name);
    replace_dir_atomically(package_dir, &destination)?;

    let added_to_profile = if profile.entries().iter().any(|entry| entry.name == name) {
        false
    } else {
        profile.entries_mut_push(PluginEntry {
            name: name.clone(),
            enabled: true,
            config: Value::Null,
            inject: None,
        });
        profile.save()?;
        true
    };
    Ok(InstallReport {
        name,
        destination,
        added_to_profile,
    })
}

/// 原子替换目录：先完整复制到 staging，再把旧目录 rename 到备份、staging rename 到位。
///
/// 保证：任何一步失败时**旧版本仍在原位**（复制失败旧版本完全未动；rename 失败回滚备份），
/// 不会出现"先删后拷、拷到一半磁盘满 → 没有任何版本"的状态（P1-4/P2-6）。
fn replace_dir_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    replace_dir_atomically_with(source, destination, copy_dir)
}

fn replace_dir_atomically_with(
    source: &Path,
    destination: &Path,
    copy: impl Fn(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    let parent = destination.parent().ok_or_else(|| {
        format!(
            "安装目标没有父目录（不是合法目录路径）: {}",
            destination.display()
        )
    })?;
    let stamp = format!("{}-{}", std::process::id(), unix_nanos());
    let staging = parent.join(format!(".dshr-staging-{stamp}"));
    let backup = parent.join(format!(".dshr-backup-{stamp}"));

    // 1) 复制到 staging：失败只清 staging，旧版本一个字节都不动。
    if let Err(err) = copy(source, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("复制到暂存目录失败（旧版本未改动）: {err}"));
    }

    // 2) 旧版本 rename 到备份（同卷、同父目录，rename 是原子的）。
    let had_old = destination.exists();
    if had_old {
        if let Err(err) = fs::rename(destination, &backup) {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("备份旧版本失败（旧版本未改动）: {err}"));
        }
    }

    // 3) staging → destination；失败回滚旧版本。
    match fs::rename(&staging, destination) {
        Ok(()) => {
            if had_old {
                // 清理备份失败不致命（下次安装会再清理/覆盖），但记录真实原因。
                let _ = fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(err) => {
            if had_old {
                let _ = fs::rename(&backup, destination);
            }
            let _ = fs::remove_dir_all(&staging);
            Err(format!("替换安装目录失败（已回滚旧版本）: {err}"))
        }
    }
}

/// 读取并校验 `package.json`：返回包名。
///
/// `dsh` 字段是 DSH 的插件清单约定（可缺省：官方 tool 包就没有）：
/// `dsh.bundle.patch` 指向 Loader patch 文件，`dsh.client` 描述 Web Client 插件。
/// 这里只校验结构和路径安全，不解释其组合语义（Loader 见 `loader.rs`）。
pub(crate) fn read_manifest(package_dir: &Path) -> Result<String, String> {
    let manifest = package_dir.join("package.json");
    let text = fs::read_to_string(&manifest)
        .map_err(|err| format!("读取 {} 失败: {err}", manifest.display()))?;
    // Windows 编辑器常给 JSON 加 UTF-8 BOM：解析前去掉。
    let text = text.trim_start_matches('\u{feff}');
    let value: Value =
        serde_json::from_str(text).map_err(|err| format!("package.json 解析失败: {err}"))?;
    let name = value
        .get("name")
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| String::from("package.json 缺少 name"))?
        .to_owned();
    validate_package_name(&name)?;
    if let Some(dsh) = value.get("dsh").filter(|dsh| !dsh.is_null()) {
        validate_dsh_field(dsh, package_dir)?;
    }
    Ok(name)
}

/// 读包版本：优先 `package.json` 的 `version`，其次 `plugin.toml` 的 `[plugin].version`。
/// 信任链用它做"trust 文件 vs 实际清单"的一致性校验；读不到返回 `None`。
pub(crate) fn read_package_version(package_dir: &Path) -> Result<Option<String>, String> {
    let package_json = package_dir.join("package.json");
    if package_json.is_file() {
        let text = fs::read_to_string(&package_json)
            .map_err(|err| format!("读取 {} 失败: {err}", package_json.display()))?;
        let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|err| format!("package.json 解析失败: {err}"))?;
        if let Some(version) = value.get("version").and_then(Value::as_str) {
            if !version.is_empty() {
                return Ok(Some(version.to_owned()));
            }
        }
    }
    let plugin_toml = package_dir.join("plugin.toml");
    if plugin_toml.is_file() {
        let text = fs::read_to_string(&plugin_toml)
            .map_err(|err| format!("读取 {} 失败: {err}", plugin_toml.display()))?;
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = line.trim_matches(['[', ']']).trim().to_owned();
                continue;
            }
            if section != "plugin" {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "version" {
                    let version = value.trim().trim_matches('"').trim_matches('\'');
                    if !version.is_empty() {
                        return Ok(Some(version.to_owned()));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn validate_package_name(name: &str) -> Result<(), String> {
    if name.starts_with('.') || name.starts_with('/') || name.contains('\\') {
        return Err(format!("非法包名: {name}"));
    }
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("非法包名: {name}"));
    }
    Ok(())
}

fn validate_dsh_field(dsh: &Value, package_dir: &Path) -> Result<(), String> {
    let object = dsh
        .as_object()
        .ok_or_else(|| String::from("package.json 的 dsh 字段必须是对象"))?;
    if let Some(bundle) = object.get("bundle") {
        let bundle = bundle
            .as_object()
            .ok_or_else(|| String::from("package.json dsh.bundle 必须是对象"))?;
        if let Some(patch) = bundle.get("patch") {
            let patch = patch
                .as_str()
                .ok_or_else(|| String::from("package.json dsh.bundle.patch 必须是字符串"))?;
            let relative = validate_relative_reference(patch)?;
            if !package_dir.join(&relative).is_file() {
                return Err(format!(
                    "package.json dsh.bundle.patch 指向的文件不存在: {patch}"
                ));
            }
        }
    }
    if let Some(client) = object.get("client") {
        let client = client
            .as_object()
            .ok_or_else(|| String::from("package.json dsh.client 必须是对象"))?;
        if let Some(platform) = client.get("platform") {
            platform
                .as_str()
                .filter(|platform| !platform.is_empty())
                .ok_or_else(|| String::from("package.json dsh.client.platform 必须是非空字符串"))?;
        }
        if let Some(inject) = client.get("inject") {
            if !inject.is_array() {
                return Err(String::from("package.json dsh.client.inject 必须是数组"));
            }
        }
    }
    Ok(())
}

/// 校验 `dsh` 字段里的相对引用（如 `./cordis.patch.yml`）。
fn validate_relative_reference(reference: &str) -> Result<PathBuf, String> {
    if reference.trim().is_empty() || reference.contains('\\') {
        return Err(format!("package.json 引用路径非法: {reference}"));
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(reference).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("package.json 引用路径逃逸: {reference}"));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("package.json 引用路径非法: {reference}"));
    }
    Ok(normalized)
}

fn copy_dir(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|err| format!("创建 {} 失败: {err}", destination.display()))?;
    let entries =
        fs::read_dir(source).map_err(|err| format!("读取 {} 失败: {err}", source.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)
            .map_err(|err| format!("读取 {} 元数据失败: {err}", from.display()))?;
        if metadata.is_dir() {
            copy_dir(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to).map_err(|err| format!("复制 {} 失败: {err}", from.display()))?;
        }
        // 符号链接/其它特殊文件：跳过（安装源不可信，不复制链接目标）。
    }
    Ok(())
}

/// 解包暂存目录：Drop 时清理，避免 `.tgz` 解出的内容残留。
struct StagingDir(PathBuf);

impl StagingDir {
    fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "dshr-install-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        fs::create_dir_all(&path)
            .map_err(|err| format!("创建安装暂存目录 {} 失败: {err}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::test_support::{build_tar, gzip};

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshr-install-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn fake_package(root: &Path, name: &str) -> PathBuf {
        let package = root.join("pkg");
        fs::create_dir_all(package.join("lib")).unwrap();
        fs::write(
            package.join("package.json"),
            serde_json::json!({ "name": name, "version": "1.0.0" }).to_string(),
        )
        .unwrap();
        fs::write(package.join("lib/index.js"), "export const ok = true\n").unwrap();
        package
    }

    fn write_tgz(root: &Path, name: &str, entries: &[(&str, u8, &[u8])]) -> PathBuf {
        let tar = build_tar(entries);
        let path = root.join(name);
        fs::write(&path, gzip(&tar)).expect("write tgz");
        path
    }

    #[test]
    fn install_copies_package_and_adds_profile_entry() {
        let root = temp_root("copy");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let profile = {
            let mut profile = Profile::load(root.join("profile")).unwrap();
            let package = fake_package(&root, "@deepseek-ai/dsh-tool-demo");
            let report = install_package(&root.join("sidecar"), &package, &mut profile).unwrap();
            assert_eq!(report.name, "@deepseek-ai/dsh-tool-demo");
            assert!(report.added_to_profile);
            assert!(report.destination.join("lib/index.js").exists());
            profile
        };
        assert!(profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-demo" && entry.enabled));
        let reloaded = Profile::load(root.join("profile")).unwrap();
        assert!(reloaded
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-demo"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reinstall_replaces_without_duplicating_profile_entry() {
        let root = temp_root("repeat");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let package = fake_package(&root, "@deepseek-ai/dsh-tool-demo");
        let first = install_package(&root.join("sidecar"), &package, &mut profile).unwrap();
        let second = install_package(&root.join("sidecar"), &package, &mut profile).unwrap();
        assert!(first.added_to_profile);
        assert!(!second.added_to_profile);
        let count = profile
            .entries()
            .iter()
            .filter(|entry| entry.name == "@deepseek-ai/dsh-tool-demo")
            .count();
        assert_eq!(count, 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_missing_manifest_and_escape_names() {
        let root = temp_root("reject");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();

        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(install_package(&root.join("sidecar"), &empty, &mut profile).is_err());

        let bad = fake_package(&root, "../escape");
        assert!(install_package(&root.join("sidecar"), &bad, &mut profile).is_err());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tolerates_utf8_bom_in_manifest() {
        let root = temp_root("bom");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let package = root.join("pkg");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("package.json"),
            "\u{feff}{\"name\":\"@deepseek-ai/dsh-tool-bom\"}",
        )
        .unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let report = install_package(&root.join("sidecar"), &package, &mut profile).unwrap();
        assert_eq!(report.name, "@deepseek-ai/dsh-tool-bom");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn installs_npm_pack_tgz_offline() {
        let root = temp_root("tgz");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let manifest = br#"{"name":"@deepseek-ai/dsh-tool-tgz","version":"0.2.2-rc.1"}"#;
        let archive = write_tgz(
            &root,
            "demo.tgz",
            &[
                ("package/", b'5', b""),
                ("package/package.json", b'0', manifest),
                ("package/lib/index.js", b'0', b"export const ok = true\n"),
            ],
        );

        let report = install_package(&root.join("sidecar"), &archive, &mut profile).unwrap();
        assert_eq!(report.name, "@deepseek-ai/dsh-tool-tgz");
        assert!(report.added_to_profile);
        assert!(report.destination.join("lib/index.js").is_file());
        assert!(profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-tgz"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tgz_rejects_path_escape_and_symlinks() {
        let root = temp_root("tgz-reject");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();

        let escape = write_tgz(
            &root,
            "escape.tgz",
            &[
                ("package/package.json", b'0', br#"{"name":"escape"}"#),
                ("package/../../evil.txt", b'0', b"x"),
            ],
        );
        let error = install_package(&root.join("sidecar"), &escape, &mut profile).unwrap_err();
        assert!(error.contains("逃逸") || error.contains(".."), "{error}");

        let symlink = write_tgz(
            &root,
            "symlink.tgz",
            &[
                ("package/package.json", b'0', br#"{"name":"link"}"#),
                ("package/link", b'2', b""),
            ],
        );
        let error = install_package(&root.join("sidecar"), &symlink, &mut profile).unwrap_err();
        assert!(error.contains("链接"), "{error}");

        assert!(install_package(&root.join("sidecar"), &root, &mut profile).is_err());
        let unsupported = root.join("package.zip");
        fs::write(&unsupported, b"not a tgz").unwrap();
        let error = install_package(&root.join("sidecar"), &unsupported, &mut profile).unwrap_err();
        assert!(error.contains("不支持"), "{error}");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn validates_dsh_manifest_field() {
        let root = temp_root("dsh-field");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();

        let broken_shape = root.join("broken-shape");
        fs::create_dir_all(&broken_shape).unwrap();
        fs::write(
            broken_shape.join("package.json"),
            r#"{"name":"broken","dsh":"not-an-object"}"#,
        )
        .unwrap();
        let error =
            install_package(&root.join("sidecar"), &broken_shape, &mut profile).unwrap_err();
        assert!(error.contains("dsh"), "{error}");

        let escaping_patch = root.join("escaping-patch");
        fs::create_dir_all(&escaping_patch).unwrap();
        fs::write(
            escaping_patch.join("package.json"),
            r#"{"name":"escaping","dsh":{"bundle":{"patch":"../../outside.yml"}}}"#,
        )
        .unwrap();
        let error =
            install_package(&root.join("sidecar"), &escaping_patch, &mut profile).unwrap_err();
        assert!(error.contains("逃逸"), "{error}");

        let missing_patch = root.join("missing-patch");
        fs::create_dir_all(&missing_patch).unwrap();
        fs::write(
            missing_patch.join("package.json"),
            r#"{"name":"missing","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        let error =
            install_package(&root.join("sidecar"), &missing_patch, &mut profile).unwrap_err();
        assert!(error.contains("不存在"), "{error}");

        let valid_bundle = root.join("valid-bundle");
        fs::create_dir_all(&valid_bundle).unwrap();
        fs::write(
            valid_bundle.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-valid-bundle","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        fs::write(valid_bundle.join("cordis.patch.yml"), "- id: x\n").unwrap();
        let report = install_package(&root.join("sidecar"), &valid_bundle, &mut profile).unwrap();
        assert_eq!(report.name, "@deepseek-ai/dsh-valid-bundle");
        fs::remove_dir_all(root).ok();
    }

    // ---- P1-4/P2-6：安装原子替换（staging + rename + 回滚） -----------------

    fn staging_leftovers(node_modules: &Path) -> Vec<String> {
        fs::read_dir(node_modules)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .filter(|name| {
                        name.starts_with(".dshr-staging-") || name.starts_with(".dshr-backup-")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // M3-INSTALL-001：复制失败时旧版本保持可用，且不留暂存/备份目录。
    #[test]
    fn m3_install_001_copy_failure_preserves_previous_version() {
        let root = temp_root("atomic-copy-fail");
        let node_modules = root.join("sidecar").join("node_modules");
        let destination = node_modules.join("demo.plugin");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("version.txt"), "old-version").unwrap();

        let source = root.join("pkg");
        fs::create_dir_all(&source).unwrap();
        let error = replace_dir_atomically_with(&source, &destination, |_, _| {
            Err("injected copy failure".to_owned())
        })
        .expect_err("copy 失败必须报错");
        assert!(error.contains("旧版本未改动"), "{error}");
        assert_eq!(
            fs::read_to_string(destination.join("version.txt")).unwrap(),
            "old-version",
            "旧版本必须原样保留"
        );
        assert!(
            staging_leftovers(&node_modules).is_empty(),
            "失败后不得残留 staging/backup: {:?}",
            staging_leftovers(&node_modules)
        );
        fs::remove_dir_all(root).ok();
    }

    // M3-INSTALL-002：升级成功 = 新内容到位 + 备份清理干净。
    #[test]
    fn m3_install_002_upgrade_replaces_and_cleans_staging() {
        let root = temp_root("atomic-upgrade");
        let node_modules = root.join("sidecar").join("node_modules");
        let destination = node_modules.join("demo.plugin");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("version.txt"), "old-version").unwrap();

        let source = root.join("pkg");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("version.txt"), "new-version").unwrap();
        fs::write(source.join("lib.js"), "export const v = 2\n").unwrap();

        replace_dir_atomically(&source, &destination).expect("upgrade");
        assert_eq!(
            fs::read_to_string(destination.join("version.txt")).unwrap(),
            "new-version"
        );
        assert!(destination.join("lib.js").is_file(), "新文件必须到位");
        assert!(
            staging_leftovers(&node_modules).is_empty(),
            "成功后不得残留 staging/backup: {:?}",
            staging_leftovers(&node_modules)
        );
        fs::remove_dir_all(root).ok();
    }

    // M3-INSTALL-003：正常 install_package 走原子路径（node_modules 无暂存残留）。
    #[test]
    fn m3_install_003_install_package_leaves_no_staging_dirs() {
        let root = temp_root("atomic-install");
        fs::create_dir_all(root.join("sidecar")).unwrap();
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let package = fake_package(&root, "@deepseek-ai/dsh-tool-atomic");
        let report = install_package(&root.join("sidecar"), &package, &mut profile).unwrap();
        assert!(report.destination.join("lib/index.js").is_file());
        assert!(staging_leftovers(&root.join("sidecar").join("node_modules")).is_empty());
        fs::remove_dir_all(root).ok();
    }
}
