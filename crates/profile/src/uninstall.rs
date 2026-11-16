//! 插件卸载：原子移除安装目录 + profile 条目 + 审计（M4 W1）。
//!
//! 语义与 [`crate::install`] 对称，但顺序是反的：
//!
//! 1. 读已安装元数据（版本/摘要/签名状态用于审计）；
//! 2. 目录先 rename 到同父目录的 `.dshr-remove-*`，再删除；删除失败则 rename
//!    回原位，保证不会留下"删到一半"的插件目录（Windows 上被占用的文件尤其
//!    需要这一步）；
//! 3. 移除 profile 同名条目并落盘；
//! 4. 追加 `action: "uninstall"` 审计。
//!
//! 部分失败是可恢复的：目录已删除但 profile 落盘失败时返回错误并保留
//! "目录不存在、条目仍在"的状态，重跑卸载即可完成；审计失败不改变卸载结果，
//! 只在报告里给出 `audit_error` 供调用方展示。

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::install::{read_package_version, validate_package_name};
use crate::trust::{append_audit, read_installed_meta, AuditRecord};
use crate::Profile;

/// 卸载结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    pub name: String,
    pub version: Option<String>,
    pub digest_sha256: Option<String>,
    pub removed_dir: bool,
    pub removed_profile_entry: bool,
    /// 审计写入失败时的可读原因；卸载本身已经生效。
    pub audit_error: Option<String>,
}

/// 卸载一个已安装插件：`<sidecar>/node_modules/<name>` + profile 条目。
///
/// 只要目录或 profile 条目存在就执行；两者都不存在返回错误（避免把拼错的
/// 插件名当作"卸载成功"）。Loader 派生 profile 只读，直接拒绝。
pub fn uninstall_package(
    sidecar_dir: &Path,
    name: &str,
    profile: &mut Profile,
) -> Result<UninstallReport, String> {
    validate_package_name(name)?;
    if !profile.can_edit() {
        return Err(String::from("Loader 模式的插件列表只读，不能卸载"));
    }

    let destination = sidecar_dir.join("node_modules").join(name);
    let had_dir = destination.exists();
    let had_entry = profile.entries().iter().any(|entry| entry.name == name);
    if !had_dir && !had_entry {
        return Err(format!("插件未安装：{name}"));
    }

    let meta = read_installed_meta(&destination);
    let version = meta
        .as_ref()
        .map(|meta| meta.version.clone())
        .or_else(|| read_package_version(&destination).ok().flatten());
    let digest_sha256 = meta.as_ref().map(|meta| meta.digest_sha256.clone());

    let removed_dir = if had_dir {
        remove_dir_atomically(&destination)?;
        true
    } else {
        false
    };

    let removed_profile_entry = profile.remove_entry(name);
    if removed_profile_entry {
        profile
            .save()
            .map_err(|err| format!("插件目录已移除，但 profile 保存失败（可重跑卸载）: {err}"))?;
    }

    let signature_status = meta
        .as_ref()
        .map(|meta| meta.signature.status.clone())
        .unwrap_or_else(|| String::from("unknown"));
    let key_id = meta.as_ref().and_then(|meta| meta.signature.key_id.clone());
    let audit_error = append_audit(
        profile.path().parent().unwrap_or_else(|| Path::new(".")),
        &AuditRecord {
            ts: now_ms(),
            action: String::from("uninstall"),
            name: String::from(name),
            version: version.clone().unwrap_or_default(),
            digest_sha256: digest_sha256.clone().unwrap_or_default(),
            signature_status,
            key_id,
            decision: String::from("accepted"),
            reason: None,
            // 卸载没有"用户确认安装"的语义；保留字段以复用 v1 审计格式。
            user_confirmed: false,
            first_install: false,
            downgrade: false,
        },
    )
    .err()
    .map(|err| err.to_string());

    Ok(UninstallReport {
        name: String::from(name),
        version,
        digest_sha256,
        removed_dir,
        removed_profile_entry,
        audit_error,
    })
}

/// 原子移除目录：rename 到同父目录的临时名下再删除，失败则恢复原名。
fn remove_dir_atomically(destination: &Path) -> Result<(), String> {
    remove_dir_atomically_with(destination, |path| fs::remove_dir_all(path))
}

fn remove_dir_atomically_with(
    destination: &Path,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> Result<(), String> {
    if !destination.exists() {
        return Ok(());
    }
    let parent = destination.parent().ok_or_else(|| {
        format!(
            "卸载目标没有父目录（不是合法目录路径）: {}",
            destination.display()
        )
    })?;
    let stamp = format!("{}-{}", std::process::id(), unix_nanos());
    let trash = parent.join(format!(".dshr-remove-{stamp}"));

    fs::rename(destination, &trash).map_err(|err| {
        format!(
            "无法移除插件目录（可能正在被占用，插件保持原状）: {}: {err}",
            destination.display()
        )
    })?;

    if let Err(err) = remove(&trash) {
        // 删除阶段失败：恢复原名，保证"要么完整存在，要么完整消失"。
        return match fs::rename(&trash, destination) {
            Ok(()) => Err(format!(
                "删除插件目录失败（已恢复原目录）: {}: {err}",
                destination.display()
            )),
            Err(restore_err) => Err(format!(
                "删除插件目录失败，且恢复原目录也失败（残留 {}）: {err} / {restore_err}",
                trash.display()
            )),
        };
    }
    Ok(())
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{install_package_trusted, InstalledMeta, TrustPolicy, AUDIT_FILE};
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dshr-uninstall-{name}-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn fake_package(root: &Path, name: &str, version: &str) -> PathBuf {
        let package = root.join(format!("pkg-{}", name.replace(['/', '@', '.'], "_")));
        fs::create_dir_all(package.join("lib")).unwrap();
        fs::write(
            package.join("package.json"),
            serde_json::json!({ "name": name, "version": version }).to_string(),
        )
        .unwrap();
        fs::write(package.join("lib/index.js"), "export const ok = true\n").unwrap();
        package
    }

    fn install(root: &Path, profile: &mut Profile, name: &str, version: &str) -> PathBuf {
        let package = fake_package(root, name, version);
        // 走产品安装路径，才有 `.dshr-install.json`（版本/摘要用于卸载审计）。
        install_package_trusted(
            &root.join("sidecar"),
            &package,
            profile,
            &TrustPolicy::default(),
            |_| true,
        )
        .expect("install")
        .install
        .destination
    }

    // M4-UNINSTALL-001：安装后卸载 → 目录、profile 条目、元数据消失，审计含 uninstall。
    #[test]
    fn m4_uninstall_001_removes_dir_profile_and_audits() {
        let root = temp_root("basic");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let destination = install(&root, &mut profile, "m4.demo", "0.1.0");
        assert!(destination.exists());

        let report =
            uninstall_package(&root.join("sidecar"), "m4.demo", &mut profile).expect("uninstall");
        assert!(report.removed_dir);
        assert!(report.removed_profile_entry);
        assert_eq!(report.version.as_deref(), Some("0.1.0"));
        assert!(report.digest_sha256.is_some());
        assert!(report.audit_error.is_none(), "{:?}", report.audit_error);
        assert!(!destination.exists(), "安装目录必须删除");
        assert!(!profile
            .entries()
            .iter()
            .any(|entry| entry.name == "m4.demo"));
        assert!(!root
            .join("sidecar/node_modules")
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".dshr-remove-")));
        let audit = fs::read_to_string(root.join("profile").join(AUDIT_FILE)).unwrap();
        let last: AuditRecord = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert_eq!(last.action, "uninstall");
        assert_eq!(last.decision, "accepted");
        assert_eq!(last.name, "m4.demo");
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-002：目录与 profile 条目都不存在 → 明确报错，不写审计。
    #[test]
    fn m4_uninstall_002_missing_plugin_is_error() {
        let root = temp_root("missing");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let error =
            uninstall_package(&root.join("sidecar"), "m4.not-installed", &mut profile).unwrap_err();
        assert!(error.contains("未安装"), "{error}");
        assert!(!root.join("profile").join(AUDIT_FILE).exists());
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-003：只剩 profile 条目（目录已被手动删）也能收敛。
    #[test]
    fn m4_uninstall_003_removes_orphan_profile_entry() {
        let root = temp_root("orphan");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        profile.entries_mut_push(crate::PluginEntry {
            name: String::from("m4.orphan"),
            enabled: true,
            config: serde_json::Value::Null,
            inject: None,
        });
        profile.save().unwrap();

        let report =
            uninstall_package(&root.join("sidecar"), "m4.orphan", &mut profile).expect("uninstall");
        assert!(!report.removed_dir);
        assert!(report.removed_profile_entry);
        assert!(!profile
            .entries()
            .iter()
            .any(|entry| entry.name == "m4.orphan"));
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-004：删除阶段失败 → 目录恢复原名，不出现半删状态。
    #[test]
    fn m4_uninstall_004_removal_failure_restores_directory() {
        let root = temp_root("restore");
        let node_modules = root.join("sidecar/node_modules");
        let destination = node_modules.join("m4.demo");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "keep").unwrap();

        let error = remove_dir_atomically_with(&destination, |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
        })
        .expect_err("删除失败必须报错");
        assert!(error.contains("已恢复原目录"), "{error}");
        assert!(destination.join("keep.txt").is_file());
        assert!(!node_modules.read_dir().unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".dshr-remove-")));
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-005：卸载一个插件不影响其它插件的目录与 profile 条目。
    #[test]
    fn m4_uninstall_005_preserves_other_plugins() {
        let root = temp_root("siblings");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let keep = install(&root, &mut profile, "m4.keep", "0.1.0");
        let remove = install(&root, &mut profile, "m4.remove", "0.1.0");

        uninstall_package(&root.join("sidecar"), "m4.remove", &mut profile).expect("uninstall");
        assert!(!remove.exists());
        assert!(keep.exists(), "其它插件目录必须保留");
        assert!(profile
            .entries()
            .iter()
            .any(|entry| entry.name == "m4.keep"));
        assert!(!profile
            .entries()
            .iter()
            .any(|entry| entry.name == "m4.remove"));
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-006：Loader 派生 profile 只读，拒绝卸载且不动目录。
    #[test]
    fn m4_uninstall_006_loader_profile_is_read_only() {
        let root = temp_root("loader");
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("cordis.yml");
        fs::write(&bundle, "- id: demo\n  name: 'm4.loader'\n").unwrap();
        let mut profile = Profile::from_loader(&root, &bundle, &[]).unwrap();
        let destination = root.join("sidecar/node_modules/m4.loader");
        fs::create_dir_all(&destination).unwrap();

        let error =
            uninstall_package(&root.join("sidecar"), "m4.loader", &mut profile).unwrap_err();
        assert!(error.contains("只读"), "{error}");
        assert!(destination.exists());
        fs::remove_dir_all(root).ok();
    }

    // M4-UNINSTALL-007：审计里的版本/摘要来自安装元数据（不是重新读包）。
    #[test]
    fn m4_uninstall_007_audit_uses_installed_metadata() {
        let root = temp_root("meta");
        let mut profile = Profile::load(root.join("profile")).unwrap();
        let destination = install(&root, &mut profile, "m4.meta", "1.2.3");
        let meta: InstalledMeta = read_installed_meta(&destination).expect("meta");

        uninstall_package(&root.join("sidecar"), "m4.meta", &mut profile).expect("uninstall");
        let audit = fs::read_to_string(root.join("profile").join(AUDIT_FILE)).unwrap();
        let last: AuditRecord = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert_eq!(last.version, meta.version);
        assert_eq!(last.digest_sha256, meta.digest_sha256);
        assert_eq!(last.signature_status, meta.signature.status);
        fs::remove_dir_all(root).ok();
    }
}
