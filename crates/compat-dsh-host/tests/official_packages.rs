//! G3 官方包验收（RPC-L2-501..）：**未修改官方包**在真实 v2 sidecar 下的装载/往返/错误面。
//!
//! 组合（真实 `sidecar/dsh-official/sidecar-dsh.mjs` + vendored node_modules）：
//! - `@deepseek-ai/dsh-tool-working-directory`（来自 `.tmp/dsh-tools-all`，测试时复制进
//!   sidecar node_modules，源码不改）：装载绿 + `execute` 的 `exec.agent` 债务证据；
//! - `@deepseek-ai/dsh-storage-domain`（未修改）：`defineDomain`/`domainTable`(zod) +
//!   `DomainFacility` → 我们的 `storage-kv` backend（Rust 介质）真实往返；
//! - 测试专用 driver（`sidecar/dsh-official/lib/official-l2-driver.mjs`）：按官方签名调用
//!   `workingDirectory` 与 `storageDomain`，把证据作为 tool.result 返回。
//!
//! 缺 node / vendored node_modules / `.tmp/dsh-tools-all` 时：默认 `#[ignore]`（cargo 记
//! `0 passed; 6 ignored`），由 `tools/official-l2-gate.ps1` 显式判定；**绝不静默 SKIP 当 pass**。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{
    EventPage, HostOptions, HostServiceRouter, ServiceError, SessionAccess, SessionEventView,
    SessionHeaderView, SessionLogService, SessionProjectionsService, SidecarHost, StorageKvService,
    WorkingDirectoryService, SESSION_LOG_V1, SESSION_PROJECTIONS_V1, STORAGE_KV_V1,
    WORKING_DIRECTORY_V1,
};
#[cfg(unix)]
use dshr_compat_dsh_host::{
    ShellProgram, ToolJobService, ToolProcessService, ToolTerminalService, CAP_AGENT_TOOL_CONTEXT,
    CAP_BACKGROUND_JOBS, CAP_SHELL_PROCESS, CAP_TERMINAL_PROCESS, TOOL_JOBS_V1, TOOL_PROCESS_V1,
    TOOL_TERMINALS_V1,
};
use dshr_compat_dsh_protocol::{Envelope, CAP_SERVICE_PROXY, PROTOCOL_MAX};
use dshr_tool_runtime::{ApprovalBroker, ApprovalOutcome};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).expect("repo root")
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "dshr-official-l2-{label}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn norm(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("canonicalize");
    let text = canonical.to_string_lossy().into_owned();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 把 `.tmp/dsh-tools-all` 里的官方工具包放进 sidecar 的 node_modules（仅运行期，gitignored）。
/// 返回 `false` = 该验收路径不可用（调用方 panic，**不静默跳过**）。
///
/// 测试并行运行时用一个进程内锁串行化复制，避免两个测试同时 copy 同一目录造成半成品。
fn ensure_official_tool_packages() -> bool {
    static COPY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = COPY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for name in ["dsh-tool-working-directory", "dsh-tool-todo"] {
        let source = repo_root().join(".tmp/dsh-tools-all").join(name);
        if !source.is_dir() {
            return false;
        }
        let destination = repo_root()
            .join("sidecar/dsh-official/node_modules/@deepseek-ai")
            .join(name);
        if destination.join("lib/index.js").exists() {
            continue;
        }
        if let Err(error) = copy_dir_all(&source, &destination) {
            eprintln!("official packages: 复制 {name} 到 node_modules 失败: {error}");
            return false;
        }
        if !destination.join("lib/index.js").exists() {
            return false;
        }
    }
    true
}

fn file_url(path: &Path) -> String {
    let absolute = std::path::absolute(path).expect("absolute path");
    let text = absolute.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[derive(Clone)]
struct FakeSession {
    header: SessionHeaderView,
    events: Vec<SessionEventView>,
}

#[derive(Default)]
struct FakeAccess {
    sessions: Mutex<BTreeMap<String, FakeSession>>,
}

impl FakeAccess {
    fn with_session(session_id: &str, cwd: &str) -> Arc<Self> {
        let access = Arc::new(Self::default());
        access.add_session(session_id, cwd);
        access
    }

    fn add_session(&self, session_id: &str, cwd: &str) {
        self.sessions.lock().expect("sessions").insert(
            session_id.to_owned(),
            FakeSession {
                header: SessionHeaderView {
                    id: session_id.to_owned(),
                    cwd: Some(cwd.to_owned()),
                    created_at: "2026-11-14T00:00:00Z".to_owned(),
                    parent_session_id: None,
                    delegation_depth: 0,
                },
                events: Vec::new(),
            },
        );
    }

    /// 测试侧读取已落库事件（`events.append` 断言用）。
    fn events(&self, session_id: &str) -> Vec<SessionEventView> {
        self.sessions
            .lock()
            .expect("sessions")
            .get(session_id)
            .map(|session| session.events.clone())
            .unwrap_or_default()
    }
}

impl SessionAccess for FakeAccess {
    fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError> {
        self.sessions
            .lock()
            .expect("sessions")
            .get(session_id)
            .map(|session| session.header.clone())
            .ok_or_else(|| {
                ServiceError::new(
                    "E_SERVICE_UNAVAILABLE",
                    format!("unknown session `{session_id}`"),
                )
            })
    }

    fn read_events(
        &self,
        session_id: &str,
        from_seq: u64,
        max_events: usize,
    ) -> Result<EventPage, ServiceError> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(session_id).ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("unknown session `{session_id}`"),
            )
        })?;
        let start = session
            .events
            .iter()
            .position(|event| event.seq >= from_seq)
            .unwrap_or(session.events.len());
        let events: Vec<SessionEventView> = session.events[start..]
            .iter()
            .take(max_events)
            .cloned()
            .collect();
        let eof = start + events.len() >= session.events.len();
        let next_seq = events.last().map(|event| event.seq + 1).unwrap_or(from_seq);
        Ok(EventPage {
            events,
            next_seq,
            eof,
        })
    }

    fn append_event(
        &self,
        session_id: &str,
        event_type: &str,
        data: Value,
    ) -> Result<u64, ServiceError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(session_id).ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("unknown session `{session_id}`"),
            )
        })?;
        let seq = session
            .events
            .last()
            .map(|event| event.seq + 1)
            .unwrap_or(1);
        session.events.push(SessionEventView {
            seq,
            event_type: event_type.to_owned(),
            data,
        });
        Ok(seq)
    }

    fn latest_working_directory(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(session_id)?;
        session
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == "working-directory/change")
            .and_then(|event| event.data.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

struct Fixture {
    host: SidecarHost,
    workspace: PathBuf,
    target: PathBuf,
    storage: PathBuf,
    /// 测试侧保留的 session access，用来核对 `events.append` 落库。
    access: Arc<FakeAccess>,
    /// 官方写工具的审批 broker（507 用）。
    broker: Arc<ApprovalBroker>,
}

impl Fixture {
    fn cleanup(self) {
        drop(self.host);
        let _ = std::fs::remove_dir_all(&self.workspace);
        let _ = std::fs::remove_dir_all(&self.target);
        let _ = std::fs::remove_dir_all(&self.storage);
    }
}

fn official_plugins(driver_url: &str, include_todo: bool) -> String {
    let mut plugins = vec![
        json!({
            "name": "@deepseek-ai/dsh-storage-domain",
            "config": { "backend": "storage-kv" },
        }),
        json!({
            "name": "@deepseek-ai/dsh-tool-working-directory",
            "config": {},
        }),
        // 官方 fs 工具：只开 read，验证未修改工具的 execute 走 sidecar 的 fs/systemPrompt seam。
        json!({
            "name": "@deepseek-ai/dsh-tool-fs",
            "inject": ["tools", "fs", "systemPrompt"],
            "config": {
                "read": true,
                "readImage": false,
                "write": true,
                "edit": true,
                "applyPatch": true
            },
        }),
    ];
    if include_todo {
        // 官方 dsh-tool-todo：inject ["tools","sessionProjections"]（P1-5 装载绿证据）。
        plugins.push(json!({
            "name": "@deepseek-ai/dsh-tool-todo",
            "config": { "allowParallelInProgress": true },
        }));
    }
    plugins.push(json!({ "name": driver_url }));
    Value::Array(plugins).to_string()
}

fn spawn_official_sidecar(label: &str, include_todo: bool) -> Option<Fixture> {
    if !node_available() {
        panic!("#[{label}] official-package gate 需要 node（PATH 上找不到）；先运行 tools/official-l2-gate.ps1 的环境检查");
    }
    let sidecar_dir = repo_root().join("sidecar/dsh-official");
    if !sidecar_dir.join("sidecar-dsh.mjs").exists() || !sidecar_dir.join("node_modules").exists() {
        panic!("#[{label}] official-package gate 需要 vendored 官方 sidecar：先运行 node tools/dsh-vendor.mjs");
    }
    if !ensure_official_tool_packages() {
        panic!("#[{label}] official-package gate 需要 .tmp/dsh-tools-all/dsh-tool-working-directory：请从本机 DSH 安装抽取（见 docs/m3-l2-services.md §9）");
    }
    let driver = sidecar_dir.join("lib/official-l2-driver.mjs");
    if !driver.exists() {
        panic!("#[{label}] 缺少 sidecar/dsh-official/lib/official-l2-driver.mjs");
    }
    Some(spawn_fixture(
        label,
        &sidecar_dir,
        official_plugins(&file_url(&driver), include_todo),
    ))
}

fn spawn_fixture(label: &str, sidecar_dir: &Path, plugins: String) -> Fixture {
    spawn_fixture_with(label, sidecar_dir, plugins, |_, _, _, _, _| {})
}

fn spawn_fixture_with(
    label: &str,
    sidecar_dir: &Path,
    plugins: String,
    configure: impl FnOnce(
        &Path,
        &Arc<FakeAccess>,
        &mut HostServiceRouter,
        &mut Vec<String>,
        &mut HostOptions,
    ),
) -> Fixture {
    let workspace = temp_dir(&format!("{label}-ws"));
    let target = temp_dir(&format!("{label}-target"));
    let storage = temp_dir(&format!("{label}-storage"));
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let mut router = HostServiceRouter::new(
        "node-placeholder",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .expect("router");
    router
        .register(Box::new(
            StorageKvService::open(&storage).expect("storage service"),
        ))
        .expect("register storage-kv");
    router
        .register(Box::new(WorkingDirectoryService::new(
            access.clone(),
            &workspace,
        )))
        .expect("register working-directory");
    router
        .register(Box::new(SessionLogService::new(access.clone())))
        .expect("register session-log");
    router
        .register(Box::new(SessionProjectionsService::new(access.clone())))
        .expect("register session-projections");
    let mut inject = vec![
        STORAGE_KV_V1.to_owned(),
        WORKING_DIRECTORY_V1.to_owned(),
        SESSION_LOG_V1.to_owned(),
        SESSION_PROJECTIONS_V1.to_owned(),
    ];
    let broker = ApprovalBroker::new();
    let fs_write =
        Arc::new(FsWriteService::open(&workspace, broker.clone()).expect("fs write service opens"));
    let mut options = HostOptions::default()
        .with_env("DSHR_DSH_PLUGINS", plugins)
        .with_env("DSHR_WORKSPACE", workspace.to_string_lossy())
        .with_registration_window(Duration::from_secs(8))
        .with_fs_write(fs_write);
    configure(&workspace, &access, &mut router, &mut inject, &mut options);
    router.register_plugin_inject("dsh-official", inject);
    let options = options.with_host_services(Arc::new(Mutex::new(router)));
    let script = sidecar_dir.join("sidecar-dsh.mjs");
    let host = SidecarHost::spawn_with(&script, &script, options)
        .unwrap_or_else(|error| panic!("#[{label}] sidecar 启动失败: {error}"));
    Fixture {
        host,
        workspace,
        target,
        storage,
        access,
        broker,
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires rc.5 Bash package and /bin/bash"]
fn stage2_official_bash_runs_on_linux() {
    let sidecar = std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar");
    let mut fixture = spawn_fixture_with(
        "rc5-bash",
        Path::new(&sidecar),
        json!([
            { "name": "@deepseek-ai/dsh-tool-bash", "config": { "enableRunInBackground": false } },
            { "name": "@deepseek-ai/dsh-tool-working-directory" },
        ])
        .to_string(),
        |workspace, _, router, inject, options| {
            let service = ToolProcessService::new(
                workspace,
                None,
                Some(ShellProgram {
                    name: "bash",
                    path: "/bin/bash".into(),
                }),
            )
            .unwrap();
            router.register(Box::new(service)).unwrap();
            inject.push(TOOL_PROCESS_V1.to_owned());
            options.extra_caps.push(CAP_SHELL_PROCESS.to_owned());
        },
    );
    let directory = fixture.workspace.join("child");
    std::fs::create_dir(&directory).unwrap();
    let result = invoke(
        &mut fixture.host,
        "working_directory",
        json!({ "cd": "child" }),
    );
    assert_eq!(result.payload["is_error"], false, "{result:?}");
    let result = invoke(
        &mut fixture.host,
        "bash",
        json!({
            "description": "Check bash exit and output",
            "command": "printf '%s\\n' \"$DSH_SESSION_ID\"; printf '%s\\n' 'stderr marker' >&2; exit 7",
        }),
    );
    assert_eq!(result.payload["is_error"], false, "{result:?}");
    let value = &result.payload["value"];
    assert_eq!(value["exitCode"], 7);
    assert_eq!(value["stdout"]["text"].as_str().unwrap().trim(), "s-probe");
    assert_eq!(
        value["stderr"]["text"].as_str().unwrap().trim(),
        "stderr marker"
    );
    assert_eq!(value["cwd"], norm(&directory));
    let timed = invoke(
        &mut fixture.host,
        "bash",
        json!({
            "description": "Validate foreground timeout",
            "command": "sleep 10",
            "timeoutMs": 100,
        }),
    );
    assert_eq!(timed.payload["value"]["timedOut"], true, "{timed:?}");
    assert_eq!(timed.payload["value"]["signal"], "SIGKILL");
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = cancelled.clone();
    let marker = directory.join("started.txt");
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, std::sync::atomic::Ordering::Release);
        marker.exists()
    });
    let result = fixture.host.invoke_as_cancellable_in_session(
        "bash-cancel",
        Some("s-probe"),
        "bash",
        json!({
            "description": "Cancel a running Bash command",
            "command": "printf started > started.txt; sleep 10; printf finished > finished.txt",
        }),
        Duration::from_secs(15),
        Some(&cancelled),
    );
    assert!(
        watcher.join().unwrap(),
        "Bash did not reach its running state"
    );
    assert!(match result {
        Err(dshr_compat_dsh_host::HostError::Cancelled(_)) => true,
        Ok(result) =>
            result.terminal.kind == "tool.aborted"
                || result
                    .terminal
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == "E_CANCELLED"),
        _ => false,
    });
    assert!(!directory.join("finished.txt").exists());
    fixture.cleanup();
}

#[cfg(unix)]
#[test]
#[ignore = "requires rc.5 Bash and job packages"]
fn stage4_official_bash_background_jobs_return_the_host_output() {
    let sidecar = std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar");
    let mut fixture = spawn_fixture_with(
        "rc5-bash-jobs",
        Path::new(&sidecar),
        json!([
            { "name": "@deepseek-ai/dsh-tool-jobs", "config": { "completionDelivery": "quiet" } },
            { "name": "@deepseek-ai/dsh-tool-bash", "config": { "promoteOnTimeout": false } },
        ])
        .to_string(),
        |workspace, _, router, inject, options| {
            let bash = ShellProgram {
                name: "bash",
                path: "/bin/bash".into(),
            };
            let processes = ToolProcessService::new(workspace, None, Some(bash.clone())).unwrap();
            let jobs = ToolJobService::new(workspace, workspace.join("job-proof"), bash).unwrap();
            router.register(Box::new(processes)).unwrap();
            router.register(Box::new(jobs)).unwrap();
            inject.extend([TOOL_PROCESS_V1.to_owned(), TOOL_JOBS_V1.to_owned()]);
            options.extra_caps.extend([
                CAP_AGENT_TOOL_CONTEXT.to_owned(),
                CAP_SHELL_PROCESS.to_owned(),
                CAP_BACKGROUND_JOBS.to_owned(),
            ]);
        },
    );
    let started = invoke(
        &mut fixture.host,
        "bash",
        json!({
            "command": "printf 'linux-background-proof\\n'",
            "description": "Check background output", "run_in_background": true,
        }),
    );
    assert!(!started.is_error(), "{started:?}");
    let id = started.payload["value"]["jobId"].as_str().unwrap();
    let claimed = invoke(
        &mut fixture.host,
        "job_output",
        json!({
            "job_id": id, "wait": true, "timeout_ms": 3000,
        }),
    );
    assert_eq!(
        claimed.payload["value"]["job"]["status"], "completed",
        "{claimed:?}"
    );
    assert!(claimed.payload["value"]["text"]
        .as_str()
        .unwrap()
        .contains("linux-background-proof"));
    let repeated = invoke(&mut fixture.host, "job_output", json!({ "job_id": id }));
    assert_eq!(repeated.payload["value"]["text"], "", "{repeated:?}");
    fixture.cleanup();
}

#[cfg(unix)]
#[test]
#[ignore = "requires rc.5 Bash terminal packages and the installed POSIX PTY backend"]
fn stage4_official_bash_persistent_uses_a_real_pty() {
    let sidecar = std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar");
    let mut fixture = spawn_fixture_with(
        "rc5-bash-persistent",
        Path::new(&sidecar),
        json!([
            { "name": "@deepseek-ai/dsh-tool-terminal", "config": { "enableRunInBackground": false } },
            { "name": "@deepseek-ai/dsh-tool-bash-persistent", "config": { "timeoutMs": 8_000 } },
        ]).to_string(),
        |workspace, access, router, inject, options| {
            router.register(Box::new(ToolTerminalService::new(workspace, access.clone()).unwrap())).unwrap();
            inject.push(TOOL_TERMINALS_V1.to_owned());
            options.extra_caps.extend([CAP_TERMINAL_PROCESS.to_owned(), CAP_AGENT_TOOL_CONTEXT.to_owned()]);
            options.env.push(("DSHR_TERMINAL_SHELL".to_owned(), "/bin/bash".to_owned()));
        },
    );
    let first = invoke(
        &mut fixture.host,
        "bash",
        json!({
            "command": "test -t 0 && marker=retained-in-bash",
        }),
    );
    assert!(!first.is_error(), "{first:?}");
    let second = invoke(
        &mut fixture.host,
        "bash",
        json!({
            "command": r#"printf '%s\n' "$marker""#,
        }),
    );
    assert!(!second.is_error(), "{second:?}");
    assert!(
        second.payload["value"]
            .as_str()
            .is_some_and(|text| text.contains("retained-in-bash")),
        "{second:?}"
    );
    let list = invoke(&mut fixture.host, "terminal_list", json!({}));
    let rows = list.payload["value"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{list:?}");
    let terminal = rows[0]["sessionId"].as_str().unwrap();
    fixture
        .access
        .add_session("s-outsider", &norm(&fixture.workspace));
    let foreign = fixture
        .host
        .invoke_as_cancellable_in_session(
            "wrong-terminal",
            Some("s-outsider"),
            "terminal_read",
            json!({ "sessionId": terminal }),
            Duration::from_secs(15),
            None,
        )
        .unwrap();
    assert!(foreign.terminal.is_error(), "{:?}", foreign.terminal);
    let closed = invoke(
        &mut fixture.host,
        "terminal_close",
        json!({ "sessionId": terminal }),
    );
    assert_eq!(closed.payload["value"]["outcome"], "closed", "{closed:?}");
    assert!(fixture
        .access
        .events("s-probe")
        .iter()
        .any(|event| event.event_type == "terminal/closed"));
    fixture.cleanup();
}

fn registration_names(host: &SidecarHost) -> Vec<String> {
    let mut names: Vec<String> = host
        .registrations()
        .iter()
        .map(|registration| registration.name.clone())
        .collect();
    names.sort();
    names
}

fn invoke(host: &mut SidecarHost, tool: &str, arguments: Value) -> Envelope {
    let outcome = host
        .invoke(tool, arguments, Duration::from_secs(20))
        .unwrap_or_else(|error| panic!("invoke `{tool}` failed: {error}"));
    assert_eq!(
        outcome.terminal.kind, "tool.result",
        "{tool} did not settle: {:?}",
        outcome.terminal
    );
    outcome.terminal
}

#[test]
#[ignore = "requires the rc.5 fixture prepared by tools/official-rc5-gate.ps1"]
fn rc5_files_use_the_bound_session_and_host_write_service() {
    let sidecar = std::env::var_os("DSHR_OFFICIAL_RC5_DIR").expect("isolated rc.5 sidecar");
    let runtime = std::env::var("DSHR_OFFICIAL_RC5_RUNTIME").expect("rc.5 primary-runtime");
    let plugins = json!([
        { "name": "@deepseek-ai/dsh-tool-workspace-dependencies", "config": { "source": runtime } },
        { "name": "@deepseek-ai/dsh-tool-fs", "config": { "read": true, "readImage": true } },
        { "name": "@deepseek-ai/dsh-tool-todo", "config": { "allowParallelInProgress": true } },
        { "name": "@deepseek-ai/dsh-tool-str-replace-editor" },
    ]);
    let mut fixture = spawn_fixture("rc5-files", Path::new(&sidecar), plugins.to_string());
    let python = invoke(&mut fixture.host, "load_workspace_dependencies", json!({}));
    assert_eq!(python.payload["is_error"], false, "{python:?}");
    assert!(python.payload.to_string().to_lowercase().contains("python"));
    assert!(!registration_names(&fixture.host)
        .iter()
        .any(|name| name == "read_image"));

    let first = fixture.workspace.join("one");
    let second = fixture.workspace.join("two");
    for (directory, marker) in [(&first, "first"), (&second, "second")] {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(directory.join("marker.txt"), marker).unwrap();
    }
    std::fs::write(fixture.workspace.join("marker.txt"), "workspace root").unwrap();
    fixture
        .access
        .add_session("s-other", &norm(&fixture.workspace));
    let roster = fixture
        .host
        .sync_presets(&json!({
            "cordis": [{ "name": "@deepseek-ai/dsh-tool-working-directory" }]
        }))
        .unwrap();
    assert!(roster["cordis"]["error"].is_null(), "{roster}");
    for (session, directory, marker) in
        [("s-probe", &first, "first"), ("s-other", &second, "second")]
    {
        fixture.host.bind_preset(session, "cordis", None).unwrap();
        let result = invoke_session(
            &mut fixture.host,
            session,
            "working_directory",
            json!({ "cd": directory }),
        );
        assert_eq!(result.payload["is_error"], false, "{result:?}");
        assert_eq!(result.payload["value"]["cwd"], norm(directory));
        let read = invoke_session(
            &mut fixture.host,
            session,
            "read",
            json!({ "file_path": "marker.txt" }),
        );
        assert_eq!(
            read.payload["value"]["lines"][0]["text"], marker,
            "{read:?}"
        );
    }

    let todo = invoke(
        &mut fixture.host,
        "todo_write",
        json!({
            "todos": [{ "content": "verify rc.5", "status": "completed" }]
        }),
    );
    assert_eq!(todo.payload["is_error"], false, "{todo:?}");
    assert!(fixture.access.events("s-probe").iter().any(|event| {
        event.event_type == "todo/write" && event.data["todos"][0]["status"] == "completed"
    }));

    let listing = invoke(
        &mut fixture.host,
        "str_replace_editor",
        json!({ "command": "view", "path": first }),
    );
    assert_eq!(listing.payload["is_error"], false, "{listing:?}");
    assert!(
        listing.payload.to_string().contains("marker.txt"),
        "{listing:?}"
    );
    let editor_path = first.join("editor.txt");
    for (args, decision, expected) in [
        (
            json!({ "command": "create", "path": editor_path, "file_text": "hello" }),
            ApprovalOutcome::AllowOnce,
            "hello",
        ),
        (
            json!({ "command": "str_replace", "path": editor_path, "old_str": "hello", "new_str": "changed" }),
            ApprovalOutcome::Deny,
            "hello",
        ),
        (
            json!({ "command": "str_replace", "path": editor_path, "old_str": "hello", "new_str": "changed" }),
            ApprovalOutcome::AllowOnce,
            "changed",
        ),
        (
            json!({ "command": "insert", "path": editor_path, "insert_line": 1, "new_str": "next" }),
            ApprovalOutcome::AllowOnce,
            "changed\nnext",
        ),
    ] {
        let denied = decision == ApprovalOutcome::Deny;
        let resolver = resolve_one_approval(fixture.broker.clone(), decision);
        let result = invoke(&mut fixture.host, "str_replace_editor", args);
        resolver.join().unwrap();
        assert_eq!(result.payload["is_error"], denied, "{result:?}");
        assert_eq!(std::fs::read_to_string(&editor_path).unwrap(), expected);
    }

    let resolver = resolve_one_approval(fixture.broker.clone(), ApprovalOutcome::AllowOnce);
    let result = invoke(
        &mut fixture.host,
        "write",
        json!({ "file_path": "relative.txt", "content": "one" }),
    );
    resolver.join().unwrap();
    assert_eq!(result.payload["is_error"], false, "{result:?}");
    assert_eq!(
        std::fs::read_to_string(first.join("relative.txt")).unwrap(),
        "one"
    );
    assert!(!fixture.workspace.join("relative.txt").exists());
    let result = invoke(
        &mut fixture.host,
        "write",
        json!({ "file_path": fixture.target.join("outside.txt"), "content": "denied" }),
    );
    assert_eq!(result.payload["is_error"], true, "{result:?}");
    assert!(!fixture.target.join("outside.txt").exists());
    let stop = std::sync::atomic::AtomicBool::new(true);
    let cancelled = fixture.host.invoke_as_cancellable_in_session(
        "cancelled-write",
        Some("s-probe"),
        "write",
        json!({ "file_path": "cancelled.txt", "content": "must not be written" }),
        Duration::from_secs(5),
        Some(&stop),
    );
    assert!(
        match cancelled {
            Err(dshr_compat_dsh_host::HostError::Cancelled(_)) => true,
            Ok(result) =>
                result.terminal.kind == "tool.aborted"
                    || result
                        .terminal
                        .error
                        .as_ref()
                        .is_some_and(|error| error.code == "E_CANCELLED"),
            _ => false,
        },
        "cancelled write must abort"
    );
    assert!(!first.join("cancelled.txt").exists());
    fixture.cleanup();
}

fn invoke_session(host: &mut SidecarHost, session: &str, tool: &str, arguments: Value) -> Envelope {
    host.invoke_in_session(
        tool,
        Some(session),
        tool,
        arguments,
        Duration::from_secs(10),
    )
    .unwrap()
    .terminal
}

fn resolve_one_approval(
    broker: Arc<ApprovalBroker>,
    decision: ApprovalOutcome,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(pending) = broker.pending().first() {
                broker.resolve(&pending.call_id, decision);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("official tool did not reach host approval");
    })
}

/// RPC-L2-201（P1-5）：未修改官方 `@deepseek-ai/dsh-tool-todo` 在 v2 sidecar 下装载绿：
/// `inject ["tools","sessionProjections"]` 满足（Sidecar 提供投影注册表 shim），
/// `todo_write` 注册成功、无 plugin load 失败。`execute` 受 exec.agent 债务限制（m3 口径）。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_201_official_tool_todo_loads_green() {
    let mut fixture = spawn_official_sidecar("201", true)
        .expect("spawn_official_sidecar panics on missing fixtures; expect only for Option");
    assert_eq!(fixture.host.negotiated_protocol(), 2);
    assert!(fixture.host.host_services_active());
    let names = registration_names(&fixture.host);
    assert!(
        names.iter().any(|name| name == "todo_write"),
        "官方 dsh-tool-todo 未注册 todo_write: {names:?}"
    );
    let load_errors: Vec<&String> = fixture
        .host
        .boot_errors()
        .iter()
        .filter(|message| message.contains("failed to load"))
        .collect();
    assert!(
        load_errors.is_empty(),
        "官方 todo 包装载失败: {load_errors:?}"
    );

    // M4 W6：官方 todo 的 execute 现在经 exec.agent.session.append → session-log/v1
    // events.append 写入宿主会话，不再被 exec.agent 债务阻塞。
    let result = invoke(
        &mut fixture.host,
        "todo_write",
        json!({ "todos": [
            { "content": "write official todo", "status": "pending" },
            { "content": "verify append", "status": "in_progress" }
        ]}),
    );
    assert_eq!(
        result.payload["is_error"], false,
        "官方 todo_write 失败: {result:?}"
    );
    let events = fixture.access.events("s-probe");
    assert!(
        events.iter().any(|event| event.event_type == "todo/write"
            && event.data["todos"]
                .as_array()
                .is_some_and(|todos| todos.len() == 2)),
        "events.append 未落库 todo/write: {events:?}"
    );
    fixture.cleanup();
}

/// RPC-L2-501：官方工具包装载绿（v2）——`working_directory` 与 driver 都收到 tool.register，
/// 无 plugin load 失败；握手协商到 v2。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_501_official_working_directory_tool_loads_green() {
    let Some(fixture) = spawn_official_sidecar("501", false) else {
        return;
    };
    assert_eq!(fixture.host.negotiated_protocol(), 2);
    assert!(fixture.host.host_services_active());
    let names = registration_names(&fixture.host);
    assert!(
        names.iter().any(|name| name == "working_directory"),
        "官方 dsh-tool-working-directory 未注册工具: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "l2_official_roundtrip"),
        "L2 driver 未注册: {names:?}"
    );
    let load_errors: Vec<&String> = fixture
        .host
        .boot_errors()
        .iter()
        .filter(|message| message.contains("failed to load"))
        .collect();
    assert!(load_errors.is_empty(), "官方包装载失败: {load_errors:?}");
    fixture.cleanup();
}

/// RPC-L2-502（M4 W6）：官方 `working_directory` 的 `execute` 经最小 `exec.agent`
/// facade 闭环——不再返回 "requires an agent Session"，而是真实执行并返回 cwd。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_502_official_tool_execute_closes_with_agent_facade() {
    let Some(mut fixture) = spawn_official_sidecar("502", false) else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "working_directory",
        json!({ "cd": fixture.target.to_string_lossy() }),
    );
    assert!(
        envelope.error.is_none(),
        "官方 working_directory execute 应成功: {envelope:?}"
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "官方工具执行失败: {payload}");
    assert_eq!(
        payload["value"]["cwd"],
        json!(norm(&fixture.target)),
        "execute 应返回目标 cwd: {payload}"
    );
    assert_eq!(
        fixture.host.negotiated_protocol(),
        2,
        "execute 成功后仍保持 v2 会话"
    );
    let roster = fixture
        .host
        .sync_presets(&json!({
            "cordis": [{ "name": "@deepseek-ai/dsh-tool-working-directory" }]
        }))
        .unwrap();
    assert!(roster["cordis"]["error"].is_null(), "{roster}");
    fixture.host.bind_preset("s-probe", "cordis", None).unwrap();
    let tool = fixture
        .host
        .preset_tool_definitions("s-probe")
        .into_iter()
        .find(|tool| tool.name == "working_directory")
        .unwrap();
    let result = fixture.host.invoke_batch(
        &[dshr_compat_dsh_host::BatchInvokeCall::new(
            "cwd-preset",
            "working_directory",
            json!({"cd": fixture.workspace.to_string_lossy()}),
            Duration::from_secs(5),
        )
        .with_session("s-probe")
        .with_binding(tool.origin)],
        1,
        None,
    );
    let terminal = &result[0].as_ref().unwrap().terminal;
    assert!(!terminal.is_error(), "{terminal:?}");
    assert_eq!(
        terminal.payload["value"]["cwd"],
        json!(norm(&fixture.workspace))
    );
    fixture.cleanup();
}

/// RPC-L2-503：官方 `dsh-working-directory` 签名的服务调用（get/ensure/set）经 shim →
/// host.service.request 真实往返（driver 触发，不依赖 execute/agent）。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_503_working_directory_official_signature_round_trip() {
    let Some(mut fixture) = spawn_official_sidecar("503", false) else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "l2_official_roundtrip",
        json!({ "target": fixture.target.to_string_lossy() }),
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "driver 失败: {payload}");
    let evidence = &payload["value"]["working_directory"];
    assert_eq!(evidence["get"], norm(&fixture.workspace));
    assert_eq!(evidence["set"], norm(&fixture.target));
    assert_eq!(evidence["get_after_set"], norm(&fixture.target));
    assert_eq!(evidence["ensure"], norm(&fixture.target));
    assert_eq!(evidence["recovered"], false);
    fixture.cleanup();
}

/// RPC-L2-504：未修改官方 `dsh-storage-domain`（defineDomain/zod + DomainFacility）经
/// storage-kv backend → Rust `StorageKvService` 的真实 kv facet 往返 + `already-open` 错误面。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_504_official_storage_domain_round_trip() {
    let Some(mut fixture) = spawn_official_sidecar("504", false) else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "l2_official_roundtrip",
        json!({ "target": fixture.target.to_string_lossy() }),
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "driver 失败: {payload}");
    let storage = &payload["value"]["storage"];
    assert_eq!(
        storage["initial_global"],
        json!({ "theme": "dark" }),
        "未写 global 时必须回落到 schema initial: {storage}"
    );
    assert_eq!(storage["get"], json!({ "n": 1 }), "put/get 往返: {storage}");
    assert_eq!(storage["delete"], true);
    assert_eq!(storage["size_after_delete"], 1);
    assert_eq!(storage["global"], json!({ "theme": "light" }));
    assert_eq!(
        storage["duplicate_open_code"], "already-open",
        "官方 domain 错误面: {storage}"
    );
    assert_eq!(storage["closed"], true);

    // 数据落在宿主侧 Rust 介质（plugin_id=dsh-official），而不是 Sidecar 内存。
    let medium = fixture
        .storage
        .join("dsh-official")
        .join("official_acceptance.json");
    assert!(
        medium.exists(),
        "宿主侧 storage 介质缺失: {}",
        medium.display()
    );
    let file: Value =
        serde_json::from_slice(&std::fs::read(&medium).expect("read medium")).expect("medium json");
    assert_eq!(file["tables"]["items"]["b"]["n"], 2);
    assert_eq!(file["global"], json!({ "theme": "light" }));
    fixture.cleanup();
}

/// RPC-L2-506（M4 W6）：未修改官方 `@deepseek-ai/dsh-tool-fs` 的 `read` execute 闭环。
/// fs 工具不依赖 `exec.agent`，但依赖 sidecar 的 `fs`/`systemPrompt` seam；这里用只读配置
/// 验证真实读取，不碰写审批。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_506_official_fs_read_execute() {
    let mut fixture = spawn_official_sidecar("506", false)
        .expect("spawn_official_sidecar panics on missing fixtures; expect only for Option");
    let hello = fixture.workspace.join("hello.txt");
    std::fs::write(&hello, "hello official fs\n").expect("write fixture file");
    let envelope = invoke(
        &mut fixture.host,
        "read",
        json!({ "file_path": hello.to_string_lossy() }),
    );
    assert!(
        envelope.error.is_none(),
        "官方 read execute 失败: {envelope:?}"
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "{payload}");
    let lines = payload["value"]["lines"]
        .as_array()
        .expect("read value.lines");
    assert!(
        lines.iter().any(|line| line["text"]
            .as_str()
            .is_some_and(|text| text.contains("hello official fs"))),
        "read 未返回文件内容: {payload}"
    );
    fixture.cleanup();
}

/// RPC-L2-507（M4 W6）：未修改官方 `dsh-tool-fs` 的 `write` execute，经 Rust
/// `FsWriteService` 审批后落盘。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_507_official_fs_write_execute() {
    let mut fixture = spawn_official_sidecar("507", false)
        .expect("spawn_official_sidecar panics on missing fixtures; expect only for Option");
    let broker = fixture.broker.clone();
    let resolver = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let pending = broker.pending();
            if !pending.is_empty() {
                for item in pending {
                    broker.resolve(&item.call_id, ApprovalOutcome::AllowOnce);
                }
                return;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let envelope = invoke(
        &mut fixture.host,
        "write",
        json!({ "file_path": "written.txt", "content": "hello official write\n" }),
    );
    resolver.join().expect("approval resolver");
    assert!(
        envelope.error.is_none(),
        "官方 write execute 失败: {envelope:?}"
    );
    assert_eq!(
        envelope.payload["is_error"], false,
        "{:?}",
        envelope.payload
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("written.txt")).expect("written file"),
        "hello official write\n"
    );
    fixture.cleanup();
}

/// RPC-L2-505：`@deepseek-ai/dsh-storage` 是 **Cordis Service class**（没有插件 apply/inject），
/// 不能作为普通 Cordis plugin 装载；官方 kv facet 契约由它的消费者（storage-domain）验证。
/// 这条用例固化"官方 hub + Rust backend adapter"的边界证据。
#[test]
#[ignore = "requires .tmp/dsh-tools-all + vendored sidecar; run tools/official-l2-gate.ps1"]
fn rpc_l2_505_official_storage_is_a_service_class_not_a_plugin() {
    if !node_available() {
        panic!("rpc_l2_505 需要 node；先运行 tools/official-l2-gate.ps1 的环境检查");
    }
    let sidecar_dir = repo_root().join("sidecar/dsh-official");
    if !sidecar_dir
        .join("node_modules/@deepseek-ai/dsh-storage")
        .exists()
    {
        panic!("rpc_l2_505 需要 vendored dsh-storage：先运行 node tools/dsh-vendor.mjs");
    }
    let script = "import('@deepseek-ai/dsh-storage').then((m) => { console.log(JSON.stringify({ hasApply: typeof m.apply === 'function', hasInject: Array.isArray(m.inject), hasDefaultClass: typeof m.default === 'function' })) })";
    let output = std::process::Command::new("node")
        .args(["-e", script])
        .current_dir(&sidecar_dir)
        .output()
        .expect("run node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let shape: Value = serde_json::from_str(text.trim()).expect("node JSON output");
    assert_eq!(shape["hasApply"], false);
    assert_eq!(shape["hasInject"], false);
    assert_eq!(shape["hasDefaultClass"], true);
}

// RPC-REG-001
#[test]
#[ignore = "requires the official sidecar's vendored Node dependencies"]
fn scoped_tool_bindings_are_revoked_and_never_retargeted() {
    use dshr_compat_dsh_host::BatchInvokeCall;

    let root = repo_root();
    let script = root.join("sidecar/dsh-official/sidecar-dsh.mjs");
    let plugin = root.join("crates/compat-dsh-host/tests/fixtures/scoped-tool-plugin.mjs");
    let options = HostOptions::default()
        .with_env(
            "DSHR_DSH_PLUGINS",
            json!([{ "name": file_url(&plugin) }]).to_string(),
        )
        .with_registration_window(Duration::from_secs(2));
    let mut host = SidecarHost::spawn_with(&script, &plugin, options).unwrap();
    let binding = |host: &SidecarHost, session| {
        host.tool_definitions_at_scope(session)
            .into_iter()
            .find(|tool| tool.name == "scoped_echo")
            .unwrap()
    };
    let a = binding(&host, Some("s-1")).origin;
    let b = binding(&host, Some("s-2")).origin;
    let global = binding(&host, None).origin;
    let timeout = Duration::from_secs(5);
    let make_call = |session: &str| {
        BatchInvokeCall::new(format!("call-{session}"), "scoped_echo", json!({}), timeout)
            .with_session(session)
    };
    let label = |outcome: &dshr_compat_dsh_host::InvokeOutcome| {
        outcome.terminal.payload["value"]["label"].clone()
    };

    let result = host.invoke_batch(
        &[
            make_call("s-1").with_binding(a.clone()),
            make_call("s-2").with_binding(b.clone()),
            make_call("s-3").with_binding(global),
            make_call("s-2").with_binding(a.clone()),
        ],
        4,
        None,
    );
    assert_eq!(label(result[0].as_ref().unwrap()), "A");
    assert_eq!(label(result[1].as_ref().unwrap()), "B");
    assert_eq!(label(result[2].as_ref().unwrap()), "global");
    assert!(result[3]
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("E_TOOL_UNAVAILABLE"));

    let result = host
        .invoke("replace_scoped_echo", json!({}), timeout)
        .unwrap();
    assert_eq!(
        result.terminal.payload["value"],
        json!({ "before": "A", "duplicateRejected": true })
    );
    host.refresh_registrations().unwrap();
    let replacement = binding(&host, Some("s-1")).origin;
    assert_ne!(a, replacement);
    assert!(host.tool_definitions_at_scope(Some("s-3")).is_empty());

    let result = host.invoke_batch(
        &[
            make_call("s-1").with_binding(a),
            make_call("s-1").with_binding(replacement),
            make_call("s-2").with_binding(b),
        ],
        3,
        None,
    );
    assert!(result[0]
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("E_TOOL_UNAVAILABLE"));
    assert_eq!(label(result[1].as_ref().unwrap()), "A replacement");
    assert_eq!(label(result[2].as_ref().unwrap()), "B");

    host.retire_session("s-1").unwrap();
    assert!(host.tool_definitions_at_scope(Some("s-1")).is_empty());
    assert_eq!(
        label(
            &host
                .invoke_in_session("retired", Some("s-1"), "scoped_echo", json!({}), timeout)
                .unwrap()
        ),
        "global"
    );
}

// M3-PRESET-002 / M3-PRESET-003
#[test]
#[ignore = "requires the official sidecar's vendored Node dependencies"]
fn agent_presets_keep_bound_revisions_and_collect_retired_plugins() {
    let root = repo_root();
    let script = root.join("sidecar/dsh-official/sidecar-dsh.mjs");
    let plugin = file_url(&root.join("crates/compat-dsh-host/tests/fixtures/preset-counter.mjs"));
    let directory = temp_dir("preset-revisions");
    let disposed = directory.join("disposed.txt");
    let options = HostOptions::default()
        .with_env(
            "DSHR_DSH_PLUGINS",
            json!([{
                "name": &plugin, "config": { "label": "global" }
            }])
            .to_string(),
        )
        .with_registration_window(Duration::from_secs(2));
    let mut host = SidecarHost::spawn_with(&script, &script, options).unwrap();
    assert!(host.supports_agent_presets());
    let catalog = |label| {
        json!({
            "cordis": [{ "name": &plugin, "config": { "label": label, "disposed": &disposed } }]
        })
    };
    let rev1 = host.sync_presets(&catalog("v1")).unwrap()["cordis"]["revision"].clone();
    assert_eq!(
        host.sync_presets(&catalog("v1")).unwrap()["cordis"]["revision"],
        rev1
    );
    for session in ["s-1", "s-2"] {
        host.bind_preset(session, "cordis", None).unwrap();
    }

    let count = |host: &mut SidecarHost, session: &str| {
        host.invoke_in_session(
            "preset-call",
            Some(session),
            "preset_counter",
            json!({}),
            Duration::from_secs(3),
        )
        .unwrap()
        .terminal
        .payload["value"]
            .clone()
    };
    assert_eq!(count(&mut host, "s-1"), json!({"label": "v1", "count": 1}));
    assert_eq!(count(&mut host, "s-2"), json!({"label": "v1", "count": 2}));
    assert_eq!(
        count(&mut host, "s-9"),
        json!({"label": "global", "count": 1})
    );
    let rev2 = host.sync_presets(&catalog("v2")).unwrap()["cordis"]["revision"].clone();
    assert_ne!(rev1, rev2);

    host.bind_preset("s-3", "cordis", None).unwrap();
    host.bind_preset("s-4", "cordis", Some("s-1")).unwrap();
    assert_eq!(count(&mut host, "s-1"), json!({"label": "v1", "count": 3}));
    assert_eq!(count(&mut host, "s-4"), json!({"label": "v1", "count": 4}));
    assert_eq!(count(&mut host, "s-3"), json!({"label": "v2", "count": 1}));
    assert!(host.preset_tool_definitions("s-9").is_empty());

    host.retire_session("s-1").unwrap();
    host.retire_session("s-2").unwrap();
    assert!(!disposed.exists());
    host.retire_session("s-4").unwrap();
    assert_eq!(std::fs::read_to_string(&disposed).unwrap(), "v1\n");
    assert!(!host
        .registrations()
        .iter()
        .any(|r| r.preset_revision.as_deref() == rev1.as_str()));

    let broken = json!({"cordis": [{"name": "@dshr-fixture/missing-preset-plugin"}]});
    let roster = host.sync_presets(&broken).unwrap();
    assert!(roster["cordis"]["error"]
        .as_str()
        .unwrap()
        .contains("missing-preset-plugin"));
    assert!(host.bind_preset("s-5", "cordis", None).is_err());
    assert!(host.bind_preset("s-5", "missing", None).is_err());
    assert_eq!(count(&mut host, "s-3"), json!({"label": "v2", "count": 2}));

    host.sync_presets(&json!({})).unwrap();
    host.retire_session("s-3").unwrap();
    assert_eq!(std::fs::read_to_string(&disposed).unwrap(), "v1\nv2\n");
}
