use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, TOOL_TEAMS_V1,
};
use dshr_session_store::{recover_backup, save_atomic, try_lock_session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ChatSessionAccess, dsh_subagents};

const MAX_TASKS: usize = 64;
const MAX_BOARD_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: String,
    revision: u64,
    subject: String,
    description: String,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_id: Option<String>,
    blocked_by: Vec<String>,
    write_scopes: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Board {
    tasks: Vec<Task>,
}

impl Board {
    fn index(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|task| task.id == id)
    }

    fn ready(&self, task: &Task) -> bool {
        task.blocked_by.iter().all(|id| {
            self.index(id)
                .is_some_and(|n| self.tasks[n].status == Status::Completed)
        })
    }

    fn view(&self, task: &Task, members: &HashMap<String, String>) -> Value {
        let mut value = serde_json::to_value(task).expect("serializable team task");
        if let Some((name, _)) = members
            .iter()
            .find(|(_, id)| task.owner_id.as_ref() == Some(*id))
        {
            value["ownerName"] = json!(name);
        }
        let warnings: Vec<_> = self
            .tasks
            .iter()
            .filter(|other| {
                other.id != task.id
                    && other.status == Status::InProgress
                    && task.write_scopes.iter().any(|left| {
                        other
                            .write_scopes
                            .iter()
                            .any(|right| scopes_overlap(left, right))
                    })
            })
            .map(|other| format!("write scopes overlap with {}", other.id))
            .collect();
        value["writeScopeWarnings"] = json!(warnings);
        value["ready"] = json!(task.status == Status::Pending && self.ready(task));
        value
    }

    fn dependencies(&self, value: &Value, task_id: &str) -> Result<Vec<String>, ServiceError> {
        let ids = list(value)?;
        if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
            return Err(invalid("task blocker IDs must be distinct"));
        }
        let mut checked = HashSet::new();
        let mut pending: Vec<_> = ids.iter().map(String::as_str).collect();
        while let Some(id) = pending.pop() {
            if id == task_id {
                return Err(invalid("task dependencies would contain a cycle"));
            }
            if !checked.insert(id) {
                continue;
            }
            let task = self
                .index(id)
                .map(|n| &self.tasks[n])
                .filter(|task| task.status != Status::Deleted)
                .ok_or_else(|| invalid("task blocker does not exist"))?;
            pending.extend(task.blocked_by.iter().map(String::as_str));
        }
        Ok(ids)
    }

    fn create(&mut self, args: &Value) -> Result<usize, ServiceError> {
        if self.tasks.len() >= MAX_TASKS {
            return Err(invalid("team task limit reached"));
        }
        let id = format!("task-{}", self.tasks.len() + 1);
        let task = Task {
            blocked_by: self.dependencies(&args["blocked_by"], &id)?,
            id,
            revision: 1,
            subject: required(&args["subject"], 200)?,
            description: required(&args["description"], 2048)?,
            status: Status::Pending,
            owner_id: None,
            write_scopes: scopes(&args["write_scopes"])?,
        };
        self.tasks.push(task);
        Ok(self.tasks.len() - 1)
    }

    fn update(
        &mut self,
        args: &Value,
        caller: &str,
        root: &str,
        members: &HashMap<String, String>,
    ) -> Result<usize, ServiceError> {
        let offset = args["task_id"]
            .as_str()
            .and_then(|id| self.index(id))
            .ok_or_else(|| invalid("team task was not found"))?;
        let mut task = self.tasks[offset].clone();
        if args["expected_revision"].as_u64() != Some(task.revision) {
            return Err(invalid("stale team task revision"));
        }
        if task.status == Status::Deleted {
            return Err(invalid("team task was deleted"));
        }
        let action = args["action"].as_str().unwrap_or_default();
        if action != "claim" && caller != root && task.owner_id.as_deref() != Some(caller) {
            return Err(invalid(
                "only the task owner or team lead may change this task",
            ));
        }
        match action {
            "claim"
                if task.status == Status::Pending
                    && self.ready(&task)
                    && task.owner_id.as_deref().is_none_or(|owner| owner == caller) =>
            {
                task.owner_id = Some(caller.to_owned());
                task.status = Status::InProgress;
            }
            "release" if task.status == Status::InProgress => {
                task.status = Status::Pending;
                task.owner_id = None;
            }
            "edit"
                if args.get("subject").is_some()
                    || args.get("description").is_some()
                    || args.get("write_scopes").is_some() =>
            {
                if let Some(subject) = args.get("subject") {
                    task.subject = required(subject, 200)?;
                }
                if let Some(description) = args.get("description") {
                    task.description = required(description, 2048)?;
                }
                if let Some(write_scopes) = args.get("write_scopes") {
                    task.write_scopes = scopes(write_scopes)?;
                }
            }
            "set_dependencies" if args.get("blocked_by").is_some() => {
                task.blocked_by = self.dependencies(&args["blocked_by"], &task.id)?;
            }
            "complete" if task.status == Status::InProgress => {
                task.status = Status::Completed;
            }
            "reopen" if task.status == Status::Completed => {
                task.status = Status::Pending;
                task.owner_id = None;
            }
            "reassign"
                if caller == root
                    && matches!(task.status, Status::Pending | Status::InProgress) =>
            {
                if let Some(owner) = args["owner"].as_str() {
                    if !self.ready(&task) {
                        return Err(invalid("team task is blocked"));
                    }
                    task.owner_id = Some(
                        members
                            .get(owner)
                            .ok_or_else(|| invalid("owner is outside the team"))?
                            .clone(),
                    );
                    task.status = Status::InProgress;
                } else {
                    task.owner_id = None;
                    task.status = Status::Pending;
                }
            }
            "delete"
                if !self.tasks.iter().any(|other| {
                    other.status != Status::Deleted && other.blocked_by.contains(&task.id)
                }) =>
            {
                task.status = Status::Deleted;
            }
            _ => return Err(invalid("team task transition is unavailable")),
        }
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("task revision limit reached"))?;
        self.tasks[offset] = task;
        Ok(offset)
    }
}

pub(super) struct TeamService {
    access: Arc<ChatSessionAccess>,
    table: HostServiceTable,
}

impl TeamService {
    pub(super) fn new(access: Arc<ChatSessionAccess>) -> Self {
        Self {
            access,
            table: HostServiceTable::new(
                TOOL_TEAMS_V1,
                vec![
                    method("list", HostServiceKind::Read),
                    method("create", HostServiceKind::Write),
                    method("update", HostServiceKind::Write),
                ],
            ),
        }
    }
}

impl HostService for TeamService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let caller = context
            .session_id
            .as_deref()
            .ok_or_else(|| invalid("team call requires a session"))?;
        let (directory, root, members) = dsh_subagents::team_scope(&self.access, caller)?;
        let directory = directory.join("team-boards");
        std::fs::create_dir_all(&directory).map_err(internal)?;
        let path = directory.join(format!("{root}.json"));
        let _writer = try_lock_session(&path).map_err(internal)?;
        recover_backup(&path).map_err(internal)?;
        let mut board: Board = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(internal)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Board::default(),
            Err(error) => return Err(internal(error)),
        };
        let args = &args["arguments"];
        let changed = match method {
            "list" => {
                return Ok(
                    json!({"value":board.tasks.iter().map(|task| board.view(task, &members)).collect::<Vec<_>>()}),
                );
            }
            "create" => board.create(args)?,
            "update" => board.update(args, caller, &root, &members)?,
            _ => return Err(ServiceError::new("E_SERVICE_METHOD", "unknown team method")),
        };
        let encoded = serde_json::to_vec(&board).map_err(internal)?;
        if encoded.len() > MAX_BOARD_BYTES {
            return Err(invalid("team task board storage limit reached"));
        }
        save_atomic(&path, &encoded).map_err(internal)?;
        Ok(json!({"value":board.view(&board.tasks[changed], &members)}))
    }
}

fn required(value: &Value, limit: usize) -> Result<String, ServiceError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.len() <= limit)
        .map(str::to_owned)
        .ok_or_else(|| invalid("required task text is empty or too large"))
}

fn list(value: &Value) -> Result<Vec<String>, ServiceError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .filter(|items| items.len() <= 32)
        .ok_or_else(|| invalid("too many task options"))?
        .iter()
        .map(|item| required(item, 256))
        .collect()
}

fn scopes(value: &Value) -> Result<Vec<String>, ServiceError> {
    let mut scopes = Vec::new();
    for raw in list(value)? {
        let value = raw.replace('\\', "/");
        let value = value
            .strip_prefix("./")
            .unwrap_or(&value)
            .trim_end_matches('/');
        if value.is_empty()
            || value.contains(':')
            || value.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(invalid("write scopes must be workspace-relative"));
        }
        if !scopes.iter().any(|scope| scope == value) {
            scopes.push(value.to_owned());
        }
    }
    Ok(scopes)
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn method(name: &str, kind: HostServiceKind) -> HostServiceMethod {
    let read = kind == HostServiceKind::Read;
    let (required, properties) = if read {
        (json!([]), json!({}))
    } else {
        (json!(["arguments"]), json!({"arguments":{"type":"object"}}))
    };
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        idempotent: read,
        timeout_ms: 5_000,
        requires_session: true,
        args_schema: json!({"type":"object","additionalProperties":false,"required":required,"properties":properties}),
        result_schema: json!({"type":"object","additionalProperties":false,"required":["value"],"properties":{"value":{}}}),
    }
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_ARGS", message)
}
fn internal(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("E_INTERNAL", error.to_string())
}
