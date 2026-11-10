//! OpenAI Chat Completions（及任何兼容端点）的 `ModelProvider`。
//!
//! 与 DeepSeek provider 共用 OpenAI 线格式，但**独立实现**，作为第二大 provider
//! 家族验证 Agent 层不依赖某一家 API 细节：
//! - 配置：`OPENAI_API_KEY`（或 `DSHR_OPENAI_API_KEY`）、
//!   `OPENAI_BASE_URL`（默认 `https://api.openai.com/v1`）、
//!   `OPENAI_MODEL`（默认 `gpt-4o-mini`）；
//! - 同步 HTTP（`ureq`），非流式；`complete_streaming` 用默认实现，
//!   整段文本作为一个 delta 回放（UI 仍能显示，只是不分段）；
//! - 工具调用按 OpenAI `tool_calls` 解析。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dshr_agent_core::{ChatMessage, CompletionRequest, ModelProvider, ModelReply, ModelToolCall};
use serde_json::{json, Value};

const DEFAULT_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const SYSTEM_PROMPT: &str = "你是一个桌面 Agent。回答简洁；需要外部能力时调用提供的工具。";

pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    model: String,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_owned()),
            model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            timeout: Duration::from_secs(60),
            cancel: None,
        }
    }

    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .ok()
            .or_else(|| std::env::var("DSHR_OPENAI_API_KEY").ok())?;
        if key.trim().is_empty() {
            return None;
        }
        Some(Self::new(key))
    }

    pub fn with_cancel(mut self, token: Arc<AtomicBool>) -> Self {
        self.cancel = Some(token);
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn request_body(&self, request: &CompletionRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        let mut has_system = false;
        for message in &request.messages {
            if matches!(message, ChatMessage::System(_)) {
                has_system = true;
            }
            messages.push(message_json(message));
        }
        if !has_system {
            messages.insert(0, json!({ "role": "system", "content": SYSTEM_PROMPT }));
        }
        let mut body = json!({ "model": self.model, "messages": messages });
        if !request.tool_specs.is_empty() {
            body["tools"] = Value::Array(
                request
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
                    .collect(),
            );
        }
        body
    }

    fn send(&self, body: &Value) -> Result<Value, String> {
        if let Some(token) = &self.cancel {
            token.store(false, Ordering::Relaxed);
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .new_agent();
        let response = agent
            .post(&self.endpoint())
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(body)
            .map_err(|err| format!("openai request failed: {err}"))?;
        response
            .into_body()
            .read_json::<Value>()
            .map_err(|err| format!("openai body read failed: {err}"))
    }
}

impl ModelProvider for OpenAiProvider {
    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        self.cancel.clone()
    }

    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        if let Some(token) = &self.cancel {
            if token.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
        }
        let body = self.send(&self.request_body(request))?;
        parse_reply(&body)
    }
}

fn message_json(message: &ChatMessage) -> Value {
    match message {
        ChatMessage::System(text) => json!({ "role": "system", "content": text }),
        ChatMessage::User(text) => json!({ "role": "user", "content": text }),
        ChatMessage::Assistant { text, tool_calls } => {
            let mut object = json!({ "role": "assistant", "content": text });
            if !tool_calls.is_empty() {
                object["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments.to_string(),
                                }
                            })
                        })
                        .collect(),
                );
            }
            object
        }
        ChatMessage::Tool { call_id, content } => json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": content,
        }),
    }
}

fn parse_reply(body: &Value) -> Result<ModelReply, String> {
    let message = body
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| format!("openai reply missing choices[0].message: {body}"))?;
    let text = message
        .get("content")
        .and_then(|content| content.as_str())
        .unwrap_or_default()
        .to_owned();
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(|calls| calls.as_array()) {
        for call in calls {
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let arguments = function
                .get("arguments")
                .and_then(|arguments| arguments.as_str())
                .unwrap_or("{}");
            tool_calls.push(ModelToolCall {
                id: call
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                name: function
                    .get("name")
                    .and_then(|name| name.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                arguments: serde_json::from_str(arguments).unwrap_or(Value::Null),
            });
        }
    }
    Ok(ModelReply { text, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_agent_core::ToolSpec;

    fn request(messages: Vec<ChatMessage>) -> CompletionRequest {
        CompletionRequest {
            messages,
            tool_specs: Vec::new(),
        }
    }

    #[test]
    fn request_body_adds_system_prompt_and_maps_roles() {
        let provider = OpenAiProvider::new("k");
        let body = provider.request_body(&request(vec![ChatMessage::user("hi")]));
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn request_body_keeps_explicit_system_and_tool_messages() {
        let provider = OpenAiProvider::new("k");
        let body = provider.request_body(&request(vec![
            ChatMessage::System("自定义".to_owned()),
            ChatMessage::Assistant {
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: "c1".to_owned(),
                    name: "read".to_owned(),
                    arguments: json!({ "path": "a" }),
                }],
            },
            ChatMessage::Tool {
                call_id: "c1".to_owned(),
                content: "done".to_owned(),
            },
        ]));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "自定义");
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "c1");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "c1");
    }

    #[test]
    fn request_body_includes_tools() {
        let provider = OpenAiProvider::new("k");
        let mut req = request(vec![ChatMessage::user("hi")]);
        req.tool_specs = vec![ToolSpec {
            name: "read".to_owned(),
            description: "读取文件".to_owned(),
            parameters: json!({ "type": "object" }),
        }];
        let body = provider.request_body(&req);
        assert_eq!(body["tools"][0]["function"]["name"], "read");
    }

    #[test]
    fn parses_text_and_tool_calls() {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "我先读文件",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read", "arguments": "{\"path\":\"a.txt\"}" }
                    }]
                }
            }]
        });
        let reply = parse_reply(&body).expect("reply");
        assert_eq!(reply.text, "我先读文件");
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(reply.tool_calls[0].arguments["path"], "a.txt");
    }

    #[test]
    fn missing_choices_is_an_error() {
        assert!(parse_reply(&json!({ "error": { "message": "bad key" } })).is_err());
    }

    #[test]
    fn from_env_requires_key() {
        // 环境里可能恰好没有；只断言"没有 key 时是 None"。
        let previous = std::env::var("OPENAI_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("DSHR_OPENAI_API_KEY");
        assert!(OpenAiProvider::from_env().is_none());
        if let Some(previous) = previous {
            std::env::set_var("OPENAI_API_KEY", previous);
        }
    }
}
