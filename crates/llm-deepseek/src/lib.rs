//! DeepSeek Chat Completions 的 `ModelProvider` 实现。
//!
//! 说明：
//! - 同步 HTTP（`ureq`），因为 GPUI 侧目前是同步调用；真正的流式要等 Agent 接口改成
//!   `Future`/`Stream`，那是 M2 的后续工作。这里先做 `stream: false`。
//! - 工具结果的第二轮请求用 `role: user` 传递（简化）。严格的 OpenAI 兼容做法要求
//!   回放 assistant 的 `tool_calls` + `role: tool` + `tool_call_id`，留作 M2 修正。
//! - API key 只从环境变量或 `~/.dsh/.env` 读取，绝不打印、不写日志。

#![forbid(unsafe_code)]

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

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .config()
            .timeout_global(Some(self.timeout))
            .build()
            .send_json(&body)
            .map_err(|err| format!("deepseek request failed: {err}"))?;

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|err| format!("deepseek body read failed: {err}"))?;
        parse_reply(&text)
    }
}

/// 解析并校验 `chat/completions` 响应。单独抽出来便于无网络单测。
pub fn parse_reply(body: &str) -> Result<ModelReply, String> {
    let value: Value = serde_json::from_str(body).map_err(|err| format!("invalid json: {err}"))?;

    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("deepseek error: {message}"));
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
}
