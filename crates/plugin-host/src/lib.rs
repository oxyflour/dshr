//! Native Plugin 宿主：加载、原子提交声明、卸载清理、调用分发。
//!
//! 这是 CPP-v1 的第一条真实路径（`rust-static`）。`PluginHost` 负责 ToolRegistry /
//! 面板的原子提交；Fiber 状态机、Service 依赖与 Effect 撤销在 [`fiber`] 模块里
//! （本机离线，按 PLAN §3.5 采用自研最小实现，替换点为 `fiber` 模块文档里的适配表）；
//! `external-process` 插件运行时在 [`external`] 模块里（spec/cpp-v1.md §12）。
//! `PluginHost::load_with_manifest` 是先校验 manifest / 版本 / 配额再走 `load` 的衔接点，
//! 因此 `chat.rs` 依赖的公共 API 完全不变。
//!
//! 保证：
//! 1. `apply` 失败 → 本次声明**一条都不落库**（原子性）；
//! 2. `unload` → 该插件的工具与面板全部撤销；
//! 3. 工具来源标识为 `Native { plugin_id }`，与 sidecar/MCP 在 Registry 中可区分。

#![forbid(unsafe_code)]

pub mod external;
pub mod fiber;

pub use external::{ExternalPlugin, ExternalPluginSpec};

use std::collections::BTreeMap;

use dshr_plugin_api::{
    HostVersion, Manifest, PanelContribution, Plugin, PluginContext, PluginError,
};
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
    /// `external-process` 运行时错误：启动/握手/协议/子进程退出（spec/cpp-v1.md §12）。
    #[error("external plugin `{plugin_id}`: {reason}")]
    External { plugin_id: String, reason: String },
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

    /// 从 typed manifest 装载（CPP-A 接入点）：
    /// 1. `Manifest::validate` 校验 api / `min_host_version` / 配额 / `inject` / tool / ui；
    /// 2. `manifest.plugin.id` 必须与插件实现声明的 id 一致（防止包与代码错配）；
    /// 3. 再走原有 [`PluginHost::load`] 的原子提交路径。
    ///
    /// 这是 additive API：`load` 与既有测试保持不变。
    pub fn load_with_manifest(
        &mut self,
        manifest: &Manifest,
        plugin: Box<dyn Plugin>,
        host: &HostVersion,
    ) -> Result<(), HostError> {
        manifest.validate(host)?;
        if plugin.id() != manifest.plugin.id {
            return Err(HostError::Plugin(PluginError::InvalidManifest(format!(
                "manifest plugin.id `{}` does not match plugin id `{}`",
                manifest.plugin.id,
                plugin.id()
            ))));
        }
        self.load(plugin)
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

    /// 渲染一个插件 view（ADC-v1）：`ui.render` → 元素树 JSON。
    ///
    /// 这是 chat 右侧 panel 的宿主入口；宿主只负责取树，不碰 GPUI（渲染由
    /// `ui-gpui::adc_panel_body` 完成，保持本 crate 的边界）：
    /// - 未知插件 → [`HostError::NotFound`]；
    /// - 插件不支持 view 或 view 不存在 → 插件的可读 [`PluginError`]（`HostError::Plugin`）。
    pub fn render_panel(&mut self, plugin_id: &str, view: &str) -> Result<Value, HostError> {
        let plugin = self
            .plugins
            .get_mut(plugin_id)
            .ok_or_else(|| HostError::NotFound(plugin_id.to_owned()))?;
        plugin.render_view(view).map_err(HostError::Plugin)
    }

    /// 已通过策略审批后的调用：只做 schema 校验，不再问策略。
    pub fn call_approved(&mut self, tool: &str, arguments: &Value) -> Result<Value, HostError> {
        let definition = self.registry.authorize_approved(tool, arguments)?.clone();
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

    /// 供外层把 Sidecar / MCP 工具登记进同一个注册表（ADR-0006 切片 2）。
    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Sidecar 重启：整代撤销，Native 与 MCP 工具不受影响。
    pub fn invalidate_generation(&mut self, generation: &str) -> usize {
        self.registry.invalidate_generation(generation)
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
            ctx.register_panel(
                PanelContribution::new("probe.panel", "Probe Panel", "workspace.panel")
                    .with_view("probe.panel"),
            );

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

        /// ADC panel：原生插件也能渲染 view（与 external-process 走同一 trait 方法）。
        fn render_view(&mut self, view: &str) -> Result<Value, PluginError> {
            if view != "probe.panel" {
                return Err(PluginError::Invalid(format!("unknown view `{view}`")));
            }
            Ok(json!({
                "type": "Card",
                "props": {"title": "Probe Panel"},
                "children": [
                    {"type": "Text", "props": {"value": "原生插件 ADC 面板"}},
                    {"type": "Badge", "props": {"label": "native", "tone": "success"}}
                ]
            }))
        }
    }

    /// 不实现 view 的插件（验证 trait 默认实现的降级错误）。
    struct NoViewNative;

    impl Plugin for NoViewNative {
        fn id(&self) -> &'static str {
            "builtin.no-view"
        }

        fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
            Err(PluginError::Invalid("no tools".to_owned()))
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

    fn manifest_json(id: &str, api: &str, min_host_version: Option<&str>) -> serde_json::Value {
        let mut plugin = json!({
            "id": id,
            "version": "0.1.0",
            "api": api,
        });
        if let Some(required) = min_host_version {
            plugin["min_host_version"] = json!(required);
        }
        json!({ "plugin": plugin, "runtime": { "kind": "rust-static" } })
    }

    fn host_version() -> HostVersion {
        HostVersion::new(0, 1, 0)
    }

    // CPP-FIBER-023..027：PluginHost 的 manifest 装载接入（CPP-A）。
    #[test]
    fn cpp_fiber_023_load_with_manifest_validates_then_loads() {
        let manifest = Manifest::from_json_str(
            &manifest_json("builtin.probe", "cordis-portable/v1", None).to_string(),
        )
        .unwrap();
        let mut host = PluginHost::new(AllowAll);
        host.load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
            .unwrap();
        assert!(host.is_loaded("builtin.probe"));
        assert!(host.registry().get("probe_native").is_some());
        assert_eq!(host.panels().len(), 1);
    }

    #[test]
    fn cpp_fiber_024_load_with_manifest_rejects_too_old_host() {
        let manifest = Manifest::from_json_str(
            &manifest_json("builtin.probe", "cordis-portable/v1", Some("9.9.9")).to_string(),
        )
        .unwrap();
        let mut host = PluginHost::new(AllowAll);
        let err = host
            .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
            .unwrap_err();
        let HostError::Plugin(error) = &err else {
            panic!("预期 PluginError，实际 {err:?}");
        };
        assert_eq!(error.code(), "E_VERSION");
        assert!(matches!(error, PluginError::HostTooOld { .. }));
        assert!(!host.is_loaded("builtin.probe"));
        assert!(host.registry().is_empty());
    }

    #[test]
    fn cpp_fiber_025_load_with_manifest_rejects_api_mismatch() {
        // 直接 serde 解析以绕过 `from_json_str` 的 api 检查，验证装载期仍会拒绝。
        let raw = manifest_json("builtin.probe", "cordis-portable/v2", None).to_string();
        let manifest: Manifest = serde_json::from_str(&raw).unwrap();
        let mut host = PluginHost::new(AllowAll);
        let err = host
            .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
            .unwrap_err();
        let HostError::Plugin(error) = &err else {
            panic!("预期 PluginError，实际 {err:?}");
        };
        assert_eq!(error.code(), "E_VERSION");
        assert!(matches!(error, PluginError::UnsupportedApi { .. }));
        assert!(host.registry().is_empty());
    }

    #[test]
    fn cpp_fiber_026_load_with_manifest_rejects_plugin_id_mismatch() {
        let manifest = Manifest::from_json_str(
            &manifest_json("other.plugin", "cordis-portable/v1", None).to_string(),
        )
        .unwrap();
        let mut host = PluginHost::new(AllowAll);
        let err = host
            .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
            .unwrap_err();
        let HostError::Plugin(error) = &err else {
            panic!("预期 PluginError，实际 {err:?}");
        };
        assert_eq!(error.code(), "E_MANIFEST");
        assert!(matches!(error, PluginError::InvalidManifest(_)));
        assert!(!host.is_loaded("builtin.probe"));
        assert!(host.registry().is_empty());
    }

    #[test]
    fn cpp_fiber_027_load_with_manifest_leaves_registry_untouched_on_plugin_failure() {
        let manifest = Manifest::from_json_str(
            &manifest_json("builtin.probe", "cordis-portable/v1", None).to_string(),
        )
        .unwrap();
        let mut host = PluginHost::new(AllowAll);
        let err = host
            .load_with_manifest(&manifest, Box::new(ProbeNative::failing()), &host_version())
            .unwrap_err();
        assert!(matches!(err, HostError::Plugin(_)));
        assert!(host.registry().is_empty());
        assert!(host.panels().is_empty());
        assert!(!host.is_loaded("builtin.probe"));
    }

    // ---- ADC-v1 panel 渲染入口（ADC-ELEM-PANEL-HOST-*） --------------------

    // ADC-ELEM-PANEL-HOST-001
    #[test]
    fn render_panel_returns_adc_tree_from_native_plugin() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).unwrap();

        // panel 元数据带 view：render_panel 的入参（id 与 view 可以不同）。
        let registration = host
            .panels()
            .into_iter()
            .find(|registration| registration.panel.slot == "workspace.panel")
            .expect("panel registered");
        assert_eq!(registration.panel.view_name(), "probe.panel");

        let tree = host
            .render_panel("builtin.probe", "probe.panel")
            .expect("render_panel");
        assert_eq!(tree["type"], json!("Card"));
        assert_eq!(tree["props"]["title"], json!("Probe Panel"));

        // 未知 view → 插件的可读错误（不是 panic / NotFound）。
        let error = host
            .render_panel("builtin.probe", "missing.view")
            .unwrap_err();
        assert!(matches!(&error, HostError::Plugin(PluginError::Invalid(_))));
        assert!(error.to_string().contains("missing.view"), "{error}");
    }

    // ADC-ELEM-PANEL-HOST-002
    #[test]
    fn render_panel_unknown_plugin_is_not_found() {
        let mut host = PluginHost::new(AllowAll);
        let error = host
            .render_panel("missing.plugin", "workspace.panel")
            .unwrap_err();
        assert!(matches!(&error, HostError::NotFound(_)));
        assert!(error.to_string().contains("missing.plugin"), "{error}");
    }

    // ADC-ELEM-PANEL-HOST-003
    #[test]
    fn render_panel_default_impl_is_readable_for_plugins_without_views() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(NoViewNative)).unwrap();
        let error = host
            .render_panel("builtin.no-view", "anything")
            .unwrap_err();
        assert!(
            error.to_string().contains("does not render views"),
            "{error}"
        );
        assert!(matches!(&error, HostError::Plugin(PluginError::Invalid(_))));
    }
}
