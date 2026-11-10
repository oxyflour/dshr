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
//!     "config": { "read": true, "write": false, ... } }
//! ] }
//! ```
//!
//! `sidecar_json()` 输出 `DSHR_DSH_PLUGINS` 需要的数组（只含 enabled 项）。
//! Loader YAML（`cordis.yml` / `cordis.patch.yml`）是下一步，不在这一层。

#![forbid(unsafe_code)]

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

/// 插件 profile（文件 + 内存条目）。
#[derive(Debug, Clone)]
pub struct Profile {
    path: PathBuf,
    entries: Vec<PluginEntry>,
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
            });
        }
        let profile = Self {
            path,
            entries: default_entries(),
        };
        profile.save()?;
        Ok(profile)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 内存中的默认 profile（加载失败时的兜底，不落盘）。
    pub fn defaults(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("plugins.json"),
            entries: default_entries(),
        }
    }

    pub fn entries(&self) -> &[PluginEntry] {
        &self.entries
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

    pub fn save(&self) -> Result<(), String> {
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
                "write": false,
                "edit": false,
                "applyPatch": false,
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
        };
        let json: Value = serde_json::from_str(&profile.sidecar_json()).expect("json");
        let array = json.as_array().expect("array");
        let fs_entry = array
            .iter()
            .find(|entry| entry["name"] == "@deepseek-ai/dsh-tool-fs")
            .expect("fs");
        assert_eq!(fs_entry["config"]["write"], false);
        assert_eq!(fs_entry["inject"][0], "tools");
    }

    #[test]
    fn unknown_plugin_toggle_returns_false() {
        let dir = temp_dir("unknown");
        let mut profile = Profile::load(&dir).expect("load");
        assert!(!profile.set_enabled("not-installed", false));
        fs::remove_dir_all(dir).ok();
    }
}
