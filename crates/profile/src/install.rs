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
//! - 目标已存在时先删再复制（等于升级）；
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
    if package.is_dir() {
        return install_dir(sidecar_dir, package, profile);
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
    install_dir(sidecar_dir, &unpacked.root, profile)
}

fn install_dir(
    sidecar_dir: &Path,
    package_dir: &Path,
    profile: &mut Profile,
) -> Result<InstallReport, String> {
    let name = read_manifest(package_dir)?;

    let node_modules = sidecar_dir.join("node_modules");
    let destination = node_modules.join(&name);
    if destination.exists() {
        fs::remove_dir_all(&destination).map_err(|err| format!("清理旧版本失败: {err}"))?;
    }
    copy_dir(package_dir, &destination)?;

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

/// 读取并校验 `package.json`：返回包名。
///
/// `dsh` 字段是 DSH 的插件清单约定（可缺省：官方 tool 包就没有）：
/// `dsh.bundle.patch` 指向 Loader patch 文件，`dsh.client` 描述 Web Client 插件。
/// 这里只校验结构和路径安全，不解释其组合语义（Loader 见 `loader.rs`）。
fn read_manifest(package_dir: &Path) -> Result<String, String> {
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
}
