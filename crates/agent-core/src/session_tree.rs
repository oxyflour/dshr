//! 会话树 v1（PLAN §6.5）：`spawn_child` 工具、子会话生命周期、中断传播与 drain。
//!
//! 设计边界：
//!
//! - **父链接的权威是事件**：`session.created.parent_session_id` / `spawn_source` 写在
//!   子会话自己的 Event Log 里（见 `dshr-session-store`），本模块只负责生成运行态。
//!   因此回放/重启后仍能重建整棵树。
//! - **父子会话各自一份 Event Log**：`seq` 空间互不共享，父日志只记录
//!   `child.spawned` / `child.finished` 这两个"引用事件"。
//! - **中断先传播再升级**：`interrupt_tree` 给每个活跃后代写
//!   `session.interrupted_by_parent` 并返回 interrupt 计划；调用方等待超时后用
//!   `escalate_interrupt` 升级为 kill（写 `session.killed`）。
//! - **drain 期间父保持 active**：[`SessionTreeRuntime::is_idle`] 会考虑未完成的
//!   drain 等待，避免 UI/调度器把等待子会话的父会话误判为闲置。
//!
//! 当前是同步模型：子会话的模型调用/工具执行由调用方驱动，本模块只维护状态机。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dshr_session_store::{
    event_type, SessionCreated, SessionLog, SessionNode, SessionTree, SpawnSource,
};
use serde_json::Value;
use thiserror::Error;

use crate::{ToolCallRequest, ToolSpec};

/// 工具名（模型可见）。
pub const SPAWN_CHILD_TOOL: &str = "spawn_child";

/// 默认深度上限：根 0 → 子 1 → 孙 2（PLAN §6.5）。
pub const DEFAULT_MAX_DEPTH: u32 = 2;

/// `spawn_child` 的工具定义（name / description / JSON Schema）。
///
/// 只暴露规范 §6.5 承诺的参数：`prompt`（必需）与 `preset`（可选）。
/// 策略收紧（白/黑名单、更小深度）是宿主 API，不交给模型自由决定。
pub fn spawn_child_spec() -> ToolSpec {
    ToolSpec {
        name: SPAWN_CHILD_TOOL.to_owned(),
        description: "创建一个子会话（独立事件日志，继承父会话的工作区与策略，只允许收紧）。\
                      子会话看不到父会话历史，prompt 必须是完整任务说明；子会话结束后父日志会记录 \
                      child.finished。默认最多派生到深度 2（根 0 / 子 1 / 孙 2）。"
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "交给子会话的完整任务说明（子会话看不到父会话历史）"
                },
                "preset": {
                    "type": "string",
                    "description": "子会话预设；缺省继承父会话预设"
                }
            },
            "additionalProperties": false
        }),
    }
}

pub fn is_spawn_child_tool(name: &str) -> bool {
    name == SPAWN_CHILD_TOOL
}

/// `spawn_child` 的参数（从模型工具调用解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnChildArgs {
    pub prompt: String,
    pub preset: Option<String>,
}

impl SpawnChildArgs {
    pub fn from_json(arguments: &Value) -> Result<Self, SpawnError> {
        let prompt = arguments
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .ok_or(SpawnError::MissingPrompt)?
            .to_owned();
        let preset = arguments
            .get("preset")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|preset| !preset.is_empty())
            .map(str::to_owned);
        Ok(Self { prompt, preset })
    }

    pub fn from_call(call: &ToolCallRequest) -> Result<Self, SpawnError> {
        if !is_spawn_child_tool(&call.tool) {
            return Err(SpawnError::WrongTool(call.tool.clone()));
        }
        Self::from_json(&call.arguments)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SpawnError {
    #[error("`spawn_child` 的 prompt 缺失或为空")]
    MissingPrompt,
    #[error("不是 `{SPAWN_CHILD_TOOL}` 工具调用: {0}")]
    WrongTool(String),
    #[error("会话 `{0}` 不存在")]
    UnknownSession(String),
    #[error("会话 `{0}` 已存在，不能重复注册/派生")]
    DuplicateSession(String),
    #[error("父会话 `{0}` 当前不可派生（状态 {1:?}）")]
    ParentNotActive(String, SessionStatus),
    #[error("会话 `{session_id}` 深度 {depth} 超过上限 {limit}")]
    DepthExceeded {
        session_id: String,
        depth: u32,
        limit: u32,
    },
    #[error("子会话不能放宽父策略: {0}")]
    PolicyWidening(String),
    #[error("事件日志属于会话 `{actual}`，期望 `{expected}`")]
    LogSessionMismatch { expected: String, actual: String },
    #[error("会话树不变量被破坏: {0}")]
    Tree(String),
    #[error("session log: {0}")]
    Log(#[from] dshr_session_store::LogError),
}

/// 会话级策略：工作区 + 深度上限 + 工具白/黑名单。
///
/// **只允许收紧**：子会话的工作区必须与父一致；`max_depth` 只能更小；
/// `allowed_tools` 只能取子集；`denied_tools` 只能追加。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPolicy {
    pub workspace_id: String,
    pub max_depth: u32,
    /// None = 不限制；Some = 只允许集合内的工具。
    pub allowed_tools: Option<BTreeSet<String>>,
    /// 一律禁止的工具，只增不减。
    pub denied_tools: BTreeSet<String>,
}

impl SessionPolicy {
    /// 根会话策略：默认深度上限。
    pub fn root(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            max_depth: DEFAULT_MAX_DEPTH,
            allowed_tools: None,
            denied_tools: BTreeSet::new(),
        }
    }

    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    pub fn with_allowed_tools(
        mut self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn with_denied_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.denied_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// 工具是否可用（白名单命中且不在黑名单里）。
    pub fn allows_tool(&self, tool: &str) -> bool {
        let allowed = self
            .allowed_tools
            .as_ref()
            .map(|allowed| allowed.contains(tool))
            .unwrap_or(true);
        allowed && !self.denied_tools.contains(tool)
    }

    /// 派生下一层策略；任何"放宽"都返回 [`SpawnError::PolicyWidening`]。
    pub fn descend(&self, tightening: &PolicyTightening) -> Result<Self, SpawnError> {
        if let Some(requested) = tightening.max_depth {
            if requested > self.max_depth {
                return Err(SpawnError::PolicyWidening(format!(
                    "max_depth {requested} > 父策略 {}",
                    self.max_depth
                )));
            }
        }
        let max_depth = tightening.max_depth.unwrap_or(self.max_depth);

        let allowed_tools = match (&self.allowed_tools, &tightening.allowed_tools) {
            (None, None) => None,
            (Some(parent), None) => Some(parent.clone()),
            (None, Some(child)) => Some(child.clone()),
            (Some(parent), Some(child)) => {
                if let Some(extra) = child.difference(parent).next() {
                    return Err(SpawnError::PolicyWidening(format!(
                        "allowed_tools 新增 `{extra}`（父策略未允许）"
                    )));
                }
                Some(child.clone())
            }
        };

        let mut denied_tools = self.denied_tools.clone();
        denied_tools.extend(tightening.denied_tools.iter().cloned());

        Ok(Self {
            workspace_id: self.workspace_id.clone(),
            max_depth,
            allowed_tools,
            denied_tools,
        })
    }
}

/// 子会话对父策略的**收紧请求**（v1 的 `spawn_child` schema 不暴露，宿主 API 可用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyTightening {
    /// 只能 <= 父策略的 max_depth。
    pub max_depth: Option<u32>,
    /// 只能取父白名单的子集。
    pub allowed_tools: Option<BTreeSet<String>>,
    /// 与父黑名单取并集。
    pub denied_tools: BTreeSet<String>,
}

/// 会话运行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// 正在运行（含等待模型/工具/drain）。
    Active,
    /// 已发出中断，等待真正停止（调用方持有 cancel token）。
    Interrupting { by_parent: bool, reason: String },
    /// 已按中断语义结束；`killed = true` 表示走了超时 kill 升级。
    Interrupted {
        by_parent: bool,
        reason: String,
        killed: bool,
    },
    /// 正常结束（completed / failed / cancelled）。
    Finished { outcome: String },
}

impl SessionStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active | Self::Interrupting { .. })
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::Interrupted { .. })
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Interrupting { .. } => "interrupting",
            Self::Interrupted { .. } => "interrupted",
            Self::Finished { .. } => "finished",
        }
    }
}

/// 运行时树中的一个会话。
#[derive(Debug, Clone)]
pub struct TreeSession {
    pub id: String,
    pub parent_session_id: Option<String>,
    /// 根 0 / 子 1 / 孙 2。
    pub depth: u32,
    pub spawn_source: Option<SpawnSource>,
    pub status: SessionStatus,
    pub policy: SessionPolicy,
    /// 会话自己的完整 Event Log（与父日志物理分离）。
    pub log: SessionLog,
    /// spawn 时的任务说明（子会话有，根会话为 None）。
    pub prompt: Option<String>,
    pub preset: Option<String>,
    /// `finish_child` 是否已向父日志写过 `child.finished`（幂等标记）。
    finished_reported: bool,
}

impl TreeSession {
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }

    pub fn is_child(&self) -> bool {
        self.parent_session_id.is_some()
    }

    pub fn finished_reported(&self) -> bool {
        self.finished_reported
    }
}

/// 派生子会话所需的上下文（时间戳、父消息 id、策略收紧请求）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnContext {
    /// 事件时间戳（RFC 3339）；测试可固定。
    pub ts: String,
    /// 触发 spawn 的父消息/工具调用 id，写入 `child.spawned.parent_message_id`。
    pub parent_message_id: Option<String>,
    /// 对父策略的收紧请求（默认完全继承）。
    pub tightening: PolicyTightening,
}

impl SpawnContext {
    pub fn at(ts: impl Into<String>) -> Self {
        Self {
            ts: ts.into(),
            ..Self::default()
        }
    }

    pub fn with_parent_message(mut self, id: impl Into<String>) -> Self {
        self.parent_message_id = Some(id.into());
        self
    }

    pub fn tighten(mut self, tightening: PolicyTightening) -> Self {
        self.tightening = tightening;
        self
    }
}

/// `spawn_child` 的结果：子会话的元数据 + 已含 `session.created` 的独立日志。
#[derive(Debug, Clone)]
pub struct SpawnedChild {
    pub session_id: String,
    pub parent_session_id: String,
    pub depth: u32,
    pub spawn_source: SpawnSource,
    pub prompt: String,
    pub preset: Option<String>,
    pub policy: SessionPolicy,
    /// 子会话日志（已写入 `session.created`）；调用方负责落盘与运行子轮次。
    pub log: SessionLog,
}

/// drain 等待语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainMode {
    /// 只等直接子级。
    DirectChildren,
    /// 等全部后代。
    AllDescendants,
}

impl DrainMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectChildren => "direct_children",
            Self::AllDescendants => "all_descendants",
        }
    }
}

/// 一次 drain 等待：`scope` 是开始等待时仍在运行的子/后代，`pending` 随结束事件缩小。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainWait {
    pub parent_session_id: String,
    pub mode: DrainMode,
    /// 开始等待时的快照（BFS 顺序，UI 可显示"还在等谁"）。
    pub scope: Vec<String>,
    pub pending: BTreeSet<String>,
}

impl DrainWait {
    pub fn is_satisfied(&self) -> bool {
        self.pending.is_empty()
    }

    /// 仍在等待的会话 id（按 BFS 快照顺序）。
    pub fn pending(&self) -> Vec<&str> {
        self.scope
            .iter()
            .filter(|id| self.pending.contains(*id))
            .map(String::as_str)
            .collect()
    }
}

/// 中断动作：第一段 `Interrupt`，超时后第二段 `Kill`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptAction {
    Interrupt,
    Kill,
}

/// 中断传播的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptTarget {
    pub session_id: String,
    pub depth: u32,
    pub action: InterruptAction,
}

/// 一次中断传播计划（层序，父在前）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptPlan {
    pub root: String,
    pub reason: String,
    pub targets: Vec<InterruptTarget>,
}

impl InterruptPlan {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.targets
            .iter()
            .map(|target| target.session_id.as_str())
            .collect()
    }
}

/// 会话树运行时：内存态（active / 中断 / drain），可与 `session-store` 的静态树互转。
#[derive(Debug, Clone)]
pub struct SessionTreeRuntime {
    max_depth: u32,
    sessions: BTreeMap<String, TreeSession>,
    drains: BTreeMap<String, DrainWait>,
}

impl SessionTreeRuntime {
    pub fn new(max_depth: u32) -> Self {
        Self {
            max_depth,
            sessions: BTreeMap::new(),
            drains: BTreeMap::new(),
        }
    }

    /// 使用 PLAN §6.5 的默认深度上限 2。
    pub fn with_default_max_depth() -> Self {
        Self::new(DEFAULT_MAX_DEPTH)
    }

    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub fn session(&self, session_id: &str) -> Option<&TreeSession> {
        self.sessions.get(session_id)
    }

    pub fn status(&self, session_id: &str) -> Option<&SessionStatus> {
        self.sessions.get(session_id).map(|session| &session.status)
    }

    pub fn drain_wait(&self, parent_session_id: &str) -> Option<&DrainWait> {
        self.drains.get(parent_session_id)
    }

    /// 注册根会话（空日志）。已有加载好的日志请用
    /// [`SessionTreeRuntime::register_root_with_log`]。
    pub fn register_root(
        &mut self,
        session_id: &str,
        workspace_id: &str,
    ) -> Result<(), SpawnError> {
        self.register_root_with_log(session_id, workspace_id, SessionLog::new(session_id))
    }

    /// 注册根会话并接管它的 Event Log（例如从磁盘恢复后的父日志）。
    pub fn register_root_with_log(
        &mut self,
        session_id: &str,
        workspace_id: &str,
        log: SessionLog,
    ) -> Result<(), SpawnError> {
        if log.session_id() != session_id {
            return Err(SpawnError::LogSessionMismatch {
                expected: session_id.to_owned(),
                actual: log.session_id().to_owned(),
            });
        }
        if log.parent_session_id().is_some() {
            return Err(SpawnError::Tree(String::from(
                "带 parent_session_id 的子会话日志不能注册为根会话",
            )));
        }
        // `session.created.workspace_id` 是权威；旧日志没有该事件时才用调用方给的值。
        let workspace_id = log
            .created()
            .map(|created| created.workspace_id.clone())
            .filter(|workspace| !workspace.is_empty())
            .unwrap_or_else(|| workspace_id.to_owned());
        let policy = SessionPolicy::root(workspace_id).with_max_depth(self.max_depth);
        let preset = log.created().and_then(|created| created.preset.clone());
        let session = TreeSession {
            id: session_id.to_owned(),
            parent_session_id: None,
            depth: 0,
            spawn_source: None,
            status: SessionStatus::Active,
            policy,
            log,
            prompt: None,
            preset,
            finished_reported: false,
        };
        self.insert(session)
    }

    fn insert(&mut self, session: TreeSession) -> Result<(), SpawnError> {
        if self.sessions.contains_key(&session.id) {
            return Err(SpawnError::DuplicateSession(session.id.clone()));
        }
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    /// 派生子会话：深度/父状态/策略校验 → 子会话独立 Event Log（`session.created`）
    /// → 父日志 `child.spawned` → 运行态注册。
    ///
    /// 返回的子会话日志**不包含**任务消息：调用方拿到 `prompt` 后按正常轮次执行
    /// （由 `Agent::run_turn` 写 `message.user`），避免同一条 prompt 落两次。
    pub fn spawn_child(
        &mut self,
        child_id: &str,
        parent_id: &str,
        args: &SpawnChildArgs,
        context: &SpawnContext,
    ) -> Result<SpawnedChild, SpawnError> {
        if args.prompt.trim().is_empty() {
            return Err(SpawnError::MissingPrompt);
        }
        if child_id == parent_id {
            return Err(SpawnError::PolicyWidening(String::from("子会话不能是自己")));
        }
        if self.sessions.contains_key(child_id) {
            return Err(SpawnError::DuplicateSession(child_id.to_owned()));
        }

        let (depth, policy, workspace_id, parent_preset) = {
            let parent = self
                .sessions
                .get(parent_id)
                .ok_or_else(|| SpawnError::UnknownSession(parent_id.to_owned()))?;
            if !parent.status.is_active() {
                return Err(SpawnError::ParentNotActive(
                    parent_id.to_owned(),
                    parent.status.clone(),
                ));
            }
            let limit = self.max_depth.min(parent.policy.max_depth);
            let depth = parent.depth + 1;
            if depth > limit {
                return Err(SpawnError::DepthExceeded {
                    session_id: child_id.to_owned(),
                    depth,
                    limit,
                });
            }
            let policy = parent.policy.descend(&context.tightening)?;
            (
                depth,
                policy,
                parent.policy.workspace_id.clone(),
                parent.preset.clone(),
            )
        };

        let preset = args.preset.clone().or(parent_preset);
        let created = SessionCreated::child(workspace_id, parent_id, SpawnSource::Agent)
            .with_optional_preset(preset.clone());
        let mut log = SessionLog::new(child_id);
        log.append_created(context.ts.clone(), &created)?;

        // 父日志记录引用事件（父会话自己的 seq 空间）。
        self.sessions
            .get_mut(parent_id)
            .expect("parent 已在上面的检查中确认存在")
            .log
            .append_child_spawned(
                context.ts.clone(),
                child_id,
                &args.prompt,
                context.parent_message_id.as_deref(),
            )?;

        let spawn_source = SpawnSource::Agent;
        self.sessions.insert(
            child_id.to_owned(),
            TreeSession {
                id: child_id.to_owned(),
                parent_session_id: Some(parent_id.to_owned()),
                depth,
                spawn_source: Some(spawn_source),
                status: SessionStatus::Active,
                policy: policy.clone(),
                log: log.clone(),
                prompt: Some(args.prompt.clone()),
                preset: preset.clone(),
                finished_reported: false,
            },
        );

        Ok(SpawnedChild {
            session_id: child_id.to_owned(),
            parent_session_id: parent_id.to_owned(),
            depth,
            spawn_source,
            prompt: args.prompt.clone(),
            preset,
            policy,
            log,
        })
    }

    /// 标记任一会话自身结束（不写父日志、不推进 drain）。根会话的轮次结束时用它；
    /// 若该会话还在 drain 等子会话，`is_idle` 仍为 false。
    pub fn mark_finished(&mut self, session_id: &str, outcome: &str) -> Result<(), SpawnError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SpawnError::UnknownSession(session_id.to_owned()))?;
        session.status = match &session.status {
            SessionStatus::Interrupting { by_parent, reason } => SessionStatus::Interrupted {
                by_parent: *by_parent,
                reason: reason.clone(),
                killed: false,
            },
            SessionStatus::Interrupted { .. } | SessionStatus::Finished { .. } => {
                session.status.clone()
            }
            SessionStatus::Active => SessionStatus::Finished {
                outcome: outcome.to_owned(),
            },
        };
        self.settle_drains(session_id);
        Ok(())
    }

    /// 子会话结束：更新状态、父日志写 `child.finished`、推进所有相关 drain。幂等。
    pub fn finish_child(
        &mut self,
        child_id: &str,
        outcome: &str,
        ts: &str,
    ) -> Result<(), SpawnError> {
        let parent_id = {
            let child = self
                .sessions
                .get(child_id)
                .ok_or_else(|| SpawnError::UnknownSession(child_id.to_owned()))?;
            if child.finished_reported {
                return Ok(());
            }
            child.parent_session_id.clone()
        };

        self.mark_finished(child_id, outcome)?;

        let reported_outcome = match self.sessions.get(child_id).map(|child| &child.status) {
            Some(SessionStatus::Interrupted { .. }) => "interrupted",
            _ => outcome,
        };
        if let Some(parent_id) = parent_id {
            if let Some(parent) = self.sessions.get_mut(&parent_id) {
                parent
                    .log
                    .append_child_finished(ts, child_id, reported_outcome)?;
            }
        }
        if let Some(child) = self.sessions.get_mut(child_id) {
            child.finished_reported = true;
        }
        Ok(())
    }

    /// 中断传播（第一段）：给每个活跃后代写 `session.interrupted_by_parent`，
    /// 发起方日志写 `session.interrupted`，返回按层序排列的 interrupt 计划。
    ///
    /// 调用方对 `InterruptAction::Interrupt` 的目标请求停止（cancel token）；超时后
    /// 调用 [`SessionTreeRuntime::escalate_interrupt`]。
    pub fn interrupt_tree(
        &mut self,
        session_id: &str,
        reason: &str,
        ts: &str,
    ) -> Result<InterruptPlan, SpawnError> {
        if !self.sessions.contains_key(session_id) {
            return Err(SpawnError::UnknownSession(session_id.to_owned()));
        }
        let descendants: Vec<(String, u32)> = self
            .descendants(session_id)
            .into_iter()
            .filter(|session| session.status.is_active())
            .map(|session| (session.id.clone(), session.depth))
            .collect();

        // 发起方（父/根）自己记录"我发起了中断"。
        self.sessions
            .get_mut(session_id)
            .expect("session 已确认存在")
            .log
            .append_interrupted(ts, reason)?;

        let mut targets = Vec::new();
        for (child_id, depth) in descendants {
            let child = self
                .sessions
                .get_mut(&child_id)
                .expect("descendants 来自同一张表");
            let parent_id = child
                .parent_session_id
                .clone()
                .unwrap_or_else(|| session_id.to_owned());
            match &child.status {
                SessionStatus::Active => {
                    child
                        .log
                        .append_interrupted_by_parent(ts, &parent_id, reason)?;
                    child.status = SessionStatus::Interrupting {
                        by_parent: true,
                        reason: reason.to_owned(),
                    };
                    targets.push(InterruptTarget {
                        session_id: child_id,
                        depth,
                        action: InterruptAction::Interrupt,
                    });
                }
                SessionStatus::Interrupting { .. } => {
                    // 已在中断中：仍把目标交给调用方，避免它漏发 cancel。
                    targets.push(InterruptTarget {
                        session_id: child_id,
                        depth,
                        action: InterruptAction::Interrupt,
                    });
                }
                _ => {}
            }
        }

        Ok(InterruptPlan {
            root: session_id.to_owned(),
            reason: reason.to_owned(),
            targets,
        })
    }

    /// 中断超时升级（第二段）：仍处于 `Interrupting` 的后代按 kill 语义结束，
    /// 写 `session.killed`，并推进 drain。返回真正被 kill 的目标。
    pub fn escalate_interrupt(
        &mut self,
        plan: &InterruptPlan,
        ts: &str,
    ) -> Result<Vec<InterruptTarget>, SpawnError> {
        let mut kills = Vec::new();
        for target in &plan.targets {
            let is_interrupting = self
                .sessions
                .get(&target.session_id)
                .map(|session| matches!(session.status, SessionStatus::Interrupting { .. }))
                .unwrap_or(false);
            if !is_interrupting {
                continue;
            }
            {
                let session = self
                    .sessions
                    .get_mut(&target.session_id)
                    .expect("is_interrupting 已确认存在");
                let (by_parent, reason) = match &session.status {
                    SessionStatus::Interrupting { by_parent, reason } => {
                        (*by_parent, reason.clone())
                    }
                    _ => unreachable!("上面已确认是 Interrupting"),
                };
                session.status = SessionStatus::Interrupted {
                    by_parent,
                    reason,
                    killed: true,
                };
                session.log.append_killed(ts, "interrupt_timeout")?;
            }
            self.settle_drains(&target.session_id);
            kills.push(InterruptTarget {
                session_id: target.session_id.clone(),
                depth: target.depth,
                action: InterruptAction::Kill,
            });
        }
        Ok(kills)
    }

    /// 开始 drain 等待。`DirectChildren` 只等直接子级，`AllDescendants` 等全部后代。
    pub fn begin_drain(
        &mut self,
        parent_id: &str,
        mode: DrainMode,
    ) -> Result<DrainWait, SpawnError> {
        if !self.sessions.contains_key(parent_id) {
            return Err(SpawnError::UnknownSession(parent_id.to_owned()));
        }
        let scope: Vec<String> = match mode {
            DrainMode::DirectChildren => self
                .children(parent_id)
                .into_iter()
                .filter(|session| session.status.is_active())
                .map(|session| session.id.clone())
                .collect(),
            DrainMode::AllDescendants => self
                .descendants(parent_id)
                .into_iter()
                .filter(|session| session.status.is_active())
                .map(|session| session.id.clone())
                .collect(),
        };
        let wait = DrainWait {
            parent_session_id: parent_id.to_owned(),
            mode,
            pending: scope.iter().cloned().collect(),
            scope,
        };
        if wait.is_satisfied() {
            self.drains.remove(parent_id);
        } else {
            self.drains.insert(parent_id.to_owned(), wait.clone());
        }
        Ok(wait)
    }

    /// 轮询 drain：仍在等待返回 `Some`，已满足返回 `None` 并清除等待标记。
    pub fn poll_drain(&mut self, parent_id: &str) -> Option<DrainWait> {
        match self.drains.get(parent_id) {
            Some(wait) if !wait.is_satisfied() => Some(wait.clone()),
            _ => {
                self.drains.remove(parent_id);
                None
            }
        }
    }

    /// 主动取消 drain（例如用户停止等待）。
    pub fn cancel_drain(&mut self, parent_id: &str) -> bool {
        self.drains.remove(parent_id).is_some()
    }

    /// 父会话是否仍处于 active（含"正在 drain 等待子会话"）。
    pub fn is_active(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .map(|session| session.status.is_active())
            .unwrap_or(false)
            || self.drains.contains_key(session_id)
    }

    /// 是否可被判定为闲置。等待子会话（drain）期间 MUST 为 false。
    pub fn is_idle(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id) && !self.is_active(session_id)
    }

    pub fn children(&self, parent_id: &str) -> Vec<&TreeSession> {
        self.sessions
            .values()
            .filter(|session| session.parent_session_id.as_deref() == Some(parent_id))
            .collect()
    }

    /// 全部后代（BFS，父在子前，不含自己）。
    pub fn descendants(&self, session_id: &str) -> Vec<&TreeSession> {
        let mut result = Vec::new();
        let mut queue: VecDeque<&str> = self
            .children(session_id)
            .into_iter()
            .map(|session| session.id.as_str())
            .collect();
        while let Some(current) = queue.pop_front() {
            let Some(session) = self.sessions.get(current) else {
                continue;
            };
            result.push(session);
            let child_ids: Vec<&str> = self
                .children(current)
                .into_iter()
                .map(|child| child.id.as_str())
                .collect();
            queue.extend(child_ids);
        }
        result
    }

    pub fn depth(&self, session_id: &str) -> Option<u32> {
        self.sessions.get(session_id).map(|session| session.depth)
    }

    /// 把运行时的会话日志同步给调用方持有的 `SessionLog`（chat.rs 同步集成用）。
    ///
    /// 宿主若仍以 `ChatState.log` 为写入主体，可在 `spawn_child` / `interrupt_tree` /
    /// `finish_child` 之后调用一次，避免两份日志分叉。返回 false 表示会话不存在。
    pub fn sync_log(&self, session_id: &str, target: &mut SessionLog) -> bool {
        match self.sessions.get(session_id) {
            Some(session) => {
                *target = session.log.clone();
                true
            }
            None => false,
        }
    }

    /// 运行态 → 静态树（复用 `session-store` 的查询/校验）。
    pub fn to_tree(&self) -> Result<SessionTree, SpawnError> {
        let nodes = self.sessions.values().map(|session| SessionNode {
            id: session.id.clone(),
            parent_session_id: session.parent_session_id.clone(),
            spawn_source: session.spawn_source,
            title: session.prompt.clone().unwrap_or_else(|| session.id.clone()),
            updated_at: 0,
        });
        SessionTree::from_nodes(nodes).map_err(|err| SpawnError::Tree(err.to_string()))
    }

    /// 从 Event Log 重建运行时树（重启恢复/回放）。
    ///
    /// - 父链接/来源来自每个日志的 `session.created`；
    /// - 状态来自 `session.interrupted_by_parent` / `session.killed` / 父日志的
    ///   `child.finished`；
    /// - 策略只能重建工作区（收紧信息 v1 尚未持久化，见 docs/m2-session-tree.md）。
    pub fn from_logs(
        logs: impl IntoIterator<Item = SessionLog>,
        max_depth: u32,
    ) -> Result<Self, SpawnError> {
        let logs: Vec<SessionLog> = logs.into_iter().collect();

        let mut creations: BTreeMap<String, Option<SessionCreated>> = BTreeMap::new();
        let mut finished_outcomes: BTreeMap<String, String> = BTreeMap::new();
        let mut prompts: BTreeMap<String, String> = BTreeMap::new();
        let mut interruptions: BTreeMap<String, (bool, bool, String)> = BTreeMap::new();

        for log in &logs {
            let session_id = log.session_id().to_owned();
            creations.insert(session_id.clone(), log.created().cloned());
            for event in log.events() {
                match event.event_type.as_str() {
                    event_type::CHILD_FINISHED => {
                        if let (Some(child_id), Some(outcome)) = (
                            payload_str(&event.payload, "child_session_id"),
                            payload_str(&event.payload, "outcome"),
                        ) {
                            finished_outcomes.insert(child_id.to_owned(), outcome.to_owned());
                        }
                    }
                    event_type::CHILD_SPAWNED => {
                        if let (Some(child_id), Some(prompt)) = (
                            payload_str(&event.payload, "child_session_id"),
                            payload_str(&event.payload, "prompt"),
                        ) {
                            prompts.insert(child_id.to_owned(), prompt.to_owned());
                        }
                    }
                    event_type::SESSION_INTERRUPTED_BY_PARENT => {
                        interruptions.insert(
                            session_id.clone(),
                            (
                                true,
                                false,
                                payload_str(&event.payload, "reason")
                                    .unwrap_or_default()
                                    .to_owned(),
                            ),
                        );
                    }
                    event_type::SESSION_INTERRUPTED => {
                        interruptions.insert(
                            session_id.clone(),
                            (
                                payload_bool(&event.payload, "by_parent"),
                                false,
                                payload_str(&event.payload, "reason")
                                    .unwrap_or_default()
                                    .to_owned(),
                            ),
                        );
                    }
                    event_type::SESSION_KILLED => {
                        let entry = interruptions.entry(session_id.clone()).or_insert((
                            true,
                            false,
                            String::new(),
                        ));
                        entry.1 = true;
                    }
                    _ => {}
                }
            }
        }

        // 先用 session-store 的纯树做无环/深度计算（深度是查询结果，不参与校验）。
        let nodes = logs.iter().map(|log| {
            let session_id = log.session_id().to_owned();
            let created = creations.get(&session_id).cloned().flatten();
            SessionNode {
                id: session_id.clone(),
                parent_session_id: created
                    .as_ref()
                    .and_then(|created| created.parent_session_id.clone()),
                spawn_source: created.as_ref().and_then(|created| created.spawn_source),
                title: session_id,
                updated_at: 0,
            }
        });
        let tree =
            SessionTree::from_nodes(nodes).map_err(|err| SpawnError::Tree(err.to_string()))?;

        let mut runtime = Self::new(max_depth);
        for log in logs {
            let session_id = log.session_id().to_owned();
            let created = creations.get(&session_id).cloned().flatten();
            let depth = tree.depth(&session_id).unwrap_or(0);
            let workspace_id = created
                .as_ref()
                .map(|created| created.workspace_id.clone())
                .unwrap_or_default();
            let parent_session_id = created
                .as_ref()
                .and_then(|created| created.parent_session_id.clone());
            let spawn_source = created.as_ref().and_then(|created| created.spawn_source);
            let preset = created.as_ref().and_then(|created| created.preset.clone());
            let status = match interruptions.get(&session_id) {
                Some((by_parent, killed, reason)) => SessionStatus::Interrupted {
                    by_parent: *by_parent,
                    reason: reason.clone(),
                    killed: *killed,
                },
                None => match finished_outcomes.get(&session_id) {
                    Some(outcome) => SessionStatus::Finished {
                        outcome: outcome.clone(),
                    },
                    None => SessionStatus::Active,
                },
            };
            runtime.sessions.insert(
                session_id.clone(),
                TreeSession {
                    id: session_id.clone(),
                    parent_session_id,
                    depth,
                    spawn_source,
                    status,
                    policy: SessionPolicy::root(workspace_id).with_max_depth(max_depth),
                    log,
                    prompt: prompts.get(&session_id).cloned(),
                    preset,
                    finished_reported: false,
                },
            );
        }
        Ok(runtime)
    }

    /// 从 drain 等待集合里移除已结束的会话；满足的 drain 立即清除。
    fn settle_drains(&mut self, session_id: &str) {
        let mut satisfied = Vec::new();
        for (parent_id, wait) in self.drains.iter_mut() {
            wait.pending.remove(session_id);
            if wait.is_satisfied() {
                satisfied.push(parent_id.clone());
            }
        }
        for parent_id in satisfied {
            self.drains.remove(&parent_id);
        }
    }
}

fn payload_str<'a>(payload: &'a Value, field: &str) -> Option<&'a str> {
    payload.get(field).and_then(Value::as_str)
}

fn payload_bool(payload: &Value, field: &str) -> bool {
    payload.get(field).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> &'static str {
        "2026-11-11T10:00:00.000Z"
    }

    fn runtime() -> SessionTreeRuntime {
        let mut runtime = SessionTreeRuntime::new(DEFAULT_MAX_DEPTH);
        runtime.register_root("s-root", "ws-1").unwrap();
        runtime
    }

    fn args(prompt: &str) -> SpawnChildArgs {
        SpawnChildArgs {
            prompt: prompt.to_owned(),
            preset: None,
        }
    }

    fn has_event(log: &SessionLog, event_type: &str) -> bool {
        log.events()
            .iter()
            .any(|event| event.event_type == event_type)
    }

    #[test]
    fn tool_spec_exposes_only_prompt_and_preset() {
        let spec = spawn_child_spec();
        assert_eq!(spec.name, SPAWN_CHILD_TOOL);
        assert!(spec.description.contains("子会话"));
        assert_eq!(spec.parameters["required"], serde_json::json!(["prompt"]));
        assert_eq!(spec.parameters["properties"]["prompt"]["type"], "string");
        assert_eq!(spec.parameters["properties"]["preset"]["type"], "string");
        assert_eq!(spec.parameters["additionalProperties"], false);
        assert!(is_spawn_child_tool("spawn_child"));
        assert!(!is_spawn_child_tool("spawn_agent"));
    }

    #[test]
    fn spawn_arguments_are_validated() {
        assert_eq!(
            SpawnChildArgs::from_json(&serde_json::json!({})).unwrap_err(),
            SpawnError::MissingPrompt
        );
        assert_eq!(
            SpawnChildArgs::from_json(&serde_json::json!({ "prompt": "   " })).unwrap_err(),
            SpawnError::MissingPrompt
        );

        let parsed = SpawnChildArgs::from_json(&serde_json::json!({
            "prompt": "  修一下窗口  ",
            "preset": "reviewer"
        }))
        .unwrap();
        assert_eq!(parsed.prompt, "修一下窗口");
        assert_eq!(parsed.preset.as_deref(), Some("reviewer"));

        let call = ToolCallRequest {
            call_id: "call-1".to_owned(),
            tool: "spawn_child".to_owned(),
            arguments: serde_json::json!({ "prompt": "hi" }),
        };
        assert_eq!(SpawnChildArgs::from_call(&call).unwrap().prompt, "hi");

        let wrong = ToolCallRequest {
            tool: "probe_native".to_owned(),
            ..call
        };
        assert_eq!(
            SpawnChildArgs::from_call(&wrong).unwrap_err(),
            SpawnError::WrongTool("probe_native".to_owned())
        );
    }

    #[test]
    fn spawn_creates_independent_log_with_parent_link() {
        let mut runtime = runtime();
        let child = runtime
            .spawn_child(
                "s-child",
                "s-root",
                &args("检查 PLAN §6.5"),
                &SpawnContext::at(ts()).with_parent_message("m-1"),
            )
            .unwrap();

        assert_eq!(child.session_id, "s-child");
        assert_eq!(child.parent_session_id, "s-root");
        assert_eq!(child.depth, 1);
        assert_eq!(child.spawn_source, SpawnSource::Agent);
        assert_eq!(child.log.session_id(), "s-child");
        assert_eq!(child.log.parent_session_id(), Some("s-root"));
        assert_eq!(child.log.spawn_source(), Some(SpawnSource::Agent));
        assert_eq!(
            child.log.created().unwrap().workspace_id,
            "ws-1",
            "子会话继承父工作区"
        );
        assert!(has_event(&child.log, event_type::SESSION_CREATED));
        assert_eq!(
            child.log.created().unwrap().preset.as_deref(),
            None,
            "父没有 preset 时子也不编造"
        );

        let parent = runtime.session("s-root").unwrap();
        assert_eq!(parent.log.len(), 1, "父日志只有 child.spawned");
        let spawned = &parent.log.events()[0];
        assert_eq!(spawned.event_type, event_type::CHILD_SPAWNED);
        assert_eq!(spawned.payload["child_session_id"], "s-child");
        assert_eq!(spawned.payload["parent_message_id"], "m-1");
        assert_eq!(runtime.depth("s-child"), Some(1));
    }

    #[test]
    fn spawn_inherits_parent_preset_and_accepts_override() {
        let mut runtime = SessionTreeRuntime::new(DEFAULT_MAX_DEPTH);
        let mut root_log = SessionLog::new("s-root");
        root_log
            .append_created(
                ts(),
                &SessionCreated::root("ws-1").with_preset("parent-preset"),
            )
            .unwrap();
        runtime
            .register_root_with_log("s-root", "ws-1", root_log)
            .unwrap();

        let inherited = runtime
            .spawn_child("s-child-a", "s-root", &args("a"), &SpawnContext::at(ts()))
            .unwrap();
        assert_eq!(inherited.preset.as_deref(), Some("parent-preset"));

        let mut override_args = args("b");
        override_args.preset = Some("reviewer".to_owned());
        let overridden = runtime
            .spawn_child(
                "s-child-b",
                "s-root",
                &override_args,
                &SpawnContext::at(ts()),
            )
            .unwrap();
        assert_eq!(overridden.preset.as_deref(), Some("reviewer"));
    }

    #[test]
    fn default_depth_limit_is_two_but_configurable() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap();
        let err = runtime
            .spawn_child("s-great", "s-grand", &args("3"), &SpawnContext::at(ts()))
            .unwrap_err();
        assert_eq!(
            err,
            SpawnError::DepthExceeded {
                session_id: "s-great".to_owned(),
                depth: 3,
                limit: 2,
            }
        );

        let mut shallow = SessionTreeRuntime::new(1);
        shallow.register_root("s-root", "ws-1").unwrap();
        // max_depth = 1：子会话（深度 1）允许，孙会话（深度 2）拒绝。
        shallow
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        let err = shallow
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap_err();
        assert!(matches!(err, SpawnError::DepthExceeded { limit: 1, .. }));

        // 子策略可以收紧深度：孙会话被拒绝，但兄弟子会话仍可派生。
        let mut tightened = SessionTreeRuntime::new(DEFAULT_MAX_DEPTH);
        tightened.register_root("s-root", "ws-1").unwrap();
        let context = SpawnContext::at(ts()).tighten(PolicyTightening {
            max_depth: Some(1),
            ..PolicyTightening::default()
        });
        tightened
            .spawn_child("s-child", "s-root", &args("1"), &context)
            .unwrap();
        let err = tightened
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap_err();
        assert!(matches!(err, SpawnError::DepthExceeded { limit: 1, .. }));
    }

    #[test]
    fn policy_inherits_and_can_only_tighten() {
        let parent = SessionPolicy::root("ws-1")
            .with_max_depth(2)
            .with_allowed_tools(["read", "bash"])
            .with_denied_tools(["delete"]);

        let inherited = parent.descend(&PolicyTightening::default()).unwrap();
        assert_eq!(inherited, parent);

        // 白名单取子集：允许。
        let narrowed = parent
            .descend(&PolicyTightening {
                allowed_tools: Some(["read".to_owned()].into_iter().collect()),
                ..PolicyTightening::default()
            })
            .unwrap();
        assert!(narrowed.allows_tool("read"));
        assert!(!narrowed.allows_tool("bash"));

        // 白名单新增父没有的工具：拒绝（放宽）。
        let widening = parent.descend(&PolicyTightening {
            allowed_tools: Some(["read", "write"].map(str::to_owned).into_iter().collect()),
            ..PolicyTightening::default()
        });
        assert!(matches!(widening, Err(SpawnError::PolicyWidening(_))));

        // 深度变大：拒绝。
        assert!(matches!(
            parent.descend(&PolicyTightening {
                max_depth: Some(3),
                ..PolicyTightening::default()
            }),
            Err(SpawnError::PolicyWidening(_))
        ));

        // 黑名单只增：子策略追加一个，不能再移除父的。
        let mut denied = BTreeSet::new();
        denied.insert("rm".to_owned());
        let tightened = parent
            .descend(&PolicyTightening {
                denied_tools: denied,
                ..PolicyTightening::default()
            })
            .unwrap();
        assert!(!tightened.allows_tool("delete"));
        assert!(!tightened.allows_tool("rm"));
        assert_eq!(tightened.workspace_id, "ws-1");
    }

    #[test]
    fn spawn_applies_policy_tightening_and_inherits_workspace() {
        let mut runtime = SessionTreeRuntime::new(DEFAULT_MAX_DEPTH);
        let mut root_log = SessionLog::new("s-root");
        root_log
            .append_created(ts(), &SessionCreated::root("ws-1"))
            .unwrap();
        runtime
            .register_root_with_log("s-root", "ws-1", root_log)
            .unwrap();

        let context = SpawnContext::at(ts()).tighten(PolicyTightening {
            allowed_tools: Some(["read".to_owned()].into_iter().collect()),
            ..PolicyTightening::default()
        });
        let child = runtime
            .spawn_child("s-child", "s-root", &args("只读任务"), &context)
            .unwrap();
        assert_eq!(child.policy.workspace_id, "ws-1");
        assert!(child.policy.allows_tool("read"));
        assert!(!child.policy.allows_tool("write"));
        assert_eq!(
            child.log.created().unwrap().workspace_id,
            "ws-1",
            "工作区随 session.created 持久化"
        );
    }

    #[test]
    fn finished_or_unknown_parents_cannot_spawn() {
        let mut runtime = runtime();
        assert_eq!(
            runtime
                .spawn_child("c", "s-missing", &args("x"), &SpawnContext::at(ts()))
                .unwrap_err(),
            SpawnError::UnknownSession("s-missing".to_owned())
        );

        runtime
            .spawn_child("s-child", "s-root", &args("x"), &SpawnContext::at(ts()))
            .unwrap();
        runtime.finish_child("s-child", "completed", ts()).unwrap();
        let err = runtime
            .spawn_child("s-grand", "s-child", &args("y"), &SpawnContext::at(ts()))
            .unwrap_err();
        assert!(matches!(err, SpawnError::ParentNotActive(_, _)));

        // 重复 id 不能被覆盖。
        let err = runtime
            .spawn_child("s-child", "s-root", &args("z"), &SpawnContext::at(ts()))
            .unwrap_err();
        assert_eq!(err, SpawnError::DuplicateSession("s-child".to_owned()));
    }

    #[test]
    fn finish_child_writes_parent_event_once() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("x"), &SpawnContext::at(ts()))
            .unwrap();
        runtime.finish_child("s-child", "completed", ts()).unwrap();
        runtime.finish_child("s-child", "completed", ts()).unwrap();

        assert_eq!(
            runtime.status("s-child"),
            Some(&SessionStatus::Finished {
                outcome: "completed".to_owned()
            })
        );
        let events: Vec<&str> = runtime
            .session("s-root")
            .unwrap()
            .log
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        assert_eq!(
            events,
            vec![event_type::CHILD_SPAWNED, event_type::CHILD_FINISHED]
        );
        assert!(runtime.session("s-child").unwrap().finished_reported());
    }

    #[test]
    fn interrupt_propagates_to_all_active_descendants_before_kill() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap();

        let plan = runtime
            .interrupt_tree("s-root", "parent_interrupt", ts())
            .unwrap();
        assert_eq!(plan.ids(), vec!["s-child", "s-grand"]);
        assert!(plan
            .targets
            .iter()
            .all(|target| target.action == InterruptAction::Interrupt));
        assert_eq!(
            runtime.status("s-child"),
            Some(&SessionStatus::Interrupting {
                by_parent: true,
                reason: "parent_interrupt".to_owned(),
            })
        );

        // 发起方与每个子会话都要落中断事件。
        assert!(has_event(
            &runtime.session("s-root").unwrap().log,
            event_type::SESSION_INTERRUPTED
        ));
        for child_id in ["s-child", "s-grand"] {
            let log = &runtime.session(child_id).unwrap().log;
            assert!(
                has_event(log, event_type::SESSION_INTERRUPTED_BY_PARENT),
                "{child_id} 必须记录 interrupted_by_parent"
            );
            assert!(log.interrupted_by_parent());
            assert!(log.replay().interrupted_by_parent());
        }
        let interrupted = runtime
            .session("s-child")
            .unwrap()
            .log
            .events()
            .iter()
            .find(|event| event.event_type == event_type::SESSION_INTERRUPTED_BY_PARENT)
            .expect("s-child 必须记录 interrupted_by_parent");
        assert_eq!(interrupted.payload["parent_session_id"], "s-root");
    }

    #[test]
    fn interrupt_escalates_to_kill_after_timeout() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        let plan = runtime.interrupt_tree("s-root", "user_stop", ts()).unwrap();

        let kills = runtime.escalate_interrupt(&plan, ts()).unwrap();
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].action, InterruptAction::Kill);
        assert_eq!(
            runtime.status("s-child"),
            Some(&SessionStatus::Interrupted {
                by_parent: true,
                reason: "user_stop".to_owned(),
                killed: true,
            })
        );
        let log = &runtime.session("s-child").unwrap().log;
        assert!(has_event(log, event_type::SESSION_KILLED));
        assert!(log.was_killed());

        // 已经 kill 过的目标不会被重复升级。
        assert!(runtime.escalate_interrupt(&plan, ts()).unwrap().is_empty());
    }

    #[test]
    fn drain_direct_children_only() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child-a", "s-root", &args("a"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-child-b", "s-root", &args("b"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-grand", "s-child-a", &args("g"), &SpawnContext::at(ts()))
            .unwrap();

        let wait = runtime
            .begin_drain("s-root", DrainMode::DirectChildren)
            .unwrap();
        assert_eq!(wait.pending(), vec!["s-child-a", "s-child-b"]);
        assert_eq!(runtime.poll_drain("s-root"), Some(wait));

        runtime
            .finish_child("s-child-a", "completed", ts())
            .unwrap();
        let wait = runtime.poll_drain("s-root").expect("还在等 child-b");
        assert_eq!(wait.pending(), vec!["s-child-b"]);

        runtime
            .finish_child("s-child-b", "completed", ts())
            .unwrap();
        assert_eq!(
            runtime.poll_drain("s-root"),
            None,
            "直接子级都结束即可返回（孙会话仍在跑）"
        );
        assert!(runtime.session("s-grand").unwrap().is_active());
    }

    #[test]
    fn drain_all_descendants_waits_for_grandchildren() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap();

        let wait = runtime
            .begin_drain("s-root", DrainMode::AllDescendants)
            .unwrap();
        assert_eq!(wait.scope, vec!["s-child", "s-grand"]);
        runtime.finish_child("s-child", "completed", ts()).unwrap();
        let wait = runtime.poll_drain("s-root").expect("还在等孙会话");
        assert_eq!(wait.pending(), vec!["s-grand"]);
        runtime.finish_child("s-grand", "completed", ts()).unwrap();
        assert_eq!(runtime.poll_drain("s-root"), None);
    }

    #[test]
    fn parent_stays_active_while_draining() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .begin_drain("s-root", DrainMode::DirectChildren)
            .unwrap();

        // 父的一轮已经结束，但还在等子会话：不能被判闲置。
        runtime.mark_finished("s-root", "completed").unwrap();
        assert!(!runtime.is_idle("s-root"));
        assert!(runtime.is_active("s-root"));

        runtime.finish_child("s-child", "completed", ts()).unwrap();
        assert!(runtime.is_idle("s-root"));
    }

    #[test]
    fn from_logs_rebuilds_tree_links_depths_and_statuses() {
        let mut original = runtime();
        original
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        original
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap();
        original
            .interrupt_tree("s-root", "user_stop", ts())
            .unwrap();

        // 只把三个会话的日志交给恢复逻辑（模拟重启后从磁盘读取）。
        let logs: Vec<SessionLog> = ["s-root", "s-child", "s-grand"]
            .into_iter()
            .map(|id| original.session(id).unwrap().log.clone())
            .collect();
        let restored =
            SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH).expect("restore tree");

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.depth("s-root"), Some(0));
        assert_eq!(restored.depth("s-child"), Some(1));
        assert_eq!(restored.depth("s-grand"), Some(2));
        assert_eq!(
            restored
                .session("s-child")
                .unwrap()
                .parent_session_id
                .as_deref(),
            Some("s-root")
        );
        assert_eq!(
            restored.session("s-grand").unwrap().spawn_source,
            Some(SpawnSource::Agent)
        );
        assert_eq!(
            restored.session("s-grand").unwrap().prompt.as_deref(),
            Some("2"),
            "prompt 来自父日志的 child.spawned"
        );
        assert!(matches!(
            restored.status("s-child"),
            Some(SessionStatus::Interrupted {
                by_parent: true,
                ..
            })
        ));
        assert!(matches!(
            restored.status("s-grand"),
            Some(SessionStatus::Interrupted {
                by_parent: true,
                ..
            })
        ));
        // 发起中断的根会话自身也记录了 session.interrupted（by_parent: false）。
        assert_eq!(
            restored.status("s-root"),
            Some(&SessionStatus::Interrupted {
                by_parent: false,
                reason: "user_stop".to_owned(),
                killed: false,
            })
        );

        // 重建后的树还能继续校验深度。
        let tree = restored.to_tree().unwrap();
        assert_eq!(tree.max_depth(), 2);
        let err = tree.validate_max_depth(1).unwrap_err();
        assert!(err.to_string().contains("深度"), "{err}");
    }

    #[test]
    fn to_tree_reuses_session_store_queries() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .spawn_child("s-grand", "s-child", &args("2"), &SpawnContext::at(ts()))
            .unwrap();
        let tree = runtime.to_tree().unwrap();
        let ids: Vec<&str> = tree
            .flatten()
            .into_iter()
            .map(|(_, node)| node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["s-root", "s-child", "s-grand"]);
        assert_eq!(tree.depth("s-grand"), Some(2));
    }

    #[test]
    fn cancelled_drain_does_not_keep_parent_active() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();
        runtime
            .begin_drain("s-root", DrainMode::AllDescendants)
            .unwrap();
        assert!(runtime.cancel_drain("s-root"));
        assert!(!runtime.is_idle("s-root"), "根会话自己的轮次还没结束");
        runtime.mark_finished("s-root", "cancelled").unwrap();
        assert!(runtime.is_idle("s-root"));
    }

    #[test]
    fn register_root_with_wrong_log_is_rejected() {
        let mut runtime = SessionTreeRuntime::new(DEFAULT_MAX_DEPTH);
        let err = runtime
            .register_root_with_log("s-root", "ws-1", SessionLog::new("s-other"))
            .unwrap_err();
        assert_eq!(
            err,
            SpawnError::LogSessionMismatch {
                expected: "s-root".to_owned(),
                actual: "s-other".to_owned(),
            }
        );
    }

    #[test]
    fn sync_log_hands_parent_events_back_to_the_host() {
        let mut runtime = runtime();
        runtime
            .spawn_child("s-child", "s-root", &args("1"), &SpawnContext::at(ts()))
            .unwrap();

        // 宿主仍持有旧日志：同步后必须能看到 child.spawned。
        let mut host_log = SessionLog::new("s-root");
        assert!(runtime.sync_log("s-root", &mut host_log));
        assert_eq!(host_log.len(), 1);
        assert_eq!(host_log.events()[0].event_type, event_type::CHILD_SPAWNED);

        let mut missing = SessionLog::new("s-root");
        assert!(!runtime.sync_log("s-missing", &mut missing));
        assert_eq!(missing.len(), 0);
    }
}
