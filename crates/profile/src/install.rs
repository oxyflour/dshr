//! 离线安装插件包：复制到 sidecar 的 `node_modules`，并写入 profile。
//!
//! 这是 M2 的最小"安装"语义：**不做 npm 网络解析**，只接受一个已经在本地的
//! 包目录（例如从本机 DSH 的 `app.asar` 里抽出来，或用户自己的插件源码目录）。
//! 包目录必须有 `package.json` 且带 `name`，复制目标是
//! `<sidecar>/node_modules/<name>`；随后在 profile 里追加 enabled 条目。
//!
//! 安全边界：包名不允许 `..` / 绝对路径 / 空段；目标已存在时先删再复制；
//! 复制是递归的、只跟随目录与文件（不处理符号链接）。

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::{PluginEntry, Profile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub name: String,
    pub destination: PathBuf,
    pub added_to_profile: bool,
}

/// 安装一个本地包目录。
pub fn install_package(
    sidecar_dir: &Path,
    package_dir: &Path,
    profile: &mut Profile,
) -> Result<InstallReport, String> {
    if !profile.can_edit() {
        return Err(String::from("Loader 模式的插件列表只读，不能安装"));
    }
    let manifest = package_dir.join("package.json");
    let text = fs::read_to_string(&manifest)
        .map_err(|err| format!("读取 {} 失败: {err}", manifest.display()))?;
    // Windows 编辑器常给 JSON 加 UTF-8 BOM：解析前去掉。
    let text = text.trim_start_matches('\u{feff}');
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("package.json 解析失败: {err}"))?;
    let name = value
        .get("name")
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| String::from("package.json 缺少 name"))?
        .to_owned();
    validate_package_name(&name)?;

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
            config: serde_json::Value::Null,
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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
}
