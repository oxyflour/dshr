//! Native Plugin 宿主：加载、原子提交声明、卸载清理、调用分发。
//!
//! 这是 CPP-v1 的第一条真实路径（`rust-static`）。`PluginHost` 负责 ToolRegistry /
//! 面板的原子提交；Fiber 状态机、Service 依赖与 Effect 撤销在 [`fiber`] 模块里
//! （本机离线，按 PLAN §3.5 采用自研最小实现，替换点为 `fiber` 模块文档里的适配表）；
//! `external-process` 插件运行时在 [`external`] 模块里（spec/cpp-v1.md §12）；
//! `wasm-component` 由 [`dshr_plugin_wasm::WasmPlugin`] 适配后经
//! [`PluginHost::load_wasm_plugin`] 装载（P1-1）。
//!
//! 保证：
//! 1. `apply` 失败 / 工具或 panel 冲突 → 本次声明**一条都不落库**（原子性）；
//! 2. `unload` → 该插件的工具与面板全部撤销；
//! 3. 工具来源标识为 `Native { plugin_id }`，与 sidecar/MCP 在 Registry 中可区分。

#![forbid(unsafe_code)]

pub mod external;
pub mod fiber;

pub use dshr_plugin_wasm::{WasmEngineConfig, WasmPlugin};
pub use external::{ExternalPlugin, ExternalPluginSpec};

use std::path::Path;

use std::collections::BTreeMap;

use dshr_plugin_api::manifest::RuntimeKind;
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
    /// `wasm-component` 运行时错误：模块校验/实例化/配额/ABI（review P1-1）。
    #[error("wasm plugin `{plugin_id}`: {reason}")]
    Wasm { plugin_id: String, reason: String },
    /// panel id 全局冲突（review P2-3）：拒绝覆盖其它插件（或自己重复）的面板。
    #[error("panel id `{panel_id}` conflicts with plugin `{owner}`")]
    PanelConflict { panel_id: String, owner: String },
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

        // 2) panel id 全局唯一（review P2-3）：在提交任何东西之前拒绝覆盖。
        //    同一插件内重复声明也算冲突（否则 BTreeMap 会静默丢弃后一条）。
        let mut declared_panels: Vec<String> = Vec::new();
        for panel in &panels {
            if let Some(existing) = self.panels.get(&panel.id) {
                return Err(HostError::PanelConflict {
                    panel_id: panel.id.clone(),
                    owner: existing.plugin_id.clone(),
                });
            }
            if declared_panels.contains(&panel.id) {
                return Err(HostError::PanelConflict {
                    panel_id: panel.id.clone(),
                    owner: id,
                });
            }
            declared_panels.push(panel.id.clone());
        }

        // 3) 原子提交工具：逐个注册，失败则回滚已提交的部分。
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

    /// 装载一个 `wasm-component` 插件（review P1-1）：
    /// 1. `Manifest::validate`（api / min_host / 配额 / inject / tools / ui）；
    /// 2. `runtime.kind` 必须是 `wasm-component`（否则可读错误）；
    /// 3. 按 manifest 的 `runtime.entry` 在 `package_dir` 内加载模块并适配成
    ///    [`WasmPlugin`]，再走原子 [`PluginHost::load`]。
    ///
    /// 失败返回 [`HostError::Wasm`]（模块/ABI/配额）或 [`HostError::Plugin`]（manifest）。
    pub fn load_wasm_plugin(
        &mut self,
        manifest: &Manifest,
        package_dir: impl AsRef<Path>,
        host: &HostVersion,
    ) -> Result<(), HostError> {
        let plugin_id = manifest.plugin.id.clone();
        let plugin =
            WasmPlugin::from_manifest(manifest, package_dir.as_ref(), host).map_err(|error| {
                HostError::Wasm {
                    plugin_id: plugin_id.clone(),
                    reason: error.to_string(),
                }
            })?;
        self.load(Box::new(plugin))
    }

    /// 从 typed manifest 装载（CPP-A 接入点）：
    /// 1. `Manifest::validate` 校验 api / `min_host_version` / 配额 / `inject` / tool / ui；
    /// 2. `manifest.plugin.id` 必须与插件实现声明的 id 一致（防止包与代码错配）；
    /// 3. `runtime.kind == "wasm-component"` 走 [`PluginHost::load_wasm_plugin`]（需要包目录），
    ///    不能把 WASM 宿主误当 native trait 对象塞进来；
    /// 4. 再走原有 [`PluginHost::load`] 的原子提交路径。
    ///
    /// 这是 additive API：`load` 与既有测试保持不变。
    pub fn load_with_manifest(
        &mut self,
        manifest: &Manifest,
        plugin: Box<dyn Plugin>,
        host: &HostVersion,
    ) -> Result<(), HostError> {
        manifest.validate(host)?;
        // review P1-1：wasm-component 必须走带包目录的专用入口，不能静默塞进 native 路径。
        match manifest.runtime.kind {
            RuntimeKind::WasmComponent => {
                return Err(HostError::Wasm {
                    plugin_id: manifest.plugin.id.clone(),
                    reason:
                        "wasm-component requires `load_wasm_plugin(manifest, package_dir, host)`"
                            .to_owned(),
                })
            }
            RuntimeKind::RustStatic | RuntimeKind::ExternalProcess => {}
        }
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

    /// 把 ADC `ui.action` 派发给指定插件（M4 W3）。
    ///
    /// fire-and-forget：调用方随后应重新 `render_panel` 取新树；未知插件返回 `NotFound`，
    /// 插件不支持动作时返回可读 `PluginError`（不静默成功）。
    pub fn dispatch_action(
        &mut self,
        plugin_id: &str,
        view: &str,
        action: &str,
        payload: &Value,
    ) -> Result<(), HostError> {
        let plugin = self
            .plugins
            .get_mut(plugin_id)
            .ok_or_else(|| HostError::NotFound(plugin_id.to_owned()))?;
        plugin
            .handle_action(view, action, payload)
            .map_err(HostError::Plugin)
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

    /// M4 W4：按插件分组并行调用 native 工具；同一插件的多个调用保持串行。
    ///
    /// 返回顺序与 `calls` 一致。插件在 worker 线程内临时移出 map，完成后放回；
    /// 某个插件 panic 时该组结果保持错误，不拖垮其它组（插件本身不再放回）。
    /// 这里假定调用方已完成审批（与 [`PluginHost::call_approved`] 同一契约）。
    pub fn call_approved_batch(
        &mut self,
        calls: &[(String, Value)],
        max_parallel: usize,
    ) -> Vec<Result<Value, String>> {
        let mut results: Vec<Result<Value, String>> = (0..calls.len())
            .map(|_| Err("batch result missing".to_owned()))
            .collect();
        let mut groups: BTreeMap<String, Vec<(usize, String, Value)>> = BTreeMap::new();

        for (index, (tool, arguments)) in calls.iter().enumerate() {
            let definition = match self.registry.authorize_approved(tool, arguments) {
                Ok(definition) => definition.clone(),
                Err(error) => {
                    results[index] = Err(error.to_string());
                    continue;
                }
            };
            match definition.origin {
                ToolOrigin::Native { plugin_id } => {
                    groups.entry(plugin_id).or_default().push((
                        index,
                        tool.clone(),
                        arguments.clone(),
                    ));
                }
                other => {
                    results[index] = Err(format!(
                        "tool `{tool}` has non-native origin `{}`",
                        other.kind()
                    ));
                }
            }
        }

        let parallelism = max_parallel.max(1);
        while !groups.is_empty() {
            let ids: Vec<String> = groups.keys().take(parallelism).cloned().collect();
            let mut jobs = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(plugin) = self.plugins.remove(&id) else {
                    let pending = groups.remove(&id).unwrap_or_default();
                    for (index, _, _) in pending {
                        results[index] = Err(format!("plugin `{id}` is not loaded"));
                    }
                    continue;
                };
                let pending = groups.remove(&id).unwrap_or_default();
                jobs.push((id, plugin, pending));
            }

            std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .into_iter()
                    .map(|(id, mut plugin, pending)| {
                        scope.spawn(move || {
                            let mut out = Vec::with_capacity(pending.len());
                            for (index, tool, arguments) in pending {
                                out.push((
                                    index,
                                    plugin
                                        .call(&tool, &arguments)
                                        .map_err(|error| error.to_string()),
                                ));
                            }
                            (id, plugin, out)
                        })
                    })
                    .collect();
                for handle in handles {
                    match handle.join() {
                        Ok((id, plugin, out)) => {
                            self.plugins.insert(id, plugin);
                            for (index, result) in out {
                                results[index] = result;
                            }
                        }
                        Err(_) => {
                            // 插件 panic：该组结果保持错误；插件不重新注册，避免半死状态。
                        }
                    }
                }
            });
        }
        results
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

    // M4-ADC-002（M4 W3）：ui.action 路由到指定插件，未知插件报 NotFound。
    #[derive(Clone)]
    struct ActionProbe {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
    }

    impl Plugin for ActionProbe {
        fn id(&self) -> &'static str {
            "builtin.action-probe"
        }

        fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
            Err(PluginError::Invalid("no tools".to_owned()))
        }

        fn handle_action(
            &mut self,
            view: &str,
            action: &str,
            payload: &Value,
        ) -> Result<(), PluginError> {
            self.seen
                .lock()
                .unwrap()
                .push((view.to_owned(), action.to_owned(), payload.clone()));
            Ok(())
        }
    }

    #[test]
    fn dispatch_action_routes_payload_and_unknown_plugin_is_not_found() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ActionProbe {
            seen: std::sync::Arc::clone(&seen),
        }))
        .unwrap();

        host.dispatch_action(
            "builtin.action-probe",
            "fixture.panel",
            "call",
            &json!({ "method": "refresh", "args": { "n": 1 } }),
        )
        .unwrap();
        let recorded = seen.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "fixture.panel");
        assert_eq!(recorded[0].1, "call");
        assert_eq!(recorded[0].2["method"], "refresh");

        let error = host
            .dispatch_action("missing.plugin", "v", "call", &Value::Null)
            .unwrap_err();
        assert!(matches!(&error, HostError::NotFound(_)), "{error:?}");
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

    // ---- review P2-3：panel id 全局冲突 -----------------------------------

    /// 与 `ProbeNative` 用**同一个 panel id** 的第二个插件：原名应被拒绝而不是覆盖。
    struct ProbePanelConflict {
        plugin_id: &'static str,
        repeat_panel: bool,
    }

    impl ProbePanelConflict {
        fn new(plugin_id: &'static str) -> Self {
            Self {
                plugin_id,
                repeat_panel: false,
            }
        }

        fn repeating(plugin_id: &'static str) -> Self {
            Self {
                plugin_id,
                repeat_panel: true,
            }
        }
    }

    impl Plugin for ProbePanelConflict {
        fn id(&self) -> &'static str {
            self.plugin_id
        }

        fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            ctx.register_tool(ToolDefinition::new(
                format!("{}_tool", self.plugin_id.replace('.', "_")),
                "conflict probe tool",
                ToolOrigin::Native {
                    plugin_id: self.plugin_id.to_owned(),
                },
            ))?;
            ctx.register_panel(PanelContribution::new(
                "probe.panel",
                "Probe Panel",
                "workspace.panel",
            ));
            if self.repeat_panel {
                // 同一插件内重复声明同一 panel id 也必须拒绝。
                ctx.register_panel(PanelContribution::new(
                    "probe.panel",
                    "Probe Panel Duplicated",
                    "workspace.panel",
                ));
            }
            Ok(())
        }

        fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
            Ok(json!({ "ok": true }))
        }
    }

    // CPP-WASM-033 / review P2-3：跨插件 panel id 冲突 → 拒绝且原子回滚。
    #[test]
    fn cpp_wasm_033_cross_plugin_panel_id_conflict_is_rejected_atomically() {
        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(ProbeNative::new())).expect("first load");
        let error = host
            .load(Box::new(ProbePanelConflict::new("builtin.conflict")))
            .expect_err("panel id 冲突必须拒绝");
        assert!(
            matches!(&error, HostError::PanelConflict { panel_id, owner } if panel_id == "probe.panel" && owner == "builtin.probe"),
            "error={error:?}"
        );
        // 原子性：第二个插件的工具一条都不能落库，第一个插件的 panel 原样保留。
        assert!(host.registry().get("builtin_conflict_tool").is_none());
        assert!(!host.is_loaded("builtin.conflict"));
        assert_eq!(host.panels().len(), 1);
        assert_eq!(host.panels()[0].plugin_id, "builtin.probe");
    }

    // review P2-3：同一插件内重复声明 panel id 也拒绝（不能静默丢一条）。
    #[test]
    fn cpp_panel_002_same_plugin_duplicate_panel_id_is_rejected() {
        let mut host = PluginHost::new(AllowAll);
        let error = host
            .load(Box::new(ProbePanelConflict::repeating("builtin.repeat")))
            .expect_err("同一插件重复 panel id 必须拒绝");
        assert!(
            matches!(&error, HostError::PanelConflict { panel_id, owner } if panel_id == "probe.panel" && owner == "builtin.repeat"),
            "error={error:?}"
        );
        assert!(host.panels().is_empty());
        assert!(!host.is_loaded("builtin.repeat"));
    }

    // ---- review P1-1：WASM 接入 PluginHost --------------------------------

    /// 最简 echo WAT（回显请求 JSON；`WasmPlugin` 测试用）。
    const WASM_ECHO_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (func (export "dshr_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $ptr))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $len))))
)
"#;

    fn write_wasm_package(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("schemas")).expect("schemas dir");
        std::fs::write(dir.join("plugin.wasm"), WASM_ECHO_WAT).expect("write wat");
        for suffix in ["input", "output"] {
            std::fs::write(
                dir.join(format!("schemas/echo.{suffix}.json")),
                json!({ "type": "object" }).to_string(),
            )
            .unwrap();
        }
    }

    fn wasm_manifest() -> Manifest {
        let raw = json!({
            "plugin": { "id": "cpp.wasm.host", "version": "0.1.0", "api": "cordis-portable/v1" },
            "runtime": {
                "kind": "wasm-component",
                "entry": "plugin.wasm",
                "fuel": 2_000_000,
                "max_memory_mb": 32,
                "timeout_ms": 30_000
            },
            "tools": [{
                "name": "echo",
                "description": "echo",
                "input_schema": "schemas/echo.input.json",
                "output_schema": "schemas/echo.output.json"
            }],
            "ui": [{
                "slot": "workspace.panel",
                "id": "cpp.wasm.host.panel",
                "label": "WASM Host Panel",
                "view": "cpp.wasm.host.view"
            }]
        })
        .to_string();
        Manifest::from_json_str(&raw).expect("wasm manifest")
    }

    fn temp_wasm_package(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dshr-host-wasm-{tag}-{}-{nanos}",
            std::process::id()
        ));
        write_wasm_package(&dir);
        dir
    }

    // CPP-WASM-031：load_wasm_plugin 装载 → 工具注册 → 调用 → 卸载。
    #[test]
    fn cpp_wasm_031_load_wasm_plugin_registers_tool_and_calls() {
        let dir = temp_wasm_package("ok");
        let manifest = wasm_manifest();
        let mut host = PluginHost::new(AllowAll);
        host.load_wasm_plugin(&manifest, &dir, &host_version())
            .expect("load wasm plugin");
        assert!(host.is_loaded("cpp.wasm.host"));
        assert!(host.registry().get("echo").is_some());
        assert_eq!(host.panels().len(), 1);
        assert_eq!(host.panels()[0].panel.id, "cpp.wasm.host.panel");
        assert_eq!(host.panels()[0].panel.view_name(), "cpp.wasm.host.view");

        let value = host
            .call("echo", &json!({ "text": "wasm-host" }))
            .expect("call wasm tool");
        assert_eq!(value["tool"], json!("echo"));
        assert_eq!(value["arguments"]["text"], json!("wasm-host"));

        host.unload("cpp.wasm.host").expect("unload");
        assert!(!host.is_loaded("cpp.wasm.host"));
        assert!(host.registry().is_empty());
        assert!(host.panels().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // CPP-WASM-032：wasm-component 走 native `load_with_manifest` 必须报错（不能静默）。
    #[test]
    fn cpp_wasm_032_load_with_manifest_rejects_wasm_component() {
        let manifest = wasm_manifest();
        let mut host = PluginHost::new(AllowAll);
        let error = host
            .load_with_manifest(&manifest, Box::new(ProbeNative::new()), &host_version())
            .expect_err("wasm-component 不能被 native 路径静默装载");
        assert!(
            matches!(&error, HostError::Wasm { plugin_id, reason } if plugin_id == "cpp.wasm.host" && reason.contains("load_wasm_plugin")),
            "error={error:?}"
        );
        assert!(error.to_string().contains("load_wasm_plugin"), "{error}");
        assert!(!host.is_loaded("cpp.wasm.host"));
    }

    // CPP-WASM-034：wasm 装载失败（entry 缺失）→ 结构化错误且宿主无残留。
    #[test]
    fn cpp_wasm_034_load_wasm_plugin_missing_entry_is_structured_error() {
        let dir = temp_wasm_package("missing");
        let mut manifest = wasm_manifest();
        manifest.runtime.entry = Some("missing.wasm".to_owned());
        let mut host = PluginHost::new(AllowAll);
        let error = host
            .load_wasm_plugin(&manifest, &dir, &host_version())
            .expect_err("entry 缺失必须失败");
        assert!(matches!(&error, HostError::Wasm { .. }), "error={error:?}");
        assert!(
            error.to_string().contains("manifest error")
                || error.to_string().contains("not accessible"),
            "{error}"
        );
        assert!(!host.is_loaded("cpp.wasm.host"));
        assert!(host.registry().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // M4-CONC-001：不同 native 插件的批调用并行；同一插件仍串行（由分组保证）。
    #[test]
    fn m4_conc_001_native_batch_parallelizes_distinct_plugins() {
        struct SlowPlugin {
            id: &'static str,
            tool: &'static str,
        }
        impl Plugin for SlowPlugin {
            fn id(&self) -> &str {
                self.id
            }
            fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
                ctx.register_tool(ToolDefinition::new(
                    self.tool,
                    "slow probe",
                    ToolOrigin::Native {
                        plugin_id: self.id.to_owned(),
                    },
                ))?;
                Ok(())
            }
            fn call(&mut self, _tool: &str, arguments: &Value) -> Result<Value, PluginError> {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(arguments.clone())
            }
        }

        let mut host = PluginHost::new(AllowAll);
        host.load(Box::new(SlowPlugin {
            id: "slow.a",
            tool: "slow_a",
        }))
        .expect("load slow.a");
        host.load(Box::new(SlowPlugin {
            id: "slow.b",
            tool: "slow_b",
        }))
        .expect("load slow.b");

        let started = std::time::Instant::now();
        let results = host.call_approved_batch(
            &[
                ("slow_a".to_owned(), json!({ "n": 1 })),
                ("slow_b".to_owned(), json!({ "n": 2 })),
            ],
            2,
        );
        let elapsed = started.elapsed();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(550),
            "distinct plugins should overlap (serial is ~600ms): {elapsed:?}"
        );
    }
}
