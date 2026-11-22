use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_protocol::{
    HostServiceKind, HostServiceMethod, HostServiceTable, TOOL_PROCESS_V1,
};
use dshr_sidecar_guard::{process::spawn_prepared, GuardLimits, GuardedChild};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{HostService, ServiceCallContext, ServiceError};

const MAX_PROCESSES: usize = 16;
const MAX_STDOUT: u64 = 20_000_000;
const MAX_STDERR: u64 = 1024 * 1024;
const PAGE_BYTES: usize = 32 * 1024;

#[derive(Clone)]
pub struct ShellProgram {
    pub name: &'static str,
    pub path: PathBuf,
}

pub struct ToolProcessService {
    table: HostServiceTable,
    workspace: PathBuf,
    ripgrep: Option<PathBuf>,
    shell: Option<ShellProgram>,
    next_id: u64,
    processes: HashMap<u64, RunningProcess>,
}

#[derive(Deserialize)]
pub(crate) struct StartArgs {
    pub(crate) engine: String,
    pub(crate) cwd: PathBuf,
    pub(crate) timeout_ms: u64,
    pub(crate) stdout_max: u64,
    pub(crate) stderr_max: u64,
    pub(crate) argv: Option<Vec<String>>,
    pub(crate) command: Option<String>,
}

pub(crate) struct ProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) timed_out: bool,
    pub(crate) aborted: bool,
    pub(crate) stdout: Captured,
    pub(crate) stderr: Captured,
}

pub(crate) struct Captured {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) type ProcessObserver = Box<dyn FnOnce(&Result<ProcessOutput, String>) + Send>;

pub(crate) struct RunningProcess {
    owner: String,
    cancel: Arc<AtomicBool>,
    settled: mpsc::Receiver<Result<ProcessOutput, String>>,
    output: Option<Result<ProcessOutput, String>>,
}

impl RunningProcess {
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub(crate) fn poll(&mut self) -> Result<Option<&ProcessOutput>, ServiceError> {
        if self.output.is_none() {
            match self.settled.try_recv() {
                Ok(output) => self.output = Some(output),
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.output = Some(Err(
                        "process supervisor stopped without an exit status".to_owned()
                    ));
                }
            }
        }
        match self.output.as_ref() {
            Some(Ok(output)) => Ok(Some(output)),
            Some(Err(error)) => Err(ServiceError::new("E_INTERNAL", error)),
            None => Ok(None),
        }
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl ToolProcessService {
    pub fn new(
        workspace: &Path,
        ripgrep: Option<PathBuf>,
        shell: Option<ShellProgram>,
    ) -> io::Result<Self> {
        let method = |name: &str,
                      kind,
                      required: &[&str],
                      properties,
                      output_required: &[&str],
                      output_properties| {
            HostServiceMethod {
                name: name.to_owned(),
                kind,
                timeout_ms: 5000,
                idempotent: name != "start",
                requires_session: true,
                args_schema: object_schema(required, properties),
                result_schema: object_schema(output_required, output_properties),
            }
        };
        Ok(Self {
            table: HostServiceTable::new(
                TOOL_PROCESS_V1,
                vec![
                    method(
                        "start",
                        HostServiceKind::Write,
                        &["engine", "cwd", "timeout_ms", "stdout_max", "stderr_max"],
                        json!({
                            "engine": { "type": "string", "enum": ["ripgrep", "pwsh", "bash"] },
                            "cwd": { "type": "string" }, "timeout_ms": { "type": "integer" },
                            "stdout_max": { "type": "integer" }, "stderr_max": { "type": "integer" },
                            "argv": { "type": "array", "items": { "type": "string" } },
                            "command": { "type": "string" },
                        }),
                        &["id"],
                        json!({ "id": { "type": "integer" } }),
                    ),
                    method(
                        "poll",
                        HostServiceKind::Read,
                        &["id"],
                        json!({ "id": { "type": "integer" } }),
                        &["done"],
                        json!({
                            "done": { "type": "boolean" },
                            "exit_code": { "type": ["integer", "null"] },
                            "signal": { "type": ["string", "null"] },
                            "timed_out": { "type": "boolean" }, "aborted": { "type": "boolean" },
                            "stdout_truncated": { "type": "boolean" }, "stderr_truncated": { "type": "boolean" },
                        }),
                    ),
                    method(
                        "read",
                        HostServiceKind::Read,
                        &["id", "stream", "offset"],
                        json!({
                            "id": { "type": "integer" }, "offset": { "type": "integer" },
                            "stream": { "type": "string", "enum": ["stdout", "stderr"] },
                        }),
                        &["text", "next_offset", "end"],
                        json!({
                            "text": { "type": "string" }, "next_offset": { "type": "integer" },
                            "end": { "type": "boolean" },
                        }),
                    ),
                    method(
                        "release",
                        HostServiceKind::Write,
                        &["id"],
                        json!({ "id": { "type": "integer" } }),
                        &[],
                        json!({}),
                    ),
                ],
            ),
            workspace: workspace.canonicalize()?,
            ripgrep,
            shell,
            next_id: 1,
            processes: HashMap::new(),
        })
    }

    fn workspace_path(&self, path: &Path) -> Result<PathBuf, ServiceError> {
        let absolute = path.canonicalize().map_err(|error| {
            ServiceError::new(
                "E_SERVICE_ARGS",
                format!("cannot access {}: {error}", path.display()),
            )
        })?;
        if !absolute.starts_with(&self.workspace) {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "process path escapes the workspace",
            ));
        }
        Ok(absolute)
    }

    fn search_command(&self, cwd: &Path, argv: Vec<String>) -> Result<Command, ServiceError> {
        let binary = self.ripgrep.as_ref().ok_or_else(|| {
            ServiceError::new("E_SERVICE_UNAVAILABLE", "ripgrep executable is unavailable")
        })?;
        let separator = argv
            .iter()
            .position(|argument| argument == "--")
            .unwrap_or(argv.len());
        let flags = &argv[..separator];
        if !flags
            .iter()
            .any(|argument| argument == "--files" || argument == "--json")
            || !flags.iter().all(|argument| {
                matches!(
                    argument.as_str(),
                    "--files"
                        | "--json"
                        | "--no-config"
                        | "--sort=modified"
                        | "--no-ignore"
                        | "--hidden"
                ) || argument.starts_with("--glob=")
                    || argument.starts_with("--regexp=")
            })
            || argv.len() > separator.saturating_add(2)
        {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "ripgrep flags are outside the official search interface",
            ));
        }
        if let Some(search) = argv.get(separator + 1) {
            let _ = self.workspace_path(&cwd.join(search))?;
        }
        let mut command = Command::new(binary);
        command.arg("--no-config").args(argv);
        Ok(command)
    }

    fn shell_command(&self, engine: &str, script: &str) -> Result<Command, ServiceError> {
        let program = self
            .shell
            .as_ref()
            .filter(|shell| shell.name == engine)
            .ok_or_else(|| {
                ServiceError::new(
                    "E_SERVICE_UNAVAILABLE",
                    "the requested shell is unavailable on this host",
                )
            })?;
        let mut command = Command::new(&program.path);
        if engine == "pwsh" {
            let script =
                format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); {script}");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ]);
        } else {
            command.args(["--noprofile", "--norc", "-c", script]);
        }
        Ok(command)
    }

    fn start(
        &mut self,
        owner: &str,
        session: &str,
        spec: StartArgs,
    ) -> Result<Value, ServiceError> {
        if self.processes.len() >= MAX_PROCESSES {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "too many active tool processes",
            ));
        }
        let entry = self.launch(owner, session, spec, None)?;
        let id = self.next_id;
        self.next_id += 1;
        self.processes.insert(id, entry);
        Ok(json!({ "id": id }))
    }

    pub(crate) fn launch(
        &self,
        owner: &str,
        session: &str,
        spec: StartArgs,
        on_completion: Option<ProcessObserver>,
    ) -> Result<RunningProcess, ServiceError> {
        let cwd = self.workspace_path(&spec.cwd)?;
        if !cwd.is_dir() {
            return Err(ServiceError::args(
                "/args/cwd",
                "working directory must be a directory",
            ));
        }
        let max_timeout = if on_completion.is_some() {
            crate::tool_jobs::JOB_LIFETIME_MS
        } else {
            120_000
        };
        if !(1..=max_timeout).contains(&spec.timeout_ms)
            || !(1..=MAX_STDOUT).contains(&spec.stdout_max)
            || !(1..=MAX_STDERR).contains(&spec.stderr_max)
        {
            return Err(ServiceError::args(
                "/args",
                "invalid process timeout or output allowance",
            ));
        }
        let mut command = match (spec.engine.as_str(), spec.argv, spec.command) {
            ("ripgrep", Some(argv), None) => self.search_command(&cwd, argv)?,
            ("pwsh" | "bash", None, Some(script)) => self.shell_command(&spec.engine, &script)?,
            _ => return Err(ServiceError::args("/args", "ambiguous process arguments")),
        };
        command
            .current_dir(&cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("DSH_SESSION_ID", session)
            .env("DSH_WORKSPACE", &self.workspace);
        for key in [
            "PATH",
            "SystemRoot",
            "ComSpec",
            "TEMP",
            "TMP",
            "LANG",
            "LC_ALL",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let child = spawn_prepared(command, &GuardLimits::sidecar_default()).map_err(|error| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("cannot launch tool process: {error}"),
            )
        })?;
        Ok(supervise(
            child,
            owner.to_owned(),
            Duration::from_millis(spec.timeout_ms),
            spec.stdout_max as usize,
            spec.stderr_max as usize,
            on_completion,
        ))
    }

    fn entry(
        &mut self,
        owner: &str,
        args: &Value,
    ) -> Result<(&mut RunningProcess, u64), ServiceError> {
        let id = args["id"]
            .as_u64()
            .ok_or_else(|| ServiceError::args("/args/id", "invalid process identity"))?;
        self.processes
            .get_mut(&id)
            .filter(|entry| entry.owner == owner)
            .map(|entry| (entry, id))
            .ok_or_else(|| {
                ServiceError::new(
                    "E_SERVICE_DENIED",
                    "process does not belong to the active call",
                )
            })
    }
}

impl HostService for ToolProcessService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let owner = context.tool_request_id.as_deref().ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_DENIED",
                "tool process must be bound to an active call",
            )
        })?;
        if method == "start" {
            let session = context
                .session_id
                .as_deref()
                .ok_or_else(|| ServiceError::args("/session_id", "missing session"))?;
            let args = serde_json::from_value(args.clone())
                .map_err(|error| ServiceError::args("/args", error.to_string()))?;
            return self.start(owner, session, args);
        }
        let (entry, id) = self.entry(owner, args)?;
        match method {
            "poll" => Ok(match entry.poll()? {
                Some(output) => json!({
                    "done": true, "exit_code": output.status.code(),
                    "signal": signal(&output.status).or_else(|| (output.timed_out || output.aborted).then(|| "SIGKILL".to_owned())),
                    "timed_out": output.timed_out, "aborted": output.aborted,
                    "stdout_truncated": output.stdout.truncated, "stderr_truncated": output.stderr.truncated,
                }),
                None => json!({ "done": false }),
            }),
            "read" => {
                let output = entry.poll()?.ok_or_else(|| {
                    ServiceError::new("E_SERVICE_UNAVAILABLE", "process has not settled")
                })?;
                let stream = if args["stream"] == "stderr" {
                    &output.stderr
                } else {
                    &output.stdout
                };
                let offset = args["offset"]
                    .as_u64()
                    .and_then(|offset| usize::try_from(offset).ok())
                    .filter(|offset| stream.text.is_char_boundary(*offset))
                    .ok_or_else(|| {
                        ServiceError::args("/args/offset", "invalid UTF-8 output offset")
                    })?;
                let mut end = offset.saturating_add(PAGE_BYTES).min(stream.text.len());
                while !stream.text.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(
                    json!({ "text": &stream.text[offset..end], "next_offset": end, "end": end == stream.text.len() }),
                )
            }
            "release" => {
                self.processes.remove(&id);
                Ok(json!({}))
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown process method {method}"),
            )),
        }
    }

    fn release_tool_call(&mut self, request_id: &str) {
        self.processes
            .retain(|_, process| process.owner != request_id);
    }
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    json!({ "type": "object", "additionalProperties": false, "required": required, "properties": properties })
}

fn capture(
    mut stream: impl Read + Send + 'static,
    limit: usize,
) -> thread::JoinHandle<io::Result<Captured>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        let mut buffer = [0u8; 8192];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let retain = count.min(limit - bytes.len());
            bytes.extend_from_slice(&buffer[..retain]);
            truncated |= retain < count;
        }
        Ok(Captured {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            truncated,
        })
    })
}

fn supervise(
    mut child: GuardedChild,
    owner: String,
    timeout: Duration,
    stdout_max: usize,
    stderr_max: usize,
    on_completion: Option<ProcessObserver>,
) -> RunningProcess {
    let stdout = capture(
        child.child_mut().stdout.take().expect("piped stdout"),
        stdout_max,
    );
    let stderr = capture(
        child.child_mut().stderr.take().expect("piped stderr"),
        stderr_max,
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    let (send, settled) = mpsc::channel();
    thread::spawn(move || {
        let deadline = Instant::now() + timeout;
        let settle = loop {
            let aborted = cancelled.load(Ordering::Acquire);
            let timed_out = Instant::now() >= deadline;
            if aborted || timed_out {
                terminate(&mut child);
                break child.wait().map(|status| (status, timed_out, aborted));
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok((status, false, false)),
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => break Err(error),
            }
        };
        // Descendants can keep the output pipes open after the foreground process exits.
        terminate(&mut child);
        let result =
            settle
                .map_err(|error| error.to_string())
                .and_then(|(status, timed_out, aborted)| {
                    Ok(ProcessOutput {
                        status,
                        timed_out,
                        aborted,
                        stdout: stdout
                            .join()
                            .map_err(|_| "stdout reader failed")?
                            .map_err(|error| error.to_string())?,
                        stderr: stderr
                            .join()
                            .map_err(|_| "stderr reader failed")?
                            .map_err(|error| error.to_string())?,
                    })
                });
        if let Some(observer) = on_completion {
            observer(&result);
        }
        let _ = send.send(result);
    });
    RunningProcess {
        owner,
        cancel,
        settled,
        output: None,
    }
}

fn terminate(child: &mut GuardedChild) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
}

fn signal(status: &ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|signal| match signal {
            2 => "SIGINT".to_owned(),
            9 => "SIGKILL".to_owned(),
            15 => "SIGTERM".to_owned(),
            signal => format!("signal {signal}"),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}
