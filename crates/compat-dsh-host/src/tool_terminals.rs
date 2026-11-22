use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dshr_compat_dsh_protocol::{
    HostServiceKind, HostServiceMethod, HostServiceTable, TOOL_TERMINALS_V1,
};
use serde_json::{json, Value};

use crate::services::display_path;
use crate::{HostService, ServiceCallContext, ServiceError, SessionAccess};

pub(crate) const TERMINAL_OPENED: &str = "terminal/opened";
pub(crate) const TERMINAL_CLOSED: &str = "terminal/closed";
const MAX_ACTIVE: usize = 4;
const MAX_HISTORY: usize = 64;
static TERMINAL_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TerminalLease {
    owner: String,
    request: Option<String>,
    snapshot: Value,
}

pub struct ToolTerminalService {
    table: HostServiceTable,
    access: Arc<dyn SessionAccess>,
    workspace: PathBuf,
    leases: HashMap<String, TerminalLease>,
}

impl ToolTerminalService {
    pub fn new(workspace: &Path, access: Arc<dyn SessionAccess>) -> std::io::Result<Self> {
        let id = json!({ "type": "string", "minLength": 1, "maxLength": 96 });
        let methods = vec![
            method(
                "reserve",
                HostServiceKind::Write,
                &["cwd", "type"],
                json!({
                    "cwd": { "type": "string" }, "type": { "type": "string", "enum": ["shell"] },
                    "name": { "type": "string", "minLength": 1, "maxLength": 80 },
                }),
                json!({ "id": id, "cwd": { "type": "string" } }),
            ),
            method(
                "confirm",
                HostServiceKind::Write,
                &["id"],
                json!({ "id": id }),
                json!({}),
            ),
            method(
                "list",
                HostServiceKind::Read,
                &[],
                json!({}),
                json!({
                    "terminals": { "type": "array", "items": { "type": "object" } },
                }),
            ),
            method(
                "allow",
                HostServiceKind::Read,
                &["id", "operation"],
                json!({
                    "id": id, "operation": { "type": "string", "enum": ["send", "read", "signal", "close"] },
                    "value": { "type": "string" },
                    "origin_command": { "type": "string" },
                }),
                json!({}),
            ),
            method(
                "closed",
                HostServiceKind::Write,
                &["id"],
                json!({ "id": id }),
                json!({}),
            ),
        ];
        Ok(Self {
            table: HostServiceTable::new(TOOL_TERMINALS_V1, methods),
            access,
            workspace: workspace.canonicalize()?,
            leases: HashMap::new(),
        })
    }

    fn list(&self, owner: &str) -> Result<Value, ServiceError> {
        let mut history: VecDeque<Value> = VecDeque::new();
        let mut from = 0;
        loop {
            let page = self.access.read_events(owner, from, 512)?;
            for event in page.events {
                let id = event.data["sessionId"].as_str().unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                if event.event_type == TERMINAL_OPENED {
                    if history.len() == MAX_HISTORY {
                        history.pop_front();
                    }
                    let live = self
                        .leases
                        .get(id)
                        .is_some_and(|lease| lease.owner == owner && lease.request.is_none());
                    let mut row = event.data;
                    if !live {
                        row["status"] = exited();
                    }
                    history.push_back(row);
                } else if event.event_type == TERMINAL_CLOSED {
                    if let Some(row) = history.iter_mut().find(|row| row["sessionId"] == id) {
                        row["status"] = exited();
                    }
                }
            }
            if page.eof {
                break;
            }
            if page.next_seq <= from {
                return Err(ServiceError::new(
                    "E_INTERNAL",
                    "terminal history cursor did not advance",
                ));
            }
            from = page.next_seq;
        }
        Ok(json!({ "terminals": history }))
    }

    fn reserve(&mut self, owner: &str, request: &str, args: &Value) -> Result<Value, ServiceError> {
        if self
            .leases
            .values()
            .filter(|lease| lease.owner == owner)
            .count()
            >= MAX_ACTIVE
        {
            return Err(ServiceError::new(
                "E_SERVICE_BUSY",
                "too many active terminals in this session",
            ));
        }
        let cwd = args["cwd"]
            .as_str()
            .map(Path::new)
            .filter(|path| path.is_absolute())
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| path.starts_with(&self.workspace) && path.is_dir())
            .ok_or_else(|| denied("terminal directory must be inside the workspace"))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = TERMINAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let id = format!("term-{stamp}-{}-{sequence}", std::process::id());
        let mut snapshot = json!({
            "sessionId": id, "type": args["type"], "status": { "kind": "running" },
        });
        if let Some(name) = args["name"].as_str() {
            snapshot["name"] = json!(name);
        }
        self.leases.insert(
            id.clone(),
            TerminalLease {
                owner: owner.to_owned(),
                request: Some(request.to_owned()),
                snapshot,
            },
        );
        Ok(json!({ "id": id, "cwd": display_path(&cwd) }))
    }
}

impl HostService for ToolTerminalService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let (Some(owner), Some(request)) = (&context.session_id, &context.tool_request_id) else {
            return Err(denied(
                "terminal service requires an active tool and session",
            ));
        };
        match name {
            "reserve" => self.reserve(owner, request, args),
            "list" => self.list(owner),
            "confirm" | "allow" | "closed" => {
                let id = args["id"]
                    .as_str()
                    .ok_or_else(|| ServiceError::args("/id", "missing terminal ID"))?;
                let lease = self
                    .leases
                    .get_mut(id)
                    .filter(|lease| lease.owner == *owner)
                    .ok_or_else(|| denied("terminal is not active in this session"))?;
                if name == "confirm" {
                    if lease.request.as_deref() != Some(request.as_str()) {
                        return Err(denied("terminal reservation has a different owner"));
                    }
                    self.access
                        .append_event(owner, TERMINAL_OPENED, lease.snapshot.clone())?;
                    lease.request = None;
                } else {
                    if lease.request.is_some() {
                        return Err(denied("terminal has not been confirmed"));
                    }
                    if name == "closed" {
                        self.access.append_event(
                            owner,
                            TERMINAL_CLOSED,
                            json!({ "sessionId": id }),
                        )?;
                        self.leases.remove(id);
                    }
                }
                Ok(json!({}))
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                "unknown terminal method",
            )),
        }
    }

    fn release_tool_call(&mut self, request: &str) {
        self.leases
            .retain(|_, lease| lease.request.as_deref() != Some(request));
    }
}

fn method(
    name: &str,
    kind: HostServiceKind,
    required_args: &[&str],
    properties: Value,
    result: Value,
) -> HostServiceMethod {
    let required_result: Vec<_> = result
        .as_object()
        .expect("method result properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        timeout_ms: 5_000,
        idempotent: matches!(name, "list" | "allow"),
        requires_session: true,
        args_schema: json!({
            "type": "object", "required": required_args, "properties": properties, "additionalProperties": false,
        }),
        result_schema: json!({
            "type": "object", "required": required_result, "properties": result, "additionalProperties": false,
        }),
    }
}

fn denied(message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_DENIED", message)
}

fn exited() -> Value {
    json!({ "kind": "exited", "exitCode": null, "signal": null })
}
