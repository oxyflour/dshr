use super::*;

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
