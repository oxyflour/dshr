//! `cpp-process/v1` 协议类型（spec/cpp-v1.md §12）。
//!
//! 传输是 **NDJSON over stdio**：一行一条 JSON envelope，帧上限 1 MiB
//! （[`MAX_FRAME_BYTES`]）。本模块只定义形状与校验，不做 I/O——I/O 在
//! [`crate::session`]（子进程侧）与 `dshr-plugin-host::external`（宿主侧）。
//!
//! envelope 与 [`docs/spec/sidecar-rpc.md`](../../../docs/spec/sidecar-rpc.md) §4 同构：
//! `version / kind / request_id / generation / trace_id / ts / payload / error`。
//! 错误统一走 `error` 字段（`E_*` 闭集，见 [`error_code`]）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 唯一的协议版本；与宿主 `process.hello` 的 `protocol` 字段比对。
pub const PROTOCOL_VERSION: &str = "cpp-process/v1";
/// 单帧上限：1 MiB（spec §12.1）。
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// `process.ready` 里内联 JSON Schema 的上限：8 KiB（spec §12.3）。
pub const MAX_SCHEMA_BYTES: usize = 8 * 1024;
/// 工具描述上限：1 KiB（spec §7.1）。
pub const MAX_DESCRIPTION_BYTES: usize = 1024;
/// 未声明 `timeout_ms` 时的默认值：120 s（spec §7.1）。
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;
/// 工具超时上限：600 s（spec §3.5）。
pub const MAX_TOOL_TIMEOUT_MS: u64 = 600_000;
/// 未声明 `process.shutdown.timeout_ms` 时的默认值：5 s。
pub const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

/// 协议 kind 常量（spec/cpp-v1.md §12.2 的表格，顺序一致）。
pub mod kind {
    pub const PROCESS_HELLO: &str = "process.hello";
    pub const PROCESS_READY: &str = "process.ready";
    pub const PROCESS_PING: &str = "process.ping";
    pub const PROCESS_PONG: &str = "process.pong";
    pub const TOOL_INVOKE: &str = "tool.invoke";
    pub const TOOL_CHUNK: &str = "tool.chunk";
    pub const TOOL_RESULT: &str = "tool.result";
    pub const TOOL_CANCEL: &str = "tool.cancel";
    pub const UI_RENDER: &str = "ui.render";
    pub const UI_RESULT: &str = "ui.result";
    pub const UI_ACTION: &str = "ui.action";
    pub const PROCESS_SHUTDOWN: &str = "process.shutdown";
    pub const PROCESS_BYE: &str = "process.bye";

    /// 全部 kind（协议自检 / conformance 用）。
    pub const ALL: [&str; 13] = [
        PROCESS_HELLO,
        PROCESS_READY,
        PROCESS_PING,
        PROCESS_PONG,
        TOOL_INVOKE,
        TOOL_CHUNK,
        TOOL_RESULT,
        TOOL_CANCEL,
        UI_RENDER,
        UI_RESULT,
        UI_ACTION,
        PROCESS_SHUTDOWN,
        PROCESS_BYE,
    ];

    /// 该 kind 是否属于协议闭集。
    pub fn is_known(kind: &str) -> bool {
        ALL.contains(&kind)
    }
}

/// 错误码闭集（spec/cpp-v1.md §12.2；`E_TOO_LARGE` 见 §12.4 输出配额）。
pub mod error_code {
    pub const E_PROTOCOL: &str = "E_PROTOCOL";
    pub const E_VERSION: &str = "E_VERSION";
    pub const E_SCHEMA: &str = "E_SCHEMA";
    pub const E_TIMEOUT: &str = "E_TIMEOUT";
    pub const E_CANCELLED: &str = "E_CANCELLED";
    pub const E_QUOTA: &str = "E_QUOTA";
    pub const E_INTERNAL: &str = "E_INTERNAL";
    /// 输出/帧超限（spec §12.4：超出 1 MB 上限时用这个码结算）。
    pub const E_TOO_LARGE: &str = "E_TOO_LARGE";

    pub const ALL: [&str; 8] = [
        E_PROTOCOL,
        E_VERSION,
        E_SCHEMA,
        E_TIMEOUT,
        E_CANCELLED,
        E_QUOTA,
        E_INTERNAL,
        E_TOO_LARGE,
    ];

    pub fn is_known(code: &str) -> bool {
        ALL.contains(&code)
    }
}

/// 协议错误（帧/版本/序列化），`code()` 映射到 `E_PROTOCOL` / `E_VERSION`。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("invalid protocol frame: {0}")]
    InvalidFrame(String),
    #[error("unsupported protocol version `{actual}`, expected `{expected}`")]
    UnsupportedVersion { actual: String, expected: String },
    #[error("frame of {size} bytes exceeds limit {limit}")]
    FrameTooLarge { size: usize, limit: usize },
    #[error("cannot encode protocol frame: {0}")]
    Encode(String),
}

impl ProtocolError {
    /// 稳定的错误码（写进 envelope `error.code`）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion { .. } => error_code::E_VERSION,
            _ => error_code::E_PROTOCOL,
        }
    }
}

/// envelope 的 `error` 字段；`message` 不得含凭据/绝对路径/堆栈（spec §12.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeError {
    pub code: String,
    pub message: String,
}

impl EnvelopeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// 一条 `cpp-process/v1` 消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// 固定 `cpp-process/v1`；缺省时按版本错误处理。
    #[serde(default)]
    pub version: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EnvelopeError>,
}

impl Envelope {
    /// 构造一条当前协议版本的 envelope（payload 为 `null`）。
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION.to_owned(),
            kind: kind.into(),
            request_id: None,
            generation: None,
            trace_id: None,
            ts: Some(now_ms()),
            payload: Value::Null,
            error: None,
        }
    }

    /// 用可序列化 payload 构造。
    pub fn with_payload<T: Serialize>(
        kind: impl Into<String>,
        payload: T,
    ) -> Result<Self, ProtocolError> {
        let mut envelope = Self::new(kind);
        envelope.payload = serde_json::to_value(payload)
            .map_err(|err| ProtocolError::Encode(format!("serialize payload: {err}")))?;
        Ok(envelope)
    }

    /// 构造一条错误 envelope。
    pub fn with_error(
        kind: impl Into<String>,
        request_id: Option<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut envelope = Self::new(kind);
        envelope.request_id = request_id;
        envelope.error = Some(EnvelopeError::new(code, message));
        envelope
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    pub fn with_generation(mut self, generation: u64) -> Self {
        self.generation = Some(generation);
        self
    }

    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn error_code(&self) -> Option<&str> {
        self.error.as_ref().map(|error| error.code.as_str())
    }

    /// 序列化后的 JSON 字节数（frame 上限检查用）。
    pub fn encoded_len(&self) -> Result<usize, ProtocolError> {
        serde_json::to_string(self)
            .map(|line| line.len())
            .map_err(|err| ProtocolError::Encode(format!("serialize envelope: {err}")))
    }

    /// 序列化为一行 NDJSON（含行尾 `\n`）；超过 [`MAX_FRAME_BYTES`] 即拒绝。
    pub fn encode_line(&self) -> Result<String, ProtocolError> {
        let line = serde_json::to_string(self)
            .map_err(|err| ProtocolError::Encode(format!("serialize envelope: {err}")))?;
        if line.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                size: line.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let mut line = line;
        line.push('\n');
        Ok(line)
    }

    /// 解析一行 NDJSON，并校验协议版本。
    pub fn parse(line: &str) -> Result<Self, ProtocolError> {
        let line = line.trim();
        if line.is_empty() {
            return Err(ProtocolError::InvalidFrame("empty frame".to_owned()));
        }
        if line.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                size: line.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let envelope: Self = serde_json::from_str(line)
            .map_err(|err| ProtocolError::InvalidFrame(err.to_string()))?;
        if envelope.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                actual: envelope.version,
                expected: PROTOCOL_VERSION.to_owned(),
            });
        }
        Ok(envelope)
    }
}

/// 当前 Unix 毫秒时间戳（`ts` 字段）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 声明（process.ready，spec §12.3）
// ---------------------------------------------------------------------------

/// 一个工具声明；宿主转发给 `ToolRegistry`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default = "default_tool_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub cancelable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<String>,
}

fn default_tool_timeout() -> u64 {
    DEFAULT_TOOL_TIMEOUT_MS
}

fn default_true() -> bool {
    true
}

impl ToolDecl {
    /// 最小声明：名字 + 描述；input_schema 默认 `{"type":"object"}`。
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
            timeout_ms: DEFAULT_TOOL_TIMEOUT_MS,
            cancelable: true,
            presentation: None,
        }
    }

    pub fn with_input_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn with_cancelable(mut self, cancelable: bool) -> Self {
        self.cancelable = cancelable;
        self
    }

    pub fn with_presentation(mut self, presentation: impl Into<String>) -> Self {
        self.presentation = Some(presentation.into());
        self
    }

    /// 声明校验（spec §7.1 / §12.3）；错误信息英文，供宿主日志使用。
    pub fn validate(&self) -> Result<(), String> {
        let bytes = self.name.as_bytes();
        let valid_name = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0].is_ascii_lowercase()
            && bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
        if !valid_name {
            return Err(format!(
                "tool `{}`: name must match `[a-z][a-z0-9_]{{0,63}}`",
                self.name
            ));
        }
        let description_len = self.description.chars().count();
        if description_len == 0 || self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(format!(
                "tool `{}`: description length {description_len} must be within 1..={} bytes",
                self.name, MAX_DESCRIPTION_BYTES
            ));
        }
        if !self.input_schema.is_object() {
            return Err(format!(
                "tool `{}`: input_schema must be a JSON object",
                self.name
            ));
        }
        let schema_len = serde_json::to_vec(&self.input_schema)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if schema_len > MAX_SCHEMA_BYTES {
            return Err(format!(
                "tool `{}`: input_schema is {schema_len} bytes, limit {MAX_SCHEMA_BYTES}",
                self.name
            ));
        }
        if let Some(output_schema) = &self.output_schema {
            if !output_schema.is_object() {
                return Err(format!(
                    "tool `{}`: output_schema must be a JSON object",
                    self.name
                ));
            }
            let output_len = serde_json::to_vec(output_schema)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX);
            if output_len > MAX_SCHEMA_BYTES {
                return Err(format!(
                    "tool `{}`: output_schema is {output_len} bytes, limit {MAX_SCHEMA_BYTES}",
                    self.name
                ));
            }
        }
        if !(1..=MAX_TOOL_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!(
                "tool `{}`: timeout_ms {} must be within 1..={MAX_TOOL_TIMEOUT_MS}",
                self.name, self.timeout_ms
            ));
        }
        Ok(())
    }
}

/// 一个面板声明（对齐 ADC-v1 slot 契约；渲染仍由宿主/ADC 负责）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelDecl {
    pub slot: String,
    pub id: String,
    pub label: String,
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
}

impl PanelDecl {
    pub fn new(
        slot: impl Into<String>,
        id: impl Into<String>,
        label: impl Into<String>,
        view: impl Into<String>,
    ) -> Self {
        Self {
            slot: slot.into(),
            id: id.into(),
            label: label.into(),
            view: view.into(),
            location: None,
            order: None,
        }
    }

    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn with_order(mut self, order: f64) -> Self {
        self.order = Some(order);
        self
    }

    /// 声明校验（slot/字段非空、location 白名单、order 有限）。
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("slot", &self.slot),
            ("id", &self.id),
            ("label", &self.label),
            ("view", &self.view),
        ] {
            if value.trim().is_empty() {
                return Err(format!("panel `{}`: `{field}` must not be empty", self.id));
            }
        }
        if let Some(location) = &self.location {
            if !matches!(location.as_str(), "left" | "right" | "bottom" | "center") {
                return Err(format!(
                    "panel `{}`: unknown location `{location}`",
                    self.id
                ));
            }
        }
        if let Some(order) = self.order {
            if !order.is_finite() {
                return Err(format!("panel `{}`: order must be finite", self.id));
            }
        }
        Ok(())
    }
}

/// `process.ready` 的声明集合（工具 + 面板）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyDecls {
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub panels: Vec<PanelDecl>,
}

impl ReadyDecls {
    pub fn new(tools: Vec<ToolDecl>, panels: Vec<PanelDecl>) -> Self {
        Self { tools, panels }
    }

    /// 按名字查声明。
    pub fn tool(&self, name: &str) -> Option<&ToolDecl> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// 全量校验（宿主在 `process.ready` 后调用）；工具名必须唯一。
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for tool in &self.tools {
            tool.validate()?;
            if seen.contains(&tool.name.as_str()) {
                return Err(format!("duplicate tool declaration `{}`", tool.name));
            }
            seen.push(tool.name.as_str());
        }
        for panel in &self.panels {
            panel.validate()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 各 direction 的 payload
// ---------------------------------------------------------------------------

/// `process.hello`（host → child）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloPayload {
    pub protocol: String,
    pub host_version: String,
    pub plugin_id: String,
    pub generation: u64,
    /// 原始 manifest（可选，诊断用；child 不应信任其中的路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Value>,
}

/// `process.ready`（child → host）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyPayload {
    pub protocol: String,
    pub plugin_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub panels: Vec<PanelDecl>,
}

/// `tool.invoke`（host → child）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvokePayload {
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default = "default_tool_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub cancelable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

/// `tool.cancel`（host → child）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelPayload {
    #[serde(default)]
    pub reason: String,
}

/// `process.shutdown`（host → child）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownPayload {
    #[serde(default = "default_shutdown_timeout")]
    pub timeout_ms: u64,
}

fn default_shutdown_timeout() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_MS
}

/// `ui.render`（host → child）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderPayload {
    pub view: String,
    #[serde(default)]
    pub state_seq: u64,
}

/// `ui.action`（host → child）：ADC 交互动作回传给插件。
///
/// `action` 是 ADC-v1 的动作类别（`call` / `command`），`payload` 是动作参数：
/// - `call` → `{ "method": "...", "args": {...} }`
/// - `command` → `{ "id": "..." }`
///
/// fire-and-forget：协议没有 `ui.action.result`；插件处理完由宿主再发 `ui.render`
/// 取新树。错误只能写 stderr / 影响下一次 render，不得让宿主等待。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionPayload {
    pub view: String,
    pub action: String,
    #[serde(default)]
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // CPP-PROC-001..010：协议形状、版本、帧上限、声明校验。
    #[test]
    fn cpp_proc_001_kind_and_error_codes_match_spec_table() {
        assert_eq!(kind::ALL.len(), 13);
        for name in kind::ALL {
            assert!(kind::is_known(name), "{name} 应在闭集内");
        }
        assert!(!kind::is_known("tool.unknown"));
        assert_eq!(
            error_code::ALL.len(),
            8,
            "spec §12.2 的 7 个 + §12.4 的 E_TOO_LARGE"
        );
        assert!(error_code::is_known(error_code::E_TOO_LARGE));
    }

    #[test]
    fn cpp_proc_002_envelope_round_trips_through_ndjson() {
        let envelope = Envelope::with_payload(
            kind::TOOL_INVOKE,
            InvokePayload {
                tool: "echo".to_owned(),
                arguments: json!({ "text": "hi" }),
                timeout_ms: 120_000,
                cancelable: true,
                generation: Some(3),
            },
        )
        .unwrap()
        .with_request_id("req-1")
        .with_generation(3)
        .with_trace_id("trace-1");

        let line = envelope.encode_line().unwrap();
        assert!(line.ends_with('\n'));
        let parsed = Envelope::parse(&line).unwrap();
        assert_eq!(parsed, envelope);
        assert_eq!(parsed.kind, kind::TOOL_INVOKE);
        assert_eq!(parsed.request_id.as_deref(), Some("req-1"));
        assert_eq!(parsed.generation, Some(3));
    }

    #[test]
    fn cpp_proc_003_envelope_rejects_wrong_version() {
        let line = json!({
            "version": "cpp-process/v2",
            "kind": "process.ping",
            "payload": {}
        })
        .to_string();
        let err = Envelope::parse(&line).unwrap_err();
        assert_eq!(
            err,
            ProtocolError::UnsupportedVersion {
                actual: "cpp-process/v2".to_owned(),
                expected: PROTOCOL_VERSION.to_owned(),
            }
        );
        assert_eq!(err.code(), "E_VERSION");
    }

    #[test]
    fn cpp_proc_004_envelope_rejects_malformed_frames() {
        assert_eq!(
            Envelope::parse("").unwrap_err().code(),
            error_code::E_PROTOCOL
        );
        assert_eq!(
            Envelope::parse("{not json}").unwrap_err().code(),
            error_code::E_PROTOCOL
        );
        assert_eq!(
            Envelope::parse(r#"{"version":"cpp-process/v1","kind":1}"#)
                .unwrap_err()
                .code(),
            error_code::E_PROTOCOL
        );
    }

    #[test]
    fn cpp_proc_005_frame_limit_is_enforced_on_encode() {
        let envelope = Envelope::with_payload(
            kind::TOOL_RESULT,
            json!({ "blob": "x".repeat(MAX_FRAME_BYTES + 16) }),
        )
        .unwrap()
        .with_request_id("req-big");
        let err = envelope.encode_line().unwrap_err();
        assert!(matches!(err, ProtocolError::FrameTooLarge { .. }));
        assert_eq!(err.code(), error_code::E_PROTOCOL);
    }

    #[test]
    fn cpp_proc_006_error_envelope_helpers() {
        let envelope = Envelope::with_error(
            kind::TOOL_RESULT,
            Some("req-1".to_owned()),
            error_code::E_TOO_LARGE,
            "result too large",
        );
        assert!(envelope.is_error());
        assert_eq!(envelope.error_code(), Some("E_TOO_LARGE"));
        assert_eq!(envelope.request_id.as_deref(), Some("req-1"));
        assert!(!Envelope::new(kind::PROCESS_PING).is_error());
    }

    #[test]
    fn cpp_proc_007_tool_decl_validation() {
        let valid = ToolDecl::new("echo", "echo input")
            .with_input_schema(json!({ "type": "object" }))
            .with_timeout_ms(300);
        assert!(valid.validate().is_ok());

        assert!(ToolDecl::new("Echo", "x").validate().is_err());
        assert!(ToolDecl::new("echo", "").validate().is_err());
        assert!(ToolDecl::new("echo", "x")
            .with_timeout_ms(0)
            .validate()
            .is_err());
        assert!(ToolDecl::new("echo", "x")
            .with_timeout_ms(MAX_TOOL_TIMEOUT_MS + 1)
            .validate()
            .is_err());
        assert!(ToolDecl::new("echo", "x")
            .with_input_schema(json!([1, 2]))
            .validate()
            .is_err());
        let huge = ToolDecl::new("echo", "x")
            .with_input_schema(json!({ "blob": "x".repeat(MAX_SCHEMA_BYTES + 1) }));
        assert!(huge.validate().is_err());
    }

    #[test]
    fn cpp_proc_008_panel_decl_validation_and_defaults() {
        let panel = PanelDecl::new(
            "workspace.panel",
            "fixture.one",
            "Fixture One",
            "fixture.one",
        )
        .with_location("right")
        .with_order(100.0);
        assert!(panel.validate().is_ok());
        assert_eq!(panel.location.as_deref(), Some("right"));
        assert_eq!(panel.order, Some(100.0));

        assert!(PanelDecl::new("", "id", "label", "view")
            .validate()
            .is_err());
        assert!(PanelDecl::new("workspace.panel", "id", "label", "view")
            .with_location("middle")
            .validate()
            .is_err());
        assert!(PanelDecl::new("workspace.panel", "id", "label", "view")
            .with_order(f64::INFINITY)
            .validate()
            .is_err());
    }

    #[test]
    fn cpp_proc_009_ready_decls_round_trip_and_duplicate_tools_rejected() {
        let decls = ReadyDecls::new(
            vec![ToolDecl::new("echo", "echo input")],
            vec![PanelDecl::new(
                "workspace.panel",
                "fixture.one",
                "Fixture One",
                "fixture.one",
            )],
        );
        decls.validate().unwrap();
        let payload = ReadyPayload {
            protocol: PROTOCOL_VERSION.to_owned(),
            plugin_id: "cpp.iso.fixture".to_owned(),
            generation: Some(1),
            tools: decls.tools.clone(),
            panels: decls.panels.clone(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let parsed: ReadyPayload = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, payload);
        assert_eq!(parsed.tools[0].timeout_ms, DEFAULT_TOOL_TIMEOUT_MS);
        assert!(parsed.tools[0].cancelable);

        let duplicate = ReadyDecls::new(
            vec![ToolDecl::new("echo", "a"), ToolDecl::new("echo", "b")],
            Vec::new(),
        );
        assert!(duplicate.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn cpp_proc_010_payloads_use_expected_wire_field_names() {
        let hello = HelloPayload {
            protocol: PROTOCOL_VERSION.to_owned(),
            host_version: "0.0.0".to_owned(),
            plugin_id: "cpp.iso.fixture".to_owned(),
            generation: 1,
            manifest: None,
        };
        let wire = serde_json::to_value(hello).unwrap();
        assert_eq!(wire["protocol"], json!("cpp-process/v1"));
        assert_eq!(wire["generation"], json!(1));

        let invoke: InvokePayload =
            serde_json::from_value(json!({ "tool": "echo", "arguments": { "text": "x" } }))
                .unwrap();
        assert_eq!(invoke.timeout_ms, DEFAULT_TOOL_TIMEOUT_MS);
        assert!(invoke.cancelable);
        assert_eq!(invoke.arguments["text"], json!("x"));

        let shutdown: ShutdownPayload = serde_json::from_value(json!({})).unwrap();
        assert_eq!(shutdown.timeout_ms, DEFAULT_SHUTDOWN_TIMEOUT_MS);
    }
}
