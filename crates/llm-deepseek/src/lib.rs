//! DeepSeek Chat Completions 的 `ModelProvider` 实现（含 SSE 流式）。
//!
//! 说明：
//! - 同步 HTTP（`ureq`）；`complete_streaming` 读 SSE，每段 `delta.content` 立即回调，
//!   UI 因此可以"边收边显示"，而不是等整段响应回来。
//! - 工具调用按 OpenAI 兼容格式累积：`delta.tool_calls[].function.name/arguments`
//!   是分片到达的，必须拼完再解析。
//! - 工具结果的第二轮请求用 `role: user` 传递（简化）。严格的 OpenAI 兼容做法要求
//!   回放 assistant 的 `tool_calls` + `role: tool` + `tool_call_id`，留作后续修正。
//! - API key 只从环境变量或 `~/.dsh/.env` 读取，绝不打印、不写日志。

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader};
use std::time::Duration;

use dshr_agent_core::{CompletionRequest, ModelProvider, ModelReply};
use serde_json::{json, Value};

const DEFAULT_BASE: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-chat";

const SYSTEM_PROMPT: &str = "你是一个桌面 Agent。回答简洁；需要外部能力时调用提供的工具。";

pub struct DeepSeekProvider {
    api_key: String,
    base_url: String,
    model: String,
    timeout: Duration,
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE.to_owned()),
            model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn model(&self) -> &str {
        &self.model
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
        let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];
        messages.push(json!({ "role": "user", "content": request.user_text }));
        if let Some(previous) = &request.previous {
            let label = if previous.is_error {
                "工具执行失败"
            } else {
                "工具执行结果"
            };
            messages.push(json!({
                "role": "user",
                "content": format!("{label}：{}", previous.content),
            }));
        }

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
            "temperature": 0.3,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
        }
        if stream {
            body["stream"] = json!(true);
        }
        body
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

impl ModelProvider for DeepSeekProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
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
        let body = self.request_body(request, true);
        let response = self.post(&body)?;
        let reader = BufReader::new(response.into_reader());
        let mut decoder = SseDecoder::default();
        for line in reader.lines() {
            let line = line.map_err(|err| format!("deepseek stream read failed: {err}"))?;
            if let Some(chunk) = decoder.feed(&line)? {
                on_delta(&chunk);
            }
            if decoder.is_done() {
                break;
            }
        }
        Ok(decoder.finish())
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

    if let Some(call) = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .and_then(|calls| calls.first())
    {
        let name = call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| "tool call missing function.name".to_owned())?;
        let raw_args = call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let arguments: Value =
            serde_json::from_str(raw_args).unwrap_or_else(|_| json!({ "raw": raw_args }));
        return Ok(ModelReply {
            text,
            tool_call: Some((name.to_owned(), arguments)),
        });
    }

    Ok(ModelReply {
        text,
        tool_call: None,
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
/// 是"分片拼接"与"[DONE] 边界"，不是 HTTP 本身。
#[derive(Debug, Default)]
pub struct SseDecoder {
    text: String,
    tool_name: Option<String>,
    tool_arguments: String,
    done: bool,
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

        let mut chunk = String::new();
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                self.text.push_str(content);
                chunk.push_str(content);
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let Some(function) = call.get("function") else {
                    continue;
                };
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    if !name.is_empty() {
                        self.tool_name = Some(name.to_owned());
                    }
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    // 参数是分片到达的，必须按到达顺序拼接后再解析。
                    self.tool_arguments.push_str(arguments);
                }
            }
        }

        Ok(if chunk.is_empty() { None } else { Some(chunk) })
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn finish(self) -> ModelReply {
        let tool_call = self.tool_name.map(|name| {
            let arguments: Value = serde_json::from_str(&self.tool_arguments)
                .unwrap_or_else(|_| json!({ "raw": self.tool_arguments }));
            (name, arguments)
        });
        ModelReply {
            text: self.text,
            tool_call,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text_reply() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"你好"}}]}"#;
        let reply = parse_reply(body).unwrap();
        assert_eq!(reply.text, "你好");
        assert!(reply.tool_call.is_none());
    }

    #[test]
    fn parses_tool_call_reply() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function",
            "function":{"name":"probe_native","arguments":"{\"text\":\"hi\"}"}}]}}]}"#;
        let reply = parse_reply(body).unwrap();
        let (name, args) = reply.tool_call.expect("tool call");
        assert_eq!(name, "probe_native");
        assert_eq!(args["text"], "hi");
    }

    #[test]
    fn surfaces_api_errors() {
        let body = r#"{"error":{"message":"invalid api key"}}"#;
        let err = parse_reply(body).unwrap_err();
        assert!(err.contains("invalid api key"));
    }

    #[test]
    fn rejects_malformed_body() {
        assert!(parse_reply("not json").is_err());
        assert!(parse_reply("{}").is_err());
    }

    #[test]
    fn sse_text_deltas_accumulate_in_order() {
        let mut decoder = SseDecoder::default();
        let mut chunks = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"content":"你"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"好"}}]}"#,
            ": keep-alive",
            r#"data: {"choices":[{"delta":{"content":"呀"},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ] {
            if let Some(chunk) = decoder.feed(line).expect("valid sse") {
                chunks.push(chunk);
            }
        }
        assert_eq!(chunks, vec!["你", "好", "呀"]);
        assert!(decoder.is_done());
        let reply = decoder.finish();
        assert_eq!(reply.text, "你好呀");
        assert!(reply.tool_call.is_none());
    }

    #[test]
    fn sse_tool_call_fragments_are_joined() {
        let mut decoder = SseDecoder::default();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"probe_native","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]}"#,
            "data: [DONE]",
        ] {
            decoder.feed(line).expect("valid sse");
        }
        let reply = decoder.finish();
        let (name, args) = reply.tool_call.expect("tool call");
        assert_eq!(name, "probe_native");
        assert_eq!(args["text"], "hi");
    }

    #[test]
    fn sse_error_payload_is_reported() {
        let mut decoder = SseDecoder::default();
        let err = decoder
            .feed(r#"data: {"error":{"message":"rate limited"}}"#)
            .unwrap_err();
        assert!(err.contains("rate limited"));
    }

    #[test]
    fn sse_ignores_non_data_lines_and_trims_cr() {
        let mut decoder = SseDecoder::default();
        assert_eq!(decoder.feed("event: message").unwrap(), None);
        assert_eq!(
            decoder
                .feed("data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\r")
                .unwrap(),
            Some("a".to_owned())
        );
    }

    #[test]
    fn sse_malformed_data_line_is_reported() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.feed("data: not-json").is_err());
    }

    #[test]
    fn request_body_adds_stream_flag_only_when_streaming() {
        let provider = DeepSeekProvider::new("test-key");
        let request = CompletionRequest {
            user_text: "hi".to_owned(),
            tool_specs: Vec::new(),
            previous: None,
        };
        let plain = provider.request_body(&request, false);
        assert!(plain.get("stream").is_none());
        let streamed = provider.request_body(&request, true);
        assert_eq!(streamed["stream"], json!(true));
        assert!(streamed.get("tools").is_none());
        // 密钥不能出现在请求体里。
        assert!(!streamed.to_string().contains("test-key"));
    }
}
