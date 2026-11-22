//! Agent Loop：Turn / Step / 工具调用事件流。
//!
//! 这一层是 PLAN §6.1 的最小实现，也是 M2 的入口：
//! **模型只负责产出文本与工具调用意图，执行与记录由宿主完成**。
//!
//! 两个可替换的接口：
//! - [`ModelProvider`]：目前是 [`MockProvider`]（关键词决定行为），M2 换成真实 LLM；
//! - [`ToolExecutor`]：由应用实现（chat 里接的是 PluginHost + Sidecar）。
//!
//! `Agent` 只负责编排与事件产出，不碰 Session Log、不碰 UI——这让它可以被单测。

#![forbid(unsafe_code)]

pub mod session_tree;

pub use session_tree::{
    spawn_child_spec, DrainMode, DrainWait, InterruptAction, InterruptPlan, InterruptTarget,
    PolicyTightening, SessionPolicy, SessionStatus, SessionTreeRuntime, SpawnChildArgs,
    SpawnContext, SpawnError, SpawnedChild, TreeSession, DEFAULT_MAX_DEPTH, SPAWN_CHILD_TOOL,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// 取消原因字符串：与 `session.interrupted` / sidecar `tool.cancel` 的 reason 对齐。
const CANCEL_REASON: &str = "cancel";

/// 一轮内的可选预算（默认**不限制**，对齐官方 DSH）。
///
/// 官方 DSH 0.2.2-rc.1 的 `@deepseek-ai/dsh-agent-loop` 只声明
/// `maxParallelToolCalls`（默认 10），**没有**每轮工具调用总数或 step 上限：
/// 一个 step 里模型发多少 `tool_calls` 就分批全执行，循环由模型自己收敛。
/// dshr 早期用 `MAX_TOOL_CALLS = 8` 直接 `TurnFailed`，把"一步并发 9 个调用"
/// 这类正常场景判成整轮失败；现在默认放开，预算只保留为**可选**的兜底：
///
/// - [`ToolBudget::max_tool_calls`]：一轮内最多真正执行的工具调用数。超出的调用
///   **不执行**，但事件流仍然发 `ToolCall` + `is_error` 的 `ToolResult`（降级而非
///   失败），并记入本轮的 `tool` 消息；本步剩余调用逐条标记失败后结束该步，然后
///   直接 `TurnEnded`（不再给模型继续发工具调用的机会，避免预算外的"空转步"无限
///   循环）。
/// - [`ToolBudget::max_steps`]：一轮内最多 step 数。命中后不再发起新的模型请求，
///   直接 `TurnEnded`（同样不是 `TurnFailed`）。
///
/// 两者都可以用环境变量显式收紧（`DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS`）；
/// 空值、`0`、非法值都表示"不限制"，与 DSH 默认一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolBudget {
    pub max_tool_calls: Option<usize>,
    pub max_steps: Option<usize>,
}

/// 解析预算环境变量的值：空/`0`/非法值都返回 `None`（即不限制）。
pub fn parse_positive_limit(raw: &str) -> Option<usize> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<usize>().ok().filter(|limit| *limit > 0)
}

impl ToolBudget {
    /// 从 `DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS` 读取可选预算。
    pub fn from_env() -> Self {
        Self {
            max_tool_calls: parse_budget_env("DSHR_MAX_TOOL_CALLS"),
            max_steps: parse_budget_env("DSHR_MAX_STEPS"),
        }
    }
}

fn parse_budget_env(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_positive_limit(&value))
}

/// DSH `maxParallelToolCalls` 的默认值（`@deepseek-ai/dsh-agent-loop` 0.2.2-rc.1）。
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;

/// 并行上限的环境变量（空/`0`/非法 → 默认 10，与 DSH 一致）。
pub const MAX_PARALLEL_TOOL_CALLS_ENV: &str = "DSHR_MAX_PARALLEL_TOOL_CALLS";

// ---------------------------------------------------------------------------
// DSH 默认差异（M3）：reasoning effort / 重复工具调用软提醒
// ---------------------------------------------------------------------------

/// DSH reasoning effort 档位（`@deepseek-ai/dsh-llm-deepseek` 的
/// `off` / `low` / `high` / `max`）。
///
/// 官方 desktop profile 默认 `high`；dshr 不设默认——未配置时不发送该参数，
/// 保持“服务端默认”语义（与 temperature 的处理一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Off,
    Low,
    High,
    Max,
}

impl ReasoningEffort {
    /// 环境变量名；provider 构造时读取（crate 内完成，不要求调用方改）。
    pub const ENV: &'static str = "DSHR_REASONING_EFFORT";

    /// 解析档位；大小写不敏感，非法返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "low" => Some(Self::Low),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    /// 从 `DSHR_REASONING_EFFORT` 读取；未配置 / 非法都返回 `None`（非法会警告）。
    pub fn from_env() -> Option<Self> {
        Self::resolve(std::env::var(Self::ENV).ok().as_deref())
    }

    /// 纯函数版（测试/显式注入）：非法值写 stderr 警告后忽略，不失败。
    pub fn resolve(raw: Option<&str>) -> Option<Self> {
        let raw = raw?;
        if raw.trim().is_empty() {
            return None;
        }
        match Self::parse(raw) {
            Some(effort) => Some(effort),
            None => {
                eprintln!(
                    "[agent-core] ignoring invalid {}={raw} (allowed: off/low/high/max)",
                    Self::ENV
                );
                None
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// 线上值：`off` 不发送，`low` / `high` / `max` 原样发送（DSH 映射）。
    pub fn wire_value(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            other => Some(other.as_str()),
        }
    }
}

/// 重复工具调用的默认阈值（DSH `repeat-tool-reminder.thresholds` = `[3, 5, 8]`）。
pub const REPEAT_TOOL_REMINDER_THRESHOLDS: [usize; 3] = [3, 5, 8];
/// 开关（`on`/`off`）。
pub const REPEAT_TOOL_REMINDER_CONFIG_ENV: &str = "DSHR_REPEAT_TOOL_REMINDERS";
/// 自定义阈值（逗号分隔，如 `3,5,8`）。
pub const REPEAT_TOOL_REMINDER_THRESHOLDS_ENV: &str = "DSHR_REPEAT_TOOL_REMINDER_THRESHOLDS";

/// 重复工具调用软提醒配置（默认开启，阈值 `[3,5,8]`，与 DSH 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatToolReminderConfig {
    pub enabled: bool,
    /// 触发提醒的“第 N 次”计数；升序、去重、只保留正数。
    pub thresholds: Vec<usize>,
}

impl Default for RepeatToolReminderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            thresholds: REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec(),
        }
    }
}

impl RepeatToolReminderConfig {
    /// 从 env 读取：`DSHR_REPEAT_TOOL_REMINDERS=off` 关闭；
    /// `DSHR_REPEAT_TOOL_REMINDER_THRESHOLDS=3,5,8` 覆盖阈值（非法值回退默认）。
    pub fn from_env() -> Self {
        let enabled = match std::env::var(REPEAT_TOOL_REMINDER_CONFIG_ENV) {
            Ok(raw) => !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no" | "disabled"
            ),
            Err(_) => true,
        };
        let thresholds = std::env::var(REPEAT_TOOL_REMINDER_THRESHOLDS_ENV)
            .ok()
            .and_then(|raw| parse_repeat_thresholds(&raw))
            .unwrap_or_else(|| REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec());
        Self {
            enabled,
            thresholds,
        }
    }

    /// 关闭提醒（测试 / 需要完全静默的调用方）。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            thresholds: Vec::new(),
        }
    }

    /// 自定义阈值：过滤 0、去重、升序。
    pub fn with_thresholds(mut self, thresholds: impl IntoIterator<Item = usize>) -> Self {
        let mut values: Vec<usize> = thresholds.into_iter().filter(|count| *count > 0).collect();
        values.sort_unstable();
        values.dedup();
        self.thresholds = values;
        self
    }
}

/// 解析逗号分隔阈值；任意一项非法即整体非法（返回 `None`，调用方回退默认）。
pub fn parse_repeat_thresholds(raw: &str) -> Option<Vec<usize>> {
    let mut values = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let count = part.parse::<usize>().ok().filter(|count| *count > 0)?;
        values.push(count);
    }
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}

/// 一次触发的软提醒（供宿主记录 `tool.repeat` 诊断/日志）。
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatToolReminder {
    pub tool: String,
    pub arguments: Value,
    /// 本轮内该（工具 + 参数）出现的第几次。
    pub count: usize,
    /// 命中的阈值（与 `count` 相同；显式保留便于日志核对）。
    pub threshold: usize,
    /// 注入模型的提醒文本。
    pub text: String,
}

impl RepeatToolReminder {
    fn new(tool: &str, arguments: &Value, count: usize) -> Self {
        let text = format!(
            "[重复工具调用提醒] 你已在本轮第 {count} 次用相同参数调用工具 `{tool}`。\
             如果已经拿到足够信息，请停止重复调用并直接给出最终回答；\
             如果确实还需要调用，请换一种参数或说明为什么必须重复。"
        );
        Self {
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            count,
            threshold: count,
            text,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema（原样交给模型；MockProvider 忽略它）。
    pub parameters: Value,
}

/// 模型返回的工具调用（OpenAI `tool_calls[]` 的形状：id 用来对结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolCall {
    /// Provider 分配的工具调用 id；`role: tool` 消息必须回填同一个 id。
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// 发给模型的一条消息。**这里是严格 OpenAI 协议的形状**：
/// assistant 的工具调用与 `tool` 结果必须按顺序配对，否则模型无法把结果关联回调用。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    UserWithAttachments {
        text: String,
        attachments: Vec<MediaAttachment>,
    },
    Assistant {
        reasoning: String,
        text: String,
        tool_calls: Vec<ModelToolCall>,
    },
    Tool {
        /// 对应 `ModelToolCall::id`。
        call_id: String,
        content: String,
    },
    /// Media projected after all textual results for a tool call batch.
    ToolImages(Vec<MediaAttachment>),
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User(text.into())
    }

    pub fn user_text(&self) -> Option<&str> {
        match self {
            Self::User(text) | Self::UserWithAttachments { text, .. } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Pdf,
}

impl AttachmentFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Pdf => "application/pdf",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            "application/pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    pub name: String,
    pub format: AttachmentFormat,
    /// ID of the durable attachment. Bytes are restored only for the retained conversation.
    pub reference: String,
    pub bytes: Option<Arc<[u8]>>,
}

impl std::fmt::Debug for MediaAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaAttachment")
            .field("name", &self.name)
            .field("format", &self.format)
            .field("reference", &self.reference)
            .field("bytes_len", &self.bytes.as_ref().map(|bytes| bytes.len()))
            .finish()
    }
}

impl MediaAttachment {
    pub fn data_url(&self) -> Option<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let bytes = self.bytes.as_ref()?;
        Some(format!(
            "data:{};base64,{}",
            self.format.mime(),
            STANDARD.encode(bytes)
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub is_error: bool,
    /// 模型可见内容（DSH: `content: ContentBlock[]`）。
    pub content: Value,
    /// 结构化返回值（DSH: `value`）。展示与回放用它，不要再从 content 里挖。
    pub value: Value,
    /// 工具自带的元数据（DSH: `meta`）。
    pub meta: Option<Value>,
}

impl ToolOutcome {
    pub fn success(content: Value, value: Value) -> Self {
        Self {
            is_error: false,
            content,
            value,
            meta: None,
        }
    }

    pub fn failure(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            is_error: true,
            content: json!([{ "type": "text", "text": reason }]),
            value: Value::Null,
            meta: None,
        }
    }
}

pub fn tool_image_references(content: &Value) -> Vec<MediaAttachment> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| {
            if block["type"] != "image" {
                return None;
            }
            let reference = block["attachment"]["attachmentId"].as_str()?;
            let hash = reference.strip_prefix("spill:")?;
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let format = AttachmentFormat::from_mime(block["attachment"]["mediaType"].as_str()?)?;
            if format == AttachmentFormat::Pdf {
                return None;
            }
            Some(MediaAttachment {
                name: block["attachment"]["name"].as_str().unwrap_or("tool image").to_owned(),
                format,
                reference: reference.to_owned(),
                bytes: None,
            })
        })
        .collect()
}

#[derive(Default)]
pub struct StepContext {
    pub system_sections: Vec<String>,
    pub turn_instructions: Vec<String>,
    pub unavailable_tools: Vec<String>,
}

pub trait ToolExecutor {
    fn specs(&self) -> Vec<ToolSpec>;

    /// Refresh the tool inventory immediately before handing it to the model.
    fn prepare_step(&mut self) -> Vec<ToolSpec> {
        self.specs()
    }
    fn prepare_step_context(&mut self, _step: u32, _user_text: Option<&str>) -> StepContext {
        StepContext::default()
    }
    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome;

    fn tool_images(&self, _outcome: &ToolOutcome) -> Vec<MediaAttachment> {
        Vec::new()
    }

    /// 一个 step 内**同一批**工具调用：返回顺序必须与 `calls` 一一对应。
    ///
    /// 默认实现逐个 `execute`（保持串行语义）；支持并发的执行器（如 sidecar）可覆盖，
    /// 在 `max_parallel` 上限内并发跑完再按原顺序返回。预算/取消在 Agent 层裁决，
    /// 这里收到的都是"本批真正要执行"的调用。
    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        let _ = max_parallel;
        calls.iter().map(|call| self.execute(call)).collect()
    }
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// 完整对话历史（含本轮用户消息、以及本轮已发生的 assistant/tool 消息）。
    pub messages: Vec<ChatMessage>,
    pub tool_specs: Vec<ToolSpec>,
}

impl CompletionRequest {
    /// 最后一条用户消息（MockProvider 用它做关键词判定）。
    pub fn last_user_text(&self) -> &str {
        self.messages
            .iter()
            .rev()
            .find_map(ChatMessage::user_text)
            .unwrap_or_default()
    }

    /// 当前这一步里的最后一条工具结果（只看最后一条用户消息之后的消息）。
    ///
    /// 不能扫全历史：上一轮的工具结果会误判成"本轮刚跑完工具"。
    pub fn last_tool_content(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .take_while(|message| message.user_text().is_none())
            .find_map(|message| match message {
                ChatMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelReply {
    pub reasoning: String,
    pub text: String,
    /// 一次回复可以有多个工具调用（OpenAI 的 parallel tool calls）。
    pub tool_calls: Vec<ModelToolCall>,
}

impl ModelReply {
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            reasoning: String::new(),
            text: text.into(),
            tool_calls: Vec::new(),
        }
    }
}

pub enum ModelDelta<'a> {
    Text(&'a str),
    Reasoning(&'a str),
}

pub trait ModelProvider: Send {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String>;

    /// 停止生成的句柄：UI 无需持有 provider 就能请求中断。
    ///
    /// 约定：token 的清零**只由 UI 在每轮开始时做**（chat.rs 的 `start_turn`），
    /// provider 自己绝不清零——否则"工具执行期间按下停止"会被下一次模型请求悄悄
    /// 丢弃（M2 review P1-2）。provider 读到 true 后尽快返回**已生成的部分**，
    /// 由 Agent 走取消语义收尾（`ToolCancelled` / `TurnCancelled`，不是 `TurnFailed`）。
    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        None
    }

    /// 流式补全：每收到一段文本就回调一次，返回完整回复。
    ///
    /// 默认实现回退到 [`ModelProvider::complete`] 并把整段文本作为**一个** delta 回放，
    /// 这样不实现流式的 provider 不需要额外代码。真正流式的 provider（SSE）应覆盖它。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let reply = self.complete(request)?;
        if !reply.text.is_empty() {
            on_delta(&reply.text);
        }
        Ok(reply)
    }

    fn complete_streaming_with_reasoning(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(ModelDelta<'_>),
    ) -> Result<ModelReply, String> {
        self.complete_streaming(request, &mut |text| on_delta(ModelDelta::Text(text)))
    }
}

/// 让 `Agent<Box<dyn ModelProvider>>` 成为可用类型（运行时选择 provider）。
impl ModelProvider for Box<dyn ModelProvider> {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        (**self).complete(request)
    }

    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        (**self).cancel_token()
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        (**self).complete_streaming(request, on_delta)
    }

    fn complete_streaming_with_reasoning(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(ModelDelta<'_>),
    ) -> Result<ModelReply, String> {
        (**self).complete_streaming_with_reasoning(request, on_delta)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: u64,
    },
    StepStarted {
        turn_id: u64,
        step: u32,
    },
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCallRequest),
    ToolResult {
        call_id: String,
        outcome: ToolOutcome,
    },
    /// 工具调用被取消（**未执行**，或执行中被打断）。每个 `ToolCall` 要么有配对的
    /// `ToolResult`，要么有配对的 `ToolCancelled`，UI/会话日志不会出现孤立调用。
    ToolCancelled {
        call_id: String,
        reason: String,
    },
    TurnFailed(String),
    /// 本轮因取消而结束（不是失败）：已产生的文本/工具结果保留，不再发起新请求。
    TurnCancelled {
        reason: String,
    },
    TurnEnded {
        text: String,
    },
}

pub struct Agent<P> {
    provider: P,
    turn: u64,
    budget: ToolBudget,
    /// 覆盖 `maxParallelToolCalls`（None = 读 env / 默认 10；测试注入确定性值用）。
    max_parallel_tool_calls: Option<usize>,
    /// 重复工具调用软提醒（默认开启，与 DSH `repeat-tool-reminder` 一致）。
    repeat_reminders: RepeatToolReminderConfig,
    reminder_log: Vec<RepeatToolReminder>,
}

impl<P: ModelProvider> Agent<P> {
    /// 默认预算来自环境变量；默认（未设置）与 DSH 一致：不限制工具调用次数/step 数。
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            turn: 0,
            budget: ToolBudget::from_env(),
            max_parallel_tool_calls: None,
            repeat_reminders: RepeatToolReminderConfig::from_env(),
            reminder_log: Vec::new(),
        }
    }

    /// 显式注入预算：测试与需要确定性上限的调用方用。
    pub fn with_budget(provider: P, budget: ToolBudget) -> Self {
        Self {
            provider,
            turn: 0,
            budget,
            max_parallel_tool_calls: None,
            repeat_reminders: RepeatToolReminderConfig::from_env(),
            reminder_log: Vec::new(),
        }
    }

    /// 覆盖每步并行工具数（`None` = 回到 env/默认 10；`0` 视为 1）。
    pub fn with_max_parallel_tool_calls(mut self, max_parallel: Option<usize>) -> Self {
        self.max_parallel_tool_calls = max_parallel.filter(|value| *value > 0);
        self
    }

    /// 当前生效的并行上限：显式覆盖 > `DSHR_MAX_PARALLEL_TOOL_CALLS` > 默认 10。
    pub fn max_parallel_tool_calls(&self) -> usize {
        self.max_parallel_tool_calls
            .or_else(|| parse_budget_env(MAX_PARALLEL_TOOL_CALLS_ENV))
            .unwrap_or(DEFAULT_MAX_PARALLEL_TOOL_CALLS)
            .max(1)
    }

    /// 显式注入重复提醒配置（测试 / 静默运行）。
    pub fn with_repeat_reminders(mut self, config: RepeatToolReminderConfig) -> Self {
        self.repeat_reminders = config;
        self
    }

    pub fn repeat_reminder_config(&self) -> &RepeatToolReminderConfig {
        &self.repeat_reminders
    }

    /// 已触发的软提醒（累计；宿主可在每轮结束后落成 `tool.repeat` 诊断）。
    pub fn repeat_reminder_log(&self) -> &[RepeatToolReminder] {
        &self.reminder_log
    }

    /// 取走已触发的软提醒（调用方负责写 Session Log）。
    pub fn take_repeat_reminders(&mut self) -> Vec<RepeatToolReminder> {
        std::mem::take(&mut self.reminder_log)
    }

    pub fn turn_count(&self) -> u64 {
        self.turn
    }

    pub fn budget(&self) -> ToolBudget {
        self.budget
    }

    /// 访问 provider：上下文压缩需要在 Agent 之外调用一次模型（摘要）。
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// 跑一轮（无历史版本，测试与单轮场景）。
    ///
    /// step 数只受 [`ToolBudget::max_steps`] 约束（默认无限）；工具调用总数只受
    /// [`ToolBudget::max_tool_calls`] 约束（默认无限，与官方 DSH 一致）。
    ///
    /// 事件顺序保证：`TurnStarted` → (`StepStarted` → `TextDelta`* →
    /// (`ToolCall`* → (`ToolResult` | `ToolCancelled`)*)?) → `TurnEnded` /
    /// `TurnFailed` / `TurnCancelled`。同一 step 的工具调用按批执行：先按原顺序发
    /// `ToolCall`，再按原顺序发 paired 终态（M3 并行工具，DSH `maxParallelToolCalls=10`）。
    /// 预算命中是**降级**，不会产生 `TurnFailed`；取消产生 `TurnCancelled`（含未执行
    /// 调用的 `ToolCancelled`），同样不是失败。
    pub fn run_turn(
        &mut self,
        user_text: &str,
        executor: &mut dyn ToolExecutor,
    ) -> Vec<AgentEvent> {
        self.run_turn_with_history(&[], user_text, executor)
    }

    pub fn run_turn_with_history(
        &mut self,
        history: &[ChatMessage],
        user_text: &str,
        executor: &mut dyn ToolExecutor,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        self.run_turn_streaming(history, user_text, executor, &mut |event| {
            events.push(event.clone())
        });
        events
    }

    /// 流式版本：事件一到就回调，不攒到轮次结束。
    ///
    /// `history` 由调用方从 Session Log 重建（`agent-context`），Agent 自己只维护**本轮**
    /// 产生的 assistant/tool 消息，保证 `role: tool` 与 `tool_call_id` 严格配对。
    ///
    /// 取消语义（M2 review P1-2；M3 并行批）：UI 通过 provider 的
    /// [`ModelProvider::cancel_token`] 置位 token；本函数在四个检查点读它
    /// （`Relaxed` 足够：只关心最终可见性）：
    /// 1. 每步开始前（发 `StepStarted` 之前）；
    /// 2. 拿到模型回复之后、执行本步工具之前；
    /// 3. 本批工具执行前（batch 一个都还没发出时）；
    /// 4. 本批执行后（batch 内已发出的调用允许完成，但不再发起下一步）。
    ///
    /// 命中取消时：未发出的调用逐个发 `ToolCall` + `ToolCancelled`（事件配对），
    /// 已完成的调用保留其 `ToolResult`，最后以 `TurnCancelled` 收尾——**不会**产生
    /// `TurnFailed`。预算命中仍是"降级"（`ToolResult(is_error)`），与取消无关。
    pub fn run_turn_streaming(
        &mut self,
        history: &[ChatMessage],
        user_text: &str,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        self.run_turn_streaming_message(history, ChatMessage::user(user_text), executor, on_event);
    }

    pub fn run_turn_streaming_message(
        &mut self,
        history: &[ChatMessage],
        user_message: ChatMessage,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        debug_assert!(user_message.user_text().is_some());
        self.turn += 1;
        let turn_id = self.turn;
        on_event(&AgentEvent::TurnStarted { turn_id });

        // 取消句柄只读：清零由 UI 在每轮开始时做（见 trait 注释）。
        // 测试/Mock provider 没暴露句柄时，取消检查恒为 false。
        let cancel = self.provider.cancel_token();

        let mut messages: Vec<ChatMessage> = history.to_vec();
        let user_text = user_message.user_text().map(str::to_owned);
        messages.push(user_message);
        let mut turn_instructions = Vec::new();

        let mut step: u32 = 1;
        let mut calls = 0usize;
        let mut text = String::new();
        // 命中 max_tool_calls 后置位：本步剩余调用全部标记失败，本步结束后直接收尾。
        let mut budget_exhausted = false;
        // 重复工具调用软提醒：key = (tool, canonical arguments)，计数按“本轮”累计。
        let mut repeat_counts: HashMap<(String, String), usize> = HashMap::new();
        let mut pending_reminders: Vec<RepeatToolReminder> = Vec::new();

        loop {
            // 取消检查点 1：步与步之间，StepStarted 之前。
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }
            if let Some(max_steps) = self.budget.max_steps {
                if step as usize > max_steps {
                    // 优雅结束：不再发起模型请求，也不把整轮判失败。
                    break;
                }
            }
            on_event(&AgentEvent::StepStarted { turn_id, step });

            let mut tool_specs = executor.prepare_step();
            let context = executor.prepare_step_context(step, if step == 1 {
                user_text.as_deref()
            } else {
                None
            });
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled { reason: CANCEL_REASON.to_owned() });
                return;
            }
            tool_specs.retain(|tool| !context.unavailable_tools.contains(&tool.name));
            turn_instructions.extend(context.turn_instructions.into_iter().map(ChatMessage::System));
            let request = CompletionRequest {
                messages: context.system_sections.into_iter().map(ChatMessage::System)
                    .chain(turn_instructions.iter().cloned())
                    .chain(messages.iter().cloned())
                    .collect(),
                tool_specs,
            };

            // provider 边流边回调；这里只负责把 delta 转成事件并累计全文。
            let mut streamed_any = false;
            let mut streamed_reasoning = false;
            let reply = {
                let mut forward = |delta: ModelDelta<'_>| match delta {
                    ModelDelta::Text(delta) if !delta.is_empty() => {
                        streamed_any = true;
                        text.push_str(delta);
                        on_event(&AgentEvent::TextDelta(delta.to_owned()));
                    }
                    ModelDelta::Reasoning(delta) if !delta.is_empty() => {
                        streamed_reasoning = true;
                        on_event(&AgentEvent::ReasoningDelta(delta.to_owned()));
                    }
                    _ => {}
                };
                match self
                    .provider
                    .complete_streaming_with_reasoning(&request, &mut forward)
                {
                    Ok(reply) => reply,
                    Err(err) => {
                        // provider 在取消后返回 Err（如 openai 非流式的 cancelled）不算失败：
                        // 统一收敛为 TurnCancelled，避免 UI 显示"处理失败：cancelled"（P2-10）。
                        if cancel_requested(cancel.as_ref()) {
                            on_event(&AgentEvent::TurnCancelled {
                                reason: CANCEL_REASON.to_owned(),
                            });
                        } else {
                            on_event(&AgentEvent::TurnFailed(err));
                        }
                        return;
                    }
                }
            };

            if !streamed_reasoning && !reply.reasoning.is_empty() {
                on_event(&AgentEvent::ReasoningDelta(reply.reasoning.clone()));
            }
            // provider 没走流式（或实现有 bug 只返回值）：补发一次，保证 UI 不空白。
            if !streamed_any && !reply.text.is_empty() {
                text.push_str(&reply.text);
                on_event(&AgentEvent::TextDelta(reply.text.clone()));
            }

            // 无论有没有工具调用，assistant 这一步都要进历史（协议要求配对）。
            if !reply.text.is_empty() || !reply.tool_calls.is_empty() {
                messages.push(ChatMessage::Assistant {
                    reasoning: reply.reasoning,
                    text: reply.text.clone(),
                    tool_calls: reply.tool_calls.clone(),
                });
            }

            // 取消检查点 2：模型回复之后、执行工具之前。本步所有调用都不执行，
            // 但每个调用仍发 ToolCall + ToolCancelled，保持事件配对。
            if cancel_requested(cancel.as_ref()) {
                cancel_calls(&reply.tool_calls, on_event);
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }

            if reply.tool_calls.is_empty() {
                break;
            }

            // 本 step 的工具调用按批执行（DSH `maxParallelToolCalls`，默认 10）：
            // 1) 按原顺序发 ToolCall、裁决预算/取消，收集"真正要执行"的批；
            // 2) 一次 execute_batch（执行器决定并发程度，默认串行）；
            // 3) 再按原顺序发 ToolResult / 写 tool 消息。
            let calls_in_step = reply.tool_calls.len();
            let mut batch_indices: Vec<usize> = Vec::with_capacity(calls_in_step);
            let mut batch_calls: Vec<ToolCallRequest> = Vec::with_capacity(calls_in_step);
            let mut executed = vec![false; calls_in_step];
            let mut outcomes: Vec<Option<ToolOutcome>> = (0..calls_in_step).map(|_| None).collect();

            // 阶段 1：按原顺序发 ToolCall + 预算/取消裁决。
            for (index, tool_call) in reply.tool_calls.iter().enumerate() {
                let call = tool_call_request(tool_call);
                // 预算耗尽也是"事件完整"的：调用与结果都要发，模型/UI 才能看到配对。
                on_event(&AgentEvent::ToolCall(call.clone()));

                // 取消检查点 3：执行前。当前调用不执行，发配对的 ToolCancelled；
                // 本步剩余调用同样补 ToolCall + ToolCancelled（配平事件流）。
                if cancel_requested(cancel.as_ref()) {
                    on_event(&AgentEvent::ToolCancelled {
                        call_id: call.call_id.clone(),
                        reason: CANCEL_REASON.to_owned(),
                    });
                    cancel_calls(&reply.tool_calls[index + 1..], on_event);
                    on_event(&AgentEvent::TurnCancelled {
                        reason: CANCEL_REASON.to_owned(),
                    });
                    return;
                }

                if let Some(max) = self.budget.max_tool_calls.filter(|max| calls >= *max) {
                    budget_exhausted = true;
                    outcomes[index] = Some(ToolOutcome::failure(format!(
                        "tool call budget exhausted ({max}): 本次调用未执行"
                    )));
                } else {
                    calls += 1;
                    executed[index] = true;
                    batch_indices.push(index);
                    batch_calls.push(call);
                }
            }

            // 阶段 2：一次批量执行（sidecar 可在 max_parallel 内并发）。
            if !batch_calls.is_empty() {
                let batch_outcomes =
                    executor.execute_batch(&batch_calls, self.max_parallel_tool_calls());
                for (slot, index) in batch_indices.iter().enumerate() {
                    outcomes[*index] =
                        Some(batch_outcomes.get(slot).cloned().unwrap_or_else(|| {
                            ToolOutcome::failure(
                                "batch executor returned fewer outcomes than calls",
                            )
                        }));
                }
            }

            // 阶段 3：按原顺序发 ToolResult / 写 tool 消息 / 统计重复提醒。
            let mut images = Vec::new();
            let mut image_bytes = 0;
            for index in 0..calls_in_step {
                let tool_call = &reply.tool_calls[index];
                let call = tool_call_request(tool_call);
                let outcome = outcomes[index]
                    .take()
                    .unwrap_or_else(|| ToolOutcome::failure("tool outcome missing for this call"));
                on_event(&AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    outcome: outcome.clone(),
                });
                messages.push(ChatMessage::Tool {
                    call_id: call.call_id.clone(),
                    content: tool_content_for_model(&outcome),
                });
                if !outcome.is_error {
                    for image in executor.tool_images(&outcome) {
                        let size = image.bytes.as_ref().map_or(0, |bytes| bytes.len());
                        if size > 0 && image_bytes + size <= 24 * 1024 * 1024 {
                            image_bytes += size;
                            images.push(image);
                        }
                    }
                }

                // 重复工具调用软提醒：只统计**真正执行**的调用；命中阈值先排队，
                // 本步工具都执行完后统一注入 System 消息（不打断配对协议）。
                if executed[index] && self.repeat_reminders.enabled {
                    let key = (call.tool.clone(), canonical_arguments(&call.arguments));
                    let count = repeat_counts.entry(key).or_insert(0);
                    *count += 1;
                    if self.repeat_reminders.thresholds.contains(count) {
                        pending_reminders.push(RepeatToolReminder::new(
                            &call.tool,
                            &call.arguments,
                            *count,
                        ));
                    }
                }
            }
            if !images.is_empty() {
                let mut remaining = 24 * 1024 * 1024 - image_bytes;
                for message in messages.iter_mut().rev() {
                    if let ChatMessage::ToolImages(previous) = message {
                        previous.retain(|image| {
                            let size = image.bytes.as_ref().map_or(0, |bytes| bytes.len());
                            if size > remaining {
                                return false;
                            }
                            remaining -= size;
                            true
                        });
                    }
                }
                messages.retain(|message| !matches!(message, ChatMessage::ToolImages(images) if images.is_empty()));
                messages.push(ChatMessage::ToolImages(images));
            }
            // 本步结束：把命中的软提醒注入历史（system 消息，软提醒不失败）。
            if !pending_reminders.is_empty() {
                for reminder in pending_reminders.drain(..) {
                    messages.push(ChatMessage::System(reminder.text.clone()));
                    self.reminder_log.push(reminder);
                }
            }

            // 取消检查点 4：批执行后。本批已发出的调用允许完成（结果已保留），
            // 但不再发起下一 step，直接 TurnCancelled 收尾。
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }
            step += 1;
            if budget_exhausted {
                // 不再给模型"继续发工具调用"的 step：预算已用尽，直接收尾。
                break;
            }
        }

        on_event(&AgentEvent::TurnEnded { text });
    }
}

/// 取消 token 是否已置位。无句柄（Mock/测试 provider）= 永不取消。
fn cancel_requested(cancel: Option<&Arc<AtomicBool>>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Relaxed))
}

/// `ModelToolCall` → 事件用请求（id 是配对依据，必须原样保留）。
fn tool_call_request(tool_call: &ModelToolCall) -> ToolCallRequest {
    ToolCallRequest {
        call_id: tool_call.id.clone(),
        tool: tool_call.name.clone(),
        arguments: tool_call.arguments.clone(),
    }
}

/// 参数规范化：`serde_json::Value` 的数字/对象序列化是确定性的（对象键有序），
/// 用来判断“同一工具 + 同一参数”。
fn canonical_arguments(arguments: &Value) -> String {
    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string())
}

/// 取消后本步剩余的调用：只发 `ToolCall` + `ToolCancelled`，一个都不执行。
fn cancel_calls(calls: &[ModelToolCall], on_event: &mut dyn FnMut(&AgentEvent)) {
    for tool_call in calls {
        on_event(&AgentEvent::ToolCall(tool_call_request(tool_call)));
        on_event(&AgentEvent::ToolCancelled {
            call_id: tool_call.id.clone(),
            reason: CANCEL_REASON.to_owned(),
        });
    }
}

/// 工具结果喂给模型时的文本投影：优先 text block，其次 `value` 的 JSON。
///
/// `role: tool` 的 content 在 OpenAI 协议里是字符串；结构化值已经通过 UI 的 `value`
/// 承载，模型侧只需要可读文本。
fn tool_content_for_model(outcome: &ToolOutcome) -> String {
    let text = if let Some(blocks) = outcome.content.as_array() {
        let parts: Vec<String> = blocks
            .iter()
            .map(|block| {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text.to_owned()
                } else if let Some(value) = block.get("value") {
                    value.to_string()
                } else {
                    String::new()
                }
            })
            .filter(|part| !part.is_empty())
            .collect();
        if parts.is_empty() {
            outcome.content.to_string()
        } else {
            parts.join("\n")
        }
    } else {
        outcome.content.to_string()
    };
    if outcome.is_error {
        format!("ERROR: {text}")
    } else {
        text
    }
}

/// M1 的替代模型：按关键词决定"回文本"还是"调工具"。
///
/// 它只依赖 `CompletionRequest`（不看历史、不保存状态），所以同一请求必然得到同一输出，
/// 这让 Agent Loop 的测试是确定性的。M2 会被真实 provider 替换。
#[derive(Debug, Default)]
pub struct MockProvider;

impl ModelProvider for MockProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        // 已经有工具结果：生成第二步回答（模拟"看结果再说话"）。
        if let Some(content) = request.last_tool_content() {
            return Ok(ModelReply::text_only(summarize(content)));
        }

        let user_text = request.last_user_text().to_owned();
        let lower = user_text.to_lowercase();
        if (user_text.contains("原生") || lower.contains("native"))
            && request.tool_specs.iter().any(|spec| spec.name == "probe_native")
        {
            return Ok(tool_reply(request, "probe_native", &user_text));
        }

        // 会话树：自检用"子会话/spawn_child"触发派发工具（参数是 prompt 而不是 text）。
        if (user_text.contains("子会话") || lower.contains("spawn_child"))
            && request
                .tool_specs
                .iter()
                .any(|spec| spec.name == "spawn_child")
        {
            return Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: format!("mock-call-spawn-{}", request.messages.len()),
                    name: String::from("spawn_child"),
                    arguments: json!({ "prompt": user_text }),
                }],
            });
        }

        if user_text.contains("工具") || lower.contains("tool") {
            if let Some(spec) = request
                .tool_specs
                .iter()
                .find(|spec| spec.name != "probe_native")
            {
                return Ok(tool_reply(request, &spec.name, &user_text));
            }
        }

        Ok(ModelReply::text_only(format!(
            "收到：{user_text}。当前是 mock agent，尚未接入 LLM。"
        )))
    }

    /// 把回复切成 8 字符的 delta 依次回调：让流式路径在 mock 下也可确定性地被测。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let reply = self.complete(request)?;
        let chars: Vec<char> = reply.text.chars().collect();
        for chunk in chars.chunks(8) {
            let piece: String = chunk.iter().collect();
            on_delta(&piece);
        }
        Ok(reply)
    }
}

/// 构造一个工具调用。id 用消息条数做序号：同一轮内唯一、跨轮不冲突。
fn tool_reply(request: &CompletionRequest, name: &str, user_text: &str) -> ModelReply {
    ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: vec![ModelToolCall {
            id: format!("mock-call-{}-{name}", request.messages.len()),
            name: name.to_owned(),
            arguments: json!({ "text": user_text }),
        }],
    }
}

fn summarize(content: &str) -> String {
    if let Some(reason) = content.strip_prefix("ERROR: ") {
        return format!("工具执行失败：{reason}。当前是 mock agent，尚未接入 LLM。");
    }
    format!("工具返回 {content}。当前是 mock agent，尚未接入 LLM。")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct EchoExecutor {
        calls: Vec<ToolCallRequest>,
    }

    impl EchoExecutor {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }
    }

    impl ToolExecutor for EchoExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            vec![
                ToolSpec {
                    name: "probe_native".to_owned(),
                    description: "native".to_owned(),
                    parameters: json!({ "type": "object" }),
                },
                ToolSpec {
                    name: "probe_echo".to_owned(),
                    description: "sidecar".to_owned(),
                    parameters: json!({ "type": "object" }),
                },
            ]
        }

        fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
            self.calls.push(call.clone());
            ToolOutcome::success(
                json!([{ "type": "json", "value": call.arguments }]),
                json!({ "echo": call.arguments }),
            )
        }
    }

    #[test]
    fn plain_turn_produces_deltas_and_final_text() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("你好", &mut executor);

        assert!(matches!(events[0], AgentEvent::TurnStarted { turn_id: 1 }));
        assert!(matches!(events[1], AgentEvent::StepStarted { step: 1, .. }));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnEnded { .. }
        ));
        assert!(executor.calls.is_empty());

        let text: String = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta(delta) => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert!(text.contains("收到：你好"));
    }

    #[test]
    fn tool_turn_calls_executor_then_answers() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("用工具试一下", &mut executor);

        assert_eq!(executor.calls.len(), 1);
        assert_eq!(executor.calls[0].tool, "probe_echo");
        assert_eq!(executor.calls[0].arguments["text"], "用工具试一下");

        let tool_call_pos = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolCall(_)))
            .expect("tool call event");
        let tool_result_pos = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
            .expect("tool result event");
        assert!(tool_call_pos < tool_result_pos);

        // 工具之后必须再来一个 step 才能给最终回答
        let steps = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StepStarted { .. }))
            .count();
        assert_eq!(steps, 2, "调用工具后应当有第二步");

        let turn_text = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::TurnEnded { text } => Some(text.clone()),
                _ => None,
            })
            .expect("turn ended");
        assert!(turn_text.contains("工具返回"));
    }

    #[test]
    fn native_keyword_selects_native_tool() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        agent.run_turn("用原生工具", &mut executor);
        assert_eq!(executor.calls[0].tool, "probe_native");
    }

    /// 每次回复都发工具调用（可限制次数），用来验证预算语义。
    struct LoopProvider {
        /// `None` = 永远发工具调用；`Some(n)` = 前 n 次回复发工具调用，之后回纯文本。
        tool_steps: Option<usize>,
        /// 每次回复里发几个工具调用。
        per_reply: usize,
        invocations: Arc<Mutex<usize>>,
    }

    impl ModelProvider for LoopProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            let index = {
                let mut invocations = self
                    .invocations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let index = *invocations;
                *invocations += 1;
                index
            };
            let emit_tools = self.tool_steps.is_none_or(|steps| index < steps);
            if !emit_tools {
                return Ok(ModelReply::text_only("done"));
            }
            Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: (0..self.per_reply)
                    .map(|call| ModelToolCall {
                        id: format!("call-{index}-{call}"),
                        name: "probe_native".to_owned(),
                        arguments: json!({}),
                    })
                    .collect(),
            })
        }
    }

    fn loop_provider(tool_steps: Option<usize>, per_reply: usize) -> LoopProvider {
        LoopProvider {
            tool_steps,
            per_reply,
            invocations: Arc::new(Mutex::new(0)),
        }
    }

    fn tool_call_ids(events: &[AgentEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCall(call) => Some(call.call_id.clone()),
                _ => None,
            })
            .collect()
    }

    fn tool_results(events: &[AgentEvent]) -> Vec<(String, bool)> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolResult { call_id, outcome } => {
                    Some((call_id.clone(), outcome.is_error))
                }
                _ => None,
            })
            .collect()
    }

    fn step_count(events: &[AgentEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StepStarted { .. }))
            .count()
    }

    fn has_turn_failed(events: &[AgentEvent]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed(_)))
    }

    fn has_turn_cancelled(events: &[AgentEvent]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCancelled { .. }))
    }

    fn tool_cancelled_ids(events: &[AgentEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCancelled { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// 显式预算：命中后**降级**而不是整轮失败（PLAN §6.4 第 3 条）。
    #[test]
    fn tool_budget_is_enforced() {
        let mut agent = Agent::with_budget(
            loop_provider(None, 1),
            ToolBudget {
                max_tool_calls: Some(2),
                max_steps: None,
            },
        );
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("loop", &mut executor);

        assert!(
            !has_turn_failed(&events),
            "预算耗尽必须是降级，不能 TurnFailed: {events:?}"
        );
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnEnded { .. }
        ));
        assert_eq!(executor.calls.len(), 2, "预算内只执行 2 次");

        // 事件完整：第 3 次调用也要发 ToolCall + 配对的失败 ToolResult。
        let calls = tool_call_ids(&events);
        let results = tool_results(&events);
        assert_eq!(calls.len(), 3, "超预算的调用仍要发 ToolCall: {events:?}");
        assert_eq!(results.len(), 3, "每个 ToolCall 必须有配对结果");
        for (index, call_id) in calls.iter().enumerate() {
            assert_eq!(&results[index].0, call_id, "结果顺序必须与调用一致");
        }
        assert!(!results[0].1 && !results[1].1, "预算内调用应成功");
        assert!(results[2].1, "超出预算的调用必须 is_error");
    }

    /// 默认预算（未设置 env）与 DSH 一致：20 次循环工具调用全部执行，不失败。
    #[test]
    fn default_budget_does_not_cap_tool_calls() {
        let mut agent = Agent::with_budget(loop_provider(Some(20), 1), ToolBudget::default());
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("loop", &mut executor);

        assert!(!has_turn_failed(&events), "{events:?}");
        assert_eq!(executor.calls.len(), 20, "默认预算下 20 次调用必须全部执行");
        assert_eq!(tool_results(&events).len(), 20);
        assert_eq!(step_count(&events), 21, "20 个工具 step + 1 个收尾 step");
        assert_eq!(turn_text(&events), "done");
    }

    /// 用户现场场景：一个 step 里 10 个工具调用（原实现第 9 个就 TurnFailed）。
    #[test]
    fn many_tool_calls_in_one_turn_are_not_capped() {
        let mut agent = Agent::with_budget(loop_provider(Some(1), 10), ToolBudget::default());
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("从 1 数到 10", &mut executor);

        assert!(!has_turn_failed(&events), "{events:?}");
        assert_eq!(executor.calls.len(), 10, "一步里的 10 个调用都要执行");
        assert_eq!(tool_call_ids(&events).len(), 10);
        let results = tool_results(&events);
        assert_eq!(results.len(), 10);
        assert!(results.iter().all(|(_, is_error)| !is_error));
        assert_eq!(step_count(&events), 2);
        assert_eq!(turn_text(&events), "done");
    }

    /// 一步内超出预算：剩余调用逐条标记失败，仍然发事件并结束本步。
    #[test]
    fn over_budget_calls_in_one_step_are_marked_failed() {
        let mut agent = Agent::with_budget(
            loop_provider(Some(1), 5),
            ToolBudget {
                max_tool_calls: Some(2),
                max_steps: None,
            },
        );
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("parallel", &mut executor);

        assert!(!has_turn_failed(&events), "{events:?}");
        assert_eq!(executor.calls.len(), 2);
        let results = tool_results(&events);
        assert_eq!(results.len(), 5, "5 个调用都要有配对结果");
        assert_eq!(
            results.iter().filter(|(_, is_error)| *is_error).count(),
            3,
            "超出预算的 3 个调用标失败"
        );
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnEnded { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // M3 并行工具（DSH `maxParallelToolCalls = 10`）：M3-PARALLEL-001..010
    // -----------------------------------------------------------------------

    /// 只记录本批看得到的调用及其 `max_parallel` 参数。
    struct BatchRecordingExecutor {
        calls: Vec<ToolCallRequest>,
    }

    impl BatchRecordingExecutor {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }
    }

    impl ToolExecutor for BatchRecordingExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
            self.calls.push(call.clone());
            ToolOutcome::success(
                json!([{ "type": "text", "text": call.tool.clone() }]),
                Value::Null,
            )
        }
    }

    /// M3-PARALLEL-001：默认 `execute_batch` 逐个 `execute`；事件按原顺序配对
    /// （先 ToolCall×N，再 ToolResult×N）。
    #[test]
    fn m3_parallel_001_batch_default_is_sequential_and_ordered() {
        let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(3));
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("同时跑三个", &mut executor);

        // 默认 execute_batch 走 3 次 execute，顺序 = 调用顺序。
        let executed: Vec<String> = executor
            .calls
            .iter()
            .map(|call| call.call_id.clone())
            .collect();
        assert_eq!(executed, tool_call_ids(&events));
        // 事件顺序：3 个 ToolCall 都在第一个 ToolResult 之前。
        let first_result = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
            .expect("tool result");
        let call_positions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, AgentEvent::ToolCall(_)))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(call_positions.len(), 3);
        assert!(call_positions
            .iter()
            .all(|position| *position < first_result));
        // 结果顺序 = 发出顺序。
        let result_ids: Vec<String> = tool_results(&events)
            .into_iter()
            .map(|(call_id, _)| call_id)
            .collect();
        assert_eq!(result_ids, tool_call_ids(&events));
    }

    /// M3-PARALLEL-002：执行器覆盖 `execute_batch` 时可真正并发；墙钟显著小于串行。
    struct ConcurrentExecutor {
        max_active: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ToolExecutor for ConcurrentExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
            ToolOutcome::failure(format!("execute should not be used for {}", call.call_id))
        }

        fn execute_batch(
            &mut self,
            calls: &[ToolCallRequest],
            max_parallel: usize,
        ) -> Vec<ToolOutcome> {
            assert_eq!(max_parallel, 3, "max_parallel 应透传给执行器");
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut handles = Vec::new();
            for call in calls {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&self.max_active);
                let call = call.clone();
                handles.push(std::thread::spawn(move || {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    active.fetch_sub(1, Ordering::SeqCst);
                    ToolOutcome::success(
                        json!([{ "type": "json", "value": { "call": call.call_id } }]),
                        json!({ "call": call.call_id }),
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("worker"))
                .collect()
        }
    }

    #[test]
    fn m3_parallel_002_execute_batch_runs_concurrently() {
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(3));
        let mut executor = ConcurrentExecutor {
            max_active: Arc::clone(&max_active),
        };
        let started = std::time::Instant::now();
        let events = agent.run_turn("并行", &mut executor);
        let elapsed = started.elapsed();

        assert_eq!(max_active.load(Ordering::SeqCst), 3, "3 个调用应同时在跑");
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "并行墙钟应远小于串行 240ms，实际 {elapsed:?}"
        );
        assert_eq!(tool_results(&events).len(), 3);
        assert_eq!(
            tool_results(&events)
                .iter()
                .filter(|(_, is_error)| *is_error)
                .count(),
            0
        );
    }

    /// M3-PARALLEL-003：预算截断只把允许的调用放进 batch；超预算的按原位置降级。
    #[test]
    fn m3_parallel_003_budget_limits_the_batch() {
        let mut agent = Agent::with_budget(
            loop_provider(Some(1), 4),
            ToolBudget {
                max_tool_calls: Some(2),
                max_steps: None,
            },
        )
        .with_max_parallel_tool_calls(Some(4));
        let mut executor = BatchRecordingExecutor::new();
        let events = agent.run_turn("预算", &mut executor);

        assert_eq!(executor.calls.len(), 2, "batch 只收到预算内的 2 个调用");
        let call_ids = tool_call_ids(&events);
        let results = tool_results(&events);
        assert_eq!(results.len(), 4, "4 个调用都要有配对结果");
        assert_eq!(results[0].0, call_ids[0]);
        assert!(!results[0].1);
        assert_eq!(results[1].0, call_ids[1]);
        assert!(!results[1].1);
        assert_eq!(results[2].0, call_ids[2]);
        assert!(results[2].1, "超预算调用标失败");
        assert_eq!(results[3].0, call_ids[3]);
        assert!(results[3].1, "超预算调用标失败");
    }

    /// M3-PARALLEL-004：batch 内单个调用失败不影响其它调用（错误隔离、顺序保留）。
    struct PartialFailureExecutor;
    impl ToolExecutor for PartialFailureExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
            ToolOutcome::failure("unused")
        }
        fn execute_batch(
            &mut self,
            calls: &[ToolCallRequest],
            _max_parallel: usize,
        ) -> Vec<ToolOutcome> {
            calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    if index == 1 {
                        ToolOutcome::failure(format!("boom {}", call.call_id))
                    } else {
                        ToolOutcome::success(
                            json!([{ "type": "text", "text": call.call_id.clone() }]),
                            json!(call.call_id.clone()),
                        )
                    }
                })
                .collect()
        }
    }

    #[test]
    fn m3_parallel_004_batch_error_is_isolated() {
        let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(3));
        let mut executor = PartialFailureExecutor;
        let events = agent.run_turn("隔离", &mut executor);

        let results = tool_results(&events);
        assert_eq!(
            results
                .iter()
                .map(|(_, is_error)| *is_error)
                .collect::<Vec<_>>(),
            vec![false, true, false],
            "中间失败不影响两侧: {results:?}"
        );
        assert_eq!(
            results
                .iter()
                .map(|(call_id, _)| call_id.clone())
                .collect::<Vec<_>>(),
            tool_call_ids(&events)
        );
        assert!(!has_turn_failed(&events), "{events:?}");
    }

    /// M3-PARALLEL-005：`max_parallel` 取值优先级（显式覆盖 > env > 默认 10）与透传。
    #[test]
    fn m3_parallel_005_max_parallel_resolution_and_passthrough() {
        let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(2));
        assert_eq!(agent.max_parallel_tool_calls(), 2);
        let mut recorder = MaxParallelRecorder::default();
        let _ = agent.run_turn("parallel", &mut recorder);
        assert_eq!(recorder.seen, vec![2], "显式覆盖必须透传给执行器");

        let mut recorder = MaxParallelRecorder::default();
        let mut agent = Agent::with_budget(loop_provider(Some(1), 10), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(10));
        let _ = agent.run_turn("parallel", &mut recorder);
        assert_eq!(recorder.seen, vec![10]);

        if std::env::var(MAX_PARALLEL_TOOL_CALLS_ENV).is_err() {
            let agent = Agent::new(MockProvider);
            assert_eq!(
                agent.max_parallel_tool_calls(),
                DEFAULT_MAX_PARALLEL_TOOL_CALLS
            );
        }
    }

    #[derive(Default)]
    struct MaxParallelRecorder {
        seen: Vec<usize>,
    }

    impl ToolExecutor for MaxParallelRecorder {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
            ToolOutcome::failure("unused")
        }
        fn execute_batch(
            &mut self,
            calls: &[ToolCallRequest],
            max_parallel: usize,
        ) -> Vec<ToolOutcome> {
            self.seen.push(max_parallel);
            calls
                .iter()
                .map(|call| ToolOutcome::success(json!([]), json!(call.call_id.clone())))
                .collect()
        }
    }

    /// M3-PARALLEL-006：batch 的重复工具提醒统计仍然只算真正执行的调用。
    #[test]
    fn m3_parallel_006_repeat_reminder_counts_batch_calls() {
        let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
            .with_max_parallel_tool_calls(Some(3))
            .with_repeat_reminders(RepeatToolReminderConfig {
                enabled: true,
                thresholds: vec![3],
            });
        let mut executor = BatchRecordingExecutor::new();
        let _ = agent.run_turn("重复", &mut executor);
        let reminders = agent.take_repeat_reminders();
        assert_eq!(reminders.len(), 1, "{reminders:?}");
        assert_eq!(reminders[0].count, 3);
        assert_eq!(reminders[0].tool, "probe_native");
    }

    /// 命中 max_steps：优雅结束 TurnEnded，不是 TurnFailed。
    #[test]
    fn max_steps_ends_turn_without_failure() {
        let mut agent = Agent::with_budget(
            loop_provider(None, 1),
            ToolBudget {
                max_tool_calls: None,
                max_steps: Some(3),
            },
        );
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("loop", &mut executor);

        assert!(!has_turn_failed(&events), "{events:?}");
        assert_eq!(step_count(&events), 3);
        assert_eq!(executor.calls.len(), 3);
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnEnded { .. }
        ));
    }

    /// 空 / 0 / 非法 env 都表示"不限制"（与 DSH 默认一致）；不依赖全局 env。
    #[test]
    fn zero_or_unset_env_means_unlimited() {
        assert_eq!(
            ToolBudget::default(),
            ToolBudget {
                max_tool_calls: None,
                max_steps: None,
            }
        );
        assert_eq!(parse_positive_limit(""), None);
        assert_eq!(parse_positive_limit("   "), None);
        assert_eq!(parse_positive_limit("0"), None);
        assert_eq!(parse_positive_limit("abc"), None);
        assert_eq!(parse_positive_limit("-1"), None);
        assert_eq!(parse_positive_limit("12"), Some(12));
        assert_eq!(parse_positive_limit(" 7 "), Some(7));

        // Agent::new 的默认预算就是环境读取结果（两者同一时刻读，无写入竞争）。
        let agent = Agent::new(MockProvider);
        assert_eq!(agent.budget(), ToolBudget::from_env());
    }

    #[test]
    fn turn_ids_increment_across_turns() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        let first = agent.run_turn("a", &mut executor);
        let second = agent.run_turn("b", &mut executor);

        assert!(matches!(first[0], AgentEvent::TurnStarted { turn_id: 1 }));
        assert!(matches!(second[0], AgentEvent::TurnStarted { turn_id: 2 }));
    }

    #[test]
    fn provider_errors_become_turn_failed() {
        struct FailingProvider;
        impl ModelProvider for FailingProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                Err("upstream 503".to_owned())
            }
        }

        let mut agent = Agent::new(FailingProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("x", &mut executor);
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnFailed(message) if message.contains("503")
        ));
    }

    /// 每次回复都发工具调用、且暴露取消 token 的 provider（取消语义测试专用）。
    struct CancelAwareProvider {
        token: Arc<AtomicBool>,
        per_reply: usize,
        /// 返回 reply 之前先置位取消：模拟"模型流式回复期间按了停止"。
        cancel_before_reply: bool,
        invocations: Arc<Mutex<usize>>,
    }

    impl CancelAwareProvider {
        fn new(token: Arc<AtomicBool>, per_reply: usize) -> Self {
            Self {
                token,
                per_reply,
                cancel_before_reply: false,
                invocations: Arc::new(Mutex::new(0)),
            }
        }

        fn cancel_before_reply(mut self) -> Self {
            self.cancel_before_reply = true;
            self
        }
    }

    impl ModelProvider for CancelAwareProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            let index = {
                let mut invocations = self
                    .invocations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let index = *invocations;
                *invocations += 1;
                index
            };
            if self.cancel_before_reply {
                self.token.store(true, Ordering::Relaxed);
            }
            Ok(ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: (0..self.per_reply)
                    .map(|call| ModelToolCall {
                        id: format!("cancel-call-{index}-{call}"),
                        name: "probe_native".to_owned(),
                        arguments: json!({}),
                    })
                    .collect(),
            })
        }

        fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
            Some(self.token.clone())
        }
    }

    /// 执行第一个工具时置位取消：覆盖"执行后取消 → 已执行结果保留、剩余调用配平"。
    struct CancellingExecutor {
        token: Arc<AtomicBool>,
        calls: Vec<ToolCallRequest>,
    }

    impl CancellingExecutor {
        fn new(token: Arc<AtomicBool>) -> Self {
            Self {
                token,
                calls: Vec::new(),
            }
        }
    }

    impl ToolExecutor for CancellingExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
            self.calls.push(call.clone());
            if self.calls.len() == 1 {
                // 模拟"用户在这个工具执行完之后按了停止"。
                self.token.store(true, Ordering::Relaxed);
            }
            ToolOutcome::success(
                json!([{ "type": "text", "text": "ok" }]),
                json!({ "ok": true }),
            )
        }
    }

    /// M3-PARALLEL-007：turn 中途取消（发生在 batch 执行内）——已发出的批允许完成、
    /// 结果保留；不再发起下一个 step，以 TurnCancelled 收尾（不是 TurnFailed）；
    /// batch 内不再有"未执行调用"，因此不产生 ToolCancelled。
    #[test]
    fn cancel_during_batch_keeps_results_then_stops() {
        let token = Arc::new(AtomicBool::new(false));
        let provider = CancelAwareProvider::new(token.clone(), 3);
        let mut agent = Agent::new(provider).with_max_parallel_tool_calls(Some(3));
        let mut executor = CancellingExecutor::new(token.clone());
        let events = agent.run_turn("loop", &mut executor);

        assert!(
            !has_turn_failed(&events),
            "取消不是失败，不能 TurnFailed: {events:?}"
        );
        assert!(
            has_turn_cancelled(&events),
            "必须发 TurnCancelled: {events:?}"
        );
        assert_eq!(
            executor.calls.len(),
            3,
            "batch 内已发出的调用允许完成（M3 并行语义）"
        );
        assert_eq!(step_count(&events), 1, "取消后不应再发起模型请求");
        assert_eq!(tool_call_ids(&events).len(), 3, "{events:?}");
        assert_eq!(
            tool_results(&events).len(),
            3,
            "批内 3 个调用都要有配对结果: {events:?}"
        );
        assert!(
            tool_cancelled_ids(&events).is_empty(),
            "batch 内没有未执行调用: {events:?}"
        );
    }

    /// 第一轮开始前 token 已置位：不发 StepStarted、不请求模型，只有 TurnCancelled。
    #[test]
    fn cancel_before_first_step() {
        let token = Arc::new(AtomicBool::new(true));
        let provider = CancelAwareProvider::new(token.clone(), 1);
        let invocations = provider.invocations.clone();
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("x", &mut executor);

        assert_eq!(
            events.len(),
            2,
            "只有 TurnStarted + TurnCancelled: {events:?}"
        );
        assert!(matches!(events[0], AgentEvent::TurnStarted { turn_id: 1 }));
        assert!(matches!(
            events[1],
            AgentEvent::TurnCancelled { ref reason } if reason == "cancel"
        ));
        assert!(!has_turn_failed(&events));
        assert!(executor.calls.is_empty());
        assert_eq!(
            *invocations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            0,
            "取消后不应发起模型请求"
        );
    }

    /// 模型回复之后、执行工具之前置位取消：本步所有调用发 ToolCall + ToolCancelled，
    /// 但一个都不执行。
    #[test]
    fn cancel_after_reply_pairs_calls_without_executing() {
        let token = Arc::new(AtomicBool::new(false));
        let provider = CancelAwareProvider::new(token.clone(), 2).cancel_before_reply();
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("loop", &mut executor);

        assert!(!has_turn_failed(&events), "{events:?}");
        assert!(has_turn_cancelled(&events), "{events:?}");
        assert!(executor.calls.is_empty(), "取消后不得执行任何工具");
        let calls = tool_call_ids(&events);
        assert_eq!(calls.len(), 2, "{events:?}");
        assert_eq!(tool_cancelled_ids(&events), calls, "每个 ToolCall 都要配对");
        assert!(tool_results(&events).is_empty());
    }

    /// provider 在取消状态下返回 Err：收敛为 TurnCancelled，不是 TurnFailed（P2-10）。
    #[test]
    fn cancelled_provider_error_is_not_a_failure() {
        struct CancelledThenFailingProvider {
            token: Arc<AtomicBool>,
        }

        impl ModelProvider for CancelledThenFailingProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                self.token.store(true, Ordering::Relaxed);
                Err("cancelled".to_owned())
            }

            fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
                Some(self.token.clone())
            }
        }

        let mut agent = Agent::new(CancelledThenFailingProvider {
            token: Arc::new(AtomicBool::new(false)),
        });
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("x", &mut executor);

        assert!(!has_turn_failed(&events), "取消不能报失败: {events:?}");
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::TurnCancelled { .. }
        ));
    }

    fn text_deltas(events: &[AgentEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta(delta) => Some(delta.clone()),
                _ => None,
            })
            .collect()
    }

    fn turn_text(events: &[AgentEvent]) -> String {
        events
            .iter()
            .find_map(|event| match event {
                AgentEvent::TurnEnded { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn streaming_deltas_are_forwarded_in_order() {
        struct StreamingProvider;
        impl ModelProvider for StreamingProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                unreachable!("流式 provider 不应回退到 complete")
            }
            fn complete_streaming(
                &mut self,
                _request: &CompletionRequest,
                on_delta: &mut dyn FnMut(&str),
            ) -> Result<ModelReply, String> {
                on_delta("你");
                on_delta("好");
                on_delta("呀");
                Ok(ModelReply::text_only("你好呀"))
            }
        }

        let mut agent = Agent::new(StreamingProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("hi", &mut executor);

        assert_eq!(text_deltas(&events), vec!["你", "好", "呀"]);
        assert_eq!(turn_text(&events), "你好呀");
    }

    #[test]
    fn provider_ignoring_on_delta_still_gets_one_delta() {
        struct SilentProvider;
        impl ModelProvider for SilentProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                Ok(ModelReply::text_only("整段返回"))
            }
        }

        let mut agent = Agent::new(SilentProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("hi", &mut executor);

        assert_eq!(text_deltas(&events), vec!["整段返回"]);
        assert_eq!(turn_text(&events), "整段返回");
    }

    #[test]
    fn mock_provider_streams_multiple_deltas() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("你好", &mut executor);

        let deltas = text_deltas(&events);
        assert!(deltas.len() > 1, "mock 也应切成多个 delta: {deltas:?}");
        assert_eq!(deltas.concat(), turn_text(&events));
    }

    #[test]
    fn streaming_tool_turn_keeps_call_before_result() {
        let mut agent = Agent::new(MockProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("用工具试一下", &mut executor);

        let call_pos = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolCall(_)))
            .expect("tool call");
        let result_pos = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
            .expect("tool result");
        assert!(call_pos < result_pos);
        // 工具调用那一步没有文本，所以 delta 只能来自调用之后的第二步。
        let first_delta_pos = events
            .iter()
            .position(|event| matches!(event, AgentEvent::TextDelta(_)))
            .expect("streamed delta");
        assert!(first_delta_pos > result_pos);
    }

    /// 记录每次请求的消息序列，用于断言协议形状。
    struct RecordingProvider {
        script: Vec<ModelReply>,
        requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    }

    impl ModelProvider for RecordingProvider {
        fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.messages.clone());
            if self.script.is_empty() {
                Ok(ModelReply::text_only("done"))
            } else {
                Ok(self.script.remove(0))
            }
        }
    }

    fn recording(
        script: Vec<ModelReply>,
    ) -> (RecordingProvider, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            RecordingProvider {
                script,
                requests: requests.clone(),
            },
            requests,
        )
    }

    fn recorded(requests: &Arc<Mutex<Vec<Vec<ChatMessage>>>>) -> Vec<Vec<ChatMessage>> {
        requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[test]
    fn previous_turn_tool_results_do_not_look_like_current_step() {
        let request = CompletionRequest {
            messages: vec![
                ChatMessage::user("跑一下工具"),
                ChatMessage::Assistant {
                    reasoning: String::new(),
                    text: String::new(),
                    tool_calls: vec![ModelToolCall {
                        id: "c1".to_owned(),
                        name: "probe_echo".to_owned(),
                        arguments: json!({}),
                    }],
                },
                ChatMessage::Tool {
                    call_id: "c1".to_owned(),
                    content: "旧结果".to_owned(),
                },
                ChatMessage::Assistant {
                    reasoning: String::new(),
                    text: "做完了".to_owned(),
                    tool_calls: Vec::new(),
                },
                ChatMessage::user("新问题"),
            ],
            tool_specs: Vec::new(),
        };
        assert_eq!(request.last_user_text(), "新问题");
        assert_eq!(
            request.last_tool_content(),
            None,
            "上一轮的工具结果不能误判"
        );
    }

    #[test]
    fn history_is_sent_before_the_new_user_message() {
        let history = vec![
            ChatMessage::user("我叫小张"),
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: "记住了".to_owned(),
                tool_calls: Vec::new(),
            },
        ];
        let (provider, requests) = recording(Vec::new());
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        agent.run_turn_with_history(&history, "我叫什么名字", &mut executor);

        let requests = recorded(&requests);
        let sent = &requests[0];
        assert_eq!(sent[0], history[0]);
        assert_eq!(sent[1], history[1]);
        assert_eq!(sent[2], ChatMessage::user("我叫什么名字"));
        assert_eq!(sent.len(), 3);
    }

    #[test]
    fn assistant_tool_calls_and_tool_role_are_paired_by_id() {
        let script = vec![ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: "call-abc".to_owned(),
                name: "probe_native".to_owned(),
                arguments: json!({ "text": "hi" }),
            }],
        }];
        let (provider, requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("用原生工具", &mut executor);
        assert!(!turn_text(&events).is_empty(), "第二步应有回答");

        let requests = recorded(&requests);
        // 第二次请求 = 用户 + assistant(tool_calls) + tool(结果)
        let second = &requests[1];
        assert_eq!(second.len(), 3);
        assert!(matches!(
            &second[1],
            ChatMessage::Assistant { text, tool_calls, .. }
                if text.is_empty() && tool_calls.len() == 1 && tool_calls[0].id == "call-abc"
        ));
        assert!(matches!(
            &second[2],
            ChatMessage::Tool { call_id, content }
                if call_id == "call-abc" && content.contains("\"text\"")
        ));
    }

    #[test]
    fn multiple_tool_calls_in_one_reply_are_all_executed() {
        let script = vec![ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![
                ModelToolCall {
                    id: "c1".to_owned(),
                    name: "probe_native".to_owned(),
                    arguments: json!({ "text": "a" }),
                },
                ModelToolCall {
                    id: "c2".to_owned(),
                    name: "probe_echo".to_owned(),
                    arguments: json!({ "text": "b" }),
                },
            ],
        }];
        let (provider, requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        agent.run_turn("两个工具", &mut executor);

        assert_eq!(executor.calls.len(), 2);
        assert_eq!(executor.calls[0].call_id, "c1");
        assert_eq!(executor.calls[1].call_id, "c2");

        let requests = recorded(&requests);
        let second = &requests[1];
        assert_eq!(second.len(), 4, "user + assistant + 两个 tool");
        assert!(matches!(
            &second[2],
            ChatMessage::Tool { call_id, .. } if call_id == "c1"
        ));
        assert!(matches!(
            &second[3],
            ChatMessage::Tool { call_id, .. } if call_id == "c2"
        ));
    }

    #[test]
    fn tool_error_content_is_prefixed_for_the_model() {
        struct FailingExecutor;
        impl ToolExecutor for FailingExecutor {
            fn specs(&self) -> Vec<ToolSpec> {
                Vec::new()
            }
            fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
                ToolOutcome::failure("permission denied")
            }
        }

        let script = vec![ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: "c1".to_owned(),
                name: "bash".to_owned(),
                arguments: json!({}),
            }],
        }];
        let (provider, requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = FailingExecutor;
        agent.run_turn("试试", &mut executor);

        let requests = recorded(&requests);
        assert!(matches!(
            &requests[1][2],
            ChatMessage::Tool { content, .. } if content.starts_with("ERROR: ")
        ));
    }

    // ------------------------------------------------------------------
    // M3-DEFAULT-001..004：reasoning effort（共享档位类型）
    // ------------------------------------------------------------------

    #[test]
    fn m3_default_001_reasoning_effort_parses_dsh_levels() {
        assert_eq!(ReasoningEffort::parse("off"), Some(ReasoningEffort::Off));
        assert_eq!(ReasoningEffort::parse("LOW"), Some(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::parse(" high "),
            Some(ReasoningEffort::High)
        );
        assert_eq!(ReasoningEffort::parse("max"), Some(ReasoningEffort::Max));
        assert_eq!(ReasoningEffort::parse("medium"), None);
        assert_eq!(ReasoningEffort::parse(""), None);
    }

    #[test]
    fn m3_default_002_reasoning_effort_maps_off_to_no_wire_value() {
        assert_eq!(ReasoningEffort::Off.wire_value(), None);
        assert_eq!(ReasoningEffort::Low.wire_value(), Some("low"));
        assert_eq!(ReasoningEffort::High.wire_value(), Some("high"));
        assert_eq!(ReasoningEffort::Max.wire_value(), Some("max"));
        for effort in [
            ReasoningEffort::Off,
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ] {
            assert_eq!(ReasoningEffort::parse(effort.as_str()), Some(effort));
        }
    }

    #[test]
    fn m3_default_003_invalid_reasoning_effort_is_ignored_not_fatal() {
        assert_eq!(ReasoningEffort::resolve(None), None);
        assert_eq!(ReasoningEffort::resolve(Some("  ")), None);
        assert_eq!(ReasoningEffort::resolve(Some("turbo")), None);
        assert_eq!(
            ReasoningEffort::resolve(Some("max")),
            Some(ReasoningEffort::Max)
        );
    }

    #[test]
    fn m3_default_004_reasoning_effort_env_name_is_stable() {
        assert_eq!(ReasoningEffort::ENV, "DSHR_REASONING_EFFORT");
    }

    // ------------------------------------------------------------------
    // M3-DEFAULT-010..015：重复工具调用软提醒
    // ------------------------------------------------------------------

    fn repeated_calls(count: usize, arguments: Value) -> ModelReply {
        ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: (0..count)
                .map(|index| ModelToolCall {
                    id: format!("repeat-call-{index}"),
                    name: "probe_echo".to_owned(),
                    arguments: arguments.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn m3_default_010_repeat_reminder_fires_at_3_5_8() {
        let script = vec![repeated_calls(8, json!({ "text": "same" }))];
        let (provider, requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        agent.run_turn("重复调用", &mut executor);

        let reminders = agent.take_repeat_reminders();
        let counts: Vec<usize> = reminders.iter().map(|reminder| reminder.count).collect();
        assert_eq!(counts, vec![3, 5, 8], "thresholds [3,5,8]：{reminders:?}");
        assert!(reminders
            .iter()
            .all(|reminder| reminder.tool == "probe_echo"));
        assert!(reminders
            .iter()
            .all(|reminder| reminder.text.contains("重复工具调用提醒")));
        assert!(agent.repeat_reminder_log().is_empty(), "take 后清空");

        // 提醒作为 system 消息注入到了下一次请求，且不打断 tool 配对。
        let requests = recorded(&requests);
        let second = &requests[1];
        let system_reminders = second
            .iter()
            .filter(|message| {
                matches!(message, ChatMessage::System(text) if text.contains("重复工具调用提醒"))
            })
            .count();
        assert_eq!(system_reminders, 3);
        assert_eq!(
            second
                .iter()
                .filter(|message| matches!(message, ChatMessage::Tool { .. }))
                .count(),
            8
        );
    }

    #[test]
    fn m3_default_011_different_arguments_do_not_trigger() {
        let calls: Vec<ModelToolCall> = (0..8)
            .map(|index| ModelToolCall {
                id: format!("diff-call-{index}"),
                name: "probe_echo".to_owned(),
                arguments: json!({ "text": format!("value-{index}") }),
            })
            .collect();
        let script = vec![ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: calls,
        }];
        let (provider, _requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        agent.run_turn("不同参数", &mut executor);
        assert!(agent.take_repeat_reminders().is_empty(), "参数不同不得提醒");
    }

    #[test]
    fn m3_default_012_different_tools_with_same_arguments_do_not_trigger() {
        // 3 个不同工具 × 各 2 次 + 相同参数：每个 (tool, args) 计数都 < 3。
        let names = ["probe_echo", "probe_native", "probe_extra"];
        let calls: Vec<ModelToolCall> = (0..6)
            .map(|index| ModelToolCall {
                id: format!("tool-call-{index}"),
                name: names[index % names.len()].to_owned(),
                arguments: json!({ "text": "same" }),
            })
            .collect();
        let script = vec![ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: calls,
        }];
        let (provider, _requests) = recording(script);
        let mut agent = Agent::new(provider);
        let mut executor = EchoExecutor::new();
        agent.run_turn("不同工具", &mut executor);
        assert!(agent.take_repeat_reminders().is_empty(), "工具不同不得提醒");
    }

    #[test]
    fn m3_default_013_repeat_reminder_can_be_disabled() {
        let script = vec![repeated_calls(8, json!({ "text": "same" }))];
        let (provider, requests) = recording(script);
        let mut agent =
            Agent::new(provider).with_repeat_reminders(RepeatToolReminderConfig::disabled());
        let mut executor = EchoExecutor::new();
        agent.run_turn("重复调用", &mut executor);
        assert!(agent.take_repeat_reminders().is_empty());
        let requests = recorded(&requests);
        assert!(
            !requests[1]
                .iter()
                .any(|message| matches!(message, ChatMessage::System(text) if text.contains("重复工具调用提醒"))),
            "关闭后不得注入提醒"
        );
    }

    #[test]
    fn m3_default_014_repeat_reminder_custom_thresholds() {
        let script = vec![repeated_calls(4, json!({ "text": "same" }))];
        let (provider, _requests) = recording(script);
        let mut agent = Agent::new(provider)
            .with_repeat_reminders(RepeatToolReminderConfig::default().with_thresholds([2, 4]));
        let mut executor = EchoExecutor::new();
        agent.run_turn("自定义阈值", &mut executor);
        let counts: Vec<usize> = agent
            .take_repeat_reminders()
            .iter()
            .map(|reminder| reminder.count)
            .collect();
        assert_eq!(counts, vec![2, 4]);
    }

    #[test]
    fn m3_default_015_repeat_threshold_config_parsing() {
        assert_eq!(
            parse_repeat_thresholds("3,5,8"),
            Some(vec![3, 5, 8]),
            "DSH 默认阈值"
        );
        assert_eq!(
            parse_repeat_thresholds("8, 3, 5, 3"),
            Some(vec![3, 5, 8]),
            "排序去重"
        );
        assert_eq!(parse_repeat_thresholds("3,x,8"), None);
        assert_eq!(parse_repeat_thresholds("0,3"), None);
        assert_eq!(parse_repeat_thresholds("  "), None);
        assert_eq!(
            RepeatToolReminderConfig::default().thresholds,
            REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec()
        );
        assert!(RepeatToolReminderConfig::disabled().thresholds.is_empty());
    }
}
