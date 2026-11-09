//! Session Log —— `docs/spec/session-log.md` 的最小可执行实现。
//!
//! 本 crate 只负责三件事：**追加校验**、**回放**、**JSONL 往返**。
//! 存储引擎（分段文件 / SQLite 索引）不在这一层，M2 再引入。
//!
//! 已实现的不变量（对应规范 §4）：
//! - I1 append-only：事件只能追加，`seq` 由日志分配；
//! - I2 `seq` 连续、从 1 开始；
//! - I3 一个 `call_id` 至多一次结算（`tool.result` XOR `tool.aborted`）；
//! - I4 `message.assistant.settled` 每个 `message_id` 恰好一次；
//! - I8 稳定 ID（`session_id` / `call_id`）与插件 generation 无关。
//!
//! 尚未实现：I5 压缩区间校验（需要 context 事件模型）、I6 脱敏、I7 落盘先于可见。

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 事件信封的 schema 版本（规范 §2）。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    Durable,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    User,
    Agent,
    Plugin {
        plugin_id: String,
        generation: String,
    },
    System,
}

/// 一条已提交的事件。字段顺序与规范 §2 一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub durability: Durability,
    pub ts: String,
    pub actor: Actor,
    pub trace_id: String,
    pub schema_version: u32,
    pub payload: Value,
}

/// 追加请求。`seq` 由日志分配，不接受调用方传入。
#[derive(Debug, Clone)]
pub struct EventSpec {
    pub event_type: String,
    pub durability: Durability,
    pub actor: Actor,
    pub ts: String,
    pub trace_id: String,
    pub payload: Value,
}

impl EventSpec {
    pub fn durable(event_type: impl Into<String>, ts: impl Into<String>, payload: Value) -> Self {
        Self {
            event_type: event_type.into(),
            durability: Durability::Durable,
            actor: Actor::System,
            ts: ts.into(),
            trace_id: String::from("t-local"),
            payload,
        }
    }

    pub fn ephemeral(event_type: impl Into<String>, ts: impl Into<String>, payload: Value) -> Self {
        Self {
            durability: Durability::Ephemeral,
            ..Self::durable(event_type, ts, payload)
        }
    }

    pub fn actor(mut self, actor: Actor) -> Self {
        self.actor = actor;
        self
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LogError {
    #[error("seq gap: expected {expected}, got {actual}")]
    SeqGap { expected: u64, actual: u64 },
    #[error("event belongs to session `{actual}`, expected `{expected}`")]
    SessionMismatch { expected: String, actual: String },
    #[error("tool call `{call_id}` is already settled")]
    DuplicateSettlement { call_id: String },
    #[error("assistant message `{message_id}` is already settled")]
    DuplicateMessage { message_id: String },
    #[error("event `{event_type}` requires `{field}` in payload")]
    MissingField { event_type: String, field: String },
    #[error("invalid event: {0}")]
    Invalid(String),
}

#[derive(Debug, Default)]
pub struct SessionLog {
    session_id: String,
    events: Vec<Event>,
    settled_calls: HashSet<String>,
    settled_messages: HashSet<String>,
}

impl SessionLog {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            events: Vec::new(),
            settled_calls: HashSet::new(),
            settled_messages: HashSet::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 追加并返回分配到的 `seq`。失败时日志保持不变。
    pub fn append(&mut self, spec: EventSpec) -> Result<u64, LogError> {
        let seq = self.events.len() as u64 + 1;

        // 结算类事件先做唯一性判定，再落盘，避免"写入后才发现重复"。
        match spec.event_type.as_str() {
            "tool.result" | "tool.aborted" => {
                let call_id = required_str(&spec.payload, "call_id", &spec.event_type)?;
                if spec.event_type == "tool.aborted" {
                    required_str(&spec.payload, "reason", &spec.event_type)?;
                }
                if !self.settled_calls.insert(call_id.to_owned()) {
                    return Err(LogError::DuplicateSettlement {
                        call_id: call_id.to_owned(),
                    });
                }
            }
            "message.assistant.settled" => {
                let message_id = required_str(&spec.payload, "message_id", &spec.event_type)?;
                if !self.settled_messages.insert(message_id.to_owned()) {
                    return Err(LogError::DuplicateMessage {
                        message_id: message_id.to_owned(),
                    });
                }
            }
            _ => {}
        }

        self.events.push(Event {
            session_id: self.session_id.clone(),
            seq,
            event_type: spec.event_type,
            durability: spec.durability,
            ts: spec.ts,
            actor: spec.actor,
            trace_id: spec.trace_id,
            schema_version: SCHEMA_VERSION,
            payload: spec.payload,
        });

        Ok(seq)
    }

    /// 从 JSONL 恢复，并重放全部不变量校验（规范 §9：读取端容忍未知字段与未知类型）。
    pub fn from_jsonl(session_id: impl Into<String>, jsonl: &str) -> Result<Self, LogError> {
        let mut log = Self::new(session_id);
        for (line_no, line) in jsonl.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(trimmed)
                .map_err(|err| LogError::Invalid(format!("line {}: {err}", line_no + 1)))?;

            if event.session_id != log.session_id {
                return Err(LogError::SessionMismatch {
                    expected: log.session_id.clone(),
                    actual: event.session_id.clone(),
                });
            }
            let expected = log.events.len() as u64 + 1;
            if event.seq != expected {
                return Err(LogError::SeqGap {
                    expected,
                    actual: event.seq,
                });
            }

            match event.event_type.as_str() {
                "tool.result" | "tool.aborted" => {
                    let call_id = required_str(&event.payload, "call_id", &event.event_type)?;
                    if !log.settled_calls.insert(call_id.to_owned()) {
                        return Err(LogError::DuplicateSettlement {
                            call_id: call_id.to_owned(),
                        });
                    }
                }
                "message.assistant.settled" => {
                    let message_id = required_str(&event.payload, "message_id", &event.event_type)?;
                    if !log.settled_messages.insert(message_id.to_owned()) {
                        return Err(LogError::DuplicateMessage {
                            message_id: message_id.to_owned(),
                        });
                    }
                }
                _ => {}
            }

            log.events.push(event);
        }
        Ok(log)
    }

    /// 导出为 JSONL（逐行一个事件，保持 `seq` 顺序）。
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for event in &self.events {
            out.push_str(&serde_json::to_string(event).expect("event is always serializable"));
            out.push('\n');
        }
        out
    }

    /// 回放：只从事件推导视图，不触发任何副作用（规范 §6）。
    pub fn replay(&self) -> SessionView {
        let mut view = SessionView::default();
        let mut tool_index: HashMap<String, usize> = HashMap::new();

        for event in &self.events {
            if event.durability == Durability::Ephemeral {
                view.ephemeral_skipped += 1;
                continue;
            }

            match event.event_type.as_str() {
                "turn.started" => view.turns += 1,
                "message.user" => {
                    view.messages.push(MessageView {
                        id: payload_str(&event.payload, "message_id"),
                        role: Role::User,
                        text: payload_text(&event.payload),
                    });
                }
                "message.assistant.settled" => {
                    view.messages.push(MessageView {
                        id: payload_str(&event.payload, "message_id"),
                        role: Role::Assistant,
                        text: payload_text(&event.payload),
                    });
                }
                "tool.call" => {
                    let call_id = payload_str(&event.payload, "call_id");
                    tool_index.insert(call_id.clone(), view.tool_calls.len());
                    view.tool_calls.push(ToolCallView {
                        call_id,
                        tool: payload_str(&event.payload, "tool"),
                        state: ToolState::Pending,
                    });
                }
                "tool.result" => {
                    let call_id = payload_str(&event.payload, "call_id");
                    if let Some(ix) = tool_index.get(&call_id) {
                        view.tool_calls[*ix].state = ToolState::Settled {
                            is_error: event
                                .payload
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        };
                    } else {
                        view.orphan_events += 1;
                    }
                }
                "tool.aborted" => {
                    let call_id = payload_str(&event.payload, "call_id");
                    if let Some(ix) = tool_index.get(&call_id) {
                        view.tool_calls[*ix].state = ToolState::Aborted {
                            reason: payload_str(&event.payload, "reason"),
                        };
                    } else {
                        view.orphan_events += 1;
                    }
                }
                _ => view.unmodeled_events += 1,
            }
        }

        view
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    pub id: String,
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    Pending,
    Settled { is_error: bool },
    Aborted { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallView {
    pub call_id: String,
    pub tool: String,
    pub state: ToolState,
}

#[derive(Debug, Default)]
pub struct SessionView {
    pub messages: Vec<MessageView>,
    pub tool_calls: Vec<ToolCallView>,
    pub turns: usize,
    /// 未知 `type`：按规范 §9 跳过并计数，不中断回放。
    pub unmodeled_events: usize,
    /// 流式 chunk：不进回放。
    pub ephemeral_skipped: usize,
    /// 结算找不到对应 `tool.call`：数据可疑，计数但不猜测修复。
    pub orphan_events: usize,
}

fn required_str<'a>(
    payload: &'a Value,
    field: &str,
    event_type: &str,
) -> Result<&'a str, LogError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| LogError::MissingField {
            event_type: event_type.to_owned(),
            field: field.to_owned(),
        })
}

fn payload_str(payload: &Value, field: &str) -> String {
    payload
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn payload_text(payload: &Value) -> String {
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    payload
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ts() -> &'static str {
        "2026-11-09T10:00:00.000Z"
    }

    fn log_with_call() -> SessionLog {
        let mut log = SessionLog::new("s-1");
        log.append(EventSpec::durable(
            "tool.call",
            ts(),
            json!({ "call_id": "call-1", "tool": "bash" }),
        ))
        .unwrap();
        log
    }

    #[test]
    fn append_assigns_contiguous_seq() {
        let mut log = SessionLog::new("s-1");
        assert_eq!(
            log.append(EventSpec::durable("turn.started", ts(), json!({})))
                .unwrap(),
            1
        );
        assert_eq!(
            log.append(EventSpec::durable("turn.ended", ts(), json!({})))
                .unwrap(),
            2
        );
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn rejects_second_settlement_for_same_call() {
        let mut log = log_with_call();
        log.append(EventSpec::durable(
            "tool.result",
            ts(),
            json!({ "call_id": "call-1", "is_error": false }),
        ))
        .unwrap();

        let err = log
            .append(EventSpec::durable(
                "tool.aborted",
                ts(),
                json!({ "call_id": "call-1", "reason": "crash" }),
            ))
            .unwrap_err();

        assert_eq!(
            err,
            LogError::DuplicateSettlement {
                call_id: "call-1".to_owned()
            }
        );
        assert_eq!(log.len(), 2, "failed append must not mutate the log");
    }

    #[test]
    fn rejects_duplicate_assistant_settlement() {
        let mut log = SessionLog::new("s-1");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            ts(),
            json!({ "message_id": "m-1", "text": "hi" }),
        ))
        .unwrap();

        let err = log
            .append(EventSpec::durable(
                "message.assistant.settled",
                ts(),
                json!({ "message_id": "m-1", "text": "hi again" }),
            ))
            .unwrap_err();
        assert!(matches!(err, LogError::DuplicateMessage { .. }));
    }

    #[test]
    fn rejects_settlement_without_call_id() {
        let mut log = SessionLog::new("s-1");
        let err = log
            .append(EventSpec::durable("tool.result", ts(), json!({})))
            .unwrap_err();
        assert_eq!(
            err,
            LogError::MissingField {
                event_type: "tool.result".to_owned(),
                field: "call_id".to_owned(),
            }
        );
    }

    #[test]
    fn replay_rebuilds_messages_tools_and_turns() {
        let mut log = SessionLog::new("s-1");
        log.append(EventSpec::durable("turn.started", ts(), json!({})))
            .unwrap();
        log.append(EventSpec::durable(
            "message.user",
            ts(),
            json!({ "message_id": "m-1", "text": "run it" }),
        ))
        .unwrap();
        log.append(EventSpec::ephemeral(
            "message.assistant.delta",
            ts(),
            json!({ "message_id": "m-2", "text": "partial" }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "message.assistant.settled",
            ts(),
            json!({ "message_id": "m-2", "text": "done" }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "tool.call",
            ts(),
            json!({ "call_id": "call-1", "tool": "bash" }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "tool.result",
            ts(),
            json!({ "call_id": "call-1", "is_error": true }),
        ))
        .unwrap();
        log.append(EventSpec::durable("some.future.event", ts(), json!({})))
            .unwrap();

        let view = log.replay();
        assert_eq!(view.turns, 1);
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[1].role, Role::Assistant);
        assert_eq!(view.messages[1].text, "done");
        assert_eq!(
            view.tool_calls[0].state,
            ToolState::Settled { is_error: true }
        );
        assert_eq!(view.unmodeled_events, 1);
        assert_eq!(view.ephemeral_skipped, 1);
    }

    #[test]
    fn replay_marks_aborted_tool_and_skips_ephemeral() {
        let mut log = log_with_call();
        log.append(EventSpec::ephemeral(
            "tool.chunk",
            ts(),
            json!({ "call_id": "call-1", "text": "..." }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "tool.aborted",
            ts(),
            json!({ "call_id": "call-1", "reason": "crash" }),
        ))
        .unwrap();

        let view = log.replay();
        assert_eq!(view.ephemeral_skipped, 1);
        assert_eq!(
            view.tool_calls[0].state,
            ToolState::Aborted {
                reason: "crash".to_owned()
            }
        );
    }

    #[test]
    fn jsonl_round_trip_preserves_events_and_view() {
        let mut log = log_with_call();
        log.append(EventSpec::durable(
            "tool.result",
            ts(),
            json!({ "call_id": "call-1", "is_error": false }),
        ))
        .unwrap();

        let jsonl = log.to_jsonl();
        let restored = SessionLog::from_jsonl("s-1", &jsonl).unwrap();

        assert_eq!(restored.events(), log.events());
        assert_eq!(
            restored.replay().tool_calls[0].state,
            ToolState::Settled { is_error: false }
        );
    }

    #[test]
    fn from_jsonl_rejects_seq_gap_and_wrong_session() {
        let mut log = log_with_call();
        log.append(EventSpec::durable(
            "tool.result",
            ts(),
            json!({ "call_id": "call-1" }),
        ))
        .unwrap();
        let mut jsonl = log.to_jsonl();

        let err = SessionLog::from_jsonl("s-2", &jsonl).unwrap_err();
        assert!(matches!(err, LogError::SessionMismatch { .. }));

        let first_line_end = jsonl.find('\n').unwrap();
        jsonl.remove(first_line_end - 1); // 破坏第一条事件的字符
        let err = SessionLog::from_jsonl("s-1", &jsonl).unwrap_err();
        assert!(
            matches!(err, LogError::Invalid(_) | LogError::SeqGap { .. }),
            "corrupted line must be rejected, got {err:?}"
        );
    }
}
