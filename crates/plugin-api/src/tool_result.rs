//! 工具结果（spec/cpp-v1.md §7.3）。
//!
//! `ToolResult` 对齐 DSH 的 `ToolResult` 形状：
//!
//! ```json
//! {
//!   "content": [{ "type": "text", "text": "3 rules failed" }],
//!   "value": { "rules_failed": 3 },
//!   "meta": { "duration_ms": 812 },
//!   "is_error": false
//! }
//! ```
//!
//! - `content` 是**模型可见**的内容（text/json/image 三类），必须非空；
//! - `value` 是结构化返回值（展示/回放只用它）；
//! - `meta` 是工具自带的展示元数据（对齐 DSH `presentationMeta`）；
//! - `is_error` 表示**工具执行失败**（模型可见），与协议错误 `E_*` 区分。
//!
//! 本类型只做数据与序列化，不依赖 `plugin-host` / GPUI / Cordis。
//! 未引入 async：trait 冻结是独立步骤，这里先提供可被 future 版本复用的形状。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PluginError;

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
    /// 失败返回 `E_INTERNAL`（这是插件自身的返回形状错误，不是模型可修正的参数错误）。
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.content.is_empty() {
            return Err(PluginError::Invalid(
                "tool result `content` must not be empty".to_owned(),
            ));
        }
        for part in &self.content {
            match part {
                ContentPart::Text { text } => {
                    if text.is_empty() {
                        return Err(PluginError::Invalid(
                            "tool result text part must not be empty".to_owned(),
                        ));
                    }
                }
                ContentPart::Image { attachment_id } => {
                    if attachment_id.is_empty() {
                        return Err(PluginError::Invalid(
                            "tool result image part must reference a non-empty attachment id"
                                .to_owned(),
                        ));
                    }
                }
                ContentPart::Json { .. } => {}
            }
        }
        Ok(())
    }

    /// 序列化成 JSON（spec/cpp-v1.md §7.3 的线上形状）。
    pub fn to_json(&self) -> Result<Value, PluginError> {
        serde_json::to_value(self)
            .map_err(|err| PluginError::Invalid(format!("cannot serialize tool result: {err}")))
    }

    /// 从 JSON 反序列化。
    pub fn from_json(value: &Value) -> Result<Self, PluginError> {
        serde_json::from_value(value.clone())
            .map_err(|err| PluginError::Invalid(format!("invalid tool result: {err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // CPP-FIBER-121..127：结果形状、构造器、序列化、校验。
    #[test]
    fn cpp_fiber_121_text_constructor_sets_ok_flag() {
        let result = ToolResult::text("3 rules failed");
        assert!(result.is_ok());
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].as_text(), Some("3 rules failed"));
        assert_eq!(result.text_content(), "3 rules failed");
        assert!(result.validate().is_ok());
    }

    #[test]
    fn cpp_fiber_122_json_constructor_keeps_value_in_sync() {
        let payload = json!({ "findings": [] });
        let result = ToolResult::json(payload.clone());
        assert_eq!(result.value, Some(payload.clone()));
        assert_eq!(result.content[0], ContentPart::json(payload));
        assert_eq!(
            result.to_json().unwrap()["value"],
            json!({ "findings": [] })
        );
    }

    #[test]
    fn cpp_fiber_123_error_constructor_sets_is_error() {
        let result = ToolResult::error("boom").with_meta(json!({ "duration_ms": 12 }));
        assert!(!result.is_ok());
        assert!(result.is_error);
        assert_eq!(result.meta, Some(json!({ "duration_ms": 12 })));
        assert_eq!(result.to_json().unwrap()["is_error"], json!(true));
    }

    #[test]
    fn cpp_fiber_124_serialization_matches_spec_shape() {
        let result = ToolResult::new(vec![
            ContentPart::text("3 rules failed"),
            ContentPart::json(json!({ "findings": [] })),
        ])
        .with_value(json!({ "rules_failed": 3 }))
        .with_meta(json!({ "duration_ms": 812 }));

        let wire = result.to_json().unwrap();
        assert_eq!(wire["content"][0]["type"], json!("text"));
        assert_eq!(wire["content"][0]["text"], json!("3 rules failed"));
        assert_eq!(wire["content"][1]["type"], json!("json"));
        assert_eq!(wire["value"]["rules_failed"], json!(3));
        assert_eq!(wire["meta"]["duration_ms"], json!(812));
        assert_eq!(wire["is_error"], json!(false));
    }

    #[test]
    fn cpp_fiber_125_round_trip_through_json() {
        let original = ToolResult::new(vec![
            ContentPart::text("done"),
            ContentPart::image("attach-42"),
        ])
        .with_value(json!({ "n": 1 }));
        let round_tripped = ToolResult::from_json(&original.to_json().unwrap()).unwrap();
        assert_eq!(round_tripped, original);
        assert_eq!(
            round_tripped.content[1],
            ContentPart::Image {
                attachment_id: "attach-42".to_owned()
            }
        );
    }

    #[test]
    fn cpp_fiber_126_empty_content_is_rejected_by_validate_but_not_constructor() {
        let empty = ToolResult::new(Vec::new());
        assert!(empty.validate().is_err());
        assert_eq!(empty.validate().unwrap_err().code(), "E_INTERNAL");
        assert!(ToolResult::new(vec![ContentPart::text("")])
            .validate()
            .is_err());
        assert!(ToolResult::new(vec![ContentPart::image("")])
            .validate()
            .is_err());
    }

    #[test]
    fn cpp_fiber_127_unknown_fields_are_ignored_and_metadata_optional() {
        let parsed = ToolResult::from_json(&json!({
            "content": [{ "type": "text", "text": "hi" }],
            "unknown_future_field": 1
        }))
        .unwrap();
        assert_eq!(parsed.text_content(), "hi");
        assert!(!parsed.is_error);
        assert_eq!(parsed.value, None);
        assert_eq!(parsed.meta, None);
    }
}
