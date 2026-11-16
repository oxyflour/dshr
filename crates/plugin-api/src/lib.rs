//! CPP-v1 的公开契约：插件实现这个 trait，宿主提供 `PluginContext`。
//!
//! 这一层**不依赖 GPUI、不依赖 Cordis 实现**：它只描述"插件能声明什么"。
//! 生命周期（Fiber / generation / 回滚）由 `plugin-host` 负责。
//!
//! ## 与 cordis-rs 的适配缝隙
//!
//! 本 crate 是 CPP-v1 的**稳定面**：`Plugin` trait、`PluginContext`、`PluginError`、
//! 结果与 capability 类型都不感知具体 Fiber 实现。ADR-0002 选定的 cordis-rs 只允许
//! 出现在 `plugin-host` 内部；将来把 `plugin-host::fiber` 换成 cordis-rs（或反之）
//! 不需要改这里的任何类型。新增的 [`capability`]、[`cancel`]、[`tool_result`] 模块
//! 都保持这一边界：纯 std + serde 数据，无执行器、无运行时依赖。

#![forbid(unsafe_code)]

pub mod cancel;
pub mod capability;
pub mod manifest;
pub mod tool_result;

use dshr_tool_runtime::ToolDefinition;
use serde_json::Value;
use thiserror::Error;

pub use cancel::{CancellationToken, Cancelled};
pub use capability::{Capability, CapabilitySet};
pub use manifest::{HostVersion, Manifest};

/// 宿主兼容基线（= `dshr-plugin-api` 的 semver）。
///
/// 插件的 `min_host_version` 与它比较；宿主应用自身的版本（workspace `0.0.0`）
/// 不参与插件兼容判定，避免产品版本与 SDK 契约版本耦合。
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
pub use tool_result::{ContentPart, ToolResult};

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PluginError {
    #[error("plugin already registers a tool named `{0}`")]
    DuplicateTool(String),
    #[error("invalid plugin: {0}")]
    Invalid(String),
    #[error("plugin `{plugin}` does not implement tool `{tool}`")]
    UnknownTool { plugin: String, tool: String },
    #[error("tool `{tool}` failed: {reason}")]
    ToolFailed { tool: String, reason: String },

    // ---- CPP-v1 manifest（spec/cpp-v1.md §3；错误里不携带 serde_json::Value，保持 Eq）----
    #[error("manifest error at `{path}`: {reason}")]
    Manifest { path: String, reason: String },
    #[error("unsupported plugin api `{actual}`, expected `{expected}`")]
    UnsupportedApi { expected: String, actual: String },
    #[error("plugin requires host version >= {required}, but host is {actual}")]
    HostTooOld { required: String, actual: String },
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    // ---- Service / 版本（spec/cpp-v1.md §5、§10、§9）----
    #[error("service key `{key}` is already provided in this realm")]
    ServiceConflict { key: String },
    #[error("required service `{key}` is not available")]
    ServiceMissing { key: String },
    #[error("invalid service key `{key}`: {reason}")]
    InvalidServiceKey { key: String, reason: String },
    #[error("unsupported version `{actual}`; expected `{expected}`")]
    Version { expected: String, actual: String },

    // ---- Fiber 生命周期与 Effect 清理（spec/cpp-v1.md §4、§6、§9）----
    #[error("invalid fiber transition `{from}` -> `{to}`")]
    InvalidTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("plugin `{plugin_id}` is already loaded")]
    AlreadyLoaded { plugin_id: String },
    #[error("plugin `{plugin_id}` is not loaded")]
    NotLoaded { plugin_id: String },
    #[error("plugin `{plugin_id}` is unloading; new registrations are rejected")]
    Unloading { plugin_id: String },
    #[error("apply of plugin `{plugin_id}` timed out after {timeout_ms} ms")]
    ApplyTimeout { plugin_id: String, timeout_ms: u64 },
    #[error("unload of plugin `{plugin_id}` timed out after {timeout_ms} ms")]
    UnloadTimeout { plugin_id: String, timeout_ms: u64 },
    #[error("effect `{effect}` did not finish within {timeout_ms} ms")]
    EffectTimeout { effect: String, timeout_ms: u64 },

    // ---- capability / 取消 / 结算（spec/cpp-v1.md §7.4、§9、§10）----
    #[error("capability `{capability}` is not available on this host")]
    CapabilityMissing { capability: String },
    #[error("operation `{operation}` was cancelled")]
    Cancelled { operation: String },
    #[error("tool call `{call_id}` is already settled")]
    AlreadySettled { call_id: String },
}

impl PluginError {
    /// 映射到 spec/cpp-v1.md §9 的稳定错误码。
    ///
    /// 这些字符串是跨进程/跨语言契约（conformance 依赖它们），不要按 `Display` 文本匹配。
    pub fn code(&self) -> &'static str {
        match self {
            Self::DuplicateTool(_) => "E_TOOL_CONFLICT",
            Self::Invalid(_) | Self::UnknownTool { .. } | Self::ToolFailed { .. } => "E_INTERNAL",
            Self::Manifest { .. } | Self::InvalidManifest(_) | Self::InvalidServiceKey { .. } => {
                "E_MANIFEST"
            }
            Self::UnsupportedApi { .. } | Self::HostTooOld { .. } | Self::Version { .. } => {
                "E_VERSION"
            }
            Self::ServiceConflict { .. } => "E_SERVICE_CONFLICT",
            Self::ServiceMissing { .. } => "E_SERVICE_MISSING",
            Self::InvalidTransition { .. }
            | Self::AlreadyLoaded { .. }
            | Self::NotLoaded { .. } => "E_INTERNAL",
            Self::Unloading { .. } => "E_UNLOADING",
            Self::ApplyTimeout { .. } => "E_APPLY_TIMEOUT",
            Self::UnloadTimeout { .. } | Self::EffectTimeout { .. } => "E_UNLOAD_TIMEOUT",
            Self::CapabilityMissing { .. } => "E_PERMISSION",
            Self::Cancelled { .. } => "E_CANCELLED",
            Self::AlreadySettled { .. } => "E_ALREADY_SETTLED",
        }
    }

    /// spec/cpp-v1.md §9 的"可恢复"列：调用方可以退避重试/重新装载。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ServiceMissing { .. }
                | Self::ApplyTimeout { .. }
                | Self::UnloadTimeout { .. }
                | Self::Cancelled { .. }
        )
    }
}

/// 一个 UI 贡献（元数据 + 可选 ADC-v1 view 名称；渲染由 ui-contributions/ui-gpui 在 M3 接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelContribution {
    pub id: String,
    pub title: String,
    pub slot: String,
    /// ADC-v1 `view` 名称（`declareView` 声明的那个）；`None` 表示插件不渲染视图。
    ///
    /// 与 `id` 分开：`id` 是注册身份（`workspace.panel` 下的唯一键），`view` 是
    /// `Plugin::render_view` 的入参（spec/adc-v1.md §4.4/§8）。
    pub view: Option<String>,
}

impl PanelContribution {
    pub fn new(id: impl Into<String>, title: impl Into<String>, slot: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            slot: slot.into(),
            view: None,
        }
    }

    /// 声明该 panel 渲染哪个 ADC view（additive：旧调用点不传 view 时行为不变）。
    pub fn with_view(mut self, view: impl Into<String>) -> Self {
        self.view = Some(view.into());
        self
    }

    /// 该 panel 渲染的 view；未声明时回落到 `id`（便于只注册一个名字的插件）。
    pub fn view_name(&self) -> &str {
        self.view.as_deref().unwrap_or(&self.id)
    }
}

/// 插件在 `apply` 期间唯一能接触的东西。
///
/// 注意：这里只做**声明**。真正写入 ToolRegistry / UI Registry 由宿主在 `apply` 返回后原子提交，
/// 所以插件中途报错不会留下半个注册。
pub struct PluginContext {
    plugin_id: String,
    tools: Vec<ToolDefinition>,
    panels: Vec<PanelContribution>,
}

impl PluginContext {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            tools: Vec::new(),
            panels: Vec::new(),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn register_tool(&mut self, definition: ToolDefinition) -> Result<(), PluginError> {
        if self.tools.iter().any(|tool| tool.name == definition.name) {
            return Err(PluginError::DuplicateTool(definition.name));
        }
        self.tools.push(definition);
        Ok(())
    }

    pub fn register_panel(&mut self, panel: PanelContribution) {
        self.panels.push(panel);
    }

    /// 宿主在 `apply` 成功后取走声明；之后再调用为空。
    pub fn take_registrations(&mut self) -> (Vec<ToolDefinition>, Vec<PanelContribution>) {
        (
            std::mem::take(&mut self.tools),
            std::mem::take(&mut self.panels),
        )
    }
}

/// CPP-v1 插件入口。`rust-static` / `wasm-component` / `external-process` 共用这一层语义。
///
/// `Send` 是硬约束：插件可能被宿主移到工作线程执行（UI 线程不允许被工具阻塞）。
pub trait Plugin: Send {
    /// 插件 id。返回借用字符串：外部进程/WASM 插件的 id 来自 manifest，不能被迫泄漏。
    fn id(&self) -> &str;
    fn version(&self) -> &str {
        "0.1.0"
    }

    /// 声明式注册 + 初始化。返回 `Err` 时宿主必须回滚本次全部声明。
    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>;

    /// 执行一次工具调用。宿主已完成 Policy 与 schema 校验。
    fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError>;

    /// 渲染一个 ADC-v1 view（`declareView` 声明的名称，见 spec/adc-v1.md §8）。
    ///
    /// 默认实现返回可读错误：不支持 UI 的插件不需要改代码即可继续工作（additive 契约）。
    /// 返回值是元素树 JSON，由宿主用 `parse_tree` + `adc_panel_body` 渲染。
    fn render_view(&mut self, _view: &str) -> Result<Value, PluginError> {
        Err(PluginError::Invalid(
            "plugin does not render views".to_owned(),
        ))
    }

    /// 处理 ADC `ui.action`（M4 W3）：`action` 为 `call` / `command`，`payload` 是其参数。
    ///
    /// fire-and-forget：宿主发完就返回；插件更新自身状态后由宿主再次 `render_view`。
    /// 默认返回可读错误，保持 additive 契约（没有 UI 动作的插件不需要改代码）。
    fn handle_action(
        &mut self,
        _view: &str,
        action: &str,
        _payload: &Value,
    ) -> Result<(), PluginError> {
        Err(PluginError::Invalid(format!(
            "plugin does not handle ADC action `{action}`"
        )))
    }

    /// 清理钩子：宿主已经保证注册被撤销，这里只处理插件自己的资源。
    fn unload(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    // CPP-FIBER-131..133：错误码映射是跨语言契约。
    #[test]
    fn cpp_fiber_131_error_codes_match_spec_table() {
        assert_eq!(
            PluginError::ServiceConflict {
                key: "tool-registry/v1".to_owned()
            }
            .code(),
            "E_SERVICE_CONFLICT"
        );
        assert_eq!(
            PluginError::ServiceMissing {
                key: "storage/v1".to_owned()
            }
            .code(),
            "E_SERVICE_MISSING"
        );
        assert_eq!(
            PluginError::Version {
                expected: "cordis-portable/v1".to_owned(),
                actual: "cordis-portable/v2".to_owned()
            }
            .code(),
            "E_VERSION"
        );
        assert_eq!(
            PluginError::Unloading {
                plugin_id: "cad.validate".to_owned()
            }
            .code(),
            "E_UNLOADING"
        );
        assert_eq!(
            PluginError::Cancelled {
                operation: "tool".to_owned()
            }
            .code(),
            "E_CANCELLED"
        );
        assert_eq!(
            PluginError::AlreadySettled {
                call_id: "call-1".to_owned()
            }
            .code(),
            "E_ALREADY_SETTLED"
        );
    }

    #[test]
    fn cpp_fiber_132_retryable_flags_match_spec_recoverable_column() {
        assert!(PluginError::ServiceMissing {
            key: "storage/v1".to_owned()
        }
        .is_retryable());
        assert!(PluginError::ApplyTimeout {
            plugin_id: "cad.validate".to_owned(),
            timeout_ms: 10_000
        }
        .is_retryable());
        assert!(!PluginError::ServiceConflict {
            key: "storage/v1".to_owned()
        }
        .is_retryable());
        assert!(!PluginError::CapabilityMissing {
            capability: capability::TOOL_STREAM.to_owned()
        }
        .is_retryable());
    }

    #[test]
    fn cpp_fiber_133_errors_stay_eq_and_clone() {
        let error = PluginError::InvalidTransition {
            from: "Pending",
            to: "Active",
        };
        assert_eq!(error.clone(), error);
        assert_eq!(error.code(), "E_INTERNAL");
    }

    // ADC-ELEM-PANEL-API-001：trait 默认 render_view 不破坏既有插件。
    struct NoViewPlugin;

    impl Plugin for NoViewPlugin {
        fn id(&self) -> &'static str {
            "api.no-view"
        }

        fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        fn call(&mut self, _tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
            Err(PluginError::Invalid("no tools".to_owned()))
        }
    }

    #[test]
    fn adc_elem_panel_api_001_default_render_view_is_a_readable_error() {
        let mut plugin = NoViewPlugin;
        let error = plugin.render_view("workspace.panel").unwrap_err();
        assert!(error.to_string().contains("does not render views"));
        assert_eq!(error.code(), "E_INTERNAL");
    }

    // ADC-ELEM-PANEL-API-002：panel 的 view 与 id 分开，未声明 view 时回落到 id。
    #[test]
    fn adc_elem_panel_api_002_panel_view_falls_back_to_id() {
        let plain = PanelContribution::new("probe.panel", "Probe", "workspace.panel");
        assert_eq!(plain.view, None);
        assert_eq!(plain.view_name(), "probe.panel");

        let with_view = PanelContribution::new("probe.panel", "Probe", "workspace.panel")
            .with_view("probe.view");
        assert_eq!(with_view.view.as_deref(), Some("probe.view"));
        assert_eq!(with_view.view_name(), "probe.view");
    }

    // M4-TRAIT-001：id/version 返回借用字符串，外部/WASM 插件的动态 id 不再被迫 leak。
    #[test]
    fn m4_trait_001_dynamic_plugin_id_is_borrowed() {
        struct DynamicIdPlugin(String);
        impl Plugin for DynamicIdPlugin {
            fn id(&self) -> &str {
                &self.0
            }
            fn version(&self) -> &str {
                "9.9.9"
            }
            fn apply(&mut self, _ctx: &mut PluginContext) -> Result<(), PluginError> {
                Ok(())
            }
            fn call(
                &mut self,
                _tool: &str,
                _arguments: &serde_json::Value,
            ) -> Result<serde_json::Value, PluginError> {
                Ok(serde_json::Value::Null)
            }
        }
        let plugin = DynamicIdPlugin("dynamic.plugin".to_owned());
        assert_eq!(plugin.id(), "dynamic.plugin");
        assert_eq!(plugin.version(), "9.9.9");
    }
}
