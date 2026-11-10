//! CPP-v1 的公开契约：插件实现这个 trait，宿主提供 `PluginContext`。
//!
//! 这一层**不依赖 GPUI、不依赖 Cordis 实现**：它只描述"插件能声明什么"。
//! 生命周期（Fiber / generation / 回滚）由 `plugin-host` 负责。

#![forbid(unsafe_code)]

use dshr_tool_runtime::ToolDefinition;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginError {
    #[error("plugin already registers a tool named `{0}`")]
    DuplicateTool(String),
    #[error("invalid plugin: {0}")]
    Invalid(String),
    #[error("plugin `{plugin}` does not implement tool `{tool}`")]
    UnknownTool { plugin: String, tool: String },
    #[error("tool `{tool}` failed: {reason}")]
    ToolFailed { tool: String, reason: String },
}

/// 一个 UI 贡献（当前只承载元数据；渲染由 ui-contributions 在 M2 接）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelContribution {
    pub id: String,
    pub title: String,
    pub slot: String,
}

impl PanelContribution {
    pub fn new(id: impl Into<String>, title: impl Into<String>, slot: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            slot: slot.into(),
        }
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
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "0.1.0"
    }

    /// 声明式注册 + 初始化。返回 `Err` 时宿主必须回滚本次全部声明。
    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>;

    /// 执行一次工具调用。宿主已完成 Policy 与 schema 校验。
    fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError>;

    /// 清理钩子：宿主已经保证注册被撤销，这里只处理插件自己的资源。
    fn unload(&mut self) {}
}
