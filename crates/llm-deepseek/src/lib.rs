//! DeepSeek Chat Completions 的 `ModelProvider` 实现（含 SSE 流式）。
//!
//! 说明：
//! - 同步 HTTP（`ureq`）；`complete_streaming` 读 SSE，每段 `delta.content` 立即回调，
//!   UI 因此可以"边收边显示"，而不是等整段响应回来。
//! - 工具调用按 OpenAI 兼容格式累积：`delta.tool_calls[].function.name/arguments`
//!   是分片到达的，必须拼完再解析。
//! - 工具轮次回传模型返回的 `reasoning_content`，并保留调用与结果的配对。
//! - API key 只从环境变量或 `~/.dsh/.env` 读取，绝不打印、不写日志。
//! - 采样参数对齐官方 DSH 0.2.2-rc.1：**默认不发送 temperature / max_tokens**，
//!   让服务的默认值生效；只有显式设置 `DEEPSEEK_TEMPERATURE` / `DEEPSEEK_MAX_TOKENS`
//!   时才写进请求体（DSH 的 `@deepseek-ai/dsh-llm-deepseek` 也是
//!   `temperature === void 0 ? {} : { temperature }`）。
//! - 模型名仍默认 `deepseek-chat`（保持既有行为）；DSH desktop 默认是
//!   `deepseek-flash`（1M 上下文），需要时用 `DEEPSEEK_MODEL=deepseek-flash` +
//!   `DSHR_CONTEXT_WINDOW=1048576` 显式选择。

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dshr_agent_core::{
    AttachmentFormat, ChatMessage, CompletionRequest, MediaAttachment, ModelDelta, ModelProvider,
    ModelReply, ModelToolCall, ReasoningEffort,
};
use serde_json::{json, Value};

const DEFAULT_BASE: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-chat";

pub const SUPPORTS_REASONING_EFFORT: bool = true;

const SYSTEM_PROMPT: &str = "你是一个桌面 Agent。回答简洁；需要外部能力时调用提供的工具。";

#[derive(Clone)]
pub struct DeepSeekProvider {
    api_key: String,
    base_url: String,
    model: String,
    /// 显式采样温度；`None` = 不发送（对齐 DSH，由服务端默认）。
    temperature: Option<f64>,
    /// 显式输出上限；`None` = 不发送（保持 provider 默认）。
    max_tokens: Option<u64>,
    /// DSH reasoning effort 档位（`off/low/high/max`）；来自 `DSHR_REASONING_EFFORT`。
    reasoning_effort: Option<ReasoningEffort>,
    timeout: Duration,
    /// UI 置 true 即请求停止生成。
    ///
    /// provider 只**读**它：清零只由 UI 在每轮开始时做（chat.rs 的 `start_turn`）。
    /// 若 provider 在请求开头清零，"工具执行期间按停止"会被下一次请求悄悄丢弃
    /// （M2 review P1-2）。
    cancel: Option<Arc<AtomicBool>>,
    uploaded_pdfs: HashMap<String, UploadedPdf>,
}

#[derive(Clone)]
struct UploadedPdf {
    id: String,
    usable_until: Instant,
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE.to_owned()),
            model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            temperature: parse_env_number("DEEPSEEK_TEMPERATURE"),
            max_tokens: parse_env_number("DEEPSEEK_MAX_TOKENS"),
            reasoning_effort: ReasoningEffort::from_env(),
            timeout: Duration::from_secs(60),
            cancel: None,
            uploaded_pdfs: HashMap::new(),
        }
    }

    /// 接入 UI 的"停止"按钮：同一个 token 可以在不持有 provider 时置位。
    pub fn with_cancel(mut self, token: Arc<AtomicBool>) -> Self {
        self.cancel = Some(token);
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 显式设置采样温度；`None` = 不发送（服务端默认，对齐 DSH）。
    pub fn with_temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    /// 显式设置输出 token 上限；`None` = 不发送（服务端默认）。
    ///
    /// 官方 DSH 的 DeepSeek 适配器默认 `maxTokens = 256_000`，但 dshr 默认模型是
    /// `deepseek-chat`（与 DSH desktop 的 `deepseek-flash` 不同），故不盲目照抄，
    /// 只在显式配置时发送。
    pub fn with_max_tokens(mut self, max_tokens: Option<u64>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// 显式设置 reasoning effort（未配置时沿用 `DSHR_REASONING_EFFORT`）。
    ///
    /// `off` 与 `None` 都不发送；DeepSeek Chat Completions 暂不支持该字段，
    pub fn with_reasoning_effort(mut self, effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// 当前 reasoning effort 档位（诊断/测试用）。
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn supports_attachments(&self) -> bool {
        matches!(
            self.model(),
            "deepseek-flash" | "deepseek-v4-flash-vision-exp"
        )
    }

    /// 优先环境变量，其次 `~/.dsh/.env`（DSH 自己的配置目录，只读）。
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .or_else(load_key_from_dsh_env)?;
        if key.trim().is_empty() {
            return None;
        }
        Some(Self::new(key))
    }

    fn request_body(&self, request: &CompletionRequest, stream: bool) -> Value {
        // 历史已经是严格协议形状（`agent-context` 负责重建），这里只做逐条映射。
        let mut messages: Vec<Value> = Vec::new();
        let has_system = request
            .messages
            .iter()
            .any(|message| matches!(message, ChatMessage::System(_)));
        if !has_system {
            messages.push(json!({ "role": "system", "content": SYSTEM_PROMPT }));
        }
        let replay_reasoning =
            !request.tool_specs.is_empty() && self.reasoning_effort != Some(ReasoningEffort::Off);
        let require_reasoning = self.reasoning_effort.is_some()
            || matches!(
                self.model(),
                "deepseek-flash" | "deepseek-v4-pro" | "deepseek-reasoner"
            );
        messages.extend(request.messages.iter().map(|message| {
            let mut wire = message_json(message, self.supports_attachments(), &self.uploaded_pdfs);
            if let ChatMessage::Assistant { reasoning, .. } = message {
                if replay_reasoning && (require_reasoning || !reasoning.is_empty()) {
                    wire["reasoning_content"] = json!(reasoning);
                }
            }
            wire
        }));

        let tools: Vec<Value> = request
            .tool_specs
            .iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.parameters,
                    }
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
        });
        // 对齐 DSH：默认不带采样参数，只有显式配置才发送。
        if let Some(temperature) = self.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(effort) = self.reasoning_effort {
            if let Some(value) = effort.wire_value() {
                body["thinking"] = json!({ "type": "enabled" });
                body["reasoning_effort"] = json!(value);
            } else {
                body["thinking"] = json!({ "type": "disabled" });
            }
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
        }
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    fn prepare_pdf_uploads(&mut self, request: &CompletionRequest) -> Result<(), String> {
        if !self.supports_attachments() {
            return Ok(());
        }
        self.uploaded_pdfs
            .retain(|_, file| file.usable_until > Instant::now());
        for message in &request.messages {
            let ChatMessage::UserWithAttachments { attachments, .. } = message else {
                continue;
            };
            for attachment in attachments {
                if attachment.format != AttachmentFormat::Pdf
                    || attachment.bytes.is_none()
                    || self.uploaded_pdfs.contains_key(&attachment.reference)
                {
                    continue;
                }
                if is_cancelled(self.cancel.as_deref()) {
                    return Err("cancelled".to_owned());
                }
                let file = self.upload_pdf(attachment)?;
                self.uploaded_pdfs
                    .insert(attachment.reference.clone(), file);
            }
        }
        Ok(())
    }

    fn upload_pdf(&self, attachment: &MediaAttachment) -> Result<UploadedPdf, String> {
        use ureq::unversioned::multipart::{Form, Part};
        let bytes = attachment.bytes.as_deref().ok_or("PDF bytes are missing")?;
        let part = Part::bytes(bytes)
            .file_name("attachment.pdf")
            .mime_str("application/pdf")
            .map_err(|err| err.to_string())?;
        let form = Form::new()
            .text("purpose", "rich_layout_recognition")
            .text("expires_after[anchor]", "created_at")
            .text("expires_after[seconds]", "3600")
            .part("file", part);
        let started = Instant::now();
        let url = format!("{}/files", self.base_url.trim_end_matches('/'));
        let response: Value = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .config()
            .timeout_global(Some(Duration::from_secs(245)))
            .build()
            .send(form)
            .map_err(|err| format!("DeepSeek PDF 上传失败（{}）：{err}", attachment.name))?
            .body_mut()
            .read_json()
            .map_err(|err| format!("DeepSeek PDF 上传响应无法读取：{err}"))?;
        let id = response["id"]
            .as_str()
            .filter(|id| id.starts_with("file-api-") && id.len() > "file-api-".len())
            .ok_or("DeepSeek PDF 上传响应缺少文件 ID")?;
        Ok(UploadedPdf {
            id: id.to_owned(),
            usable_until: started + Duration::from_secs(55 * 60),
        })
    }

    fn post(&self, body: &Value) -> Result<ureq::Body, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .config()
            .timeout_global(Some(self.timeout))
            .build()
            .send_json(body)
            .map_err(|err| format!("deepseek request failed: {err}"))?;
        Ok(response.into_body())
    }
}

/// 把内部消息映射成 OpenAI Chat Completions 的 `messages[]`。
fn message_json(
    message: &ChatMessage,
    supports_attachments: bool,
    uploaded_pdfs: &HashMap<String, UploadedPdf>,
) -> Value {
    match message {
        ChatMessage::System(text) => json!({ "role": "system", "content": text }),
        ChatMessage::User(text) => json!({ "role": "user", "content": text }),
        ChatMessage::UserWithAttachments { text, attachments } if !supports_attachments => {
            let mut content = text.clone();
            for attachment in attachments {
                content.push_str(&format!("\n附件 {}（当前模型不支持）", attachment.name));
            }
            json!({ "role": "user", "content": content })
        }
        ChatMessage::UserWithAttachments { text, attachments } => {
            media_message_json(text, attachments, uploaded_pdfs)
        }
        ChatMessage::ToolImages(images) => {
            if supports_attachments {
                media_message_json("工具返回的图片", images, uploaded_pdfs)
            } else {
                json!({ "role": "user", "content": "当前模型不支持工具返回的图片" })
            }
        }
        ChatMessage::Assistant {
            text, tool_calls, ..
        } if tool_calls.is_empty() => {
            json!({ "role": "assistant", "content": text })
        }
        ChatMessage::Assistant {
            text, tool_calls, ..
        } => {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            // OpenAI 协议里 arguments 是字符串化的 JSON。
                            "arguments": call.arguments.to_string(),
                        }
                    })
                })
                .collect();
            // 有工具调用时 content 必须是 null 或省略，空字符串也不合规。
            let content = if text.is_empty() {
                Value::Null
            } else {
                json!(text)
            };
            json!({ "role": "assistant", "content": content, "tool_calls": calls })
        }
        ChatMessage::Tool { call_id, content } => json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": content,
        }),
    }
}

fn media_message_json(
    text: &str,
    attachments: &[dshr_agent_core::MediaAttachment],
    uploaded_pdfs: &HashMap<String, UploadedPdf>,
) -> Value {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for attachment in attachments {
        content.push(json!({ "type": "text", "text": format!("附件：{}", attachment.name) }));
        let part = if attachment.format == AttachmentFormat::Pdf {
            uploaded_pdfs
                .get(&attachment.reference)
                .map(|file| json!({ "type": "file", "file_id": file.id }))
        } else {
            attachment
                .data_url()
                .map(|url| json!({ "type": "image_url", "image_url": { "url": url } }))
        };
        content.push(part.unwrap_or_else(|| json!({ "type": "text", "text": "文件未加载" })));
    }
    json!({ "role": "user", "content": content })
}

/// 读取可选数值型 env：未设置 / 空白 / 非法都返回 `None`（= 不发送该参数）。
fn parse_env_number<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn load_key_from_dsh_env() -> Option<String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    let path = std::path::Path::new(&home).join(".dsh").join(".env");
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("DEEPSEEK_API_KEY=") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// 取消 token 是否已置位。无句柄 = 永不取消。
fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Relaxed))
}

/// 读 SSE 流，直到 `[DONE]`、EOF 或取消。
///
/// 取消语义是**部分成功**：停止读取后返回已生成的部分 `ModelReply`（`Ok` 而不是
/// `Err`），由 Agent 的取消路径收尾（`TurnCancelled`）。每个 chunk 处理完之后都
/// 再检查一次 token，保证"停止"最多延后一个 chunk；预置位时连第一行都不读。
fn read_sse_stream(
    reader: impl BufRead,
    cancel: Option<&AtomicBool>,
    on_delta: &mut dyn FnMut(ModelDelta<'_>),
) -> Result<ModelReply, String> {
    let mut decoder = SseDecoder::default();
    let mut lines = reader.lines();
    while !is_cancelled(cancel) {
        let Some(line) = lines.next() else {
            break;
        };
        if is_cancelled(cancel) {
            break;
        }
        let line = line.map_err(|err| format!("deepseek stream read failed: {err}"))?;
        let previous_reasoning = decoder.reasoning.len();
        let text = decoder.feed(&line)?;
        if decoder.reasoning.len() > previous_reasoning {
            on_delta(ModelDelta::Reasoning(
                &decoder.reasoning[previous_reasoning..],
            ));
        }
        if let Some(text) = text {
            on_delta(ModelDelta::Text(&text));
        }
        if decoder.is_done() {
            break;
        }
    }
    Ok(decoder.finish())
}

impl ModelProvider for DeepSeekProvider {
    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        self.cancel.clone()
    }

    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        self.prepare_pdf_uploads(request)?;
        if is_cancelled(self.cancel.as_deref()) {
            return Err("cancelled".to_owned());
        }
        let body = self.request_body(request, false);
        let mut response = self.post(&body)?;
        let text = response
            .read_to_string()
            .map_err(|err| format!("deepseek body read failed: {err}"))?;
        parse_reply(&text)
    }

    /// SSE 流式：逐行读 `data:`，文本增量立即回调。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        self.complete_streaming_with_reasoning(request, &mut |delta| {
            if let ModelDelta::Text(text) = delta {
                on_delta(text);
            }
        })
    }

    fn complete_streaming_with_reasoning(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(ModelDelta<'_>),
    ) -> Result<ModelReply, String> {
        // 取消 token 只由 UI 在每轮开始时清零，provider 绝不清零（P1-2）：
        // 否则上一轮/工具执行期间的停止请求会被这一次请求悄悄吞掉。
        // 已置位时不再发起 HTTP 请求，直接返回"已生成的部分"（这里为空）。
        if is_cancelled(self.cancel.as_deref()) {
            return Ok(ModelReply::text_only(""));
        }
        self.prepare_pdf_uploads(request)?;
        if is_cancelled(self.cancel.as_deref()) {
            return Ok(ModelReply::text_only(""));
        }
        let body = self.request_body(request, true);
        let response = self.post(&body)?;
        read_sse_stream(
            BufReader::new(response.into_reader()),
            self.cancel.as_deref(),
            on_delta,
        )
    }
}

/// 解析 `chat/completions` 响应。单独抽出来便于无网络单测。
pub fn parse_reply(body: &str) -> Result<ModelReply, String> {
    let value: Value = serde_json::from_str(body).map_err(|err| format!("invalid json: {err}"))?;

    if let Some(error) = value.get("error") {
        return Err(error_message(error));
    }

    let message = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| "response missing choices[0].message".to_owned())?;

    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(index, call)| parse_tool_call(index, call))
                .collect()
        })
        .unwrap_or_default();

    Ok(ModelReply {
        reasoning: message["reasoning_content"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        text,
        tool_calls,
    })
}

/// 解析单个 `tool_calls[]` 元素。缺少 name 视为无效调用（跳过而非整包失败）。
fn parse_tool_call(index: usize, call: &Value) -> Option<ModelToolCall> {
    let function = call.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;
    let raw_args = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let arguments: Value =
        serde_json::from_str(raw_args).unwrap_or_else(|_| json!({ "raw": raw_args }));
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("call-{index}"), str::to_owned);
    Some(ModelToolCall {
        id,
        name: name.to_owned(),
        arguments,
    })
}

fn error_message(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    format!("deepseek error: {message}")
}

/// SSE 增量解码器：文本与工具调用分片都在这里累积。
///
/// 单独成类型是为了能在无网络单测里逐行喂 SSE 文本——流式最容易错的地方
/// 是"分片拼接"、"多工具按 index 归并"与"[DONE] 边界"，不是 HTTP 本身。
#[derive(Debug, Default)]
pub struct SseDecoder {
    reasoning: String,
    text: String,
    /// 按 `tool_calls[].index` 归并：多个工具调用是交织到达的。
    tool_calls: BTreeMap<u64, ToolCallBuilder>,
    done: bool,
}

#[derive(Debug, Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl SseDecoder {
    /// 喂一行 SSE。返回本次新增的文本（没有文本增量时返回 `None`）。
    pub fn feed(&mut self, line: &str) -> Result<Option<String>, String> {
        let line = line.trim_end_matches('\r');
        let Some(payload) = line.strip_prefix("data:") else {
            // 空行、`: keep-alive` 注释、`event:` 行都不携带增量。
            return Ok(None);
        };
        let payload = payload.trim();
        if payload.is_empty() {
            return Ok(None);
        }
        if payload == "[DONE]" {
            self.done = true;
            return Ok(None);
        }

        let value: Value =
            serde_json::from_str(payload).map_err(|err| format!("invalid sse json: {err}"))?;
        if let Some(error) = value.get("error") {
            return Err(error_message(error));
        }

        let Some(delta) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
        else {
            return Ok(None);
        };

        if let Some(reasoning) = delta["reasoning_content"].as_str() {
            self.reasoning.push_str(reasoning);
        }
        let mut chunk = String::new();
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                self.text.push_str(content);
                chunk.push_str(content);
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let builder = self.tool_calls.entry(index).or_default();
                if let Some(id) = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    builder.id = id.to_owned();
                }
                let Some(function) = call.get("function") else {
                    continue;
                };
                if let Some(name) = function
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                {
                    builder.name = name.to_owned();
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    // 参数是分片到达的，必须按到达顺序拼接后再解析。
                    builder.arguments.push_str(arguments);
                }
            }
        }

        Ok(if chunk.is_empty() { None } else { Some(chunk) })
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn finish(self) -> ModelReply {
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter_map(|(index, builder)| {
                if builder.name.is_empty() {
                    return None;
                }
                let arguments: Value = serde_json::from_str(&builder.arguments)
                    .unwrap_or_else(|_| json!({ "raw": builder.arguments }));
                Some(ModelToolCall {
                    id: if builder.id.is_empty() {
                        format!("call-{index}")
                    } else {
                        builder.id
                    },
                    name: builder.name,
                    arguments,
                })
            })
            .collect();
        ModelReply {
            reasoning: self.reasoning,
            text: self.text,
            tool_calls,
        }
    }
}

#[cfg(test)]
mod tests;
