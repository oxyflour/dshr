//! 插件 profile：启停状态 + 配置持久化 + 交给 sidecar 的插件清单。
//!
//! 文件：`<dir>/plugins.json`（默认 `.tmp/profile/plugins.json`，可用
//! `DSHR_PROFILE_DIR` 覆盖）。结构尽量贴近 DSH 的插件条目：
//!
//! ```json
//! { "plugins": [
//!   { "name": "@deepseek-ai/dsh-tool-workspace-dependencies", "enabled": true,
//!     "config": { "source": "..." } },
//!   { "name": "@deepseek-ai/dsh-tool-fs", "enabled": true,
//!     "inject": ["tools","fs","systemPrompt"],
//!     "config": { "read": true, "write": true, ... } }
//! ] }
//! ```
//!
//! `sidecar_json()` 输出 `DSHR_DSH_PLUGINS` 需要的数组（只含 enabled 项）。
//! Loader YAML（`cordis.yml` / `cordis.patch.yml` + include/group 展开）见 [`loader`]。

#![forbid(unsafe_code)]

pub mod archive;
pub mod install;
pub mod headless;
pub mod loader;
pub mod trust;
pub mod uninstall;

pub use install::{install_package, InstallReport};
pub use trust::{
    add_trust_key, install_package_trusted, package_digest, read_installed_meta, remove_trust_key,
    revoke_trust_key, rotate_trust_key, verify_package, AuditRecord, InstallPreview, InstalledMeta,
    KeyAuditRecord, KeyChangeReport, SignatureStatus, TrustError, TrustKey, TrustPolicy,
    TrustReport, TrustStore, TrustedInstallReport, TRUST_FILE,
};
pub use uninstall::{uninstall_package, UninstallReport};

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一个可启停的插件条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<Vec<String>>,
}

fn default_enabled() -> bool {
    true
}

/// profile 来源：普通 plugins.json，或 Loader YAML（只读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    File,
    Loader,
}

/// 插件 profile（文件 + 内存条目）。
#[derive(Debug, Clone)]
pub struct Profile {
    path: PathBuf,
    entries: Vec<PluginEntry>,
    source: ProfileSource,
}

impl Profile {
    /// 读取 profile；文件不存在时生成默认条目并落盘。
    pub fn load(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        let path = dir.join("plugins.json");
        if path.exists() {
            let text = fs::read_to_string(&path).map_err(|err| format!("读取失败: {err}"))?;
            let parsed: ProfileFile =
                serde_json::from_str(&text).map_err(|err| format!("解析失败: {err}"))?;
            return Ok(Self {
                path,
                entries: parsed.plugins,
                source: ProfileSource::File,
            });
        }
        let profile = Self {
            path,
            entries: default_entries(),
            source: ProfileSource::File,
        };
        profile.save()?;
        Ok(profile)
    }

    /// 从 Loader bundle + patch 文件构建只读 profile。
    ///
    /// `include` 按声明文件所在目录递归展开；group 展开为顺序条目；
    /// `!!js` 不求值（诊断见 [`loader`]）。派生结果不落盘。
    pub fn from_loader(
        dir: impl Into<PathBuf>,
        bundle: &Path,
        patches: &[PathBuf],
    ) -> Result<Self, String> {
        let (entries, warnings) = loader::entries_from_loader_files(bundle, patches)?;
        for warning in warnings {
            eprintln!("[profile] loader: {warning}");
        }
        Ok(Self {
            path: dir.into().join("plugins.json"),
            entries,
            source: ProfileSource::Loader,
        })
    }

    pub fn source(&self) -> ProfileSource {
        self.source
    }

    /// Loader 模式下不写 plugins.json（避免把派生结果固化成第二真相）。
    pub fn can_edit(&self) -> bool {
        self.source == ProfileSource::File
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 内存中的默认 profile（加载失败时的兜底，不落盘）。
    pub fn defaults(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("plugins.json"),
            entries: default_entries(),
            source: ProfileSource::File,
        }
    }

    pub fn entries(&self) -> &[PluginEntry] {
        &self.entries
    }

    /// 追加条目（安装流程用；不去重，由安装器保证）。
    pub fn entries_mut_push(&mut self, entry: PluginEntry) {
        self.entries.push(entry);
    }

    /// 移除同名的第一条插件条目（卸载流程用）；返回是否移除。不落盘。
    ///
    /// 正常 profile 不会出现同名重复（安装器去重、Loader 派生也可能重复），
    /// 这里只删一条，保留其余条目，避免卸载误伤用户手写的同名配置。
    pub fn remove_entry(&mut self, name: &str) -> bool {
        if let Some(index) = self.entries.iter().position(|entry| entry.name == name) {
            self.entries.remove(index);
            true
        } else {
            false
        }
    }

    /// 启用/禁用；未知插件返回 false。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.name == name) {
            entry.enabled = enabled;
            true
        } else {
            false
        }
    }

    pub fn set_config(&mut self, name: &str, config: Value) -> bool {
        if !self.can_edit() {
            return false;
        }
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.name == name) else {
            return false;
        };
        entry.config = config;
        true
    }

    pub fn save(&self) -> Result<(), String> {
        if self.source == ProfileSource::Loader {
            return Err(String::from(
                "Loader 模式的 profile 是派生视图，不写 plugins.json",
            ));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("创建目录失败: {err}"))?;
        }
        let file = ProfileFile {
            plugins: self.entries.clone(),
        };
        let text =
            serde_json::to_string_pretty(&file).map_err(|err| format!("序列化失败: {err}"))?;
        fs::write(&self.path, text).map_err(|err| format!("写入失败: {err}"))
    }

    /// `DSHR_DSH_PLUGINS` 需要的 JSON 数组（只含 enabled 项）。
    pub fn sidecar_json(&self) -> String {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| {
                let mut object = serde_json::json!({
                    "name": entry.name,
                    "config": entry.config,
                });
                if let Some(inject) = &entry.inject {
                    object["inject"] = serde_json::json!(inject);
                }
                object
            })
            .collect();
        serde_json::Value::Array(entries).to_string()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    plugins: Vec<PluginEntry>,
}

/// 默认插件集合：与 M2 之前的硬编码列表一致。
pub fn default_entries() -> Vec<PluginEntry> {
    vec![
        PluginEntry {
            name: String::from("@deepseek-ai/dsh-tool-workspace-dependencies"),
            enabled: true,
            config: serde_json::json!({ "source": default_primary_runtime() }),
            inject: None,
        },
        PluginEntry {
            name: String::from("@deepseek-ai/dsh-tool-fs"),
            enabled: true,
            config: serde_json::json!({
                "read": true,
                "readImage": false,
                // 写工具默认值：T3 的 sidecar gating 是"host_caps 含 fs.write 且
                // config 未显式 false 才装载"；默认 true 让写工具在具备 host
                // capacity 时可用，没有 capacity 时 sidecar 仍不会注册。
                "write": true,
                "edit": true,
                "applyPatch": true,
            }),
            inject: Some(vec![
                String::from("tools"),
                String::from("fs"),
                String::from("systemPrompt"),
            ]),
        },
    ]
}

/// 推断 DSH primary-runtime 目录（与 sidecar 的默认逻辑一致）。
pub fn default_primary_runtime() -> String {
    if let Ok(source) = std::env::var("DSHR_DSH_SOURCE") {
        if !source.is_empty() {
            return source;
        }
    }
    std::env::var("LOCALAPPDATA")
        .map(|local| {
            Path::new(&local)
                .join("Programs")
                .join("DeepSeek Harness")
                .join("resources")
                .join("runtime")
                .join("primary-runtime")
                .display()
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshr-profile-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn load_creates_defaults_and_persists() {
        let dir = temp_dir("defaults");
        let profile = Profile::load(&dir).expect("load");
        assert_eq!(profile.entries().len(), 2);
        assert!(profile.entries().iter().all(|entry| entry.enabled));
        assert!(dir.join("plugins.json").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn disable_removes_entry_from_sidecar_json() {
        let dir = temp_dir("toggle");
        let mut profile = Profile::load(&dir).expect("load");
        assert!(profile.set_enabled("@deepseek-ai/dsh-tool-fs", false));
        let json = profile.sidecar_json();
        assert!(!json.contains("dsh-tool-fs"), "{json}");
        assert!(json.contains("dsh-tool-workspace-dependencies"), "{json}");
        profile.save().expect("save");
        let reloaded = Profile::load(&dir).expect("reload");
        let fs_entry = reloaded
            .entries()
            .iter()
            .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs")
            .expect("fs entry");
        assert!(!fs_entry.enabled);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sidecar_json_keeps_inject_and_config() {
        let profile = Profile {
            path: PathBuf::from("unused"),
            entries: default_entries(),
            source: ProfileSource::File,
        };
        let json: Value = serde_json::from_str(&profile.sidecar_json()).expect("json");
        let array = json.as_array().expect("array");
        let fs_entry = array
            .iter()
            .find(|entry| entry["name"] == "@deepseek-ai/dsh-tool-fs")
            .expect("fs");
        assert_eq!(fs_entry["config"]["write"], true);
        assert_eq!(fs_entry["inject"][0], "tools");
    }

    #[test]
    fn default_fs_entry_enables_write_tools() {
        // T3 的 sidecar gating：host_caps 含 fs.write 且 config 未显式 false 才装载
        // 写工具。默认值必须是 true，否则官方 write/edit/applyPatch 永远装不上。
        let entries = default_entries();
        let fs_entry = entries
            .iter()
            .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs")
            .expect("fs entry");
        assert_eq!(fs_entry.config["read"], true);
        assert_eq!(fs_entry.config["write"], true);
        assert_eq!(fs_entry.config["edit"], true);
        assert_eq!(fs_entry.config["applyPatch"], true);
        assert_eq!(fs_entry.config["readImage"], false);
    }

    #[test]
    fn unknown_plugin_toggle_returns_false() {
        let dir = temp_dir("unknown");
        let mut profile = Profile::load(&dir).expect("load");
        assert!(!profile.set_enabled("not-installed", false));
        fs::remove_dir_all(dir).ok();
    }

    // M4-UNINSTALL-008：remove_entry 只删同名第一条，不触碰其它条目，并可落盘。
    #[test]
    fn m4_uninstall_008_remove_entry_is_targeted_and_persisted() {
        let dir = temp_dir("remove-entry");
        let mut profile = Profile::load(&dir).expect("load");
        profile.entries_mut_push(PluginEntry {
            name: String::from("m4.entry"),
            enabled: true,
            config: Value::Null,
            inject: None,
        });
        profile.entries_mut_push(PluginEntry {
            name: String::from("m4.entry"),
            enabled: false,
            config: Value::Null,
            inject: None,
        });
        profile.save().expect("save");

        assert!(profile.remove_entry("m4.entry"));
        assert_eq!(
            profile
                .entries()
                .iter()
                .filter(|entry| entry.name == "m4.entry")
                .count(),
            1,
            "同名重复只删一条"
        );
        assert!(profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-fs"));
        assert!(!profile.remove_entry("m4.missing"));
        profile.save().expect("save");
        let reloaded = Profile::load(&dir).expect("reload");
        assert_eq!(
            reloaded
                .entries()
                .iter()
                .filter(|entry| entry.name == "m4.entry")
                .count(),
            1
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn loader_profile_is_read_only_and_derives_entries() {
        let dir = temp_dir("loader");
        fs::create_dir_all(&dir).unwrap();
        let bundle = dir.join("cordis.yml");
        fs::write(
            &bundle,
            "- id: fs\n  name: '@deepseek-ai/dsh-tool-fs'\n  config:\n    read: true\n",
        )
        .unwrap();
        let patch = dir.join("cordis.patch.yml");
        fs::write(&patch, "- id: fs\n  disabled: true\n").unwrap();

        let profile = Profile::from_loader(&dir, &bundle, &[patch]).expect("loader");
        assert_eq!(profile.source(), ProfileSource::Loader);
        assert!(!profile.can_edit());
        assert_eq!(profile.entries().len(), 1);
        assert!(!profile.entries()[0].enabled);
        assert!(profile.save().is_err(), "loader profile 不落盘");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn loader_profile_expands_include_and_group() {
        // 集成测试：from_loader → entries_from_loader_files（include 基准 = bundle 目录）。
        let dir = temp_dir("loader-include");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(
            dir.join("sub/extra.yml"),
            "- id: extra\n  name: '@x/extra'\n  disabled: true\n",
        )
        .unwrap();
        fs::write(
            dir.join("cordis.yml"),
            "- include: sub/extra.yml\n- group: true\n  config:\n    - id: grouped\n      name: '@x/grouped'\n",
        )
        .unwrap();
        let patch = dir.join("cordis.patch.yml");
        fs::write(&patch, "- id: extra\n  disabled: false\n").unwrap();

        let profile =
            Profile::from_loader(&dir, &dir.join("cordis.yml"), &[patch]).expect("loader");
        let names: Vec<&str> = profile
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["@x/extra", "@x/grouped"]);
        assert!(
            profile.entries()[0].enabled,
            "patch 覆盖 include 条目的 disabled"
        );
        fs::remove_dir_all(dir).ok();
    }
}
