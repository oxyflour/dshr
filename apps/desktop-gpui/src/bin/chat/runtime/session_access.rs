use dshr_agent_context::SpillStore;
use dshr_agent_core::DEFAULT_MAX_DEPTH;
use dshr_compat_dsh_host::{
    EventPage, ServiceError, SessionAccess, SessionEventView, SessionHeaderView,
};
use dshr_session_store::SessionLog;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{dsh_deliverables, dsh_goals, dsh_subagents, dsh_user_questions, now_ts, spill_dir_in};

// ---------------------------------------------------------------------------
// L2 Host Service 的会话读写面（spec/sidecar-rpc.md §13，M3 G3）
// ---------------------------------------------------------------------------

/// `chat` 对 `SessionAccess` 的实现。
///
/// 与 service 层是**同一线程**的同步调用：`ChatExecutor` 在调 sidecar 工具前
/// `publish` 当前内存 `SessionLog` 快照；`working-directory`/`session-log` 的读走快照，
/// 服务的 `append_event` 进队列，工具返回后由 `ChatExecutor` 补写回内存日志。
/// 其它会话的读面落到磁盘（`sessions_dir/<id>.jsonl`），因为插件只在当前会话里执行。
///
/// seq 约定：快照尾部 + 1 递增；publish 与 drain 之间没有其它日志写入（同步单线程），
/// 所以队列事件的 seq 与后续真实日志 seq 一致。
pub(crate) struct ChatSessionAccess {
    pub(crate) inner: Mutex<ChatSessionAccessInner>,
    pub(crate) questions: Arc<dsh_user_questions::QuestionBroker>,
    pub(crate) goals: dsh_goals::GoalActivation,
    pub(crate) children: Arc<dsh_subagents::ChildSupervisor>,
}

pub(crate) struct ChatSessionAccessInner {
    pub(crate) sessions_dir: PathBuf,
    pub(crate) published: Option<PublishedSession>,
    pending: Vec<PendingSessionEvent>,
    pub(crate) child_environment: Option<dsh_subagents::ChildEnvironment>,
}

#[derive(Clone)]
pub(crate) struct PublishedSession {
    pub(crate) header: SessionHeaderView,
    pub(crate) events: Vec<SessionEventView>,
    pub(crate) event_times: Vec<String>,
    pub(crate) next_seq: u64,
    image_input: bool,
    pub(crate) cancel: Arc<AtomicBool>,
}

struct PendingSessionEvent {
    session_id: String,
    event_type: String,
    data: serde_json::Value,
}

impl ChatSessionAccess {
    pub fn new(sessions_dir: PathBuf) -> Arc<Self> {
        Self::with_children(
            sessions_dir,
            Arc::new(dsh_subagents::ChildSupervisor::default()),
        )
    }

    pub(crate) fn with_children(
        sessions_dir: PathBuf,
        children: Arc<dsh_subagents::ChildSupervisor>,
    ) -> Arc<Self> {
        Arc::new(Self {
            questions: Arc::new(dsh_user_questions::QuestionBroker::default()),
            goals: dsh_goals::GoalActivation::default(),
            children,
            inner: Mutex::new(ChatSessionAccessInner {
                sessions_dir,
                published: None,
                pending: Vec::new(),
                child_environment: None,
            }),
        })
    }

    /// 用当前内存日志刷新服务层可见的快照（在每次 sidecar 工具调用前调用）。
    pub fn publish(&self, log: &SessionLog, image_input: bool, cancel: Arc<AtomicBool>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let header = chat_header_from_log(log, &inner.sessions_dir);
        let events: Vec<SessionEventView> = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        let next_seq = events.last().map(|event| event.seq + 1).unwrap_or(1);
        let event_times = log.events().iter().map(|event| event.ts.clone()).collect();
        inner.published = Some(PublishedSession {
            header,
            events,
            event_times,
            next_seq,
            image_input,
            cancel,
        });
    }

    pub(crate) fn spill_store(&self) -> SpillStore {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SpillStore::new(spill_dir_in(&inner.sessions_dir))
    }

    #[expect(
        clippy::result_large_err,
        reason = "The image service uses the host's structured service error."
    )]
    pub(crate) fn image_input(&self, session: &str) -> Result<bool, ServiceError> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let published = inner
            .published
            .as_ref()
            .filter(|published| published.header.id == session)
            .ok_or_else(|| ServiceError::new("E_SERVICE_DENIED", "image session is not active"))?;
        if published.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new(
                "E_CANCELLED",
                "image tool call was cancelled",
            ));
        }
        Ok(published.image_input)
    }

    pub(crate) fn has_image(&self, session: &str, id: &str) -> bool {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.published.as_ref().is_some_and(|published| {
            published.header.id == session
                && published.events.iter().any(|event| {
                    event.event_type == dshr_agent_context::spill::EVENT_SPILL_CREATED
                        && event.data["kind"] == "tool.image"
                        && event.data["id"] == id
                })
        })
    }

    /// 取走服务层为 `session_id` 追加的事件（按序写回内存日志）。
    pub fn take_appends(&self, session_id: &str) -> Vec<(String, serde_json::Value)> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut taken = Vec::new();
        let mut keep = Vec::with_capacity(inner.pending.len());
        for pending in inner.pending.drain(..) {
            if pending.session_id == session_id {
                taken.push((pending.event_type, pending.data));
            } else {
                keep.push(pending);
            }
        }
        inner.pending = keep;
        taken
    }

    pub(crate) fn clear_snapshot(&self) {
        self.goals.disarm();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.published = None;
        inner.pending.clear();
    }

    // ServiceError 携带结构化 detail（与协议层一致的大 Err），这里按 host crate 的
    // 同样取舍保留原类型，不为了 clippy 的尺寸阈值再包一层 Box。
    #[allow(clippy::result_large_err)]
    fn disk_read(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeaderView, Vec<SessionEventView>), ServiceError> {
        let sessions_dir = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.sessions_dir.clone()
        };
        let log = load_session_log(&sessions_dir, session_id)?;
        let header = chat_header_from_log(&log, &sessions_dir);
        let events = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        Ok((header, events))
    }
}

impl SessionAccess for ChatSessionAccess {
    fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError> {
        {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    return Ok(published.header.clone());
                }
            }
        }
        self.disk_read(session_id).map(|(header, _)| header)
    }

    fn read_events(
        &self,
        session_id: &str,
        from_seq: u64,
        max_events: usize,
    ) -> Result<EventPage, ServiceError> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    Some(published.events.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id)?.1,
        };
        let start = events
            .iter()
            .position(|event| event.seq >= from_seq)
            .unwrap_or(events.len());
        let page: Vec<SessionEventView> =
            events[start..].iter().take(max_events).cloned().collect();
        let eof = start + page.len() >= events.len();
        let next_seq = page.last().map(|event| event.seq + 1).unwrap_or(from_seq);
        Ok(EventPage {
            events: page,
            next_seq,
            eof,
        })
    }

    fn append_event(
        &self,
        session_id: &str,
        event_type: &str,
        mut data: serde_json::Value,
    ) -> Result<u64, ServiceError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(published) = inner.published.as_mut() else {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "no active session snapshot for host service writes",
            ));
        };
        if published.header.id != session_id {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("session `{session_id}` is not the active session"),
            ));
        }
        if event_type == dsh_deliverables::PRESENTED_EVENT {
            if published.cancel.load(Ordering::Acquire) {
                return Err(ServiceError::new(
                    "E_CANCELLED",
                    "file delivery was cancelled",
                ));
            }
            let workspace = published.header.cwd.as_deref().ok_or_else(|| {
                ServiceError::new("E_SERVICE_DENIED", "delivery session has no workspace")
            })?;
            data = dsh_deliverables::normalize_event(Path::new(workspace), data)?;
        }
        let seq = published.next_seq;
        published.events.push(SessionEventView {
            seq,
            event_type: event_type.to_owned(),
            data: data.clone(),
        });
        published.event_times.push(now_ts());
        published.next_seq += 1;
        inner.pending.push(PendingSessionEvent {
            session_id: session_id.to_owned(),
            event_type: event_type.to_owned(),
            data,
        });
        Ok(seq)
    }

    fn latest_working_directory(&self, session_id: &str) -> Option<String> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.published.as_ref().and_then(|published| {
                (published.header.id == session_id).then(|| published.events.clone())
            })
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id).ok()?.1,
        };
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "working-directory/change")
            .and_then(|event| event.data.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn load_session_log(
    sessions_dir: &std::path::Path,
    session_id: &str,
) -> Result<SessionLog, ServiceError> {
    let path = sessions_dir.join(format!("{session_id}.jsonl"));
    if !path.exists() {
        return Err(ServiceError::new(
            "E_SERVICE_UNAVAILABLE",
            format!("unknown session `{session_id}`"),
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("read session `{session_id}`: {error}"),
        )
    })?;
    SessionLog::from_jsonl(session_id, &text).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("decode session `{session_id}`: {error}"),
        )
    })
}

pub(crate) fn chat_header_from_log(
    log: &SessionLog,
    sessions_dir: &std::path::Path,
) -> SessionHeaderView {
    let created = log.created();
    SessionHeaderView {
        id: log.session_id().to_owned(),
        cwd: created
            .and_then(|created| created.payload().get("workspace_id").cloned())
            .and_then(|workspace| workspace.as_str().map(str::to_owned)),
        created_at: log
            .events()
            .first()
            .map(|event| event.ts.clone())
            .unwrap_or_default(),
        parent_session_id: created.and_then(|created| created.parent_session_id.clone()),
        delegation_depth: session_depth(log, sessions_dir),
    }
}

/// 从 `session.created.parent_session_id` 链推导深度（根 0）；环形/断链时停止。
fn session_depth(log: &SessionLog, sessions_dir: &std::path::Path) -> u32 {
    let mut depth = 0u32;
    let mut current = log.parent_session_id().map(str::to_owned);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(parent) = current {
        if depth >= DEFAULT_MAX_DEPTH || !seen.insert(parent.clone()) {
            break;
        }
        depth += 1;
        current = load_session_log(sessions_dir, &parent)
            .ok()
            .and_then(|parent_log| parent_log.parent_session_id().map(str::to_owned));
    }
    depth
}
