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
//! - I8 稳定 ID（`session_id` / `call_id`）与插件 generation 无关；
//! - I9 `session.created` 是第一条事件，`parent_session_id` 写入一次后不可改（会话树，§7）。
//!
//! 崩溃安全写入原语见 [`save_atomic`] / [`quarantine_corrupt`] / [`recover_backup`]
//! （M2 review P1-1：损坏日志不得被静默覆盖）。
//!
//! 尚未实现：I5 压缩区间校验（需要 context 事件模型）、I6 脱敏、I7 落盘先于可见。

#![forbid(unsafe_code)]

pub mod session_index;
pub mod session_tree;

pub use session_index::{
    quarantine_corrupt, recover_backup, save_atomic, SessionIndex, SessionMeta,
};
pub use session_tree::{SessionNode, SessionTree, TreeError, DEFAULT_MAX_DEPTH};

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// 事件信封的 schema 版本（规范 §2）。
pub const SCHEMA_VERSION: u32 = 1;

/// 会话树相关的事件类型名（规范 §3.1 / §3.6 / §7）。
///
/// 事件名是**规范的一部分**：值不能随意改，读取端也必须容忍未知事件。
pub mod event_type {
    pub const SESSION_CREATED: &str = "session.created";
    /// 中断的发起方（父/用户）记录在自己的日志里。
    pub const SESSION_INTERRUPTED: &str = "session.interrupted";
    /// 子会话被父会话中断：子会话自己的日志必须各写一条（`interrupted_by_parent`）。
    pub const SESSION_INTERRUPTED_BY_PARENT: &str = "session.interrupted_by_parent";
    /// 中断超时后按 kill 语义强制结束（升级路径，审计用）。
    pub const SESSION_KILLED: &str = "session.killed";
    /// 父会话记录"派生了一个子会话"。
    pub const CHILD_SPAWNED: &str = "child.spawned";
    /// 父会话记录"子会话结束"。
    pub const CHILD_FINISHED: &str = "child.finished";
}

/// 子会话的创建来源，写入 `session.created.spawn_source`（规范 §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnSource {
    /// 用户在 UI 新建的根会话。
    User,
    /// 父 Agent 通过 `spawn_child` 工具派生。
    Agent,
}

impl SpawnSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }

    /// 读取端容忍未知来源：未来新增来源不应让旧版本回放失败。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// `session.created` 的载荷：父链接与来源**只在这里写入一次**（规范 §7）。
///
/// 这是会话树可回放的唯一权威来源；`*.meta.json` 只保存 UI 侧信息（标题），
/// 不参与树结构重建。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCreated {
    pub workspace_id: String,
    pub parent_session_id: Option<String>,
    pub spawn_source: Option<SpawnSource>,
    pub preset: Option<String>,
}

impl SessionCreated {
    /// 根会话（没有父）。
    pub fn root(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            parent_session_id: None,
            spawn_source: None,
            preset: None,
        }
    }

    /// 子会话：父链接 + 来源。
    pub fn child(
        workspace_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        spawn_source: SpawnSource,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            parent_session_id: Some(parent_session_id.into()),
            spawn_source: Some(spawn_source),
            preset: None,
        }
    }

    pub fn with_preset(mut self, preset: impl Into<String>) -> Self {
        self.preset = Some(preset.into());
        self
    }

    pub fn with_optional_preset(mut self, preset: Option<String>) -> Self {
        self.preset = preset;
        self
    }

    pub fn is_child(&self) -> bool {
        self.parent_session_id.is_some()
    }

    /// 事件载荷。可选字段缺省时不写，保持 JSON 紧凑。
    pub fn payload(&self) -> Value {
        let mut payload = json!({ "workspace_id": self.workspace_id });
        let object = payload
            .as_object_mut()
            .expect("session.created payload 一定是 object");
        if let Some(parent_session_id) = &self.parent_session_id {
            object.insert("parent_session_id".to_owned(), json!(parent_session_id));
        }
        if let Some(spawn_source) = self.spawn_source {
            object.insert("spawn_source".to_owned(), json!(spawn_source.as_str()));
        }
        if let Some(preset) = &self.preset {
            object.insert("preset".to_owned(), json!(preset));
        }
        payload
    }

    /// 从载荷还原；`workspace_id` 必需（规范 §3.1）。
    pub fn from_payload(payload: &Value) -> Result<Self, LogError> {
        let workspace_id = required_str(payload, "workspace_id", event_type::SESSION_CREATED)?;
        let parent_session_id = payload
            .get("parent_session_id")
            .and_then(Value::as_str)
            .filter(|parent| !parent.is_empty())
            .map(str::to_owned);
        let spawn_source = payload
            .get("spawn_source")
            .and_then(Value::as_str)
            .and_then(SpawnSource::parse);
        let preset = payload
            .get("preset")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            parent_session_id,
            spawn_source,
            preset,
        })
    }

    /// 写入子会话的 Event Log（必须是第一条事件，见规范 §7）。
    pub fn append_to(&self, log: &mut SessionLog, ts: impl Into<String>) -> Result<u64, LogError> {
        log.append(EventSpec::durable(
            event_type::SESSION_CREATED,
            ts,
            self.payload(),
        ))
    }
}

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

#[derive(Debug, Clone, Default)]
pub struct SessionLog {
    session_id: String,
    events: Vec<Event>,
    settled_calls: HashSet<String>,
    settled_messages: HashSet<String>,
    /// `session.created` 里的树字段；旧日志（M2 之前）没有该事件时为 None。
    created: Option<SessionCreated>,
}

impl SessionLog {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            events: Vec::new(),
            settled_calls: HashSet::new(),
            settled_messages: HashSet::new(),
            created: None,
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

    /// `session.created` 重建出的树字段（父链接/来源/预设）。
    pub fn created(&self) -> Option<&SessionCreated> {
        self.created.as_ref()
    }

    /// 父会话 ID；根会话或旧日志为 None。
    pub fn parent_session_id(&self) -> Option<&str> {
        self.created
            .as_ref()
            .and_then(|created| created.parent_session_id.as_deref())
    }

    /// 创建来源；旧日志为 None。
    pub fn spawn_source(&self) -> Option<SpawnSource> {
        self.created
            .as_ref()
            .and_then(|created| created.spawn_source)
    }

    /// 追加并返回分配到的 `seq`。失败时日志保持不变。
    pub fn append(&mut self, spec: EventSpec) -> Result<u64, LogError> {
        let seq = self.events.len() as u64 + 1;

        // 结算/树元数据类事件先做校验，再落盘，避免"写入后才发现非法"。
        match spec.event_type.as_str() {
            event_type::SESSION_CREATED => {
                if seq != 1 {
                    return Err(LogError::Invalid(String::from(
                        "`session.created` 必须是会话的第一条事件",
                    )));
                }
                let created = SessionCreated::from_payload(&spec.payload)?;
                if created.parent_session_id.as_deref() == Some(self.session_id.as_str()) {
                    return Err(LogError::Invalid(String::from(
                        "`parent_session_id` 不能等于会话自身",
                    )));
                }
                self.created = Some(created);
            }
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
    ///
    /// 逐条走 [`SessionLog::append`]，这样"恢复"与"在线追加"共享同一套校验
    /// （包括 `session.created` 必须是第一条、结算唯一性等），不会出现两条规则。
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

            log.append(EventSpec {
                event_type: event.event_type,
                durability: event.durability,
                actor: event.actor,
                ts: event.ts,
                trace_id: event.trace_id,
                payload: event.payload,
            })?;
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

    // ------------------------------------------------------------------
    // 会话树事件（规范 §3.6 / §7）。载荷形状集中在这里，避免调用方手写 JSON
    // 导致字段名漂移；读取端在 `replay` 与 `SessionView`。
    // ------------------------------------------------------------------

    /// 写入 `session.created`（必须是第一条事件）。
    pub fn append_created(
        &mut self,
        ts: impl Into<String>,
        created: &SessionCreated,
    ) -> Result<u64, LogError> {
        created.append_to(self, ts)
    }

    /// 父会话记录：派生了一个子会话。
    pub fn append_child_spawned(
        &mut self,
        ts: impl Into<String>,
        child_session_id: &str,
        prompt: &str,
        parent_message_id: Option<&str>,
    ) -> Result<u64, LogError> {
        let mut payload = json!({
            "child_session_id": child_session_id,
            "prompt": prompt,
        });
        if let Some(parent_message_id) = parent_message_id {
            payload["parent_message_id"] = json!(parent_message_id);
        }
        self.append(EventSpec::durable(event_type::CHILD_SPAWNED, ts, payload))
    }

    /// 父会话记录：子会话结束。
    pub fn append_child_finished(
        &mut self,
        ts: impl Into<String>,
        child_session_id: &str,
        outcome: &str,
    ) -> Result<u64, LogError> {
        self.append(EventSpec::durable(
            event_type::CHILD_FINISHED,
            ts,
            json!({ "child_session_id": child_session_id, "outcome": outcome }),
        ))
    }

    /// 中断发起方记录：自己发起了中断（`by_parent: false`）。
    pub fn append_interrupted(
        &mut self,
        ts: impl Into<String>,
        reason: &str,
    ) -> Result<u64, LogError> {
        self.append(EventSpec::durable(
            event_type::SESSION_INTERRUPTED,
            ts,
            json!({ "reason": reason, "by_parent": false }),
        ))
    }

    /// 子会话记录：被父会话中断（规范 §7 的 `interrupted_by_parent`）。
    pub fn append_interrupted_by_parent(
        &mut self,
        ts: impl Into<String>,
        parent_session_id: &str,
        reason: &str,
    ) -> Result<u64, LogError> {
        self.append(EventSpec::durable(
            event_type::SESSION_INTERRUPTED_BY_PARENT,
            ts,
            json!({ "parent_session_id": parent_session_id, "reason": reason }),
        ))
    }

    /// 中断超时升级为 kill（审计事件，不改变 `interrupted_by_parent` 的事实）。
    pub fn append_killed(&mut self, ts: impl Into<String>, reason: &str) -> Result<u64, LogError> {
        self.append(EventSpec::durable(
            event_type::SESSION_KILLED,
            ts,
            json!({ "reason": reason, "by_parent": true }),
        ))
    }

    /// 该会话是否被父会话中断过（回放/重启恢复用）。
    pub fn interrupted_by_parent(&self) -> bool {
        self.events.iter().any(|event| {
            event.event_type == event_type::SESSION_INTERRUPTED_BY_PARENT
                || (event.event_type == event_type::SESSION_INTERRUPTED
                    && payload_bool(&event.payload, "by_parent"))
        })
    }

    /// 是否走过"中断超时 → kill"升级路径。
    pub fn was_killed(&self) -> bool {
        self.events
            .iter()
            .any(|event| event.event_type == event_type::SESSION_KILLED)
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
                event_type::SESSION_CREATED => match SessionCreated::from_payload(&event.payload) {
                    Ok(created) => view.created = Some(created),
                    // 树字段坏掉不该中断回放：跳过并计数（规范 §9）。
                    Err(_) => view.orphan_events += 1,
                },
                event_type::SESSION_INTERRUPTED => {
                    view.interruptions.push(InterruptionView {
                        reason: payload_str(&event.payload, "reason"),
                        by_parent: payload_bool(&event.payload, "by_parent"),
                        killed: false,
                    });
                }
                // 读取端兼容别名：`session.interrupted_by_parent` 是 `by_parent: true`
                // 的专用事件名（早期草案使用）。
                event_type::SESSION_INTERRUPTED_BY_PARENT => {
                    view.interruptions.push(InterruptionView {
                        reason: payload_str(&event.payload, "reason"),
                        by_parent: true,
                        killed: false,
                    });
                }
                event_type::SESSION_KILLED => {
                    view.interruptions.push(InterruptionView {
                        reason: payload_str(&event.payload, "reason"),
                        by_parent: payload_bool(&event.payload, "by_parent"),
                        killed: true,
                    });
                }
                event_type::CHILD_SPAWNED => {
                    let child_session_id = payload_str(&event.payload, "child_session_id");
                    if !view
                        .children
                        .iter()
                        .any(|child| child.child_session_id == child_session_id)
                    {
                        view.children.push(ChildView {
                            child_session_id,
                            prompt: payload_str(&event.payload, "prompt"),
                            outcome: None,
                        });
                    }
                }
                event_type::CHILD_FINISHED => {
                    let child_session_id = payload_str(&event.payload, "child_session_id");
                    let outcome = payload_str(&event.payload, "outcome");
                    match view
                        .children
                        .iter_mut()
                        .find(|child| child.child_session_id == child_session_id)
                    {
                        Some(child) => child.outcome = Some(outcome),
                        None => view.children.push(ChildView {
                            child_session_id,
                            prompt: String::new(),
                            outcome: Some(outcome),
                        }),
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

/// 一次中断痕迹（`session.interrupted` / `interrupted_by_parent` / `session.killed`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptionView {
    pub reason: String,
    /// true = 由父会话传播而来（`interrupted_by_parent`）。
    pub by_parent: bool,
    /// true = 中断超时后按 kill 语义结束。
    pub killed: bool,
}

/// 父会话眼里的一个子会话（`child.spawned` + `child.finished`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildView {
    pub child_session_id: String,
    pub prompt: String,
    /// `child.finished.outcome`；尚未结束时为 None。
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionView {
    pub messages: Vec<MessageView>,
    pub tool_calls: Vec<ToolCallView>,
    pub turns: usize,
    /// `session.created` 重建出的树字段（旧日志可能没有）。
    pub created: Option<SessionCreated>,
    /// 自身被中断/被父中断/被 kill 的痕迹，按事件顺序。
    pub interruptions: Vec<InterruptionView>,
    /// 本会话派生的子会话（父日志侧视图）。
    pub children: Vec<ChildView>,
    /// 未知 `type`：按规范 §9 跳过并计数，不中断回放。
    pub unmodeled_events: usize,
    /// 流式 chunk：不进回放。
    pub ephemeral_skipped: usize,
    /// 结算找不到对应 `tool.call`：数据可疑，计数但不猜测修复。
    pub orphan_events: usize,
}

impl SessionView {
    /// 会话是否被中断过（任意原因）。
    pub fn is_interrupted(&self) -> bool {
        !self.interruptions.is_empty()
    }

    /// 是否被父会话中断（规范 §7 的 `interrupted_by_parent`）。
    pub fn interrupted_by_parent(&self) -> bool {
        self.interruptions
            .iter()
            .any(|interruption| interruption.by_parent)
    }

    /// 是否走过"中断超时 → kill"升级路径。
    pub fn was_killed(&self) -> bool {
        self.interruptions
            .iter()
            .any(|interruption| interruption.killed)
    }
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

fn payload_bool(payload: &Value, field: &str) -> bool {
    payload.get(field).and_then(Value::as_bool).unwrap_or(false)
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

    // ------------------------------------------------------------------
    // 会话树（规范 §3.6 / §7）
    // ------------------------------------------------------------------

    #[test]
    fn created_must_be_first_and_carries_parent_and_source() {
        let mut log = SessionLog::new("s-child");
        let created =
            SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent).with_preset("reviewer");
        assert_eq!(log.append_created(ts(), &created).unwrap(), 1);

        assert_eq!(log.parent_session_id(), Some("s-parent"));
        assert_eq!(log.spawn_source(), Some(SpawnSource::Agent));
        assert_eq!(log.created().unwrap().preset.as_deref(), Some("reviewer"));

        // 不是第一条 → 拒绝
        let mut later = SessionLog::new("s-x");
        later
            .append(EventSpec::durable("turn.started", ts(), json!({})))
            .unwrap();
        let err = later.append_created(ts(), &created).unwrap_err();
        assert!(matches!(err, LogError::Invalid(_)));

        // 自引用 → 拒绝
        let mut self_parent = SessionLog::new("s-self");
        let err = self_parent
            .append_created(
                ts(),
                &SessionCreated::child("ws", "s-self", SpawnSource::Agent),
            )
            .unwrap_err();
        assert!(matches!(err, LogError::Invalid(_)));
    }

    #[test]
    fn replay_rebuilds_tree_fields_from_session_created() {
        let mut log = SessionLog::new("s-root");
        log.append_created(ts(), &SessionCreated::root("ws-1"))
            .unwrap();
        log.append_child_spawned(ts(), "s-child", "修窗口", Some("call-1"))
            .unwrap();
        log.append_child_finished(ts(), "s-child", "completed")
            .unwrap();
        log.append_interrupted(ts(), "user_stop").unwrap();

        let view = log.replay();
        assert_eq!(view.created.as_ref().unwrap().workspace_id, "ws-1");
        assert!(!view.created.as_ref().unwrap().is_child());
        assert_eq!(view.children.len(), 1);
        assert_eq!(view.children[0].child_session_id, "s-child");
        assert_eq!(view.children[0].prompt, "修窗口");
        assert_eq!(view.children[0].outcome.as_deref(), Some("completed"));
        assert!(view.is_interrupted());
        assert!(!view.interrupted_by_parent());
        assert!(!view.was_killed());
        assert_eq!(view.unmodeled_events, 0);
    }

    #[test]
    fn replay_marks_interrupted_by_parent_and_kill_escalation() {
        let mut child = SessionLog::new("s-child");
        child
            .append_created(
                ts(),
                &SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent),
            )
            .unwrap();
        child
            .append_interrupted_by_parent(ts(), "s-parent", "parent_interrupt")
            .unwrap();
        child.append_killed(ts(), "interrupt_timeout").unwrap();

        let view = child.replay();
        assert_eq!(
            view.created.as_ref().unwrap().parent_session_id.as_deref(),
            Some("s-parent")
        );
        assert_eq!(view.interruptions.len(), 2);
        assert!(view.interrupted_by_parent());
        assert!(view.was_killed());
        assert!(child.interrupted_by_parent());
        assert!(child.was_killed());
    }

    #[test]
    fn tree_events_survive_jsonl_round_trip() {
        let mut log = SessionLog::new("s-child");
        log.append_created(
            ts(),
            &SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent),
        )
        .unwrap();
        log.append_interrupted_by_parent(ts(), "s-parent", "parent_interrupt")
            .unwrap();

        let restored = SessionLog::from_jsonl("s-child", &log.to_jsonl()).unwrap();
        assert_eq!(restored.parent_session_id(), Some("s-parent"));
        assert_eq!(restored.spawn_source(), Some(SpawnSource::Agent));
        assert!(restored.replay().interrupted_by_parent());
    }

    #[test]
    fn spawn_source_round_trips_and_tolerates_unknown_values() {
        assert_eq!(SpawnSource::parse("user"), Some(SpawnSource::User));
        assert_eq!(SpawnSource::parse("agent"), Some(SpawnSource::Agent));
        assert_eq!(SpawnSource::parse("acp"), None);
        assert_eq!(SpawnSource::User.as_str(), "user");
        assert_eq!(SpawnSource::Agent.as_str(), "agent");
    }
}
