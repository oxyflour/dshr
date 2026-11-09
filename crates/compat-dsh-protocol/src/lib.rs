//! Sidecar RPC 信封（`docs/spec/sidecar-rpc.md` §4）的可执行实现。
//!
//! 只做三件事：构造、序列化、反序列化。传输（NDJSON）与生命周期在 `compat-dsh-host`。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_DEPTH: u32 = 4;

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
    fn depth_limit_is_declared() {
        assert_eq!(MAX_DEPTH, 4);
    }
}
