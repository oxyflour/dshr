//! Sidecar 生命周期与 L1 调用路径的最小实现（`docs/spec/sidecar-rpc.md`）。
//!
//! 目的不是立刻做成生产级 Host，而是把**协议成本**测出来：
//! 进程启动 → 握手 → 注册 → invoke（含 chunk 流）→ 取消 → 崩溃。
//!
//! 传输固定为 NDJSON over stdio：规范允许抽象 transport，探针先用最便宜的一种。

#![forbid(unsafe_code)]
// 服务错误携带结构化 detail（code/message/retryable/可选 JSON），`Result<T, ServiceError>`
// 会在每个方法签名里放大 Err 尺寸。用 Box 包错误会改动公开 API 且不改善调用方（协议层本就
// 逐条分配 message/detail）；这是有意的取舍，用 crate 级 allow 固定，避免逐函数标注。
#![allow(clippy::result_large_err)]

mod program_tools;
pub mod service;
pub mod services;
pub mod storage_kv;
pub mod tool_jobs;
pub mod tool_process;
pub mod tool_terminals;
pub use program_tools::ProgramToolScope;

// 集成点（chat.rs / sidecar 读循环）的常用入口，避免深层路径。
pub use service::{
    allow as allow_host_service, HostService, HostServiceRouter, ServiceCallContext, ServiceError,
};
pub use services::{
    EventPage, SessionAccess, SessionEventView, SessionHeaderView, SessionLogService,
    SessionProjectionsService, WorkingDirectoryService, SESSION_LOG_V1, SESSION_PROJECTIONS_V1,
    WORKING_DIRECTORY_CHANGED, WORKING_DIRECTORY_V1,
};
pub use storage_kv::{StorageKvService, STORAGE_KV_V1};
pub use tool_jobs::ToolJobService;
pub use tool_process::{ShellProgram, ToolProcessService};
pub use tool_terminals::ToolTerminalService;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_protocol::{
    hello_with_caps, hello_with_caps_at, FsWriteResult, RpcError, CAP_FS_WRITE, CAP_TOOL_CANCEL,
    CAP_TOOL_STREAM, KIND_FS_WRITE_REQUEST, KIND_FS_WRITE_RESULT, KIND_HOST_SERVICE_CANCEL,
    KIND_HOST_SERVICE_REQUEST, KIND_HOST_SERVICE_RESULT, MAX_DEPTH, PROTOCOL_VERSION,
};
/// 集成方（chat.rs）构造 v2 Router 需要的协议常量；避免直接依赖 protocol crate。
pub use dshr_compat_dsh_protocol::{
    Envelope, HostServiceKind, HostServiceMethod, HostServiceTable, CAP_AGENT_TOOL_CONTEXT,
    CAP_BACKGROUND_JOBS, CAP_IMAGE_ATTACHMENTS, CAP_PROGRAM_TOOLS, CAP_SEARCH_PROCESS,
    CAP_SERVICE_PROXY, CAP_SHELL_PROCESS, CAP_TERMINAL_PROCESS, CAP_TOOL_GOALS, CAP_TOOL_SCHEDULES,
    CAP_TOOL_SESSION_QUERY, CAP_TOOL_SUBAGENTS, CAP_TOOL_TEAMS, CAP_USER_QUESTIONS,
    IMAGE_ATTACHMENTS_V1, PROTOCOL_MAX, TOOL_GOALS_V1, TOOL_JOBS_V1, TOOL_PROCESS_V1,
    TOOL_PROGRAM_V1, TOOL_SCHEDULES_V1, TOOL_SESSION_QUERY_V1, TOOL_SUBAGENTS_V1, TOOL_TEAMS_V1,
    TOOL_TERMINALS_V1, USER_QUESTIONS_V1,
};
use dshr_sidecar_guard::{GuardLimits, GuardedChild};
use dshr_tool_runtime::ToolOrigin;
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout waiting for {0}")]
    Timeout(String),
    /// 调用被宿主取消（已发 `tool.cancel`，宽限期内 sidecar 仍未给出终态）。
    #[error("cancelled waiting for {0}")]
    Cancelled(String),
    #[error("sidecar closed the pipe")]
    Closed,
    #[error("rpc error {code}: {message}")]
    Rpc { code: String, message: String },
}

/// 宿主启动选项：默认行为与 M2 之前完全一致（只读、无 fs.write）。
#[derive(Clone, Default)]
pub struct HostOptions {
    /// 默认 `tool.stream` / `tool.cancel` 之外的能力；`fs.write` 只能由
    /// [`HostOptions::with_fs_write`] 打开（手工塞进来会在协商时被剔除）。
    pub extra_caps: Vec<String>,
    /// 写入裁决服务：Some 时握手带上 `fs.write`，并自动给 sidecar 设置
    /// `DSHR_WORKSPACE`（其 `fs` 根）。
    pub fs_write: Option<Arc<FsWriteService>>,
    /// 注入 sidecar 子进程的环境变量（覆盖继承值）。
    pub env: Vec<(String, String)>,
    /// sidecar 子进程工作目录。
    pub current_dir: Option<PathBuf>,
    /// 注册收集窗口（缺省 2 s）；插件多/机器慢时可调大，自检用。
    pub registration_window: Option<Duration>,
    /// Node 可执行文件路径；缺省用 PATH 上的 `node`。
    pub program: Option<PathBuf>,
    /// 是否用 Windows Job Object 包裹 sidecar（资源上限 + 随宿主退出）。
    /// 默认关：测试/开发机行为不变；chat 启动时显式打开（见 docs/m2-runtime.md）。
    pub guard: bool,
    /// Job Object 限额（仅 `guard = true` 生效）。
    pub guard_limits: GuardLimits,
    /// v2 Host Service 路由（spec §13）：`None` = M2 行为（不发 `service.proxy`、
    /// 不服务 `host.service.*`）。Some 时握手用 `protocol_max = PROTOCOL_MAX`，
    /// 协商到 2 且 sidecar 回声 `service.proxy` 才启用。
    pub host_services: Option<Arc<Mutex<HostServiceRouter>>>,
}

impl HostOptions {
    /// 打开官方写工具：审批、沙箱、原子写都在 Rust 侧 [`FsWriteService`] 完成。
    pub fn with_fs_write(mut self, service: Arc<FsWriteService>) -> Self {
        self.fs_write = Some(service);
        self
    }

    /// 注入 v2 Host Service 路由（router 的 negotiated_version 应为 [`PROTOCOL_MAX`]；
    /// 实际是否生效由握手协商结果决定，v1 回退时 router 完全不被使用）。
    pub fn with_host_services(mut self, router: Arc<Mutex<HostServiceRouter>>) -> Self {
        self.host_services = Some(router);
        self
    }

    /// 指定 Node 可执行文件（例如 `runtime/node/node.exe`）。
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// 用 Job Object 包裹 sidecar：进程数/内存受限，Drop 时连子进程一起杀掉。
    pub fn with_guard(mut self, limits: GuardLimits) -> Self {
        self.guard = true;
        self.guard_limits = limits;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub fn with_registration_window(mut self, window: Duration) -> Self {
        self.registration_window = Some(window);
        self
    }

    /// 合并默认能力；`fs.write` 只跟随 `fs_write` 服务出现，`service.proxy` 只跟随
    /// `host_services` 路由出现——避免"协商了能力却没有裁决层"。
    pub fn effective_caps(&self) -> Vec<String> {
        let mut caps: Vec<String> = vec![CAP_TOOL_STREAM.to_owned(), CAP_TOOL_CANCEL.to_owned()];
        if self.host_services.is_some() {
            caps.push(CAP_SERVICE_PROXY.to_owned());
        }
        for cap in &self.extra_caps {
            if cap == CAP_FS_WRITE && self.fs_write.is_none() {
                continue;
            }
            if !caps.iter().any(|existing| existing == cap) {
                caps.push(cap.clone());
            }
        }
        if self.fs_write.is_some() && !caps.iter().any(|cap| cap == CAP_FS_WRITE) {
            caps.push(CAP_FS_WRITE.to_owned());
        }
        caps
    }
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub name: String,
    pub description: String,
    pub input_schema: Option<Value>,
    pub session_id: Option<String>,
    pub preset_revision: Option<String>,
    register_request_id: String,
    /// 对齐 DSH 的 `output.schema`；L1 暂不校验，但必须能穿过协议。
    pub output_schema: Option<serde_json::Value>,
    /// 对齐 DSH 的 `presentResult` 卡片类型。
    pub presentation: Option<String>,
    /// 对齐 DSH 工具声明的 `timeoutMs`（已归一化到 1s..=600s；未声明为 None）。
    pub timeout_ms: Option<u64>,
    pub registration_id: String,
    pub plugin_id: String,
}

impl Registration {
    /// 映射成注册表可登记的 `ToolDefinition`（携带 per-tool 超时）。
    pub fn to_tool_definition(&self, generation: &str) -> dshr_tool_runtime::ToolDefinition {
        let mut definition = dshr_tool_runtime::ToolDefinition::new(
            self.name.clone(),
            self.description.clone(),
            dshr_tool_runtime::ToolOrigin::Sidecar {
                plugin_id: self.plugin_id.clone(),
                registration_id: self.registration_id.clone(),
                generation: generation.to_owned(),
            },
        );
        if let Some(schema) = &self.input_schema {
            definition.input_schema = schema.clone();
        }
        if let Some(schema) = &self.output_schema {
            definition = definition.with_output_schema(schema.clone());
        }
        if let Some(card) = &self.presentation {
            definition = definition.with_presentation(card.clone());
        }
        if let Some(timeout_ms) = self.timeout_ms {
            definition = definition.with_timeout_ms(timeout_ms);
        }
        definition
    }
}

/// 从 `tool.register` 载荷构造 [`Registration`]（纯函数：不需要 sidecar 进程）。
///
/// `timeout_ms` 对齐 DSH 工具定义的 `timeoutMs`：sidecar 原样上报，这里归一化
/// 到 `MIN_TOOL_TIMEOUT_MS..=MAX_TOOL_TIMEOUT_MS`（与运行期 clamp 同口径）。
pub fn parse_tool_registration(
    payload: &serde_json::Value,
    plugin_id: &str,
    registration_id: impl Into<String>,
) -> Registration {
    Registration {
        name: payload["name"].as_str().unwrap_or_default().to_owned(),
        description: payload["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        input_schema: payload
            .get("input_schema")
            .filter(|value| value.is_object())
            .cloned(),
        session_id: None,
        preset_revision: None,
        register_request_id: String::new(),
        output_schema: payload
            .get("output_schema")
            .cloned()
            .filter(|value| !value.is_null()),
        presentation: payload
            .get("presentation")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        timeout_ms: dshr_compat_dsh_protocol::normalize_tool_timeout_ms(
            payload
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64),
        ),
        registration_id: registration_id.into(),
        plugin_id: plugin_id.to_owned(),
    }
}

#[derive(Debug)]
pub struct InvokeOutcome {
    pub chunks: Vec<String>,
    pub terminal: Envelope,
    pub elapsed: Duration,
}

/// `SidecarHost::invoke_batch` 的一次工具调用（M3 并行工具）。
///
/// `timeout` 是**本调用**的绝对截止时间（从写出 `tool.invoke` 起算），不共享批的超时。
#[derive(Debug, Clone)]
pub struct BatchInvokeCall {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub timeout: Duration,
    /// 会话身份（信封 `session_id`）；`None` = 兼容旧的 `s-probe` 占位。
    pub session_id: Option<String>,
    binding: Option<ToolOrigin>,
    approval_key: Option<String>,
    turn: Option<OpenToolTurn>,
    model_route: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy)]
pub struct OpenToolTurn {
    pub number: u64,
    pub start_seq: u64,
}

impl BatchInvokeCall {
    pub fn new(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        arguments: Value,
        timeout: Duration,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool: tool.into(),
            arguments,
            timeout,
            session_id: None,
            binding: None,
            approval_key: None,
            turn: None,
            model_route: None,
        }
    }

    pub fn with_binding(mut self, binding: ToolOrigin) -> Self {
        self.binding = Some(binding);
        self
    }

    pub fn with_approval_key(mut self, key: Option<String>) -> Self {
        self.approval_key = key;
        self
    }

    /// 显式透传会话 id（L2 服务方法按会话授权时必需）。
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_turn(mut self, turn: Option<OpenToolTurn>) -> Self {
        self.turn = turn;
        self
    }

    pub fn with_model_route(mut self, provider: &str, model: &str) -> Self {
        self.model_route = Some((provider.to_owned(), model.to_owned()));
        self
    }
}

/// Windows：GUI 子系统父进程 spawn console 子程序（node）时默认会新开一个控制台窗口；
/// 设置 `CREATE_NO_WINDOW` 让 sidecar 静默运行（release 双击时不再每个会话弹黑框）。
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`（winbase.h）。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// 批量调用等待读循环的轮询片段：既保证及时检查超时/取消，又不空转。
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// 取消后的宽限：`invoke_batch` 发出 `tool.cancel` 后最多再等这么久
/// （P1-7：不能等到 per-call 120s；sidecar 忽略 cancel 时也必须按时结算）。
const CANCEL_GRACE: Duration = Duration::from_secs(2);

pub struct SidecarHost {
    child: ChildHandle,
    stdin: ChildStdin,
    rx: Receiver<Envelope>,
    generation: String,
    script: PathBuf,
    plugin: PathBuf,
    seq: u64,
    registrations: Vec<Registration>,
    retired_sessions: HashSet<String>,
    agent_presets_supported: bool,
    agent_tool_context_supported: bool,
    preparing_step: Option<String>,
    preset_definitions: Option<Value>,
    preset_status: Value,
    preset_bindings: HashMap<String, (String, String)>,
    ready: Duration,
    host_caps: Vec<String>,
    fs_write: Option<Arc<FsWriteService>>,
    /// v2 Host Service 路由（None = M2 行为；Some 但协商 < 2 时也不会被使用）。
    host_services: Option<Arc<Mutex<HostServiceRouter>>>,
    /// 握手协商出的协议版本（v1 缺省）。
    negotiated_protocol: u32,
    /// 真正的服务开关：router 存在 && 协商到 2 && sidecar 回声 `service.proxy`。
    host_services_active: bool,
    /// 按 tool.invoke 的 request_id 追踪，模型给出的 call_id 可在其他调用中重复。
    active_calls: HashMap<String, ActiveCall>,
    program_tools: Option<ProgramToolScope>,
    /// sidecar 在启动/装载阶段上报的错误（`log.emit` level=error），供 UI 提示。
    boot_errors: Vec<String>,
}

struct ActiveCall {
    call_id: String,
    session_id: String,
    tool: String,
    turn: Option<OpenToolTurn>,
    shell_command: Option<String>,
    shell_background: bool,
    job_id: Option<String>,
    terminal_id: Option<String>,
    terminal_value: Option<String>,
    cancelled: bool,
    approval_key: Option<String>,
    goal_arguments: Option<Value>,
    agent_arguments: Option<Value>,
    parent: Option<String>,
    deadline: Option<Instant>,
}

impl ActiveCall {
    fn from_batch(call: &BatchInvokeCall, session: &str, turn: Option<OpenToolTurn>) -> Self {
        Self {
            call_id: call.call_id.clone(),
            session_id: session.to_owned(),
            tool: call.tool.clone(),
            turn,
            shell_command: if matches!(call.tool.as_str(), "bash" | "pwsh") {
                call.arguments["command"].as_str().map(str::to_owned)
            } else {
                None
            },
            shell_background: call.arguments["run_in_background"] == true,
            job_id: call.arguments["job_id"].as_str().map(str::to_owned),
            terminal_id: call.arguments["sessionId"].as_str().map(str::to_owned),
            terminal_value: match call.tool.as_str() {
                "terminal_send" => Some(
                    call.arguments["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                ),
                "terminal_signal" => call.arguments["signal"].as_str().map(str::to_owned),
                _ => None,
            },
            cancelled: false,
            approval_key: call.approval_key.clone(),
            goal_arguments: matches!(
                call.tool.as_str(),
                "get_goal" | "create_goal" | "update_goal"
            )
            .then(|| call.arguments.clone()),
            agent_arguments: matches!(
                call.tool.as_str(),
                "subagent"
                    | "send_message"
                    | "interrupt_agent"
                    | "team_task_create"
                    | "team_task_update"
                    | "schedule_create"
                    | "schedule_edit"
            )
            .then(|| call.arguments.clone()),
            parent: None,
            deadline: (!call.timeout.is_zero()).then(|| Instant::now() + call.timeout),
        }
    }
}

/// 侧车进程句柄：普通 spawn 或 Job Object 包裹（Windows 隔离）。
enum ChildHandle {
    Plain(Child),
    Guarded(GuardedChild),
}

impl ChildHandle {
    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(child) => child.kill(),
            Self::Guarded(child) => child.kill(),
        }
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Plain(child) => child.wait(),
            Self::Guarded(child) => child.wait(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Plain(child) => child.try_wait(),
            Self::Guarded(child) => child.try_wait(),
        }
    }
}

impl SidecarHost {
    /// 旧行为：只读侧车（capabilities = tool.stream / tool.cancel）。
    pub fn spawn(script: impl AsRef<Path>, plugin: impl AsRef<Path>) -> Result<Self, HostError> {
        Self::spawn_with(script, plugin, HostOptions::default())
    }

    /// 带能力协商/写裁决的启动入口（chat.rs 用 `HostOptions::with_fs_write` 打开写工具）。
    pub fn spawn_with(
        script: impl AsRef<Path>,
        plugin: impl AsRef<Path>,
        options: HostOptions,
    ) -> Result<Self, HostError> {
        let script = script.as_ref().to_path_buf();
        let plugin = plugin.as_ref().to_path_buf();

        let started = Instant::now();
        let program = options
            .program
            .clone()
            .unwrap_or_else(|| PathBuf::from("node"));
        let mut command = Command::new(&program);
        command
            .arg(&script)
            .arg(&plugin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(dir) = &options.current_dir {
            command.current_dir(dir);
        }
        let mut env = options.env.clone();
        if let Some(service) = &options.fs_write {
            // sidecar 的 fs 根必须与 Rust 裁决层的工作区一致；显式 env 优先。
            if !env
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("DSHR_WORKSPACE"))
            {
                env.push((
                    "DSHR_WORKSPACE".to_owned(),
                    service.workspace_root().to_string_lossy().into_owned(),
                ));
            }
        }
        if options.guard {
            // Job Object 包裹：环境变量先清洗（去凭据）再叠加显式 env；
            // cwd 也由 guard 统一设置。
            let cwd = options
                .current_dir
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let mut guarded =
                dshr_sidecar_guard::spawn_guarded(command, cwd, &env, &options.guard_limits)
                    .map_err(|err| HostError::Protocol(format!("guarded spawn: {err}")))?;
            let stdin = guarded
                .child_mut()
                .stdin
                .take()
                .ok_or_else(|| HostError::Protocol("no stdin".into()))?;
            let stdout = guarded
                .child_mut()
                .stdout
                .take()
                .ok_or_else(|| HostError::Protocol("no stdout".into()))?;
            return Self::from_parts(
                ChildHandle::Guarded(guarded),
                stdin,
                stdout,
                script,
                plugin,
                options,
                started,
            );
        }
        command.envs(env.iter().map(|(key, value)| (key, value)));
        hide_console_window(&mut command);
        let mut child = command.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::Protocol("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::Protocol("no stdout".into()))?;

        Self::from_parts(
            ChildHandle::Plain(child),
            stdin,
            stdout,
            script,
            plugin,
            options,
            started,
        )
    }

    /// spawn 之后共用的握手/注册收集流程（guard 与普通路径一致）。
    fn from_parts(
        child: ChildHandle,
        stdin: ChildStdin,
        stdout: std::process::ChildStdout,
        script: PathBuf,
        plugin: PathBuf,
        options: HostOptions,
        started: Instant,
    ) -> Result<Self, HostError> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(text) => match serde_json::from_str::<Envelope>(&text) {
                        Ok(envelope) => {
                            if tx.send(envelope).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            eprintln!("[sidecar] bad envelope: {err}: {text}");
                        }
                    },
                    Err(_) => break,
                }
            }
        });

        let mut host = Self {
            child,
            stdin,
            rx,
            generation: "node-pending".to_owned(),
            script,
            plugin,
            seq: 0,
            registrations: Vec::new(),
            retired_sessions: HashSet::new(),
            agent_presets_supported: false,
            agent_tool_context_supported: false,
            preparing_step: None,
            preset_definitions: None,
            preset_status: Value::Null,
            preset_bindings: HashMap::new(),
            ready: Duration::ZERO,
            host_caps: options.effective_caps(),
            fs_write: options.fs_write.clone(),
            host_services: options.host_services.clone(),
            negotiated_protocol: PROTOCOL_VERSION,
            host_services_active: false,
            active_calls: HashMap::new(),
            program_tools: None,
            boot_errors: Vec::new(),
        };

        // 规范 §5：generation 由宿主分配，sidecar 只回声。
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = format!(
            "node-{:04}",
            NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        // 有 HostServiceRouter 时才宣告 v2；否则与 M2 完全一致（hello.protocol_max = 1）。
        let mut hello = if host.host_services.is_some() {
            hello_with_caps_at(&host.host_caps, PROTOCOL_MAX)
        } else {
            hello_with_caps(&host.host_caps)
        };
        hello.payload["generation"] = json!(generation);
        host.write(&hello)?;
        let ready = host.wait_for("runtime.ready", None, Duration::from_secs(10))?;
        if let Some(err) = ready.error {
            return Err(HostError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        host.generation = generation;
        host.ready = started.elapsed();
        host.apply_negotiation(&ready)?;

        let registration_window = options
            .registration_window
            .unwrap_or(Duration::from_secs(2));
        host.collect_registrations(registration_window)?;
        Ok(host)
    }

    /// 本次握手协商出的 host_caps（诊断/自检用）。
    pub fn host_caps(&self) -> &[String] {
        &self.host_caps
    }

    pub fn fs_write_enabled(&self) -> bool {
        self.fs_write.is_some()
    }

    /// sidecar 启动/插件装载阶段上报的错误（最多保留 8 条），供 UI 提示。
    pub fn boot_errors(&self) -> &[String] {
        &self.boot_errors
    }

    pub fn ready_latency(&self) -> Duration {
        self.ready
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn has_exited(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_some())
    }

    /// 握手协商出的协议版本（v1 缺省；配置了 host_services 且 sidecar 支持时才可能为 2）。
    pub fn negotiated_protocol(&self) -> u32 {
        self.negotiated_protocol
    }

    /// v2 Host Service 是否真正启用（router + 协商到 2 + sidecar 回声 `service.proxy`）。
    pub fn host_services_active(&self) -> bool {
        self.host_services_active
    }

    pub fn host_services_router(&self) -> Option<&Arc<Mutex<HostServiceRouter>>> {
        self.host_services.as_ref()
    }

    /// 白名单方法表（`runtime.ready.host_services` / `capabilities.list` 的形状）；
    /// 未启用 v2 时返回 `None`。
    pub fn host_services_payload(&self) -> Option<Value> {
        if !self.host_services_active {
            return None;
        }
        self.host_services
            .as_ref()
            .and_then(|router| router.lock().ok())
            .map(|router| router.host_services_payload())
    }

    /// 推进 generation（崩溃重启/重连，spec §13.5）：同步给 router，旧 generation 的
    /// 请求会以 `E_SERVICE_UNAVAILABLE` 结算，去重窗口清空。
    pub fn set_generation(&mut self, generation: impl Into<String>) -> Result<(), HostError> {
        self.generation = generation.into();
        if let Some(router) = &self.host_services {
            let mut router = router
                .lock()
                .map_err(|_| HostError::Protocol("host service router poisoned".to_owned()))?;
            router.set_generation(self.generation.clone());
        }
        Ok(())
    }

    /// 应用 `runtime.ready` 的协商结果（spec §3.2/§13.5）。
    fn apply_negotiation(&mut self, ready: &Envelope) -> Result<(), HostError> {
        let reported = ready
            .payload
            .get("protocol")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(PROTOCOL_VERSION)) as u32;
        self.negotiated_protocol = if self.host_services.is_some() {
            reported.clamp(PROTOCOL_VERSION, PROTOCOL_MAX)
        } else {
            reported.clamp(1, PROTOCOL_VERSION)
        };
        self.agent_presets_supported = ready.payload["sidecar_caps"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|cap| cap == "agent.presets"));
        let sidecar_proxy = ready
            .payload
            .get("sidecar_caps")
            .and_then(Value::as_array)
            .map(|caps| {
                caps.iter()
                    .any(|cap| cap.as_str() == Some(CAP_SERVICE_PROXY))
            })
            .unwrap_or(false);
        self.host_services_active =
            self.host_services.is_some() && self.negotiated_protocol >= 2 && sidecar_proxy;
        self.agent_tool_context_supported = self.host_services_active
            && self
                .host_caps
                .iter()
                .any(|cap| cap == CAP_AGENT_TOOL_CONTEXT)
            && ready.payload["sidecar_caps"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|cap| cap == CAP_AGENT_TOOL_CONTEXT));
        if self.host_services_active {
            if let Some(router) = &self.host_services {
                let mut router = router
                    .lock()
                    .map_err(|_| HostError::Protocol("host service router poisoned".to_owned()))?;
                router.set_generation(self.generation.clone());
            }
        }
        Ok(())
    }

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn supports_agent_tool_context(&self) -> bool {
        self.agent_tool_context_supported
    }

    pub fn prepare_agent_step(&mut self, session: &str, data: Value) -> Result<Value, HostError> {
        if !self.agent_tool_context_supported || !self.active_calls.is_empty() {
            return Err(HostError::Protocol(
                "agent step context is unavailable".to_owned(),
            ));
        }
        self.seq += 1;
        let request_id = format!("req-agent-step-{}", self.seq);
        let envelope = Envelope::host("agent.step", &request_id, data)
            .with_version(self.negotiated_protocol)
            .with_generation(self.generation.clone())
            .with_session(session);
        self.write(&envelope)?;
        self.preparing_step = Some(session.to_owned());
        let result = self.wait_for("agent.step", Some(&request_id), Duration::from_secs(8));
        self.preparing_step = None;
        let result = result?;
        if result.runtime_generation != self.generation
            || result.session_id.as_deref() != Some(session)
        {
            return Err(HostError::Protocol(
                "agent step response has the wrong session or generation".to_owned(),
            ));
        }
        match result.error {
            Some(error) => Err(HostError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            ))),
            None => Ok(result.payload),
        }
    }

    /// 把 sidecar 注册映射成 Registry 可登记的 `ToolDefinition`。
    ///
    /// 这是 slice 2 的关键：sidecar 工具与原生工具进入**同一个**注册表，
    /// 由同一个 `authorize` 闸门放行。
    pub fn tool_definitions(&self) -> Vec<dshr_tool_runtime::ToolDefinition> {
        self.tool_definitions_at_scope(None)
    }

    pub fn tool_definitions_at_scope(
        &self,
        session_id: Option<&str>,
    ) -> Vec<dshr_tool_runtime::ToolDefinition> {
        self.registrations
            .iter()
            .filter(|registration| {
                registration.preset_revision.is_none()
                    && registration.session_id.as_deref() == session_id
            })
            .map(|registration| registration.to_tool_definition(&self.generation))
            .collect()
    }

    pub fn preset_tool_definitions(&self, session: &str) -> Vec<dshr_tool_runtime::ToolDefinition> {
        let Some((_, revision)) = self.preset_bindings.get(session) else {
            return Vec::new();
        };
        self.registrations
            .iter()
            .filter(|registration| registration.preset_revision.as_ref() == Some(revision))
            .map(|registration| registration.to_tool_definition(&self.generation))
            .collect()
    }

    pub fn supports_agent_presets(&self) -> bool {
        self.agent_presets_supported
    }

    pub fn sync_presets(&mut self, definitions: &Value) -> Result<&Value, HostError> {
        if self.preset_definitions.as_ref() != Some(definitions) {
            let response =
                self.preset_request("preset.sync", None, json!({ "definitions": definitions }))?;
            self.preset_status = response["presets"].clone();
            self.preset_definitions = Some(definitions.clone());
        }
        Ok(&self.preset_status)
    }

    pub fn preset_status(&self) -> &Value {
        &self.preset_status
    }

    pub fn bound_preset(&self, session_id: &str) -> Option<&str> {
        self.preset_bindings
            .get(session_id)
            .map(|(id, _)| id.as_str())
    }

    pub fn release_preset(&mut self, session_id: &str) -> Result<(), HostError> {
        if self.preset_bindings.contains_key(session_id) {
            self.preset_request("preset.release", Some(session_id), json!({}))?;
            self.preset_bindings.remove(session_id);
        }
        Ok(())
    }

    pub fn bind_preset(
        &mut self,
        session: &str,
        preset: &str,
        parent: Option<&str>,
    ) -> Result<(), HostError> {
        if self.retired_sessions.contains(session) {
            return Err(HostError::Protocol(
                "session no longer accepts a preset binding".to_owned(),
            ));
        }
        if let Some((bound, _)) = self.preset_bindings.get(session) {
            return if bound == preset {
                Ok(())
            } else {
                Err(HostError::Protocol(format!(
                    "session already uses preset {bound}"
                )))
            };
        }
        let response = self.preset_request(
            "preset.bind",
            Some(session),
            json!({ "preset": preset, "parent_session_id": parent }),
        )?;
        let revision = response["revision"]
            .as_str()
            .ok_or_else(|| HostError::Protocol("missing preset revision".to_owned()))?;
        self.preset_bindings
            .insert(session.to_owned(), (preset.to_owned(), revision.to_owned()));
        Ok(())
    }

    fn preset_request(
        &mut self,
        kind: &str,
        session: Option<&str>,
        data: Value,
    ) -> Result<Value, HostError> {
        if !self.agent_presets_supported {
            return Err(HostError::Protocol(
                "sidecar does not support agent.presets".to_owned(),
            ));
        }
        self.seq += 1;
        let request = format!("req-preset-{}", self.seq);
        let mut envelope = Envelope::host(kind, &request, data)
            .with_version(self.negotiated_protocol)
            .with_generation(self.generation.clone());
        envelope.session_id = session.map(str::to_owned);
        self.write(&envelope)?;
        let response = self.wait_for(kind, Some(&request), Duration::from_secs(10))?;
        match response.error {
            Some(error) => Err(HostError::Protocol(format!(
                "{}: {}",
                error.code, error.message
            ))),
            None => Ok(response.payload),
        }
    }

    /// Consume lifecycle messages that arrived while no tool call was in flight.
    pub fn refresh_registrations(&mut self) -> Result<(), HostError> {
        if !self.active_calls.is_empty() {
            return Ok(());
        }
        loop {
            match self.rx.try_recv() {
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.registrations.clear();
                    return Err(HostError::Closed);
                }
            }
        }
    }

    pub fn retire_session(&mut self, session_id: &str) -> Result<(), HostError> {
        self.retired_sessions.insert(session_id.to_owned());
        let release = if self.preset_bindings.remove(session_id).is_some() {
            self.preset_request("preset.release", Some(session_id), json!({}))
                .map(|_| ())
        } else {
            Ok(())
        };
        let mut revocations = Vec::new();
        self.registrations.retain(|registration| {
            if registration.session_id.as_deref() != Some(session_id) {
                return true;
            }
            revocations.push((
                registration.registration_id.clone(),
                registration.plugin_id.clone(),
            ));
            false
        });
        for (id, plugin) in revocations {
            self.seq += 1;
            let mut envelope =
                Envelope::host("tool.unregister", format!("retire-{}", self.seq), json!({}))
                    .with_version(self.negotiated_protocol)
                    .with_generation(self.generation.clone())
                    .with_plugin(plugin)
                    .with_session(session_id);
            envelope.registration_id = Some(id);
            self.write(&envelope)?;
        }
        release
    }

    pub fn script(&self) -> &Path {
        &self.script
    }

    pub fn plugin(&self) -> &Path {
        &self.plugin
    }

    pub fn invoke(
        &mut self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let call_id = self.begin_invoke(tool, arguments)?;
        self.wait_terminal(&call_id, timeout)
    }

    pub fn begin_invoke(&mut self, tool: &str, arguments: Value) -> Result<String, HostError> {
        self.begin_invoke_in_session(tool, arguments, None)
    }

    /// 带显式会话 id 的 `tool.invoke`（`None` = 兼容旧 `s-probe` 占位）。
    pub fn begin_invoke_in_session(
        &mut self,
        tool: &str,
        arguments: Value,
        session_id: Option<&str>,
    ) -> Result<String, HostError> {
        self.seq += 1;
        let call_id = format!("call-{}", self.seq);
        let request_id = format!("req-invoke-{}", self.seq);
        self.refresh_registrations()?;
        let mut call = BatchInvokeCall::new(&call_id, tool, arguments, Duration::ZERO);
        call.session_id = session_id.map(str::to_owned);
        self.send_invoke(request_id, &call)?;
        Ok(call_id)
    }

    fn send_invoke(&mut self, request_id: String, call: &BatchInvokeCall) -> Result<(), HostError> {
        let session_id = call.session_id.as_deref();
        let revision = session_id
            .and_then(|session| self.preset_bindings.get(session))
            .map(|(_, revision)| revision);
        let matches_call = |registration: &&Registration| {
            registration.name == call.tool
                && (registration.session_id.is_none()
                    || registration.session_id.as_deref() == session_id)
                && (registration.preset_revision.is_none()
                    || registration.preset_revision.as_ref() == revision)
        };
        let binding = match &call.binding {
            Some(ToolOrigin::Sidecar {
                plugin_id,
                registration_id,
                generation,
            }) => self
                .registrations
                .iter()
                .filter(matches_call)
                .find(|registration| {
                    generation == &self.generation
                        && plugin_id == &registration.plugin_id
                        && registration_id == &registration.registration_id
                })
                .ok_or_else(|| {
                    HostError::Protocol(format!(
                        "E_TOOL_UNAVAILABLE: {} ({registration_id})",
                        call.tool
                    ))
                })
                .map(Some)?,
            Some(_) => return Err(HostError::Protocol("invalid sidecar binding".to_owned())),
            None => self
                .registrations
                .iter()
                .filter(matches_call)
                .max_by_key(|registration| {
                    (
                        registration.session_id.is_some(),
                        registration.preset_revision.is_some(),
                    )
                }),
        };
        let mut envelope = Envelope::host(
            "tool.invoke",
            &request_id,
            json!({ "name": call.tool, "arguments": call.arguments }),
        )
        .with_version(self.negotiated_protocol)
        .with_generation(self.generation.clone())
        .with_call(&call.call_id)
        .with_session(session_id.unwrap_or("s-probe"));
        let turn = call.turn.filter(|_| self.agent_tool_context_supported);
        if let Some(turn) = turn {
            envelope.payload["turn"] =
                json!({ "number": turn.number, "start_seq": turn.start_seq });
        }
        if self.agent_tool_context_supported {
            if let Some((provider, model)) = &call.model_route {
                envelope.payload["model"] = json!({ "provider": provider, "model": model });
            }
        }
        if let Some(binding) = binding {
            envelope.registration_id = Some(binding.registration_id.clone());
            if !binding.plugin_id.is_empty() {
                envelope.plugin_id = Some(binding.plugin_id.clone());
            }
        }
        self.write(&envelope)?;
        self.active_calls.insert(
            request_id,
            ActiveCall::from_batch(call, session_id.unwrap_or("s-probe"), turn),
        );
        Ok(())
    }

    /// 用调用方指定的 `call_id` 发起调用：Session Log 与 IPC 必须是同一个身份。
    pub fn invoke_as(
        &mut self,
        call_id: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable(call_id, tool, arguments, timeout, None)
    }

    /// 可取消的 [`SidecarHost::invoke_as`]。
    ///
    /// 等待期间每 100ms 轮询一次 `cancel`；置位时发送一次 `tool.cancel`
    /// （`tool.cancel` RPC 之前只有定义、没有调用点），再给 sidecar 最多 2s 宽限
    /// 等 `tool.aborted` / `tool.result`；宽限期满仍无终态则返回
    /// [`HostError::Cancelled`]。正常等待的片段超时**不再**直接判 `Timeout`，
    /// 绝对 deadline 语义保持不变。
    pub fn invoke_as_cancellable(
        &mut self,
        call_id: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable_in_session(call_id, None, tool, arguments, timeout, cancel)
    }

    /// [`SidecarHost::invoke_as_cancellable`] + 显式 `session_id` 透传（P2-10）。
    ///
    /// `session_id` 是 L2 服务方法按会话授权的唯一身份锚点；`None` 保持旧行为
    /// （`s-probe` 占位，供探针/旧测试）。
    pub fn invoke_as_cancellable_in_session(
        &mut self,
        call_id: &str,
        session_id: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        self.seq += 1;
        let request_id = format!("req-invoke-{}", self.seq);
        self.refresh_registrations()?;
        let mut call = BatchInvokeCall::new(call_id, tool, arguments, timeout);
        call.session_id = session_id.map(str::to_owned);
        self.send_invoke(request_id.clone(), &call)?;
        self.finish_call(&request_id, call_id, timeout, cancel)
    }

    /// 单调用 + 显式会话 id 的便捷入口（无取消句柄）。
    pub fn invoke_in_session(
        &mut self,
        call_id: &str,
        session_id: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        self.invoke_as_cancellable_in_session(call_id, session_id, tool, arguments, timeout, None)
    }

    pub fn cancel(&mut self, call_id: &str) -> Result<(), HostError> {
        let request_id = self.request_for_call(call_id);
        self.send_cancel(request_id.as_deref(), call_id, "user")
    }

    fn request_for_call(&self, call_id: &str) -> Option<String> {
        self.active_calls
            .iter()
            .find_map(|(request, call)| (call.call_id == call_id).then(|| request.clone()))
    }

    fn send_cancel(
        &mut self,
        request_id: Option<&str>,
        call_id: &str,
        reason: &str,
    ) -> Result<(), HostError> {
        let mut payload = json!({ "call_id": call_id, "reason": reason });
        if let Some(request_id) = request_id {
            payload["tool_request_id"] = json!(request_id);
            if let Some(call) = self.active_calls.get_mut(request_id) {
                call.cancelled = true;
            }
            self.finish_program_children(request_id);
            self.release_tool_resources(request_id);
        }
        self.seq += 1;
        let envelope = Envelope::host("tool.cancel", format!("req-cancel-{}", self.seq), payload)
            .with_generation(self.generation.clone())
            .with_call(call_id.to_owned());
        self.write(&envelope)
    }

    /// 批量调用（M3 并行工具，DSH `maxParallelToolCalls`）：
    ///
    /// 按 `max_parallel` 分**波次**：每一波先把本波所有 `tool.invoke` 一次写出
    /// （request_id 区分），再在同一读循环里收集各自 `tool.chunk` / `tool.result` /
    /// `tool.aborted`，按 `request_id` 关联。返回值与 `calls` 一一对应（原顺序），
    /// 单个调用失败/超时不影响其它调用。
    ///
    /// 语义：
    /// - **per-call timeout**：每个调用独立 deadline；超时回 `HostError::Timeout`
    ///   并向 sidecar 发 `tool.cancel`，迟到结果被丢弃；
    /// - **取消**：`cancel` 置位时对本波在途调用发一次 `tool.cancel`，随后继续收结果
    ///   （`tool.aborted` 作为该调用的终态返回，聊天层映射为失败）；不再启动后续波次；
    /// - **迟到/未知结果**：丢弃已结算请求的消息，即使后续请求复用同一个 `call_id`；
    /// - **读循环仍服务** `fs.write.request` / `host.service.*` / `log.emit`，不会与
    ///   现有单调用路径互相死锁。
    pub fn invoke_batch(
        &mut self,
        calls: &[BatchInvokeCall],
        max_parallel: usize,
        cancel: Option<&AtomicBool>,
    ) -> Vec<Result<InvokeOutcome, HostError>> {
        struct BatchSlot {
            request_id: String,
            call_id: String,
            started: Instant,
            deadline: Instant,
            /// 取消宽限截止（发出 tool.cancel 后置位；P1-7）。
            cancel_deadline: Option<Instant>,
            chunks: Vec<String>,
            done: bool,
        }

        let mut results: Vec<Option<Result<InvokeOutcome, HostError>>> =
            (0..calls.len()).map(|_| None).collect();
        if calls.is_empty() {
            return Vec::new();
        }
        let parallel = max_parallel.max(1);
        let mut next = 0usize;
        'outer: while next < calls.len() {
            if let Err(error) = self.refresh_registrations() {
                for result in &mut results[next..] {
                    *result = Some(Err(HostError::Protocol(error.to_string())));
                }
                break;
            }
            let end = (next + parallel).min(calls.len());
            let mut slots: Vec<BatchSlot> = Vec::with_capacity(end - next);
            let mut by_request: HashMap<String, usize> = HashMap::new();

            // 第一波写：把本波调用的 tool.invoke 全部写出再收结果。
            for (offset, call) in calls[next..end].iter().enumerate() {
                let started = Instant::now();
                self.seq += 1;
                let request_id = format!("req-batch-{}", self.seq);
                let write = self.send_invoke(request_id.clone(), call);
                let done = write.is_err();
                if let Err(error) = write {
                    results[next + offset] = Some(Err(error));
                } else {
                    by_request.insert(request_id.clone(), offset);
                }
                slots.push(BatchSlot {
                    request_id,
                    call_id: call.call_id.clone(),
                    started,
                    deadline: started + call.timeout.max(Duration::from_millis(1)),
                    cancel_deadline: None,
                    chunks: Vec::new(),
                    done,
                });
            }

            let mut cancel_sent = false;
            let mut fatal: Option<String> = None;
            loop {
                // 取消：对本波所有在途调用发一次 tool.cancel，并置 2s 宽限；
                // 宽限内收到终态按终态结算，宽限到期未结算的一律 `Cancelled`
                //（sidecar 忽略 cancel 时不能等到 per-call 120s，P1-7）。
                if !cancel_sent && cancel.is_some_and(|token| token.load(Ordering::Relaxed)) {
                    cancel_sent = true;
                    let grace_deadline = Instant::now() + CANCEL_GRACE;
                    for slot in slots.iter_mut().filter(|slot| !slot.done) {
                        let _ = self.send_cancel(Some(&slot.request_id), &slot.call_id, "user");
                        slot.cancel_deadline = Some(grace_deadline);
                    }
                }
                let now = Instant::now();
                for (offset, slot) in slots.iter_mut().enumerate() {
                    if slot.done {
                        continue;
                    }
                    if slot.cancel_deadline.is_some_and(|deadline| now >= deadline) {
                        slot.done = true;
                        self.finish_active_call(&slot.request_id);
                        results[next + offset] = Some(Err(HostError::Cancelled(format!(
                            "tool {} did not settle within {}s of cancellation",
                            slot.call_id,
                            CANCEL_GRACE.as_secs()
                        ))));
                        continue;
                    }
                    if now < slot.deadline {
                        continue;
                    }
                    slot.done = true;
                    if slot.cancel_deadline.is_some() {
                        results[next + offset] =
                            Some(Err(HostError::Cancelled(format!("tool {}", slot.call_id))));
                    } else {
                        let _ = self.send_cancel(Some(&slot.request_id), &slot.call_id, "timeout");
                        results[next + offset] =
                            Some(Err(HostError::Timeout(format!("tool {}", slot.call_id))));
                    }
                    self.finish_active_call(&slot.request_id);
                }
                if slots.iter().all(|slot| slot.done) {
                    break;
                }
                let mut wait = WAIT_SLICE;
                for slot in slots.iter().filter(|slot| !slot.done) {
                    let mut slot_deadline = slot.deadline;
                    if let Some(cancel_deadline) = slot.cancel_deadline {
                        slot_deadline = slot_deadline.min(cancel_deadline);
                    }
                    let remaining = slot_deadline.saturating_duration_since(now);
                    wait = wait.min(remaining.max(Duration::from_millis(1)));
                }
                match self.rx.recv_timeout(wait) {
                    Ok(envelope) => {
                        match self.process_tool_lifecycle(&envelope) {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(error) => {
                                fatal = Some(error.to_string());
                                break;
                            }
                        }
                        let served = match self.maybe_serve_fs_write(&envelope) {
                            Ok(served) => served,
                            Err(err) => {
                                fatal = Some(format!("serving fs.write during batch: {err}"));
                                break;
                            }
                        };
                        if served {
                            continue;
                        }
                        let served = match self.maybe_serve_host_service(&envelope) {
                            Ok(served) => served,
                            Err(err) => {
                                fatal = Some(format!("serving host.service during batch: {err}"));
                                break;
                            }
                        };
                        if served {
                            continue;
                        }
                        if envelope.kind == "log.emit" {
                            self.note_log_emit(&envelope);
                            continue;
                        }
                        let Some(offset) = by_request.get(&envelope.request_id).copied() else {
                            // 其它调用/上一波超时后的迟到结果：丢弃（与本波无关）。
                            continue;
                        };
                        let slot = &mut slots[offset];
                        if slot.done || envelope.call_id.as_deref() != Some(slot.call_id.as_str()) {
                            continue;
                        }
                        match envelope.kind.as_str() {
                            "tool.chunk" => {
                                if let Some(text) =
                                    envelope.payload.get("text").and_then(Value::as_str)
                                {
                                    slot.chunks.push(text.to_owned());
                                }
                            }
                            "tool.result" | "tool.aborted" => {
                                let chunks = std::mem::take(&mut slot.chunks);
                                let elapsed = slot.started.elapsed();
                                slot.done = true;
                                self.finish_active_call(&slot.request_id);
                                results[next + offset] = Some(Ok(InvokeOutcome {
                                    chunks,
                                    terminal: envelope,
                                    elapsed,
                                }));
                            }
                            _ => {}
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        for (offset, slot) in slots.iter_mut().enumerate() {
                            if !slot.done {
                                slot.done = true;
                                results[next + offset] = Some(Err(HostError::Closed));
                            }
                        }
                        fatal = Some("sidecar closed the pipe during batch".to_owned());
                        break;
                    }
                }
            }

            if let Some(message) = fatal {
                for slot in &slots {
                    self.finish_active_call(&slot.request_id);
                }
                for slot in results.iter_mut().skip(next).take(calls.len() - next) {
                    if slot.is_none() {
                        *slot = Some(Err(HostError::Protocol(message.clone())));
                    }
                }
                break 'outer;
            }
            next = end;
            if cancel_sent {
                // 已取消：不再启动后续波次；本波已发出的调用已全部结算。
                for slot in results.iter_mut().skip(next) {
                    if slot.is_none() {
                        *slot = Some(Err(HostError::Cancelled(format!(
                            "batch cancelled before wave starting at {next}"
                        ))));
                    }
                }
                break 'outer;
            }
        }

        results
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    Err(HostError::Protocol(
                        "batch slot missing after invoke_batch".to_owned(),
                    ))
                })
            })
            .collect()
    }

    /// 读到达终止事件（`tool.result` / `tool.aborted`）为止，收集中间 chunk。
    pub fn wait_terminal(
        &mut self,
        call_id: &str,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        let request_id = self
            .request_for_call(call_id)
            .ok_or_else(|| HostError::Protocol(format!("tool {call_id} has no active request")))?;
        self.finish_call(&request_id, call_id, timeout, None)
    }

    fn finish_call(
        &mut self,
        request_id: &str,
        call_id: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        let result = self.wait_terminal_cancellable(request_id, call_id, timeout, cancel);
        self.finish_active_call(request_id);
        result
    }

    fn finish_active_call(&mut self, request_id: &str) {
        self.finish_program_children(request_id);
        self.active_calls.remove(request_id);
        self.release_tool_resources(request_id);
    }

    fn release_tool_resources(&self, request_id: &str) {
        if let Some(router) = &self.host_services {
            if let Ok(mut router) = router.lock() {
                router.release_tool_call(request_id);
            }
        }
    }

    fn wait_terminal_cancellable(
        &mut self,
        request_id: &str,
        call_id: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut chunks = Vec::new();
        // `tool.cancel` 只发一次；置位后改用"宽限 deadline"收尾。
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            if cancel_deadline.is_none()
                && cancel.is_some_and(|token| token.load(Ordering::Relaxed))
            {
                self.send_cancel(Some(request_id), call_id, "user")?;
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            let now = Instant::now();
            let effective_deadline = cancel_deadline.map_or(deadline, |grace| grace.min(deadline));
            let remaining = effective_deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return if cancel_deadline.is_some() {
                    Err(HostError::Cancelled(format!("tool {call_id}")))
                } else {
                    let _ = self.send_cancel(Some(request_id), call_id, "timeout");
                    Err(HostError::Timeout(format!("tool {call_id}")))
                };
            }

            // 100ms 片段超时不是失败：回到循环头部重新检查 cancel / deadline。
            let envelope = match self.rx.recv_timeout(remaining.min(WAIT_SLICE)) {
                Ok(envelope) => envelope,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            };

            if self.process_tool_lifecycle(&envelope)? {
                continue;
            }

            // 关键：等待 tool.result 期间必须同时服务 sidecar 的写入请求，
            // 否则 sidecar 等 fs.write.result、宿主等 tool.result，双向死锁。
            if self.maybe_serve_fs_write(&envelope)? {
                continue;
            }
            // v2：等待工具结果期间同样要服务 host.service.request/result/cancel。
            if self.maybe_serve_host_service(&envelope)? {
                continue;
            }
            if envelope.kind == "log.emit" {
                self.note_log_emit(&envelope);
                continue;
            }

            if envelope.request_id != request_id || envelope.call_id.as_deref() != Some(call_id) {
                continue;
            }
            match envelope.kind.as_str() {
                "tool.chunk" => {
                    if let Some(text) = envelope.payload.get("text").and_then(Value::as_str) {
                        chunks.push(text.to_owned());
                    }
                }
                "tool.result" | "tool.aborted" => {
                    return Ok(InvokeOutcome {
                        chunks,
                        terminal: envelope,
                        elapsed: started.elapsed(),
                    });
                }
                _ => {}
            }
        }
    }

    pub fn kill(&mut self) -> Result<(), HostError> {
        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
    }

    fn collect_registrations(&mut self, window: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    fn process_background_message(&mut self, envelope: &Envelope) -> Result<(), HostError> {
        if self.process_tool_lifecycle(envelope)? || self.maybe_serve_fs_write(envelope)? {
            return Ok(());
        }
        if envelope.kind == "log.emit" {
            self.note_log_emit(envelope);
        } else {
            self.maybe_serve_host_service(envelope)?;
        }
        Ok(())
    }

    fn process_tool_lifecycle(&mut self, envelope: &Envelope) -> Result<bool, HostError> {
        if !matches!(envelope.kind.as_str(), "tool.register" | "tool.unregister") {
            return Ok(false);
        }
        if envelope.runtime_generation != self.generation {
            return Ok(true);
        }
        let owner = envelope.plugin_id.as_deref().unwrap_or_default();
        let scope = &envelope.session_id;
        let revision = envelope.payload["preset_revision"].as_str();
        if envelope.kind == "tool.unregister" {
            self.registrations.retain(|registration| {
                let identity_matches = match &envelope.registration_id {
                    Some(id) => id == &registration.registration_id,
                    None => {
                        envelope.payload["register_request_id"].as_str()
                            == Some(registration.register_request_id.as_str())
                    }
                };
                !(identity_matches
                    && owner == registration.plugin_id
                    && scope == &registration.session_id
                    && revision == registration.preset_revision.as_deref())
            });
            return Ok(true);
        }

        self.seq += 1;
        let id = format!("reg-{}", self.seq);
        let mut registration = parse_tool_registration(&envelope.payload, owner, &id);
        registration.session_id = scope.clone();
        registration.preset_revision = revision.map(str::to_owned);
        registration.register_request_id = envelope.request_id.clone();
        let mut ack = envelope.reply("tool.register", json!({ "name": registration.name }));
        let error = if scope.is_some() && revision.is_some() {
            Some((
                "E_SCHEMA",
                "a registration cannot have both session and preset scope",
            ))
        } else if scope
            .as_ref()
            .is_some_and(|id| self.retired_sessions.contains(id))
        {
            Some(("E_TOOL_UNAVAILABLE", "tool registration session has ended"))
        } else if registration.name.is_empty() {
            Some(("E_SCHEMA", "tool name is required"))
        } else if self.registrations.iter().any(|item| {
            item.session_id == registration.session_id
                && item.preset_revision == registration.preset_revision
                && item.name == registration.name
        }) {
            Some((
                "E_DUPLICATE_REG",
                "tool name already registered in this scope",
            ))
        } else {
            None
        };
        if let Some((code, message)) = error {
            ack.error = Some(RpcError {
                code: code.to_owned(),
                message: message.to_owned(),
                retryable: false,
            });
            self.write(&ack)?;
        } else {
            ack.registration_id = Some(id.clone());
            ack.payload["registration_id"] = json!(id);
            self.write(&ack)?;
            self.registrations.push(registration);
        }
        Ok(true)
    }

    fn wait_for(
        &mut self,
        kind: &str,
        request: Option<&str>,
        timeout: Duration,
    ) -> Result<Envelope, HostError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HostError::Timeout(kind.to_owned()));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(envelope) if envelope.kind == KIND_FS_WRITE_REQUEST => {
                    self.maybe_serve_fs_write(&envelope)?;
                }
                Ok(envelope) if envelope.kind == "log.emit" => {
                    self.note_log_emit(&envelope);
                }
                Ok(envelope)
                    if envelope.kind == kind
                        && request.is_none_or(|id| {
                            envelope.request_id == id
                                && envelope.runtime_generation == self.generation
                        }) =>
                {
                    return Ok(envelope)
                }
                Ok(envelope) if envelope.kind == "runtime.reject" => {
                    return Err(HostError::Protocol(format!(
                        "handshake rejected: {}",
                        envelope.payload
                    )))
                }
                Ok(envelope) => self.process_background_message(&envelope)?,
                Err(RecvTimeoutError::Timeout) => return Err(HostError::Timeout(kind.to_owned())),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    /// 记录 sidecar 上报的错误日志（`log.emit` level=error），供 UI 插件错误提示。
    fn note_log_emit(&mut self, envelope: &Envelope) {
        let level = envelope
            .payload
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info");
        let message = envelope
            .payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("");
        if level == "error" && !message.is_empty() {
            if self.boot_errors.len() < 8 {
                self.boot_errors.push(message.to_owned());
            }
            eprintln!("[sidecar] {message}");
        }
    }

    fn callback_from_inactive_call(&self, envelope: &Envelope) -> bool {
        envelope
            .payload
            .get("tool_request_id")
            .and_then(Value::as_str)
            .is_some_and(|id| {
                !self
                    .active_calls
                    .get(id)
                    .is_some_and(|call| self.call_is_current(call))
            })
    }

    fn bound_service_call(&self, envelope: &Envelope) -> Option<&ActiveCall> {
        let request_id = envelope.payload["tool_request_id"].as_str()?;
        let call = self.active_calls.get(request_id)?;
        (self.call_is_current(call)
            && envelope.call_id.as_deref() == Some(call.call_id.as_str())
            && envelope.session_id.as_deref() == Some(call.session_id.as_str()))
        .then_some(call)
    }

    fn service_callback_denial(&self, envelope: &Envelope) -> Option<&'static str> {
        match envelope.service_key.as_deref() {
            Some(TOOL_GOALS_V1) => {
                let args = &envelope.payload["args"];
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TOOL_GOALS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn
                            .is_some_and(|turn| args["start_seq"] == turn.start_seq)
                            && call.goal_arguments.as_ref() == Some(&args["arguments"])
                            && args["tool"] == call.tool
                    });
                (!allowed)
                    .then_some("goal operation requires its matching active tool and open turn")
            }
            Some(TOOL_SESSION_QUERY_V1) => {
                let allowed = self
                    .host_caps
                    .iter()
                    .any(|cap| cap == CAP_TOOL_SESSION_QUERY)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn.is_some()
                            && matches!(
                                call.tool.as_str(),
                                "session_search"
                                    | "session_event_search"
                                    | "session_trace"
                                    | "session_event_trace"
                                    | "session_event_read"
                            )
                    });
                (!allowed).then_some("session query requires a matching active session query tool")
            }
            Some(TOOL_SUBAGENTS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let enabled = self.host_caps.iter().any(|cap| cap == CAP_TOOL_SUBAGENTS);
                let step = method == "notices"
                    && self.preparing_step.is_some()
                    && envelope.session_id == self.preparing_step
                    && envelope.payload["tool_request_id"].is_null();
                let allowed = enabled
                    && (step
                        || self.bound_service_call(envelope).is_some_and(|call| {
                            call.turn.is_some()
                                && match method {
                                    "list" => matches!(
                                        call.tool.as_str(),
                                        "list_agents"
                                            | "send_message"
                                            | "interrupt_agent"
                                            | "subagent"
                                            | "team_task_list"
                                            | "team_task_update"
                                    ),
                                    "pending" => call.tool == "wait_agent",
                                    "start" if call.tool == "subagent" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "message" if call.tool == "send_message" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "interrupt" if call.tool == "interrupt_agent" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "workflow.start" | "workflow.poll" | "workflow.stop" => {
                                        matches!(call.tool.as_str(), "workflow" | "ralph")
                                    }
                                    _ => false,
                                }
                        }));
                (!allowed).then_some(
                    "subagent operation requires the matching authorized call and session",
                )
            }
            Some(TOOL_TEAMS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TOOL_TEAMS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        call.turn.is_some()
                            && match method {
                                "list" => matches!(
                                    call.tool.as_str(),
                                    "team_task_list" | "team_task_get" | "team_task_update"
                                ),
                                "create" if call.tool == "team_task_create" => {
                                    call.agent_arguments.as_ref() == Some(&args["arguments"])
                                }
                                "update" if call.tool == "team_task_update" => {
                                    call.agent_arguments.as_ref() == Some(&args["arguments"])
                                }
                                _ => false,
                            }
                    });
                (!allowed).then_some(
                    "team operation requires the matching authorized team tool and session",
                )
            }
            Some(TOOL_SCHEDULES_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let enabled = self.host_caps.iter().any(|cap| cap == CAP_TOOL_SCHEDULES);
                let step = matches!(method, "due" | "claim")
                    && self.preparing_step.is_some()
                    && envelope.session_id == self.preparing_step
                    && envelope.payload["tool_request_id"].is_null();
                let allowed = enabled
                    && (step
                        || self.bound_service_call(envelope).is_some_and(|call| {
                            call.turn.is_some()
                                && match method {
                                    "list" => call.tool == "schedule_list",
                                    "view" => matches!(
                                        call.tool.as_str(),
                                        "schedule_view" | "schedule_edit"
                                    ),
                                    "create" if call.tool == "schedule_create" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    "edit" if call.tool == "schedule_edit" => {
                                        call.agent_arguments.as_ref() == Some(&args["arguments"])
                                    }
                                    _ => false,
                                }
                        }));
                (!allowed).then_some(
                    "schedule operation requires the matching authorized tool and root session",
                )
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| {
                            let name = name.trim();
                            name.starts_with("subagent/")
                                || matches!(
                                    name,
                                    "goal/change"
                                        | "turn.started"
                                        | "turn.ended"
                                        | "step.started"
                                        | "message.user"
                                        | "child.spawned"
                                        | "child.finished"
                                        | "tool.program.admitted"
                                )
                        }) =>
            {
                Some("goal and turn authority events must originate with the Rust session runtime")
            }
            Some(IMAGE_ATTACHMENTS_V1) => {
                let allowed = self
                    .host_caps
                    .iter()
                    .any(|cap| cap == CAP_IMAGE_ATTACHMENTS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        let method = envelope.payload["method"].as_str();
                        match call.tool.as_str() {
                            "read_image" => matches!(method, Some("route" | "save")),
                            "office_render" => {
                                self.agent_tool_context_supported
                                    && matches!(method, Some("route" | "rendered.page"))
                            }
                            _ => false,
                        }
                    });
                (!allowed).then_some("image attachments require a matching active image tool call")
            }
            Some(TOOL_PROCESS_V1) => {
                let allowed = self.bound_service_call(envelope).is_some_and(|call| {
                    let (cap, engine) = match call.tool.as_str() {
                        "glob" | "grep" => (CAP_SEARCH_PROCESS, "ripgrep"),
                        "pwsh" | "bash" => (CAP_SHELL_PROCESS, call.tool.as_str()),
                        _ => return false,
                    };
                    if !self.host_caps.iter().any(|available| available == cap) {
                        return false;
                    }
                    if envelope.payload["method"] != "start" {
                        return true;
                    }
                    let args = &envelope.payload["args"];
                    args["engine"] == engine
                        && (engine == "ripgrep"
                            || args["command"].as_str().is_some_and(|command| {
                                call.shell_command.as_deref() == Some(command)
                            }))
                });
                (!allowed).then_some("tool processes require the matching authorized tool call")
            }
            Some(USER_QUESTIONS_V1) => {
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_USER_QUESTIONS)
                    && self
                        .bound_service_call(envelope)
                        .is_some_and(|call| call.tool == "ask_user_question");
                (!allowed)
                    .then_some("user questions require a matching active ask_user_question call")
            }
            Some(TOOL_JOBS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                if method == "notices" {
                    let allowed = self.host_caps.iter().any(|cap| cap == CAP_BACKGROUND_JOBS)
                        && self.preparing_step.is_some()
                        && envelope.session_id == self.preparing_step
                        && envelope.payload["tool_request_id"].is_null();
                    return (!allowed)
                        .then_some("job notices require an active agent step in the same session");
                }
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_BACKGROUND_JOBS)
                    && self
                        .bound_service_call(envelope)
                        .is_some_and(|call| match method {
                            "reserve" | "start" | "confirm" => {
                                call.shell_background
                                    && matches!(call.tool.as_str(), "pwsh" | "bash")
                                    && (method != "start"
                                        || (args["engine"] == call.tool
                                            && args["command"].as_str()
                                                == call.shell_command.as_deref()))
                            }
                            "list" => {
                                matches!(call.tool.as_str(), "job_list" | "job_output" | "job_kill")
                            }
                            "inspect" => {
                                matches!(call.tool.as_str(), "job_output" | "job_kill")
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            "read" | "advance" => {
                                call.tool == "job_output"
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            "stop" => {
                                call.tool == "job_kill"
                                    && call.job_id.as_deref() == args["id"].as_str()
                            }
                            _ => false,
                        });
                (!allowed).then_some("background jobs require the matching authorized tool call")
            }
            Some(TOOL_TERMINALS_V1) => {
                let method = envelope.payload["method"].as_str().unwrap_or_default();
                let args = &envelope.payload["args"];
                let operation = args["operation"].as_str().unwrap_or_default();
                let allowed = self.host_caps.iter().any(|cap| cap == CAP_TERMINAL_PROCESS)
                    && self.bound_service_call(envelope).is_some_and(|call| {
                        let tool = call.tool.as_str();
                        let same_id = args["id"].as_str() == call.terminal_id.as_deref();
                        match method {
                            "reserve" | "confirm" => {
                                matches!(tool, "terminal_open" | "pwsh" | "bash")
                            }
                            "list" => {
                                matches!(tool, "terminal_list" | "terminal_read" | "pwsh" | "bash")
                            }
                            "closed" => {
                                (tool == "terminal_close" && same_id)
                                    || matches!(tool, "pwsh" | "bash")
                            }
                            "allow" if matches!(tool, "pwsh" | "bash") => match operation {
                                "send" => {
                                    args["origin_command"].as_str() == call.shell_command.as_deref()
                                }
                                "read" | "close" => true,
                                _ => false,
                            },
                            "allow" if same_id => match operation {
                                "send" => {
                                    tool == "terminal_send"
                                        && args["value"].as_str() == call.terminal_value.as_deref()
                                }
                                "read" => tool == "terminal_read",
                                "signal" => {
                                    tool == "terminal_signal"
                                        && args["value"].as_str() == call.terminal_value.as_deref()
                                }
                                "close" => tool == "terminal_close",
                                _ => false,
                            },
                            _ => false,
                        }
                    });
                (!allowed).then_some(
                    "terminal operation requires the matching authorized tool and session",
                )
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| {
                            matches!(
                                name.trim(),
                                tool_terminals::TERMINAL_OPENED | tool_terminals::TERMINAL_CLOSED
                            )
                        }) =>
            {
                Some("terminal lifecycle events must originate with the Rust terminal service")
            }
            Some(SESSION_LOG_V1)
                if envelope.payload["method"] == "events.append"
                    && envelope.payload["args"]["event_type"]
                        .as_str()
                        .is_some_and(|name| name.trim() == "deliverables/presented") =>
            {
                let data = &envelope.payload["args"]["data"];
                let allowed = self.bound_service_call(envelope).is_some_and(|call| {
                    call.tool == "present"
                        && data["callId"] == call.call_id
                        && call
                            .turn
                            .is_some_and(|turn| data["turn"].as_u64() == Some(turn.number))
                });
                (!allowed)
                    .then_some("file delivery requires a matching present call inside an open turn")
            }
            _ => None,
        }
    }

    /// 读循环的公共服务点：遇到 sidecar 的写入请求就地裁决并回复。
    ///
    /// 返回 `true` 表示该信封已被消费，调用方继续循环、不再把它当作
    /// `tool.result`/`tool.register` 处理。
    fn maybe_serve_fs_write(&mut self, envelope: &Envelope) -> Result<bool, HostError> {
        if envelope.kind != KIND_FS_WRITE_REQUEST {
            return Ok(false);
        }
        let result = if self.callback_from_inactive_call(envelope) {
            FsWriteResult::error("E_CANCELLED", "tool request is no longer active")
        } else {
            match &self.fs_write {
                Some(service) => {
                    let approval_key = envelope.payload["tool_request_id"]
                        .as_str()
                        .and_then(|id| self.active_calls.get(id))
                        .filter(|call| {
                            envelope.call_id.as_deref() == Some(call.call_id.as_str())
                                && envelope.payload["call_id"].as_str()
                                    == Some(call.call_id.as_str())
                        })
                        .and_then(|call| call.approval_key.as_deref());
                    service.handle_tool_payload(&envelope.payload, approval_key)
                }
                None => FsWriteResult::error(
                    "E_FS_CAPABILITY",
                    "fs.write.request received but fs.write was not negotiated in runtime.hello",
                ),
            }
        };
        let payload = serde_json::to_value(&result)
            .map_err(|err| HostError::Protocol(format!("serialize fs.write.result: {err}")))?;
        self.write(&envelope.reply(KIND_FS_WRITE_RESULT, payload))?;
        Ok(true)
    }

    /// 读循环的 v2 公共服务点（spec §13.4/§13.9）：
    /// - `host.service.request` → `HostServiceRouter::handle_request` → `host.service.result`；
    /// - `host.service.cancel` → router 取消标记（幂等，无答复）；
    /// - `capabilities.list` → 白名单方法表（仅 v2 启用时）。
    ///
    /// 返回 `true` 表示该信封已被消费。同步 dispatch 会占用当前读循环（本轮按
    /// spec §13.4 的"服务必须做小做快"取舍；未来可移到工作线程，`acquire_slot`
    /// 已为背压预留）。
    fn maybe_serve_host_service(&mut self, envelope: &Envelope) -> Result<bool, HostError> {
        match envelope.kind.as_str() {
            KIND_HOST_SERVICE_REQUEST => {
                let reply = if self.callback_from_inactive_call(envelope) {
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_CANCELLED".to_owned(),
                        message: "tool request is no longer active".to_owned(),
                        retryable: false,
                    });
                    reply
                } else if envelope.service_key.as_deref() == Some(TOOL_PROGRAM_V1) {
                    self.handle_program_callback(envelope)
                } else if let Some(message) = self.service_callback_denial(envelope) {
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_SERVICE_DENIED".to_owned(),
                        message: message.to_owned(),
                        retryable: false,
                    });
                    reply
                } else if self.host_services_active {
                    let router = self.host_services.as_ref().ok_or_else(|| {
                        HostError::Protocol("active host services without router".to_owned())
                    })?;
                    let mut router = router.lock().map_err(|_| {
                        HostError::Protocol("host service router poisoned".to_owned())
                    })?;
                    router.handle_request(envelope)
                } else {
                    // v1 回退 / sidecar 未协商也发了 v2 kind：结构化拒绝，不 panic。
                    let mut reply = envelope.reply(KIND_HOST_SERVICE_RESULT, json!({}));
                    reply.error = Some(RpcError {
                        code: "E_PROTOCOL".to_owned(),
                        message:
                            "host.service.request received but service.proxy was not negotiated"
                                .to_owned(),
                        retryable: false,
                    });
                    reply
                };
                self.write(&reply)?;
                Ok(true)
            }
            KIND_HOST_SERVICE_CANCEL => {
                if self.host_services_active {
                    if let Some(router) = &self.host_services {
                        if let Ok(mut router) = router.lock() {
                            router.cancel(&envelope.request_id);
                        }
                    }
                }
                // 取消是通知语义：恰好一次答复不适用（spec §8/§13.6）。
                Ok(true)
            }
            "capabilities.list" => {
                let reply = if self.host_services_active {
                    let payload = self.host_services_payload().unwrap_or_else(|| json!([]));
                    envelope.reply(
                        "capabilities.list",
                        json!({
                            "protocol": self.negotiated_protocol,
                            "host_services": payload,
                        }),
                    )
                } else {
                    let mut reply = envelope.reply("capabilities.list", json!({}));
                    reply.error = Some(RpcError {
                        code: "E_VERSION".to_owned(),
                        message: format!(
                            "host services are not negotiated (protocol {})",
                            self.negotiated_protocol
                        ),
                        retryable: false,
                    });
                    reply
                };
                self.write(&reply)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn write(&mut self, envelope: &Envelope) -> Result<(), HostError> {
        assert!(
            envelope.depth <= MAX_DEPTH,
            "depth must respect the spec limit"
        );
        let line = serde_json::to_string(envelope)
            .map_err(|err| HostError::Protocol(format!("serialize: {err}")))?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for SidecarHost {
    fn drop(&mut self) {
        for request_id in self.active_calls.keys() {
            self.release_tool_resources(request_id);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use dshr_tool_runtime::ApprovalBroker;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dshr-host-caps-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn default_caps_stay_read_only() {
        let caps = HostOptions::default().effective_caps();
        assert_eq!(caps, vec![CAP_TOOL_STREAM, CAP_TOOL_CANCEL]);
        assert!(!caps.iter().any(|cap| cap == CAP_FS_WRITE));
    }

    #[test]
    fn fs_write_cap_requires_the_write_service() {
        // 手工塞 fs.write 但没有裁决层：协商时必须剔除，避免"能力开了但没人裁决"。
        let smuggled = HostOptions {
            extra_caps: vec![CAP_FS_WRITE.to_owned()],
            ..HostOptions::default()
        };
        assert!(!smuggled
            .effective_caps()
            .iter()
            .any(|cap| cap == CAP_FS_WRITE));

        let root = temp_root("caps");
        let service =
            Arc::new(FsWriteService::open(&root, ApprovalBroker::new()).expect("write service"));
        let caps = HostOptions::default()
            .with_fs_write(service)
            .effective_caps();
        assert_eq!(
            caps.iter().filter(|cap| *cap == CAP_FS_WRITE).count(),
            1,
            "fs.write 只出现一次: {caps:?}"
        );
        assert!(caps.iter().any(|cap| cap == CAP_TOOL_STREAM));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn env_override_is_recorded_for_the_child_process() {
        let options = HostOptions::default()
            .with_env("DSHR_DSH_PLUGINS", "[]")
            .with_current_dir(".")
            .with_registration_window(Duration::from_millis(1500));
        assert_eq!(options.env.len(), 1);
        assert_eq!(options.env[0].0, "DSHR_DSH_PLUGINS");
        assert_eq!(
            options.registration_window,
            Some(Duration::from_millis(1500))
        );
    }

    // M3-DEFAULT-011..013：`tool.register` 的 per-tool timeout 往返。
    #[test]
    fn m3_default_011_registration_carries_declared_timeout() {
        let payload = json!({
            "name": "bash",
            "description": "run a command",
            "output_schema": {"type": "object"},
            "presentation": "terminal",
            "timeout_ms": 5_000,
        });
        let registration = parse_tool_registration(&payload, "dsh-official", "reg-1");
        assert_eq!(registration.name, "bash");
        assert_eq!(registration.timeout_ms, Some(5_000));
        assert_eq!(registration.presentation.as_deref(), Some("terminal"));

        // 与 ToolDefinition 的往返（chat 注册进 Registry 的就是它）。
        let definition = registration.to_tool_definition("gen-1");
        assert_eq!(definition.name, "bash");
        assert_eq!(definition.timeout_ms, 5_000);
        assert_eq!(definition.effective_timeout_ms(), 5_000);
        assert_eq!(definition.card(), dshr_tool_runtime::Presentation::Terminal);
        assert_eq!(definition.origin.generation(), Some("gen-1"));
    }

    #[test]
    fn m3_default_012_absent_timeout_keeps_tool_default() {
        let registration = parse_tool_registration(&json!({"name": "read"}), "p", "reg-2");
        assert_eq!(registration.timeout_ms, None);
        let definition = registration.to_tool_definition("gen-2");
        assert_eq!(
            definition.timeout_ms,
            dshr_tool_runtime::DEFAULT_TOOL_TIMEOUT_MS
        );
    }

    #[test]
    fn m3_default_013_timeout_is_normalized_on_the_way_in() {
        // 0 / 非法值 = 未声明；过小/过大按运行期口径 clamp（1s..=600s）。
        for raw in [json!(0), json!(-5), json!("3000"), json!(null)] {
            let registration =
                parse_tool_registration(&json!({"name": "t", "timeout_ms": raw}), "p", "reg-3");
            assert_eq!(registration.timeout_ms, None, "raw={raw}");
        }
        let small = parse_tool_registration(&json!({"name": "t", "timeout_ms": 10}), "p", "reg-4");
        assert_eq!(small.timeout_ms, Some(1_000));
        let huge = parse_tool_registration(
            &json!({"name": "t", "timeout_ms": 5_000_000u64}),
            "p",
            "reg-5",
        );
        assert_eq!(huge.timeout_ms, Some(600_000));
    }
}
