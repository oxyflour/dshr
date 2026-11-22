use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dshr_compat_dsh_protocol::{
    HostServiceKind, HostServiceMethod, HostServiceTable, TOOL_JOBS_V1,
};
use dshr_session_store::{recover_backup, save_atomic, try_lock_session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tool_process::{
    ProcessOutput, RunningProcess, ShellProgram, StartArgs, ToolProcessService,
};
use crate::{HostService, ServiceCallContext, ServiceError};

const ACTIVE_LIMIT: usize = 4;
const JOB_LIMIT: usize = 64;
const OUTPUT_LIMIT: u64 = 65_536;
const OUTPUT_PAGE: usize = 24 * 1024;
pub(crate) const JOB_LIFETIME_MS: u64 = 900_000;
const MAX_RECORD: u64 = 1024 * 1024;
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Running,
    Stopping,
    Completed,
    Failed,
    Killed,
}

impl Status {
    fn live(&self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

#[derive(Serialize, Deserialize)]
struct JobRecord {
    id: String,
    session: String,
    workspace: PathBuf,
    kind: String,
    label: String,
    call_id: String,
    created_at: u64,
    finished_at: Option<u64>,
    status: Status,
    detail: Option<String>,
    text: String,
    truncated: bool,
    cursor: usize,
    handed_off: bool,
    #[serde(default)]
    noticed: bool,
}

impl JobRecord {
    fn settle(&mut self, result: &Result<ProcessOutput, String>) {
        self.finished_at = Some(now_ms());
        match result {
            Ok(result) => {
                self.status = if result.aborted {
                    Status::Killed
                } else if result.status.success() && !result.timed_out {
                    Status::Completed
                } else {
                    Status::Failed
                };
                self.detail = if result.timed_out {
                    Some("background process reached its runtime limit".to_owned())
                } else {
                    result
                        .status
                        .code()
                        .filter(|code| *code != 0)
                        .map(|code| format!("exit code {code}"))
                };
                self.text.clone_from(&result.stdout.text);
                if !result.stderr.text.is_empty() {
                    if !self.text.is_empty() && !self.text.ends_with('\n') {
                        self.text.push('\n');
                    }
                    self.text.push_str("[stderr]\n");
                    self.text.push_str(&result.stderr.text);
                }
                self.truncated = result.stdout.truncated || result.stderr.truncated;
            }
            Err(error) => {
                self.status = Status::Failed;
                self.detail = Some(error.clone());
            }
        }
    }

    fn summary(&self) -> Value {
        json!({
            "id": self.id, "owner": self.session, "kind": self.kind, "label": self.label,
            "call_id": self.call_id, "created_at": self.created_at, "finished_at": self.finished_at,
            "status": self.status, "detail": self.detail, "output_bytes": self.text.len(),
            "cursor": self.cursor, "truncated": self.truncated,
        })
    }
}

pub struct ToolJobService {
    table: HostServiceTable,
    launcher: ToolProcessService,
    workspace: PathBuf,
    root: PathBuf,
    reservations: HashMap<String, String>,
    pending: HashMap<String, (String, PathBuf)>,
    running: HashMap<PathBuf, RunningProcess>,
}

impl ToolJobService {
    pub fn new(workspace: &Path, root: PathBuf, shell: ShellProgram) -> io::Result<Self> {
        let id = json!({ "id": { "type": "string", "minLength": 1, "maxLength": 80 } });
        let methods = vec![
            method("reserve", HostServiceKind::Write, &[], json!({})),
            method(
                "start",
                HostServiceKind::Write,
                &["command", "cwd", "engine"],
                json!({
                    "command": { "type": "string", "minLength": 1 },
                    "cwd": { "type": "string", "minLength": 1 },
                    "engine": { "type": "string", "enum": ["pwsh", "bash"] },
                }),
            ),
            method("confirm", HostServiceKind::Write, &[], json!({})),
            method("list", HostServiceKind::Read, &[], json!({})),
            method("notices", HostServiceKind::Write, &[], json!({})),
            method("inspect", HostServiceKind::Read, &["id"], id.clone()),
            method("read", HostServiceKind::Read, &["id"], id.clone()),
            method(
                "advance",
                HostServiceKind::Write,
                &["id", "from", "to"],
                json!({
                    "id": id["id"], "from": { "type": "integer", "minimum": 0 },
                    "to": { "type": "integer", "minimum": 0 },
                }),
            ),
            method("stop", HostServiceKind::Write, &["id"], id),
        ];
        Ok(Self {
            table: HostServiceTable::new(TOOL_JOBS_V1, methods),
            launcher: ToolProcessService::new(workspace, None, Some(shell))?,
            workspace: workspace.canonicalize()?,
            root,
            reservations: HashMap::new(),
            pending: HashMap::new(),
            running: HashMap::new(),
        })
    }

    fn directory(&self, session: &str) -> Result<PathBuf, ServiceError> {
        if session.is_empty() || session.len() > 96 {
            return Err(denied("invalid background job session"));
        }
        let directory = session
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(self.root.join(directory))
    }

    fn path(&self, session: &str, id: &str) -> Result<PathBuf, ServiceError> {
        if !id.strip_prefix("job-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 76
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'-')
        }) {
            return Err(denied("invalid background job ID"));
        }
        Ok(self.directory(session)?.join(format!("{id}.json")))
    }

    fn owned(&self, path: &Path, session: &str) -> Result<JobRecord, ServiceError> {
        let mut record = load(path).map_err(job_io)?;
        if record.session != session || record.workspace != self.workspace || !record.handed_off {
            return Err(denied(
                "background job does not belong to this session and workspace",
            ));
        }
        if record.status.live() {
            if let Ok(_lease) = try_lock_session(&path.with_extension("live")) {
                if let Ok(latest) = load(path) {
                    record = latest;
                }
                if record.status.live() {
                    record.status = Status::Killed;
                    record.detail =
                        Some("background job was interrupted when the host stopped".to_owned());
                    record.finished_at = Some(now_ms());
                    update(path, |current| {
                        if current.status.live() {
                            current.status = Status::Killed;
                            current.detail.clone_from(&record.detail);
                            current.finished_at = record.finished_at;
                        }
                    })
                    .map_err(job_io)?;
                }
            }
        }
        Ok(record)
    }

    fn list(&self, session: &str) -> Result<Vec<JobRecord>, ServiceError> {
        let directory = self.directory(session)?;
        let mut jobs: Vec<_> = match fs::read_dir(directory) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .filter_map(|entry| self.owned(&entry.path(), session).ok())
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(job_io(error)),
        };
        jobs.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then(right.id.cmp(&left.id))
        });
        jobs.truncate(JOB_LIMIT);
        Ok(jobs)
    }

    fn start(
        &mut self,
        context: &ServiceCallContext,
        session: &str,
        request: &str,
        args: &Value,
    ) -> Result<Value, ServiceError> {
        let id = self
            .reservations
            .remove(request)
            .ok_or_else(|| denied("background job has no reservation"))?;
        let jobs = self.list(session)?;
        if jobs.iter().filter(|record| record.status.live()).count()
            + self
                .pending
                .values()
                .filter(|(owner, _)| owner == session)
                .count()
            >= ACTIVE_LIMIT
        {
            return Err(ServiceError::new(
                "E_SERVICE_BUSY",
                "background job limit reached",
            ));
        }
        let command = args["command"]
            .as_str()
            .ok_or_else(|| ServiceError::args("/command", "missing command"))?;
        let cwd = args["cwd"]
            .as_str()
            .ok_or_else(|| ServiceError::args("/cwd", "missing directory"))?;
        let engine = args["engine"]
            .as_str()
            .ok_or_else(|| ServiceError::args("/engine", "missing shell"))?;
        let path = self.path(session, &id)?;
        fs::create_dir_all(path.parent().expect("job directory")).map_err(job_io)?;
        let lease = try_lock_session(&path.with_extension("live")).map_err(job_io)?;
        let record = JobRecord {
            id: id.clone(),
            session: session.to_owned(),
            workspace: self.workspace.clone(),
            kind: engine.to_owned(),
            label: command.chars().take(256).collect(),
            call_id: context.call_id.clone().unwrap_or_default(),
            created_at: now_ms(),
            finished_at: None,
            status: Status::Running,
            detail: None,
            text: String::new(),
            truncated: false,
            cursor: 0,
            handed_off: false,
            noticed: false,
        };
        persist(&path, &record).map_err(job_io)?;
        let finished_path = path.clone();
        let launched = self.launcher.launch(
            request,
            session,
            StartArgs {
                engine: engine.to_owned(),
                command: Some(command.to_owned()),
                cwd: PathBuf::from(cwd),
                timeout_ms: JOB_LIFETIME_MS,
                stdout_max: OUTPUT_LIMIT,
                stderr_max: OUTPUT_LIMIT,
                argv: None,
            },
            Some(Box::new(move |result| {
                let _lease = lease;
                if let Err(error) = update(&finished_path, |record| record.settle(result)) {
                    eprintln!("[dsh-jobs] cannot record background job completion: {error}");
                }
            })),
        );
        match launched {
            Ok(process) => {
                self.running.insert(path.clone(), process);
                self.pending
                    .insert(request.to_owned(), (session.to_owned(), path));
                Ok(json!({ "id": id }))
            }
            Err(error) => {
                let _ = fs::remove_file(path);
                Err(error)
            }
        }
    }
}

impl HostService for ToolJobService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        self.running
            .retain(|_, process| matches!(process.poll(), Ok(None)));
        let session = context
            .session_id
            .as_deref()
            .ok_or_else(|| denied("background jobs require a session"))?;
        let request = || {
            context
                .tool_request_id
                .as_deref()
                .ok_or_else(|| denied("background job action requires an active tool"))
        };
        match name {
            "reserve" => {
                let request = request()?;
                let id = self
                    .reservations
                    .entry(request.to_owned())
                    .or_insert_with(next_id);
                Ok(json!({ "id": id }))
            }
            "start" => self.start(context, session, request()?, args),
            "confirm" => {
                let request = request()?;
                let (owner, path) = self
                    .pending
                    .get(request)
                    .ok_or_else(|| denied("no job to confirm"))?;
                if owner != session {
                    return Err(denied("cannot confirm another session's job"));
                }
                update(path, |record| record.handed_off = true).map_err(job_io)?;
                self.pending.remove(request);
                Ok(json!({}))
            }
            "list" => Ok(json!({
                "jobs": self.list(session)?.iter().map(JobRecord::summary).collect::<Vec<_>>(),
            })),
            "notices" => {
                let mut notices = Vec::new();
                for record in self
                    .list(session)?
                    .into_iter()
                    .filter(|record| !record.status.live() && !record.noticed)
                    .take(12)
                {
                    let path = self.path(session, &record.id)?;
                    let mut first = false;
                    update(&path, |current| {
                        first = !current.status.live() && !current.noticed;
                        current.noticed = true;
                    })
                    .map_err(job_io)?;
                    if first {
                        notices.push(record.summary());
                    }
                }
                Ok(json!({ "jobs": notices }))
            }
            "inspect" | "read" | "advance" | "stop" => {
                let id = args["id"]
                    .as_str()
                    .ok_or_else(|| ServiceError::args("/id", "invalid job identity"))?;
                let path = self.path(session, id)?;
                let record = self.owned(&path, session)?;
                match name {
                    "inspect" => Ok(json!({ "job": record.summary() })),
                    "read" => {
                        let (from, to) = output_page(&record)?;
                        Ok(json!({
                            "job": record.summary(), "text": &record.text[from..to],
                            "from": from, "to": to, "end": to == record.text.len(),
                        }))
                    }
                    "advance" => {
                        let from = args["from"].as_u64().and_then(|n| usize::try_from(n).ok());
                        let to = args["to"].as_u64().and_then(|n| usize::try_from(n).ok());
                        let mut accepted = false;
                        update(&path, |current| {
                            if let Ok((expected_from, expected_to)) = output_page(current) {
                                accepted = from == Some(expected_from) && to == Some(expected_to);
                                if accepted {
                                    current.cursor = expected_to;
                                    if expected_to == current.text.len() && !current.status.live() {
                                        current.noticed = true;
                                    }
                                }
                            }
                        })
                        .map_err(job_io)?;
                        if !accepted {
                            return Err(ServiceError::new(
                                "E_SERVICE_BUSY",
                                "job output cursor changed, read the current page",
                            ));
                        }
                        Ok(json!({}))
                    }
                    "stop" => {
                        if !record.status.live() {
                            return Ok(json!({ "outcome": "already-finished" }));
                        }
                        let Some(process) = self.running.get(&path) else {
                            let record = self.owned(&path, session)?;
                            if !record.status.live() {
                                return Ok(json!({ "outcome": "already-finished" }));
                            }
                            return Err(ServiceError::new(
                                "E_SERVICE_UNAVAILABLE",
                                "job is active in another host",
                            ));
                        };
                        update(&path, |record| {
                            if record.status.live() {
                                record.status = Status::Stopping;
                            }
                        })
                        .map_err(job_io)?;
                        process.cancel();
                        Ok(json!({ "outcome": "requested" }))
                    }
                    _ => unreachable!("matched job record operation"),
                }
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                "unknown background job method",
            )),
        }
    }

    fn release_tool_call(&mut self, request: &str) {
        self.reservations.remove(request);
        if let Some((_, path)) = self.pending.remove(request) {
            self.running.remove(&path);
        }
    }
}

fn method(
    name: &str,
    kind: HostServiceKind,
    required: &[&str],
    properties: Value,
) -> HostServiceMethod {
    let result_properties = match name {
        "reserve" | "start" => json!({ "id": { "type": "string" } }),
        "list" | "notices" => json!({ "jobs": { "type": "array", "items": { "type": "object" } } }),
        "inspect" => json!({ "job": { "type": "object" } }),
        "read" => json!({
            "job": { "type": "object" }, "text": { "type": "string" },
            "from": { "type": "integer" }, "to": { "type": "integer" }, "end": { "type": "boolean" },
        }),
        "stop" => json!({ "outcome": { "type": "string" } }),
        _ => json!({}),
    };
    let result_required: Vec<_> = result_properties
        .as_object()
        .expect("method result properties")
        .keys()
        .collect();
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        requires_session: true,
        timeout_ms: 5_000,
        idempotent: matches!(name, "list" | "inspect" | "read"),
        args_schema: json!({
            "type": "object", "required": required, "properties": properties, "additionalProperties": false,
        }),
        result_schema: json!({
            "type": "object", "required": result_required,
            "properties": result_properties, "additionalProperties": false,
        }),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_id() -> String {
    format!(
        "job-{}-{}-{}",
        now_ms(),
        std::process::id(),
        NEXT_JOB.fetch_add(1, Ordering::Relaxed)
    )
}

fn output_page(record: &JobRecord) -> Result<(usize, usize), ServiceError> {
    let from = record.cursor;
    if !record.text.is_char_boundary(from) {
        return Err(ServiceError::new(
            "E_INTERNAL",
            "invalid saved background output cursor",
        ));
    }
    let mut to = from.saturating_add(OUTPUT_PAGE).min(record.text.len());
    while !record.text.is_char_boundary(to) {
        to -= 1;
    }
    Ok((from, to))
}

fn denied(message: &str) -> ServiceError {
    ServiceError::new("E_SERVICE_DENIED", message)
}

fn job_io(error: io::Error) -> ServiceError {
    match error.kind() {
        io::ErrorKind::NotFound => {
            denied("background job does not belong to this session and workspace")
        }
        io::ErrorKind::WouldBlock => {
            ServiceError::new("E_SERVICE_BUSY", "background job data is being updated")
        }
        _ => ServiceError::new(
            "E_INTERNAL",
            format!("background job storage error: {error}"),
        ),
    }
}

fn load(path: &Path) -> io::Result<JobRecord> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "background job record exceeds its budget",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn persist(path: &Path, record: &JobRecord) -> io::Result<()> {
    save_atomic(path, &serde_json::to_vec(record)?)
}

fn update(path: &Path, change: impl FnOnce(&mut JobRecord)) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(1500);
    let _lock = loop {
        match try_lock_session(path) {
            Ok(lock) => break lock,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    };
    recover_backup(path)?;
    let mut record = load(path)?;
    change(&mut record);
    persist(path, &record)
}
