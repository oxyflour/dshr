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

pub use dshr_plugin_wasm::{ComponentPlugin, CppComponentPlugin, WasmEngineConfig, WasmPlugin};
pub use external::{ExternalPlugin, ExternalPluginSpec};

use std::path::Path;

use std::collections::BTreeMap;

use dshr_plugin_api::manifest::RuntimeKind;
use dshr_plugin_api::{
    HostVersion, Manifest, PanelContribution, Plugin, PluginContext, PluginError,
};
use dshr_tool_runtime::{RegistryError, ToolDefinition, ToolOrigin, ToolPolicy, ToolRegistry};
use serde_json::Value;
use slots::PluginSlots;

mod slots;
pub use dshr_plugin_sdk::{ChatNodeContext, FileViewContext};
pub use slots::{
    desktop_slots, is_tool_view_slot, CommandShortcut, SlotTarget, BUILTIN_COMMAND_IDS, CHAT_NODE,
    COMMAND_SLOT, COMPOSER_ADD_MENU, DESKTOP_MENU_KEYS, DESKTOP_VIEW_SLOTS, MENU_ITEM_SLOT,
    TOOL_CALL_VIEW, TOOL_RESULT_EXTRA, VIEWER_TAB,
};
#[cfg(windows)]
pub use slots::{APPLICATION_MENU, EDIT_MENU};
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
    slots: PluginSlots,
}

impl PluginHost {
    pub fn new(policy: impl ToolPolicy + 'static) -> Self {
        Self {
            registry: ToolRegistry::new(policy),
            plugins: BTreeMap::new(),
            panels: BTreeMap::new(),
            slots: PluginSlots::default(),
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
        self.commit(plugin, tools, panels, ctx.take_slots(), false)
    }

    /// Move a validated candidate from a temporary host. Check collisions before
    /// unloading an older instance and retain the destination's tool policy.
    pub fn adopt_candidate(&mut self, mut staged: Self, plugin_id: &str) -> Result<(), HostError> {
        let plugin = staged
            .plugins
            .remove(plugin_id)
            .ok_or_else(|| HostError::NotFound(plugin_id.to_owned()))?;
        let tools = staged.registry.list().into_iter()
            .filter(|tool| matches!(&tool.origin, ToolOrigin::Native { plugin_id: owner } if owner == plugin_id))
            .cloned()
            .collect();
        let panels = staged
            .panels
            .into_values()
            .filter(|registration| registration.plugin_id == plugin_id)
            .map(|registration| registration.panel)
            .collect();
        self.commit(
            plugin,
            tools,
            panels,
            staged.slots.declarations(plugin_id),
            true,
        )
    }

    fn commit(
        &mut self,
        plugin: Box<dyn Plugin>,
        tools: Vec<ToolDefinition>,
        panels: Vec<PanelContribution>,
        slots: Vec<Value>,
        replace: bool,
    ) -> Result<(), HostError> {
        let id = plugin.id().to_owned();
        let mut declared_panels = std::collections::BTreeSet::new();
        for panel in &panels {
            if let Some(existing) = self
                .panels
                .get(&panel.id)
                .filter(|existing| !replace || existing.plugin_id != id)
            {
                return Err(HostError::PanelConflict {
                    panel_id: panel.id.clone(),
                    owner: existing.plugin_id.clone(),
                });
            }
            if !declared_panels.insert(&panel.id) {
                return Err(HostError::PanelConflict {
                    panel_id: panel.id.clone(),
                    owner: id,
                });
            }
        }
        for tool in &tools {
            if self.registry.get(&tool.name).is_some_and(|existing| {
                !(replace && matches!(&existing.origin, ToolOrigin::Native { plugin_id } if plugin_id == &id))
            }) {
                return Err(RegistryError::Duplicate(tool.name.clone()).into());
            }
        }
        slots::validate_tool_declarations(&tools, &slots)?;
        let slots = self.slots.stage(&id, slots)?;
        if replace && self.plugins.contains_key(&id) {
            self.unload(&id)?;
        }
        for mut tool in tools {
            tool.origin = ToolOrigin::Native {
                plugin_id: id.clone(),
            };
            self.registry.register(tool)?;
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
        if let Some(slots) = slots {
            self.slots = slots;
        }
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

    /// 装载实验性 component probe 插件（`cpp-component-probe/v1`，仅 `run() -> u32`）。
    ///
    /// 这条 API 把 [`ComponentPlugin`] 接入宿主原子 `apply`/注册表路径；正式 cpp-v1
    /// 工具 ABI 仍未实现，宿主不会自动从 manifest 识别 component。
    pub fn load_component_plugin(
        &mut self,
        id: &str,
        version: &str,
        source: impl AsRef<[u8]>,
    ) -> Result<(), HostError> {
        let plugin = ComponentPlugin::new(id, version, source).map_err(HostError::Plugin)?;
        self.load(Box::new(plugin))
    }

    /// 从 manifest 装载实验性 component probe 插件。
    ///
    /// 只接受 `runtime.kind=wasm-component` 且 `tools`/`ui` 为空的 probe 包；
    /// 正式 cpp-v1 工具 ABI 仍待实现，因此这里主动拒绝带工具/UI 声明的包。
    pub fn load_component_plugin_from_manifest(
        &mut self,
        manifest: &Manifest,
        package_dir: impl AsRef<Path>,
        host: &HostVersion,
    ) -> Result<(), HostError> {
        manifest.validate(host).map_err(HostError::Plugin)?;
        if manifest.runtime.kind != RuntimeKind::WasmComponent {
            return Err(HostError::Wasm {
                plugin_id: manifest.plugin.id.clone(),
                reason: "component probe requires runtime.kind=wasm-component".to_owned(),
            });
        }
        if !manifest.tools.is_empty() || !manifest.ui.is_empty() {
            return Err(HostError::Wasm {
                plugin_id: manifest.plugin.id.clone(),
                reason: "component probe manifest must not declare tools/ui".to_owned(),
            });
        }
        let entry = manifest.runtime.entry.as_deref().unwrap_or("plugin.wasm");
        let path = package_dir.as_ref().join(entry);
        let source = std::fs::read(&path).map_err(|error| HostError::Wasm {
            plugin_id: manifest.plugin.id.clone(),
            reason: format!("read {}: {error}", path.display()),
        })?;
        self.load_component_plugin(&manifest.plugin.id, &manifest.plugin.version, source)
    }

    /// 装载真实 cpp-v1 Component 插件（guest 必须实现 `cpp-v1.wit` 的 `plugin` world）。
    pub fn load_cpp_component_plugin(
        &mut self,
        id: &str,
        version: &str,
        source: &[u8],
    ) -> Result<(), HostError> {
        let plugin = CppComponentPlugin::new(id, version, source).map_err(HostError::Plugin)?;
        self.load(Box::new(plugin))
    }

    /// 从 manifest 装载真实 cpp-v1 Component 插件；工具/UI 由 guest 的
    /// `list-tools`/`list-panels` 声明，manifest 的 tools/ui 不参与注册。
    pub fn load_cpp_component_plugin_from_manifest(
        &mut self,
        manifest: &Manifest,
        package_dir: impl AsRef<Path>,
        host: &HostVersion,
    ) -> Result<(), HostError> {
        manifest.validate(host).map_err(HostError::Plugin)?;
        if manifest.runtime.kind != RuntimeKind::WasmComponent {
            return Err(HostError::Wasm {
                plugin_id: manifest.plugin.id.clone(),
                reason: "cpp component requires runtime.kind=wasm-component".to_owned(),
            });
        }
        let entry = manifest.runtime.entry.as_deref().unwrap_or("plugin.wasm");
        let path = package_dir.as_ref().join(entry);
        let source = std::fs::read(&path).map_err(|error| HostError::Wasm {
            plugin_id: manifest.plugin.id.clone(),
            reason: format!("read {}: {error}", path.display()),
        })?;
        self.load_cpp_component_plugin(&manifest.plugin.id, &manifest.plugin.version, &source)
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
        self.slots.retract(plugin_id);
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
        self.call_approved_batch_cancellable(calls, max_parallel, None)
    }

    /// 与 [`PluginHost::call_approved_batch`] 相同，但每调用前检查取消位；已取消的槽位
    /// 返回 `cancelled`，不会启动后续插件调用。正在执行的同步 `Plugin::call` 无法被
    /// 抢占，这是协作式取消的边界。
    pub fn call_approved_batch_cancellable(
        &mut self,
        calls: &[(String, Value)],
        max_parallel: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Vec<Result<Value, String>> {
        use std::sync::atomic::Ordering;

        let cancelled = || {
            cancel
                .map(|flag| flag.load(Ordering::Relaxed))
                .unwrap_or(false)
        };
        let mut results: Vec<Result<Value, String>> = (0..calls.len())
            .map(|_| Err("batch result missing".to_owned()))
            .collect();
        let mut groups: BTreeMap<String, Vec<(usize, String, Value)>> = BTreeMap::new();

        for (index, (tool, arguments)) in calls.iter().enumerate() {
            if cancelled() {
                results[index] = Err("cancelled".to_owned());
                continue;
            }
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
                                if cancelled() {
                                    out.push((index, Err("cancelled".to_owned())));
                                    continue;
                                }
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

    pub fn slot_registrations(
        &self,
        slot: &str,
    ) -> Vec<&dshr_ui_contributions::RegistrationRecord> {
        self.slots.registrations(slot)
    }

    pub fn active_slot(
        &self,
        target: SlotTarget,
    ) -> Option<&dshr_ui_contributions::RegistrationRecord> {
        self.slots.active(target)
    }

    pub fn tool_slot_targets(&self, name: &str) -> Vec<SlotTarget> {
        let Some(ToolOrigin::Native { plugin_id }) =
            self.registry.get(name).map(|tool| &tool.origin)
        else {
            return Vec::new();
        };
        [TOOL_CALL_VIEW, TOOL_RESULT_EXTRA]
            .into_iter()
            .flat_map(|slot| self.slots.registrations(slot))
            .filter(|record| slots::matches_tool(record, plugin_id, name))
            .map(SlotTarget::from)
            .collect()
    }

    fn native_tool_owner(&self, tool: &str) -> Option<&str> {
        match &self.registry.get(tool)?.origin {
            ToolOrigin::Native { plugin_id } => Some(plugin_id),
            _ => None,
        }
    }

    pub fn chat_node_targets(&self, tool: &str) -> Vec<(SlotTarget, &str)> {
        let Some(owner) = self.native_tool_owner(tool) else {
            return Vec::new();
        };
        self.slots
            .registrations(CHAT_NODE)
            .into_iter()
            .filter(|record| record.plugin_id.as_deref() == Some(owner))
            .filter_map(|record| Some((SlotTarget::from(record), record.key.as_deref()?)))
            .collect()
    }

    pub fn take_tool_chat_node(&self, tool: &str, result: &mut Value) -> Option<Value> {
        let owner = self.native_tool_owner(tool)?;
        let node = self.plugins.get(owner)?.take_chat_node(result)?;
        let fields = node.as_object()?;
        let key = node["key"].as_str()?;
        let fallback = node["fallback"].as_str()?;
        if fields.len() != 3
            || !fields.contains_key("data")
            || fallback.trim().is_empty()
            || fallback.len() > 2048
            || serde_json::to_vec(&node).ok()?.len() > 16 * 1024
            || !self
                .chat_node_targets(tool)
                .iter()
                .any(|(_, registered)| *registered == key)
        {
            return None;
        }
        Some(node)
    }

    pub fn render_chat_node_slot(
        &mut self,
        target: SlotTarget,
        tool: &str,
        context: &ChatNodeContext,
    ) -> Result<Value, HostError> {
        let inactive = || HostError::NotFound("ADC conversation node for this tool".to_owned());
        let owner = self
            .native_tool_owner(tool)
            .ok_or_else(inactive)?
            .to_owned();
        let view = self
            .slots
            .active(target)
            .filter(|record| {
                record.slot_id == CHAT_NODE
                    && record.plugin_id.as_deref() == Some(&owner)
                    && record.key.as_deref() == Some(&context.key)
            })
            .and_then(|record| record.view.as_deref())
            .ok_or_else(inactive)?;
        let context = serde_json::to_value(context)
            .map_err(|error| PluginError::Invalid(error.to_string()))?;
        self.plugins
            .get_mut(&owner)
            .ok_or_else(inactive)?
            .render_chat_node(view, &context)
            .map_err(HostError::Plugin)
    }

    pub fn render_tool_slot(
        &mut self,
        target: SlotTarget,
        context: &Value,
    ) -> Result<Value, HostError> {
        let inactive = || HostError::NotFound("ADC slot for this tool call".to_owned());
        let name = context["tool"].as_str().ok_or_else(inactive)?;
        let Some(ToolOrigin::Native { plugin_id }) =
            self.registry.get(name).map(|tool| &tool.origin)
        else {
            return Err(inactive());
        };
        let view = self
            .slots
            .active(target)
            .filter(|record| slots::matches_tool(record, plugin_id, name))
            .and_then(|record| record.view.as_deref())
            .ok_or_else(inactive)?;
        let plugin = self.plugins.get_mut(plugin_id).ok_or_else(inactive)?;
        let value = context
            .get("value")
            .and_then(|result| plugin.tool_view_value(result))
            .ok_or_else(inactive)?;
        let context = serde_json::json!({
            "call_seq": context["call_seq"], "tool": name,
            "arguments": context["arguments"], "value": value
        });
        if serde_json::to_vec(&context).map_or(true, |bytes| bytes.len() > 128 * 1024) {
            return Err(
                PluginError::Invalid("tool view context exceeds 128 KiB".to_owned()).into(),
            );
        }
        plugin
            .render_tool_view(view, &context)
            .map_err(HostError::Plugin)
    }

    pub fn dispatch_slot_action(
        &mut self,
        target: SlotTarget,
        action: &str,
        payload: &Value,
    ) -> Result<(), HostError> {
        let Some((plugin_id, view)) = self
            .slots
            .active(target)
            .filter(|record| {
                !slots::is_tool_view_slot(&record.slot_id)
                    && !matches!(record.slot_id.as_str(), VIEWER_TAB | CHAT_NODE)
            })
            .and_then(|record| record.plugin_id.clone().zip(record.view.clone()))
        else {
            return Err(PluginError::Invalid(
                "ADC slot registration is no longer active".to_owned(),
            )
            .into());
        };
        self.dispatch_action(&plugin_id, &view, action, payload)
    }

    pub fn render_file_slot(
        &mut self,
        target: SlotTarget,
        context: &FileViewContext,
    ) -> Result<Value, HostError> {
        let inactive = || HostError::NotFound("ADC preview for this file type".to_owned());
        let (plugin, view) = self
            .slots
            .active(target)
            .filter(|record| {
                record.slot_id == VIEWER_TAB && record.key.as_deref() == Some(&context.file_type)
            })
            .and_then(|record| record.plugin_id.as_deref().zip(record.view.as_deref()))
            .ok_or_else(inactive)?;
        if !context.is_valid() {
            return Err(PluginError::Invalid("invalid file preview context".to_owned()).into());
        }
        let context = serde_json::to_value(context)
            .map_err(|error| PluginError::Invalid(error.to_string()))?;
        self.plugins
            .get_mut(plugin)
            .ok_or_else(inactive)?
            .render_file_view(view, &context)
            .map_err(HostError::Plugin)
    }

    pub fn dispatch_registered_action(&mut self, target: SlotTarget) -> Result<(), HostError> {
        let record = self
            .slots
            .active(target)
            .filter(|record| matches!(record.slot_id.as_str(), COMMAND_SLOT | MENU_ITEM_SLOT))
            .ok_or_else(|| HostError::NotFound("inactive ADC command or menu item".to_owned()))?;
        let (Some(plugin), Some(run), Some(id)) =
            (&record.plugin_id, &record.run, &record.registration_id)
        else {
            return Err(HostError::NotFound(
                "ADC action registration is incomplete".to_owned(),
            ));
        };
        let (plugin, run, payload) = (plugin.clone(), run.clone(), serde_json::json!({"id": id}));
        self.dispatch_action(&plugin, &run, "command", &payload)
    }

    pub fn is_loaded(&self, plugin_id: &str) -> bool {
        self.plugins.contains_key(plugin_id)
    }

    pub fn loaded_plugins(&self) -> Vec<&str> {
        self.plugins.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests;
