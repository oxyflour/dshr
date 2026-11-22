use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, TOOL_SCHEDULES_V1,
};
use dshr_session_store::{recover_backup, save_atomic, try_lock_session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ChatSessionAccess;

const MAX_TASKS: usize = 32;
const MAX_DUE: usize = 8;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Active,
    Paused,
    Ended,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Delivery {
    scheduled_at: u64,
    delivered_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: String,
    title: String,
    prompt: String,
    rrule: String,
    scheduled_at: u64,
    updated_at: u64,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_delivery: Option<Delivery>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ledger {
    next_id: u64,
    tasks: Vec<Task>,
}

pub(super) struct ScheduleService {
    access: Arc<ChatSessionAccess>,
    table: HostServiceTable,
}

impl ScheduleService {
    pub(super) fn new(access: Arc<ChatSessionAccess>) -> Self {
        let id = json!({"id":{"type":"string"}});
        let mutation = json!({"arguments":{"type":"object"}, "timing":{}});
        Self {
            access,
            table: HostServiceTable::new(
                TOOL_SCHEDULES_V1,
                vec![
                    method("list", HostServiceKind::Read, json!({})),
                    method("view", HostServiceKind::Read, id),
                    method("create", HostServiceKind::Write, mutation.clone()),
                    method("edit", HostServiceKind::Write, mutation),
                    method("due", HostServiceKind::Read, json!({})),
                    method(
                        "claim",
                        HostServiceKind::Write,
                        json!({
                            "id":{"type":"string"},"due":{"type":"integer"},"latest":{"type":"integer"},"next":{},
                        }),
                    ),
                ],
            ),
        }
    }

    fn path(&self, session: &str, direct: bool) -> Result<PathBuf, ServiceError> {
        let inner = self
            .access
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let active = inner
            .published
            .as_ref()
            .filter(|snapshot| {
                snapshot.header.id == session && snapshot.header.parent_session_id.is_none()
            })
            .ok_or_else(|| input("subagent_session", "schedule requires a root session"))?;
        if active.cancel.load(Ordering::Acquire) {
            return Err(ServiceError::new(
                "E_CANCELLED",
                "schedule call was cancelled",
            ));
        }
        if direct {
            let user_turn = active
                .events
                .iter()
                .rev()
                .find(|event| matches!(event.event_type.as_str(), "turn.started" | "turn.ended"))
                .is_some_and(|event| {
                    event.event_type == "turn.started" && event.data["source"]["kind"] == "user"
                });
            if !user_turn {
                return Err(input(
                    "invalid_selector",
                    "schedule changes require a direct user turn",
                ));
            }
        }
        Ok(inner
            .sessions_dir
            .join("schedules")
            .join(format!("{session}.json")))
    }
}

impl HostService for ScheduleService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let session = context
            .session_id
            .as_deref()
            .ok_or_else(|| input("invalid_selector", "schedule requires its session"))?;
        if session.is_empty()
            || !session
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(input(
                "invalid_selector",
                "invalid schedule session identity",
            ));
        }
        let path = self.path(session, matches!(method, "create" | "edit"))?;
        std::fs::create_dir_all(path.parent().expect("schedule directory")).map_err(internal)?;
        let _writer = try_lock_session(&path).map_err(internal)?;
        recover_backup(&path).map_err(internal)?;
        let mut ledger: Ledger = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(internal)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
            Err(error) => return Err(internal(error)),
        };
        let now = now_ms();
        let reply = match method {
            "list" => return Ok(json!({"result":ledger.tasks})),
            "view" => {
                return Ok(json!({"result":ledger.tasks.iter().find(|task| args["id"] == task.id)}));
            }
            "due" => {
                return Ok(json!({"result":{
                    "time":now,"tasks":ledger.tasks.iter().filter(|task| task.status == Status::Active && task.scheduled_at <= now)
                        .take(MAX_DUE).collect::<Vec<_>>(),
                }}));
            }
            "create" => {
                if ledger.tasks.len() >= MAX_TASKS {
                    return Err(input("invalid_rule", "session schedule limit reached"));
                }
                let values = &args["arguments"];
                target(&values["target"])?;
                let (rrule, scheduled_at) = timing(&args["timing"], now)?;
                ledger.next_id = ledger
                    .next_id
                    .checked_add(1)
                    .ok_or_else(|| input("invalid_rule", "schedule ID limit reached"))?;
                let task = Task {
                    id: format!("schedule-{}", ledger.next_id),
                    title: text(&values["title"], 120, "invalid_title")?,
                    prompt: text(&values["prompt"], 4_096, "invalid_prompt")?,
                    rrule,
                    scheduled_at,
                    updated_at: now,
                    status: Status::Active,
                    last_delivery: None,
                };
                let view = json!(task);
                ledger.tasks.push(task);
                view
            }
            "edit" => {
                let values = &args["arguments"];
                let Some(index) = ledger.tasks.iter().position(|task| values["id"] == task.id)
                else {
                    return Ok(json!({"result":{ "id":values["id"],"code":"schedule_not_found" }}));
                };
                let current = &mut ledger.tasks[index];
                if values.get("expectedUpdatedAt").is_some()
                    && args["timing"]["expected"] != current.updated_at
                {
                    return Ok(json!({"result":{ "id":current.id,"code":"schedule_conflict" }}));
                }
                if values["action"] == "delete" {
                    let deleted = ledger.tasks.remove(index);
                    json!({"id":deleted.id,"deleted":true})
                } else {
                    if let Some(value) = values.get("target") {
                        target(value)?;
                    }
                    if let Some(title) = values.get("title") {
                        current.title = text(title, 120, "invalid_title")?;
                    }
                    if let Some(prompt) = values.get("prompt") {
                        current.prompt = text(prompt, 4_096, "invalid_prompt")?;
                    }
                    if values.get("rrule").is_some() || values["active"] == true {
                        (current.rrule, current.scheduled_at) = timing(&args["timing"], now)?;
                    }
                    if let Some(active) = values["active"].as_bool() {
                        current.status = if active {
                            Status::Active
                        } else {
                            Status::Paused
                        };
                    }
                    current.updated_at = stamp(now, current.updated_at);
                    json!(current)
                }
            }
            "claim" => {
                let Some(task) = ledger.tasks.iter_mut().find(|task| args["id"] == task.id) else {
                    return Ok(json!({"result":null}));
                };
                if task.status != Status::Active
                    || args["due"] != task.scheduled_at
                    || task.scheduled_at > now
                {
                    return Ok(json!({"result":null}));
                }
                let latest = args["latest"]
                    .as_u64()
                    .filter(|&latest| (task.scheduled_at..=now).contains(&latest));
                let Some(latest) = latest else {
                    return Ok(json!({"result":null}));
                };
                if task
                    .last_delivery
                    .as_ref()
                    .is_some_and(|last| latest <= last.scheduled_at)
                {
                    return Ok(json!({"result":null}));
                }
                if let Some(next) = args["next"].as_u64() {
                    if next <= latest {
                        return Err(input("invalid_rule", "next occurrence does not advance"));
                    }
                    task.scheduled_at = next;
                } else {
                    task.scheduled_at = latest;
                    task.status = Status::Ended;
                }
                task.last_delivery = Some(Delivery {
                    scheduled_at: latest,
                    delivered_at: now,
                });
                task.updated_at = stamp(now, task.updated_at);
                json!({"id":task.id,"prompt":task.prompt,"occurrence":latest})
            }
            _ => {
                return Err(ServiceError::new(
                    "E_SERVICE_METHOD",
                    "unknown schedule method",
                ));
            }
        };
        let encoded = serde_json::to_vec(&ledger).map_err(internal)?;
        save_atomic(&path, &encoded).map_err(internal)?;
        Ok(json!({"result":reply}))
    }
}

fn target(target: &Value) -> Result<(), ServiceError> {
    if *target == "this_session" {
        return Ok(());
    }
    Err(input(
        "invalid_selector",
        "scheduled delivery supports this_session",
    ))
}

fn timing(value: &Value, now: u64) -> Result<(String, u64), ServiceError> {
    let rrule = text(&value["rrule"], 4_096, "invalid_rule")?;
    let due = value["due"]
        .as_u64()
        .filter(|&due| due > now)
        .ok_or_else(|| input("not_future", "recurrence has no future occurrence"))?;
    Ok((rrule, due))
}

fn text(value: &Value, max: usize, code: &str) -> Result<String, ServiceError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= max)
        .map(str::to_owned)
        .ok_or_else(|| input(code, "required schedule text is empty or too large"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn stamp(now: u64, previous: u64) -> u64 {
    now.max(previous.saturating_add(1))
}

fn method(name: &str, kind: HostServiceKind, properties: Value) -> HostServiceMethod {
    let required: Vec<_> = properties
        .as_object()
        .expect("schedule method properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        timeout_ms: 5_000,
        idempotent: kind == HostServiceKind::Read,
        requires_session: true,
        args_schema: json!({"type":"object","additionalProperties":false,"required":required,"properties":properties}),
        result_schema: json!({"type":"object","additionalProperties":false,"required":["result"],"properties":{"result":{}}}),
    }
}

fn input(code: &str, message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_ARGS", message).with_detail(json!({"schedule_code":code}))
}
fn internal(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("E_INTERNAL", error.to_string())
}
