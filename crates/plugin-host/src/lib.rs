//! Native Plugin 宿主：加载、原子提交声明、卸载清理、调用分发。
//!
//! 这是 CPP-v1 的第一条真实路径（`rust-static`）。它目前是**最小生命周期**，
//! 不含 Service 依赖图与 Effect 撤销——那部分按 ADR-0002 交给 `jincdream/cordis-rs`，
//! 本文件里的 `PluginHost` 将作为它的适配层外壳，对外契约不变。
//!
//! 保证：
//! 1. `apply` 失败 → 本次声明**一条都不落库**（原子性）；
//! 2. `unload` → 该插件的工具与面板全部撤销；
//! 3. 工具来源标识为 `Native { plugin_id }`，与 sidecar/MCP 在 Registry 中可区分。

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use dshr_plugin_api::{PanelContribution, Plugin, PluginContext, PluginError};
use dshr_tool_runtime::{RegistryError, ToolOrigin, ToolPolicy, ToolRegistry};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("plugin `{0}` is already loaded")]
    Duplicate(String),
    #[error("plugin `{0}` is not loaded")]
    NotFound(String),
    #[error("plugin error: {0}")]
    Plugin(#[from] PluginError),
    #[error("registry error: {0}")]
    Registry(#[from] RegistryError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRegistration {
    pub plugin_id: String,
    pub panel: PanelContribution,
}

pub struct PluginHost {
    registry: ToolRegistry,
    plugins: BTreeMap<String, Box<dyn Plugin>>,
    panels: BTreeMap<String, PanelRegistration>,
}

impl PluginHost {
    pub fn new(policy: impl ToolPolicy + 'static) -> Self {
        Self {
            registry: ToolRegistry::new(policy),
            plugins: BTreeMap::new(),
            panels: BTreeMap::new(),
        }
    }

    pub fn load(&mut self, mut plugin: Box<dyn Plugin>) -> Result<(), HostError> {
        let id = plugin.id().to_owned();
        if self.plugins.contains_key(&id) {
            return Err(HostError::Duplicate(id));
        }

        // 1) 先让插件声明；任何错误在这里返回，注册表尚未变更。
        let mut ctx = PluginContext::new(id.clone());
        plugin.apply(&mut ctx)?;
        let (tools, panels) = ctx.take_registrations();

        // 2) 原子提交：逐个注册，失败则回滚已提交的部分。
        let mut committed: Vec<String> = Vec::new();
        for mut tool in tools {
            tool.origin = ToolOrigin::Native {
                plugin_id: id.clone(),
            };
            let name = tool.name.clone();
            match self.registry.register(tool) {
                Ok(()) => committed.push(name),
                Err(err) => {
                    for name in committed {
                        self.registry.unregister(&name);
                    }
                    return Err(HostError::Registry(err));
                }
            }
        }

        for panel in panels {
            self.panels.insert(
                panel.id.clone(),
                PanelRegistration {
                    plugin_id: id.clone(),
                    panel,
                },
            );
        }

        self.plugins.insert(id, plugin);
        Ok(())
    }

    pub fn unload(&mut self, plugin_id: &str) -> Result<(), HostError> {
        let Some(mut plugin) = self.plugins.remove(plugin_id) else {
            return Err(HostError::NotFound(plugin_id.to_owned()));
        };
        plugin.unload();

        let owned: Vec<String> = self
            .registry
            .list()
            .into_iter()
            .filter(|tool| {
                matches!(&tool.origin, ToolOrigin::Native { plugin_id: owner } if owner == plugin_id)
            })
            .map(|tool| tool.name.clone())
            .collect();
        for name in owned {
            self.registry.unregister(&name);
        }

        self.panels.retain(|_, reg| reg.plugin_id != plugin_id);
        Ok(())
    }

    /// 调用入口：Registry 做 Policy + schema，再分发给对应插件。
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, HostError> {
        let definition = self.registry.authorize(tool, arguments)?.clone();
        match definition.origin {
            ToolOrigin::Native { plugin_id } => {
                let plugin = self
                    .plugins
                    .get_mut(&plugin_id)
                    .ok_or_else(|| HostError::NotFound(plugin_id.clone()))?;
                Ok(plugin.call(tool, arguments)?)
            }
            other => Err(HostError::Plugin(PluginError::Invalid(format!(
                "tool `{tool}` has non-native origin `{}`",
                other.kind()
            )))),
        }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn panels(&self) -> Vec<&PanelRegistration> {
        self.panels.values().collect()
    }

    pub fn is_loaded(&self, plugin_id: &str) -> bool {
        self.plugins.contains_key(plugin_id)
    }

    pub fn loaded_plugins(&self) -> Vec<&str> {
        self.plugins.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_tool_runtime::{AllowAll, ToolDefinition};
    use serde_json::json;

    struct ProbeNative {
        applied: bool,
        unloaded: bool,
        fail_apply: bool,
    }

    impl ProbeNative {
        fn new() -> Self {
            Self {
                applied: false,
                unloaded: false,
                fail_apply: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail_apply: true,
                ..Self::new()
            }
        }
    }

    impl Plugin for ProbeNative {
        fn id(&self) -> &'static str {
            "builtin.probe"
        }

        fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            ctx.register_tool(
                ToolDefinition::new(
                    "probe_native",
                    "echo a string from a Rust native plugin",
                    ToolOrigin::Native {
                        plugin_id: "builtin.probe".to_owned(),
                    },
                )
                .with_schema(json!({
                    "type": "object",
                    "required": ["text"],
                    "properties": { "text": { "type": "string" } }
                })),
            )?;
            ctx.register_panel(PanelContribution::new(
                "probe.panel",
                "Probe Panel",
                "workspace.panel",
            ));

            if self.fail_apply {
                // 故意在注册之后失败：宿主必须回滚。
                return Err(PluginError::Invalid("boom".to_owned()));
            }
            self.applied = true;
            Ok(())
        }

        fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError> {
            if tool != "probe_native" {
                return Err(PluginError::UnknownTool {
                    plugin: "builtin.probe".to_owned(),
                    tool: tool.to_owned(),
                });
            }
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!({ "native_echo": text.to_uppercase() }))
        }

        fn unload(&mut self) {
            self.unloaded = true;
        }
    }

    #[test]
    fn load_registers_tool_and_panel() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();

        let tool = host
            .registry()
            .get("probe_native")
            .expect("tool registered");
        assert_eq!(tool.origin.kind(), "native");
        assert_eq!(host.panels().len(), 1);
        assert_eq!(host.panels()[0].panel.slot, "workspace.panel");
        assert!(host.is_loaded("builtin.probe"));
    }

    #[test]
    fn unload_removes_tool_and_panel() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();
        host.unload("builtin.probe").unwrap();

        assert!(host.registry().get("probe_native").is_none());
        assert!(host.panels().is_empty());
        assert!(!host.is_loaded("builtin.probe"));
        assert!(matches!(
            host.unload("builtin.probe").unwrap_err(),
            HostError::NotFound(_)
        ));
    }

    #[test]
    fn duplicate_load_is_rejected() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();
        let err = host.load(Box::new(ProbeNative::new())).unwrap_err();
        assert!(matches!(err, HostError::Duplicate(_)));
    }

    #[test]
    fn failed_apply_leaves_no_registrations() {
        let mut host = PluginHost::new(AllowAll);
        let err = host.load(Box::new(ProbeNative::failing())).unwrap_err();
        assert!(matches!(err, HostError::Plugin(_)));
        assert!(host.registry().is_empty(), "apply 失败必须原子回滚");
        assert!(host.panels().is_empty());
        assert!(!host.is_loaded("builtin.probe"));
    }

    #[test]
    fn call_goes_through_policy_and_dispatches_to_plugin() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();

        let out = host.call("probe_native", &json!({ "text": "hi" })).unwrap();
        assert_eq!(out["native_echo"], "HI");

        // schema 拦截：缺 required
        assert!(matches!(
            host.call("probe_native", &json!({})).unwrap_err(),
            HostError::Registry(RegistryError::InvalidArguments { .. })
        ));
    }

    #[test]
    fn sidecar_generation_invalidation_does_not_touch_native_tools() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();
        host.registry
            .get("probe_native")
            .expect("native tool present");

        // 通过注册表直接模拟 sidecar 工具，再让它整代失效。
        let registry = &mut host.registry;
        registry
            .register(ToolDefinition::new(
                "js_tool",
                "sidecar tool",
                ToolOrigin::Sidecar {
                    plugin_id: "probe.plugin".to_owned(),
                    registration_id: "reg-1".to_owned(),
                    generation: "node-0001".to_owned(),
                },
            ))
            .unwrap();
        assert_eq!(registry.invalidate_generation("node-0001"), 1);
        assert!(registry.get("probe_native").is_some());
        assert!(registry.get("js_tool").is_none());
    }
}
