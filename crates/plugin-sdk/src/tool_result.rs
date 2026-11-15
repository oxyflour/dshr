//! 工具结果（spec/cpp-v1.md §7.3）——SDK **自包含**副本。
//!
//! 与 `dshr-plugin-api::ToolResult` 的线上形状逐字节一致（同一 serde 属性），但
//! 不再依赖 `dshr-plugin-api`：SDK 目录可以单独拷出仓库（`<plugin>/sdk/`）作为
//! 插件作者的唯一依赖（P1-4）。宿主与 SDK 各自持有一份定义是**有意**的：协议形状
//! 是契约，两边独立编译能防止任意一侧改名悄悄破坏线上兼容。
//!
//! ```json
//! {
//!   "content": [{ "type": "text", "text": "3 rules failed" }],
//!   "value": { "rules_failed": 3 },
//!   "meta": { "duration_ms": 812 },
//!   "is_error": false
//! }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// SDK 侧的结果形状错误（不依赖 `plugin-api::PluginError`）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ToolResultError(pub String);

impl ToolResultError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 结果内容块。`image` 只能引用附件（spec/cpp-v1.md §7.2 的 `$attachment`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Json { value: Value },
    Image { attachment_id: String },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn json(value: Value) -> Self {
        Self::Json { value }
    }

    /// 图片必须通过附件引用，避免结果里内联大块 base64。
    pub fn image(attachment_id: impl Into<String>) -> Self {
        Self::Image {
            attachment_id: attachment_id.into(),
        }
    }

    /// 是否为文本块。
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text { .. })
    }

    /// 文本块返回 `Some`，其余返回 `None`。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// 一次工具调用的结果（spec/cpp-v1.md §7.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    /// 用显式内容块构造（允许暂时为空，`validate` 负责拒绝空 `content`）。
    pub fn new(content: Vec<ContentPart>) -> Self {
        Self {
            content,
            value: None,
            meta: None,
            is_error: false,
        }
    }

    /// 纯文本结果。
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![ContentPart::text(text)])
    }

    /// 结构化 JSON 结果；`content` 与 `value` 同时携带一份，便于模型与回放各自使用。
    pub fn json(value: Value) -> Self {
        Self {
            content: vec![ContentPart::json(value.clone())],
            value: Some(value),
            meta: None,
            is_error: false,
        }
    }

    /// 失败结果（`is_error = true`）。
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::text(text)],
            value: None,
            meta: None,
            is_error: true,
        }
    }

    pub fn with_value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }

    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = Some(meta);
        self
    }

    /// 追加内容块；返回 `&mut Self` 便于链式构造。
    pub fn push(&mut self, part: ContentPart) -> &mut Self {
        self.content.push(part);
        self
    }

    pub fn is_ok(&self) -> bool {
        !self.is_error
    }

    /// 模型可见的文本：把所有 `text` 块按顺序拼接（diagnostics/日志用）。
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let ContentPart::Text { text } = part {
                out.push_str(text);
            }
        }
        out
    }

    /// 协议级校验：`content` 非空、文本块非空、图片必须引用非空附件 id。
    ///
    /// 失败是"插件返回形状错误"（宿主结算为 `E_INTERNAL`），不是模型可修正的
    /// 参数错误。
    pub fn validate(&self) -> Result<(), ToolResultError> {
        if self.content.is_empty() {
            return Err(ToolResultError::invalid(
                "tool result `content` must not be empty",
            ));
        }
        for part in &self.content {
            match part {
                ContentPart::Text { text } => {
                    if text.is_empty() {
                        return Err(ToolResultError::invalid(
                            "tool result text part must not be empty",
                        ));
                    }
                }
                ContentPart::Image { attachment_id } => {
                    if attachment_id.is_empty() {
                        return Err(ToolResultError::invalid(
                            "tool result image part must reference a non-empty attachment id",
                        ));
                    }
                }
                ContentPart::Json { .. } => {}
            }
        }
        Ok(())
    }

    /// 序列化成 JSON（spec/cpp-v1.md §7.3 的线上形状）。
    pub fn to_json(&self) -> Result<Value, ToolResultError> {
        serde_json::to_value(self)
            .map_err(|err| ToolResultError::invalid(format!("cannot serialize tool result: {err}")))
    }

    /// 从 JSON 反序列化。
    pub fn from_json(value: &Value) -> Result<Self, ToolResultError> {
        serde_json::from_value(value.clone())
            .map_err(|err| ToolResultError::invalid(format!("invalid tool result: {err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // CPP-PROC-024..028：SDK 自包含 ToolResult 的线上形状必须与
    // `dshr-plugin-api::ToolResult` 逐字节一致（宿主解析依赖这个形状）。
    #[test]
    fn cpp_proc_024_sdk_tool_result_matches_wire_shape() {
        let result = ToolResult::new(vec![
            ContentPart::text("3 rules failed"),
            ContentPart::json(json!({ "findings": [] })),
        ])
        .with_value(json!({ "rules_failed": 3 }))
        .with_meta(json!({ "duration_ms": 812 }));
        let wire = result.to_json().unwrap();
        assert_eq!(
            wire,
            json!({
                "content": [
                    { "type": "text", "text": "3 rules failed" },
                    { "type": "json", "value": { "findings": [] } }
                ],
                "value": { "rules_failed": 3 },
                "meta": { "duration_ms": 812 },
                "is_error": false
            })
        );
    }

    #[test]
    fn cpp_proc_025_sdk_tool_result_constructors_and_round_trip() {
        assert!(ToolResult::text("ok").is_ok());
        assert_eq!(ToolResult::text("ok").text_content(), "ok");
        assert!(ToolResult::error("boom").is_error);
        let original = ToolResult::json(json!({ "n": 1 })).with_meta(json!({ "m": 2 }));
        let round_tripped = ToolResult::from_json(&original.to_json().unwrap()).unwrap();
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn cpp_proc_026_sdk_tool_result_validate_is_self_contained() {
        assert!(ToolResult::new(Vec::new()).validate().is_err());
        assert!(ToolResult::new(vec![ContentPart::text("")])
            .validate()
            .is_err());
        assert!(ToolResult::new(vec![ContentPart::image("")])
            .validate()
            .is_err());
        assert!(ToolResult::new(vec![ContentPart::image("attach-1")])
            .validate()
            .is_ok());
        assert_eq!(
            ToolResult::new(Vec::new())
                .validate()
                .unwrap_err()
                .to_string(),
            "tool result `content` must not be empty"
        );
    }

    #[test]
    fn cpp_proc_027_sdk_tool_result_ignores_unknown_fields() {
        let parsed = ToolResult::from_json(&json!({
            "content": [{ "type": "text", "text": "hi" }],
            "unknown_future_field": 1
        }))
        .unwrap();
        assert_eq!(parsed.text_content(), "hi");
        assert!(!parsed.is_error);
        assert_eq!(parsed.value, None);
    }
}
