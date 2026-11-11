//! Sidecar RPC 信封（`docs/spec/sidecar-rpc.md` §4）的可执行实现。
//!
//! 只做三件事：构造、序列化、反序列化。传输（NDJSON）与生命周期在 `compat-dsh-host`。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_DEPTH: u32 = 4;

/// 能力协商（`docs/spec/sidecar-rpc.md` §3）：host → sidecar 的 `host_caps` 词表。
///
/// `fs.write` 是 M2 写工具的门闩：sidecar 未收到它时必须保持只读行为，
/// 因此旧宿主（只发 tool.stream/tool.cancel）与 `--selftest-dsh` 完全不受影响。
pub const CAP_TOOL_STREAM: &str = "tool.stream";
pub const CAP_TOOL_CANCEL: &str = "tool.cancel";
pub const CAP_FS_WRITE: &str = "fs.write";

/// Sidecar → Host 的写入回调（`docs/spec/sidecar-rpc.md` §7.5）。
pub const KIND_FS_WRITE_REQUEST: &str = "fs.write.request";
/// Host → Sidecar 的写入答复。
pub const KIND_FS_WRITE_RESULT: &str = "fs.write.result";

/// 写操作的原子语义：`write` 是 create-or-replace（内容即新文件全文），
/// `delete` 删除现有文件（写前快照仍在 Rust 侧完成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FsWriteOperation {
    #[default]
    Write,
    Delete,
}

impl FsWriteOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

/// Sidecar 发起、Rust 裁决的一次写请求。
///
/// `request_id`/`call_id` 由信封承载：`request_id` 关联 `fs.write.result`，
/// `call_id` 关联发起这次写入的工具调用（审批审计用同一身份）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteRequest {
    /// 发起写入的工具调用 ID（`tool.invoke` 的 `call_id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// 工具名（`write` / `edit` / `apply_patch`），用于审批展示。
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub operation: FsWriteOperation,
    /// 目标路径：工作区相对路径，或工作区内绝对路径（Rust 负责归一化/边界校验）。
    pub path: String,
    /// sidecar 认为的工作区根；与宿主不一致时直接拒绝（防混淆代理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// `write` 必填：新文件全文；`delete` 必须缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 可选版本守卫（`mtime_ms:size`）：不匹配即 `FS_STALE_VERSION`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    /// 官方 DSH `fs/write-intent` 水位（当前只透传审计，不参与裁决）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Value>,
    /// 官方沙箱模式（当前为 `workspace-write`/`read-only` 的说明性字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl FsWriteRequest {
    pub fn summary(&self) -> Value {
        json!({
            "path": self.path,
            "operation": self.operation.as_str(),
            "bytes": self.content.as_ref().map(|content| content.len()).unwrap_or(0),
            "tool": self.tool,
        })
    }
}

/// 写请求的结构化错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl FsWriteError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

/// Rust 对一次写请求的答复（`fs.write.result.payload`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FsWriteResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// 写入后的版本（`mtime_ms:size`）；`delete` 无版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// 写前快照 ID（覆盖/删除已有文件时产生）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FsWriteError>,
}

impl FsWriteResult {
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(FsWriteError::new(code, message)),
            ..Self::default()
        }
    }

    pub fn is_ok(&self) -> bool {
        self.ok
    }
}

/// 带完整 host_caps 的握手（客户端能力协商）。
pub fn hello_with_caps(caps: &[String]) -> Envelope {
    Envelope::host(
        "runtime.hello",
        "req-hello",
        json!({
            "protocol_max": PROTOCOL_VERSION,
            "dsh_version": "0.2.2-rc.1",
            "host_caps": caps,
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub kind: String,
    pub request_id: String,
    pub runtime_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub depth: u32,
    pub trace_id: String,
    pub ts: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Envelope {
    /// 宿主侧发起：`runtime_generation` 在握手前用占位值，握手后由 host 覆写。
    pub fn host(kind: impl Into<String>, request_id: impl Into<String>, payload: Value) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind: kind.into(),
            request_id: request_id.into(),
            runtime_generation: "node-pending".to_owned(),
            plugin_id: None,
            registration_id: None,
            session_id: None,
            call_id: None,
            depth: 0,
            trace_id: "t-local".to_owned(),
            ts: "1970-01-01T00:00:00Z".to_owned(),
            payload,
            error: None,
        }
    }

    pub fn with_generation(mut self, generation: impl Into<String>) -> Self {
        self.runtime_generation = generation.into();
        self
    }

    pub fn with_plugin(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = Some(plugin_id.into());
        self
    }

    pub fn with_call(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// 结束一次请求：复制关联字段，替换 kind/payload。
    pub fn reply(&self, kind: impl Into<String>, payload: Value) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind: kind.into(),
            request_id: self.request_id.clone(),
            runtime_generation: self.runtime_generation.clone(),
            plugin_id: self.plugin_id.clone(),
            registration_id: self.registration_id.clone(),
            session_id: self.session_id.clone(),
            call_id: self.call_id.clone(),
            depth: self.depth,
            trace_id: self.trace_id.clone(),
            ts: self.ts.clone(),
            payload,
            error: None,
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// 规范 §9 的错误码闭集。
pub const ERROR_CODES: &[&str] = &[
    "E_PROTOCOL",
    "E_VERSION",
    "E_UNKNOWN_PLUGIN",
    "E_DUPLICATE_REG",
    "E_SCHEMA",
    "E_TIMEOUT",
    "E_CANCELLED",
    "E_RPC_DEPTH",
    "E_TOO_LARGE",
    "E_BACKPRESSURE",
    "E_INTERNAL",
];

/// 可安全重试的 sidecar 错误码（规范 §9.2）。
///
/// 注意：这里只回答"错误本身是否瞬时可重试"；是否真的重试还要看工具是否幂等。
/// 写工具（write/edit/apply_patch）**不适合自动重试**——重试会重复审批/重复写，
/// 应当把结构化错误交给模型，让它重读现场后再决定。
pub fn is_retryable_sidecar_error(code: &str) -> bool {
    matches!(code, "E_TIMEOUT" | "E_BACKPRESSURE" | "E_INTERNAL")
}

pub fn hello(caps: &[&str]) -> Envelope {
    Envelope::host(
        "runtime.hello",
        "req-hello",
        json!({
            "protocol_max": PROTOCOL_VERSION,
            "dsh_version": "0.2.1-rc.3",
            "host_caps": caps,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let env = hello(&["tool.stream"])
            .with_plugin("example.probe")
            .with_session("s-1")
            .with_call("call-1");
        let text = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&text).unwrap();
        assert_eq!(env, back);
        assert!(text.contains("\"kind\":\"runtime.hello\""));
    }

    #[test]
    fn optional_identity_fields_are_omitted_when_absent() {
        let env = Envelope::host("health.ping", "req-1", json!({}));
        let value: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert!(value.get("plugin_id").is_none());
        assert!(value.get("call_id").is_none());
        assert_eq!(value["version"], 1);
    }

    #[test]
    fn reply_keeps_correlation_fields() {
        let req = hello(&[])
            .with_plugin("p")
            .with_call("c-1")
            .with_session("s-1");
        let rep = req.reply("runtime.ready", json!({ "generation": "node-0001" }));
        assert_eq!(rep.request_id, req.request_id);
        assert_eq!(rep.plugin_id, req.plugin_id);
        assert_eq!(rep.call_id, req.call_id);
        assert_eq!(rep.session_id, req.session_id);
        assert_eq!(rep.kind, "runtime.ready");
    }

    #[test]
    fn error_codes_are_closed_set() {
        assert!(ERROR_CODES.contains(&"E_CANCELLED"));
        assert!(ERROR_CODES.contains(&"E_RPC_DEPTH"));
    }

    #[test]
    fn retryable_classification_is_conservative() {
        assert!(is_retryable_sidecar_error("E_TIMEOUT"));
        assert!(is_retryable_sidecar_error("E_BACKPRESSURE"));
        assert!(is_retryable_sidecar_error("E_INTERNAL"));
        for code in [
            "E_PROTOCOL",
            "E_VERSION",
            "E_SCHEMA",
            "E_CANCELLED",
            "E_TOO_LARGE",
            "E_UNKNOWN_PLUGIN",
            "E_DUPLICATE_REG",
            "E_RPC_DEPTH",
        ] {
            assert!(
                !is_retryable_sidecar_error(code),
                "{code} must not be auto-retried"
            );
        }
    }

    #[test]
    fn depth_limit_is_declared() {
        assert_eq!(MAX_DEPTH, 4);
    }

    #[test]
    fn host_caps_include_fs_write_only_when_negotiated() {
        let read_only = hello(&[CAP_TOOL_STREAM, CAP_TOOL_CANCEL]);
        assert_eq!(
            read_only.payload["host_caps"],
            json!(["tool.stream", "tool.cancel"])
        );
        assert!(!read_only.payload["host_caps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cap| cap == CAP_FS_WRITE));

        let caps = vec![
            CAP_TOOL_STREAM.to_owned(),
            CAP_TOOL_CANCEL.to_owned(),
            CAP_FS_WRITE.to_owned(),
        ];
        let writable = hello_with_caps(&caps);
        assert!(writable.payload["host_caps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cap| cap == CAP_FS_WRITE));
    }

    #[test]
    fn fs_write_request_round_trips() {
        let request = FsWriteRequest {
            call_id: Some("call-1".to_owned()),
            tool: "write".to_owned(),
            operation: FsWriteOperation::Write,
            path: "notes.txt".to_owned(),
            workspace_root: Some("C:\\ws".to_owned()),
            content: Some("hello".to_owned()),
            expected_version: Some("100:5".to_owned()),
            intent: Some(json!({ "kind": "replaceIfVersion", "version": "100:5" })),
            mode: Some("workspace-write".to_owned()),
        };
        let text = serde_json::to_string(&request).unwrap();
        let back: FsWriteRequest = serde_json::from_str(&text).unwrap();
        assert_eq!(request, back);
        assert_eq!(request.summary()["operation"], "write");
    }

    #[test]
    fn fs_write_request_defaults_are_lenient_for_forward_compat() {
        let request: FsWriteRequest =
            serde_json::from_value(json!({ "path": "a.txt", "content": "x" })).unwrap();
        assert_eq!(request.operation, FsWriteOperation::Write);
        assert!(request.call_id.is_none());
        assert!(request.expected_version.is_none());
    }

    #[test]
    fn fs_write_results_carry_structured_errors() {
        let failure = FsWriteResult::error("FS_SANDBOX_DENIED", "outside workspace");
        assert!(!failure.ok);
        assert!(!failure.is_ok());
        let error = failure.error.unwrap();
        assert_eq!(error.code, "FS_SANDBOX_DENIED");
        assert!(!error.retryable);

        let success = FsWriteResult {
            ok: true,
            path: Some("C:\\ws\\a.txt".to_owned()),
            operation: Some("create".to_owned()),
            version: Some("100:5".to_owned()),
            bytes: Some(5),
            snapshot: None,
            error: None,
        };
        let value = serde_json::to_value(&success).unwrap();
        assert_eq!(value["ok"], true);
        assert!(value.get("snapshot").is_none());
    }
}
