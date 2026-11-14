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

pub mod service;
pub mod services;
pub mod storage_kv;

// 集成点（chat.rs / sidecar 读循环）的常用入口，避免深层路径。
pub use service::{
    allow as allow_host_service, HostService, HostServiceRouter, ServiceCallContext, ServiceError,
};
pub use services::{
    EventPage, SessionAccess, SessionEventView, SessionHeaderView, SessionLogService,
    WorkingDirectoryService, SESSION_LOG_V1, WORKING_DIRECTORY_CHANGED, WORKING_DIRECTORY_V1,
};
pub use storage_kv::{StorageKvService, STORAGE_KV_V1};

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_protocol::{
    hello_with_caps, Envelope, FsWriteResult, CAP_FS_WRITE, CAP_TOOL_CANCEL, CAP_TOOL_STREAM,
    KIND_FS_WRITE_REQUEST, KIND_FS_WRITE_RESULT, MAX_DEPTH,
};
use dshr_sidecar_guard::{GuardLimits, GuardedChild};
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
}

impl HostOptions {
    /// 打开官方写工具：审批、沙箱、原子写都在 Rust 侧 [`FsWriteService`] 完成。
    pub fn with_fs_write(mut self, service: Arc<FsWriteService>) -> Self {
        self.fs_write = Some(service);
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

    /// 合并默认能力；`fs.write` 只跟随 `fs_write` 服务出现，避免"协商了能力却没有裁决层"。
    pub fn effective_caps(&self) -> Vec<String> {
        let mut caps: Vec<String> = vec![CAP_TOOL_STREAM.to_owned(), CAP_TOOL_CANCEL.to_owned()];
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
    /// 对齐 DSH 的 `output.schema`；L1 暂不校验，但必须能穿过协议。
    pub output_schema: Option<serde_json::Value>,
    /// 对齐 DSH 的 `presentResult` 卡片类型。
    pub presentation: Option<String>,
    pub registration_id: String,
    pub plugin_id: String,
}

#[derive(Debug)]
pub struct InvokeOutcome {
    pub chunks: Vec<String>,
    pub terminal: Envelope,
    pub elapsed: Duration,
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

pub struct SidecarHost {
    child: ChildHandle,
    stdin: ChildStdin,
    rx: Receiver<Envelope>,
    generation: String,
    script: PathBuf,
    plugin: PathBuf,
    seq: u64,
    registrations: Vec<Registration>,
    ready: Duration,
    host_caps: Vec<String>,
    fs_write: Option<Arc<FsWriteService>>,
    /// sidecar 在启动/装载阶段上报的错误（`log.emit` level=error），供 UI 提示。
    boot_errors: Vec<String>,
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
            ready: Duration::ZERO,
            host_caps: options.effective_caps(),
            fs_write: options.fs_write.clone(),
            boot_errors: Vec::new(),
        };

        // 规范 §5：generation 由宿主分配，sidecar 只回声。
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = format!(
            "node-{:04}",
            NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut hello = hello_with_caps(&host.host_caps);
        hello.payload["generation"] = json!(generation);
        host.write(&hello)?;
        let ready = host.wait_for("runtime.ready", Duration::from_secs(10))?;
        if let Some(err) = ready.error {
            return Err(HostError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        host.generation = generation;
        host.ready = started.elapsed();

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

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    /// 把 sidecar 注册映射成 Registry 可登记的 `ToolDefinition`。
    ///
    /// 这是 slice 2 的关键：sidecar 工具与原生工具进入**同一个**注册表，
    /// 由同一个 `authorize` 闸门放行。
    pub fn tool_definitions(&self) -> Vec<dshr_tool_runtime::ToolDefinition> {
        self.registrations
            .iter()
            .map(|reg| {
                let mut definition = dshr_tool_runtime::ToolDefinition::new(
                    reg.name.clone(),
                    reg.description.clone(),
                    dshr_tool_runtime::ToolOrigin::Sidecar {
                        plugin_id: reg.plugin_id.clone(),
                        registration_id: reg.registration_id.clone(),
                        generation: self.generation.clone(),
                    },
                );
                if let Some(schema) = &reg.output_schema {
                    definition = definition.with_output_schema(schema.clone());
                }
                if let Some(card) = &reg.presentation {
                    definition = definition.with_presentation(card.clone());
                }
                definition
            })
            .collect()
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
        self.seq += 1;
        let call_id = format!("call-{}", self.seq);
        let request_id = format!("req-invoke-{}", self.seq);
        let envelope = Envelope::host(
            "tool.invoke",
            request_id,
            json!({ "name": tool, "arguments": arguments }),
        )
        .with_generation(self.generation.clone())
        .with_call(call_id.clone())
        .with_session("s-probe");
        self.write(&envelope)?;
        Ok(call_id)
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
        self.seq += 1;
        let request_id = format!("req-invoke-{}", self.seq);
        let envelope = Envelope::host(
            "tool.invoke",
            request_id,
            json!({ "name": tool, "arguments": arguments }),
        )
        .with_generation(self.generation.clone())
        .with_call(call_id.to_owned())
        .with_session("s-probe");
        self.write(&envelope)?;
        self.wait_terminal_cancellable(call_id, timeout, cancel)
    }

    pub fn cancel(&mut self, call_id: &str) -> Result<(), HostError> {
        self.seq += 1;
        let envelope = Envelope::host(
            "tool.cancel",
            format!("req-cancel-{}", self.seq),
            json!({ "call_id": call_id, "reason": "user" }),
        )
        .with_generation(self.generation.clone())
        .with_call(call_id.to_owned());
        self.write(&envelope)
    }

    /// 读到达终止事件（`tool.result` / `tool.aborted`）为止，收集中间 chunk。
    pub fn wait_terminal(
        &mut self,
        call_id: &str,
        timeout: Duration,
    ) -> Result<InvokeOutcome, HostError> {
        self.wait_terminal_cancellable(call_id, timeout, None)
    }

    /// [`SidecarHost::wait_terminal`] 的可取消版本。
    fn wait_terminal_cancellable(
        &mut self,
        call_id: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<InvokeOutcome, HostError> {
        /// 取消后给 sidecar 收尾的宽限（它的取消检查周期是 200ms 级）。
        const CANCEL_GRACE: Duration = Duration::from_secs(2);
        /// 等待片段：既保证及时检查 cancel，又不让循环空转。
        const WAIT_SLICE: Duration = Duration::from_millis(100);

        let started = Instant::now();
        let deadline = started + timeout;
        let mut chunks = Vec::new();
        // `tool.cancel` 只发一次；置位后改用"宽限 deadline"收尾。
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            if cancel_deadline.is_none()
                && cancel.is_some_and(|token| token.load(Ordering::Relaxed))
            {
                self.cancel(call_id)?;
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            let now = Instant::now();
            let effective_deadline = cancel_deadline.map_or(deadline, |grace| grace.min(deadline));
            let remaining = effective_deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return if cancel_deadline.is_some() {
                    Err(HostError::Cancelled(format!("tool {call_id}")))
                } else {
                    Err(HostError::Timeout(format!("tool {call_id}")))
                };
            }

            // 100ms 片段超时不是失败：回到循环头部重新检查 cancel / deadline。
            let envelope = match self.rx.recv_timeout(remaining.min(WAIT_SLICE)) {
                Ok(envelope) => envelope,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            };

            // 关键：等待 tool.result 期间必须同时服务 sidecar 的写入请求，
            // 否则 sidecar 等 fs.write.result、宿主等 tool.result，双向死锁。
            if self.maybe_serve_fs_write(&envelope)? {
                continue;
            }
            if envelope.kind == "log.emit" {
                self.note_log_emit(&envelope);
                continue;
            }

            if envelope.call_id.as_deref() != Some(call_id) {
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
                Ok(envelope) if envelope.kind == KIND_FS_WRITE_REQUEST => {
                    self.maybe_serve_fs_write(&envelope)?;
                }
                Ok(envelope) if envelope.kind == "log.emit" => {
                    self.note_log_emit(&envelope);
                }
                Ok(envelope) if envelope.kind == "tool.register" => {
                    let name = envelope.payload["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let description = envelope.payload["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let output_schema = envelope
                        .payload
                        .get("output_schema")
                        .cloned()
                        .filter(|value| !value.is_null());
                    let presentation = envelope
                        .payload
                        .get("presentation")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    let plugin_id = envelope.plugin_id.clone().unwrap_or_default();
                    self.seq += 1;
                    let registration_id = format!("reg-{}", self.seq);
                    let ack = envelope.reply(
                        "tool.register",
                        json!({ "registration_id": registration_id, "name": name }),
                    );
                    self.write(&ack)?;
                    self.registrations.push(Registration {
                        name,
                        description,
                        output_schema,
                        presentation,
                        registration_id,
                        plugin_id,
                    });
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err(HostError::Closed),
            }
        }
    }

    fn wait_for(&mut self, kind: &str, timeout: Duration) -> Result<Envelope, HostError> {
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
                Ok(envelope) if envelope.kind == kind => return Ok(envelope),
                Ok(envelope) if envelope.kind == "runtime.reject" => {
                    return Err(HostError::Protocol(format!(
                        "handshake rejected: {}",
                        envelope.payload
                    )))
                }
                Ok(_) => continue,
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

    /// 读循环的公共服务点：遇到 sidecar 的写入请求就地裁决并回复。
    ///
    /// 返回 `true` 表示该信封已被消费，调用方继续循环、不再把它当作
    /// `tool.result`/`tool.register` 处理。
    fn maybe_serve_fs_write(&mut self, envelope: &Envelope) -> Result<bool, HostError> {
        if envelope.kind != KIND_FS_WRITE_REQUEST {
            return Ok(false);
        }
        let result = match &self.fs_write {
            Some(service) => service.handle_payload(&envelope.payload),
            None => FsWriteResult::error(
                "E_FS_CAPABILITY",
                "fs.write.request received but fs.write was not negotiated in runtime.hello",
            ),
        };
        let payload = serde_json::to_value(&result)
            .map_err(|err| HostError::Protocol(format!("serialize fs.write.result: {err}")))?;
        self.write(&envelope.reply(KIND_FS_WRITE_RESULT, payload))?;
        Ok(true)
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
}
