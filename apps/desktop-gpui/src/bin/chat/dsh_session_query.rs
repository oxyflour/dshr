use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, SessionEventView, SessionHeaderView, TOOL_SESSION_QUERY_V1,
};
use dshr_session_store::{SessionIndex, SessionLog};
use serde_json::{json, Value};

use super::{chat_header_from_log, load_session_log, ChatSessionAccess};

const PAGE: usize = 32;
const MAX_CATALOG: usize = 128;
const EVENT_TEXT_BYTES: usize = 4_096;

pub(super) struct SessionQueryService {
    access: Arc<ChatSessionAccess>,
    table: HostServiceTable,
}

impl SessionQueryService {
    pub(super) fn new(access: Arc<ChatSessionAccess>) -> Self {
        Self {
            access,
            table: HostServiceTable {
                service_key: TOOL_SESSION_QUERY_V1.to_owned(),
                limits: Default::default(),
                methods: vec![
                    method(
                        "catalog",
                        &[],
                        json!({}),
                        json!({"records":{"type":"array"}}),
                    ),
                    method(
                        "describe",
                        &["session"],
                        json!({"session":{"type":"string","maxLength":128}}),
                        json!({"record":{"type":"object"}, "before":{"type":"integer"}}),
                    ),
                    method(
                        "events",
                        &["session", "from"],
                        json!({
                            "session":{"type":"string","maxLength":128},"from":{"type":"integer","minimum":0},
                            "to":{"type":"integer","minimum":0},
                        }),
                        json!({"events":{"type":"array"},"next":{},"record":{"type":"object"}}),
                    ),
                ],
            },
        }
    }

    fn scope(&self, caller: &str) -> Result<QueryScope, ServiceError> {
        let inner = self
            .access
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let published = inner
            .published
            .as_ref()
            .filter(|snapshot| snapshot.header.id == caller)
            .ok_or_else(hidden)?;
        if published.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new(
                "E_CANCELLED",
                "session query was cancelled",
            ));
        }
        let mut scope = QueryScope {
            dir: inner.sessions_dir.clone(),
            caller: published.header.clone(),
            root: None,
            active_before: published
                .events
                .iter()
                .rev()
                .find(|event| matches!(event.event_type.as_str(), "step.started" | "turn.started"))
                .map(|event| event.seq)
                .unwrap_or(published.next_seq),
        };
        if let Some(workspace) = &scope.caller.cwd {
            scope.root = scope.root_of(caller, workspace);
        }
        Ok(scope)
    }

    fn events(
        &self,
        scope: &QueryScope,
        target: &str,
        from: u64,
        to: u64,
    ) -> Result<Value, ServiceError> {
        let log = scope.owned_log(target)?;
        let record = scope.record(target, log.as_ref(), None);
        let mut found = Vec::new();
        let mut next = None;
        let mut add = |event: &SessionEventView, ts: &str| {
            if event.seq < from || event.seq > to {
                return true;
            }
            let Some(event) = project(event, ts) else {
                return true;
            };
            if found.len() == PAGE {
                next = event["seq"].as_u64();
                return false;
            }
            found.push(event);
            true
        };
        if let Some(log) = log {
            for event in log
                .events()
                .iter()
                .skip_while(|event| event.seq < from)
                .take_while(|event| event.seq <= to)
            {
                let view = SessionEventView {
                    seq: event.seq,
                    event_type: event.event_type.clone(),
                    data: event.payload.clone(),
                };
                if !add(&view, &event.ts) {
                    break;
                }
            }
        } else {
            let inner = self
                .access
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let snapshot = inner
                .published
                .as_ref()
                .filter(|snapshot| snapshot.header.id == target)
                .ok_or_else(hidden)?;
            let to = to.min(scope.active_before.saturating_sub(1));
            for (event, ts) in snapshot
                .events
                .iter()
                .zip(&snapshot.event_times)
                .skip_while(|(event, _)| event.seq < from)
                .take_while(|(event, _)| event.seq <= to)
            {
                if !add(event, ts) {
                    break;
                }
            }
        }
        Ok(json!({"record":record,"events":found,"next":next}))
    }
}

impl HostService for SessionQueryService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let caller = ctx.session_id.as_deref().ok_or_else(hidden)?;
        let scope = self.scope(caller)?;
        match name {
            "catalog" => scope.catalog(),
            "describe" | "events" => {
                let target = args["session"].as_str().ok_or_else(hidden)?;
                if name == "events" {
                    return self.events(
                        &scope,
                        target,
                        args["from"].as_u64().unwrap_or_default(),
                        args["to"].as_u64().unwrap_or(u64::MAX),
                    );
                }
                let log = scope.owned_log(target)?;
                Ok(
                    json!({ "record":scope.record(target, log.as_ref(), None), "before":scope.active_before }),
                )
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                "unknown session query method",
            )),
        }
    }
}

struct QueryScope {
    dir: PathBuf,
    caller: SessionHeaderView,
    root: Option<String>,
    active_before: u64,
}

impl QueryScope {
    fn owned_log(&self, target: &str) -> Result<Option<SessionLog>, ServiceError> {
        if target == self.caller.id {
            return Ok(None);
        }
        let root = self.root.as_ref().ok_or_else(hidden)?;
        let workspace = self.caller.cwd.as_deref().ok_or_else(hidden)?;
        let log = safe_load(&self.dir, target).ok_or_else(hidden)?;
        if self.root_of(target, workspace).as_ref() == Some(root) {
            return Ok(Some(log));
        }
        Err(hidden())
    }

    fn root_of(&self, start: &str, workspace: &str) -> Option<String> {
        let mut id = start.to_owned();
        let mut visited = HashSet::new();
        for _ in 0..32 {
            if !visited.insert(id.clone()) {
                return None;
            }
            let parent = if id == self.caller.id {
                if self.caller.cwd.as_deref() != Some(workspace) {
                    return None;
                }
                self.caller.parent_session_id.clone()
            } else {
                let log = safe_load(&self.dir, &id)?;
                let created = log.created()?;
                if created.workspace_id != workspace {
                    return None;
                }
                created.parent_session_id.clone()
            };
            let Some(parent) = parent else {
                return Some(id);
            };
            id = parent;
        }
        None
    }

    fn catalog(&self) -> Result<Value, ServiceError> {
        let metas = SessionIndex::new(&self.dir)
            .scan()
            .map_err(|_| ServiceError::new("E_INTERNAL", "session index is unavailable"))?;
        let mut records = vec![self.record(
            &self.caller.id,
            None,
            metas
                .iter()
                .find(|meta| meta.id == self.caller.id)
                .map(|meta| meta.title.as_str()),
        )];
        for meta in metas {
            if records.len() == MAX_CATALOG {
                break;
            }
            if meta.id == self.caller.id || meta.workspace_id != self.caller.cwd {
                continue;
            }
            if let Ok(Some(log)) = self.owned_log(&meta.id) {
                records.push(self.record(&meta.id, Some(&log), Some(&meta.title)));
            }
        }
        Ok(json!({"records":records}))
    }

    fn record(&self, target: &str, log: Option<&SessionLog>, title: Option<&str>) -> Value {
        let header = log
            .map(|log| chat_header_from_log(log, &self.dir))
            .unwrap_or_else(|| self.caller.clone());
        let mut record = json!({
            "header":{"id":target,"createdAt":milliseconds(&header.created_at)},
            "live":log.is_none(),"persisted":self.dir.join(format!("{target}.jsonl")).is_file(),
        });
        if let Some(workspace) = header.cwd {
            record["header"]["cwd"] = json!(workspace);
        }
        if let Some(parent) = header.parent_session_id {
            if self.root.is_some() {
                record["header"]["parentSession"] = json!(parent);
            }
        }
        if let Some(title) = title.filter(|title| !title.is_empty()) {
            record["title"] = json!(bounded(title, 256));
        }
        record
    }
}

pub(super) fn safe_load(dir: &Path, id: &str) -> Option<SessionLog> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    let path = dir.join(format!("{id}.jsonl"));
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    load_session_log(dir, id).ok()
}

fn milliseconds(ts: &str) -> Option<u64> {
    ts.strip_prefix('t')
        .and_then(|raw| raw.parse::<u128>().ok())
        .and_then(|nanos| u64::try_from(nanos / 1_000_000).ok())
}

fn project(event: &SessionEventView, ts: &str) -> Option<Value> {
    let (kind, text, data, surface) = match event.event_type.as_str() {
        "message.user" | "message.assistant.settled" => {
            let content = if let Some(text) = event.data["text"].as_str() {
                bounded(text, EVENT_TEXT_BYTES).to_owned()
            } else {
                content_text(&event.data["content"])
            };
            let kind = if event.event_type == "message.user" {
                "user/message"
            } else {
                "assistant/message"
            };
            (
                kind,
                content.clone(),
                json!({ "content":[{"type":"text","text":content}] }),
                "current",
            )
        }
        "tool.result" => {
            let content = content_text(&event.data["content"]);
            (
                "tool/result",
                content.clone(),
                json!({"content":[{"type":"text","text":content}], "callSeq":event.data["call_seq"]}),
                "current",
            )
        }
        "tool.call" => (
            "tool/call",
            String::new(),
            json!({"name":event.data["tool"]}),
            "log-only",
        ),
        "context.compacted" => (
            "context/compacted",
            String::new(),
            json!({
                "fromSeq":event.data["from_seq"],"toSeq":event.data["to_seq"],"summaryId":event.data["summary_message_id"],
            }),
            "log-only",
        ),
        "session.created"
        | "turn.started"
        | "step.started"
        | "turn.ended"
        | "child.spawned"
        | "child.finished"
        | "session.interrupted" => (
            event.event_type.as_str(),
            String::new(),
            json!({}),
            "log-only",
        ),
        _ => return None,
    };
    Some(
        json!({"seq":event.seq,"type":kind,"time":milliseconds(ts),"text":text,"data":data,"surface":surface}),
    )
}

fn content_text(content: &Value) -> String {
    let mut result = String::new();
    if let Some(content) = content.as_array() {
        for block in content {
            if block["type"] != "text" {
                continue;
            }
            let Some(text) = block["text"].as_str().filter(|text| !text.is_empty()) else {
                continue;
            };
            if result.len() >= EVENT_TEXT_BYTES - 1 {
                break;
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(bounded(text, EVENT_TEXT_BYTES - result.len()));
        }
    }
    result
}

fn bounded(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

fn method(name: &str, required: &[&str], properties: Value, result: Value) -> HostServiceMethod {
    let required_result: Vec<_> = result
        .as_object()
        .expect("result properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind: HostServiceKind::Read,
        timeout_ms: 5_000,
        idempotent: true,
        requires_session: true,
        args_schema: json!({"type":"object","additionalProperties":false,"required":required,"properties":properties}),
        result_schema: json!({"type":"object","additionalProperties":false,"required":required_result,"properties":result}),
    }
}

fn hidden() -> ServiceError {
    ServiceError::new(
        "E_SERVICE_DENIED",
        "session is outside the authorized session tree",
    )
}
