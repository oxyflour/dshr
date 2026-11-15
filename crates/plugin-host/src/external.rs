//! `external-process` 插件运行时（`cpp-process/v1`，spec/cpp-v1.md §12、ADR-0010）。
//!
//! 本模块把"独立进程插件"接进与 `rust-static` 相同的 [`Plugin`] trait：
//!
//! ```text
//! spawn(process.hello → process.ready) → PluginHost::load/apply(tools+panels)
//!   → call(tool.invoke → tool.result) / render_view(ui.render → ui.result)
//!   → unload(process.shutdown → bye, 超时 TerminateJobObject)
//! ```
//!
//! 隔离（G3 退路）：子进程用 [`dshr_sidecar_guard::spawn_guarded`] 包进 Windows
//! Job Object——内存上限来自 manifest `max_memory_mb`、CPU 时间由 `fuel` 折算（缺省 60 s）、
//! 活跃进程数 4、宿主退出/句柄关闭即整 job 终止。宿主对每次调用有墙钟看门狗，
//! 超时直接杀 job；输出超过 1 MiB 帧上限由 SDK 侧结算 `E_TOO_LARGE`，宿主双重防御。
//!
//! ## 与 WASM 主方案的替换缝隙
//!
//! 协议层（envelope / process.hello / ready / tool.invoke / result）不绑定进程模型。
//! 将来 Wasmtime 可用时（ADR-0010 反悔条件），新增 `wasm` 模块实现同一个 [`Plugin`]
//! 适配：`ExternalPlugin` 的 spawn/握手/超时/kill 换成实例化/epoch/fuel，本文件其余
//! 语义（工具声明、结果映射、错误码）可整体复用。

use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dshr_plugin_api::manifest::RuntimeKind;
use dshr_plugin_api::{
    HostVersion, Manifest, PanelContribution, Plugin, PluginContext, PluginError,
};
use dshr_plugin_sdk::{
    kind, Envelope, HelloPayload, InvokePayload, PanelDecl, ReadyDecls, ReadyPayload,
    RenderPayload, ShutdownPayload, ToolDecl, ToolResult, DEFAULT_SHUTDOWN_TIMEOUT_MS,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use dshr_sidecar_guard::{spawn_guarded, GuardLimits, GuardedChild};
use dshr_tool_runtime::{ToolDefinition, ToolOrigin};

use super::HostError;

/// `process.ready` 的等待上限（spec §12.2：10 s）。
pub const READY_TIMEOUT_MS: u64 = 10_000;
/// 优雅退出的等待上限；超时 `TerminateJobObject`（spec §12.2）。
pub const SHUTDOWN_TIMEOUT_MS: u64 = DEFAULT_SHUTDOWN_TIMEOUT_MS;
/// 健康检查（`process.ping`）超时。
pub const HEALTH_TIMEOUT_MS: u64 = 3_000;
/// `ui.render` 的超时。
pub const RENDER_TIMEOUT_MS: u64 = 10_000;
/// 每个插件允许的活跃进程数（spec §12.4：每插件并发 4）。
pub const MAX_PROCESSES_PER_PLUGIN: u32 = 4;
/// 未声明 `max_memory_mb` 时的默认内存上限：128 MB（spec §3.5）。
pub const DEFAULT_MEMORY_MB: u64 = 128;
/// 未声明 `fuel` 时的 CPU 时间上限：60 s（ADR-0010 §1）。
pub const DEFAULT_CPU_MS: u64 = 60_000;
/// fuel → CPU 毫秒的折算（1e9 fuel ≈ 1 s CPU；WASM 主方案就绪后以 epoch 为准）。
pub const FUEL_PER_CPU_MS: u64 = 1_000_000;
/// stderr 诊断保留行数。
const STDERR_TAIL_LIMIT: usize = 64;
/// 每进程可执行文件的规范扩展名（Windows）。
#[cfg(windows)]
const EXECUTABLE_HINT: &str = ".exe";

/// `ExternalPlugin::spawn` 的输入（manifest 由装载方解析，包目录是安装后的真实路径）。
#[derive(Debug, Clone)]
pub struct ExternalPluginSpec {
    pub manifest: Manifest,
    pub package_dir: PathBuf,
    pub host_version: HostVersion,
    pub generation: u64,
}

impl ExternalPluginSpec {
    pub fn new(
        manifest: Manifest,
        package_dir: impl Into<PathBuf>,
        host_version: HostVersion,
        generation: u64,
    ) -> Self {
        Self {
            manifest,
            package_dir: package_dir.into(),
            host_version,
            generation,
        }
    }
}

/// 子进程 stdout 的读线程消息。
enum ChildMessage {
    Line(String),
    Exited,
}

/// 一次 host→child 往返的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExchangeError {
    Io(String),
    Timeout { timeout_ms: u64 },
    Exited { reason: String },
    Protocol { code: String, message: String },
    FrameTooLarge { size: usize },
    Child { code: String, message: String },
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "protocol io error: {reason}"),
            Self::Timeout { timeout_ms } => write!(f, "timed out after {timeout_ms} ms"),
            Self::Exited { reason } => write!(f, "plugin process exited: {reason}"),
            Self::Protocol { code, message } => write!(f, "{code}: {message}"),
            Self::FrameTooLarge { size } => {
                write!(f, "E_TOO_LARGE: frame of {size} bytes exceeds limit")
            }
            Self::Child { code, message } => write!(f, "{code}: {message}"),
        }
    }
}

impl ExchangeError {
    fn into_plugin_error(self, tool: &str) -> PluginError {
        match self {
            Self::Child { code, .. } if code == "E_CANCELLED" => PluginError::Cancelled {
                operation: format!("tool `{tool}`"),
            },
            Self::Child { code, message } => PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("{code}: {message}"),
            },
            Self::Timeout { .. } => PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("E_TIMEOUT: {self}"),
            },
            Self::FrameTooLarge { .. } => PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("E_TOO_LARGE: {self}"),
            },
            Self::Protocol { .. } | Self::Io(_) => PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("E_PROTOCOL: {self}"),
            },
            Self::Exited { .. } => PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("E_INTERNAL: {self}"),
            },
        }
    }
}

/// 一个已就绪的 `external-process` 插件实例（一份 generation 一个进程）。
pub struct ExternalPlugin {
    plugin_id: String,
    version: String,
    id_static: &'static str,
    version_static: &'static str,
    generation: u64,
    package_dir: PathBuf,
    ready: ReadyDecls,
    child: GuardedChild,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<ChildMessage>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    request_counter: u64,
    failed: bool,
    disposed: bool,
}

impl fmt::Debug for ExternalPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalPlugin")
            .field("plugin_id", &self.plugin_id)
            .field("version", &self.version)
            .field("generation", &self.generation)
            .field("pid", &self.child.id())
            .field("tools", &self.ready.tools.len())
            .field("panels", &self.ready.panels.len())
            .field("failed", &self.failed)
            .field("disposed", &self.disposed)
            .finish()
    }
}

impl ExternalPlugin {
    /// 启动子进程并完成 `process.hello` → `process.ready` 握手。
    ///
    /// 失败时（manifest 非法 / entry 逃逸 / spawn 失败 / 握手超时或协议错）子进程
    /// 一定被终止（Job Object 兜底），不会留下孤儿进程。
    pub fn spawn(spec: ExternalPluginSpec) -> Result<Self, HostError> {
        let ExternalPluginSpec {
            manifest,
            package_dir,
            host_version,
            generation,
        } = spec;
        let plugin_id = manifest.plugin.id.clone();
        let version = manifest.plugin.version.clone();
        let external_error = |reason: String| HostError::External {
            plugin_id: plugin_id.clone(),
            reason,
        };

        // 1) manifest / api / 配额校验（与 PluginHost::load_with_manifest 同一套）。
        manifest
            .validate(&host_version)
            .map_err(HostError::Plugin)?;

        // 2) runtime.kind 与 entry 路径安全（spec §12.6）。
        if manifest.runtime.kind != RuntimeKind::ExternalProcess {
            return Err(external_error(format!(
                "manifest runtime.kind `{:?}` is not `external-process`",
                manifest.runtime.kind
            )));
        }
        let entry = manifest.runtime.entry.as_deref().unwrap_or_default();
        let entry_path = resolve_entry(&package_dir, entry).map_err(external_error)?;

        // 3) Job Object 配额映射（ADR-0010 §1）。
        let limits = guard_limits(&manifest);

        // 4) 启动（环境清洗由 spawn_guarded 统做；这里只显式传非敏感身份变量）。
        let env = vec![
            ("DSHR_PLUGIN_ID".to_owned(), plugin_id.clone()),
            ("DSHR_PLUGIN_GENERATION".to_owned(), generation.to_string()),
        ];
        let mut command = Command::new(&entry_path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_guarded(command, &package_dir, &env, &limits).map_err(|err| {
            external_error(format!("spawn `{}` failed: {err}", entry_path.display()))
        })?;

        let stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| external_error("child stdin pipe is missing".to_owned()))?;
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or_else(|| external_error("child stdout pipe is missing".to_owned()))?;
        let stderr = child
            .child_mut()
            .stderr
            .take()
            .ok_or_else(|| external_error("child stderr pipe is missing".to_owned()))?;

        let (sender, stdout_rx) = mpsc::channel::<ChildMessage>();
        thread::Builder::new()
            .name(format!("cpp-process-stdout-{plugin_id}"))
            .spawn(move || read_stdout(stdout, sender))
            .map_err(|err| external_error(format!("cannot spawn stdout reader: {err}")))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        thread::Builder::new()
            .name(format!("cpp-process-stderr-{plugin_id}"))
            .spawn({
                let tail = Arc::clone(&stderr_tail);
                move || read_stderr(stderr, tail)
            })
            .map_err(|err| external_error(format!("cannot spawn stderr reader: {err}")))?;

        let mut plugin = Self {
            plugin_id: plugin_id.clone(),
            version: version.clone(),
            id_static: leak_plugin_str(&plugin_id),
            version_static: leak_plugin_str(&version),
            generation,
            package_dir,
            ready: ReadyDecls::default(),
            child,
            stdin: Some(stdin),
            stdout_rx,
            stderr_tail,
            request_counter: 0,
            failed: false,
            disposed: false,
        };

        // 5) 握手：10 s 内必须 ready。
        match plugin.handshake(&host_version) {
            Ok(ready) => {
                plugin.ready = ready;
                Ok(plugin)
            }
            Err(error) => {
                plugin.kill();
                Err(error)
            }
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// 装载 generation（宿主重启插件即新 generation）。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 子进程 PID（隔离证据/诊断）。
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// 安装后的包目录（entry 的基准目录）。
    pub fn package_dir(&self) -> &Path {
        &self.package_dir
    }

    /// `process.ready` 声明的工具 + 面板。
    pub fn declarations(&self) -> &ReadyDecls {
        &self.ready
    }

    pub fn tools(&self) -> &[ToolDecl] {
        &self.ready.tools
    }

    pub fn panels(&self) -> &[PanelDecl] {
        &self.ready.panels
    }

    /// 最近 stderr 行（诊断；宿主可转发到 log）。
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 进程是否还活着（`try_wait` 非阻塞）。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 健康检查：`process.ping` → 任意回包（通常 `process.pong`）。
    pub fn health_check(&mut self) -> Result<(), HostError> {
        if self.failed || self.disposed {
            return Err(self.host_error("plugin process is not alive".to_owned()));
        }
        let request = Envelope::new(kind::PROCESS_PING)
            .with_request_id(self.next_request_id())
            .with_generation(self.generation);
        let timeout = Duration::from_millis(HEALTH_TIMEOUT_MS);
        match self.exchange(request, timeout) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.mark_failed(&error.to_string());
                Err(self.host_error(error.to_string()))
            }
        }
    }

    /// 可选面板渲染：`ui.render` → `ui.result {tree}`；不要求接入 UI（ADC 渲染另接）。
    pub fn render_view(&mut self, view: &str) -> Result<serde_json::Value, HostError> {
        if self.failed || self.disposed {
            return Err(self.host_error("plugin process is not alive".to_owned()));
        }
        let request_id = self.next_request_id();
        let request = Envelope::with_payload(
            kind::UI_RENDER,
            RenderPayload {
                view: view.to_owned(),
                state_seq: 0,
            },
        )
        .map_err(|err| self.host_error(err.to_string()))?
        .with_request_id(request_id)
        .with_generation(self.generation);
        match self.exchange(request, Duration::from_millis(RENDER_TIMEOUT_MS)) {
            Ok(envelope) => {
                if let Some(error) = envelope.error {
                    return Err(self.host_error(format!("{}: {}", error.code, error.message)));
                }
                envelope
                    .payload
                    .get("tree")
                    .cloned()
                    .ok_or_else(|| self.host_error("ui.result is missing `tree`".to_owned()))
            }
            Err(error) => {
                self.mark_failed(&error.to_string());
                Err(self.host_error(error.to_string()))
            }
        }
    }

    // ---- 内部：握手 / 收发 / 生命周期 ----

    fn handshake(&mut self, host_version: &HostVersion) -> Result<ReadyDecls, HostError> {
        let hello = Envelope::with_payload(
            kind::PROCESS_HELLO,
            HelloPayload {
                protocol: PROTOCOL_VERSION.to_owned(),
                host_version: host_version.to_string(),
                plugin_id: self.plugin_id.clone(),
                generation: self.generation,
                manifest: None,
            },
        )
        .map_err(|err| self.host_error(err.to_string()))?;
        self.send(&hello)
            .map_err(|err| self.host_error(err.to_string()))?;

        let deadline = Instant::now() + Duration::from_millis(READY_TIMEOUT_MS);
        loop {
            let envelope = self
                .next_envelope(deadline, READY_TIMEOUT_MS)
                .map_err(|err| self.host_error(format!("waiting for process.ready: {err}")))?;
            if let Some(error) = envelope.error {
                return Err(self.host_error(format!(
                    "process.ready failed: {}: {}",
                    error.code, error.message
                )));
            }
            if envelope.kind != kind::PROCESS_READY {
                continue;
            }
            let ready: ReadyPayload = serde_json::from_value(envelope.payload)
                .map_err(|err| self.host_error(format!("invalid process.ready payload: {err}")))?;
            if ready.protocol != PROTOCOL_VERSION {
                return Err(self.host_error(format!(
                    "E_VERSION: child protocol `{}`, expected `{PROTOCOL_VERSION}`",
                    ready.protocol
                )));
            }
            if ready.plugin_id != self.plugin_id {
                return Err(self.host_error(format!(
                    "plugin id mismatch: child `{}`, manifest `{}`",
                    ready.plugin_id, self.plugin_id
                )));
            }
            let decls = ReadyDecls::new(ready.tools, ready.panels);
            decls
                .validate()
                .map_err(|reason| self.host_error(format!("invalid declarations: {reason}")))?;
            return Ok(decls);
        }
    }

    fn next_request_id(&mut self) -> String {
        self.request_counter += 1;
        format!(
            "{}-{}-{}",
            self.plugin_id, self.generation, self.request_counter
        )
    }

    fn send(&mut self, envelope: &Envelope) -> Result<(), ExchangeError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(ExchangeError::Exited {
                reason: "stdin is closed".to_owned(),
            });
        };
        let line = envelope
            .encode_line()
            .map_err(|err| ExchangeError::Protocol {
                code: err.code().to_owned(),
                message: err.to_string(),
            })?;
        stdin
            .write_all(line.as_bytes())
            .map_err(|err| ExchangeError::Io(err.to_string()))?;
        stdin
            .flush()
            .map_err(|err| ExchangeError::Io(err.to_string()))
    }

    fn next_envelope(
        &mut self,
        deadline: Instant,
        timeout_ms: u64,
    ) -> Result<Envelope, ExchangeError> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ExchangeError::Timeout { timeout_ms });
            }
            match self.stdout_rx.recv_timeout(remaining) {
                Ok(ChildMessage::Line(line)) => {
                    if line.len() > MAX_FRAME_BYTES {
                        return Err(ExchangeError::FrameTooLarge { size: line.len() });
                    }
                    let envelope =
                        Envelope::parse(&line).map_err(|err| ExchangeError::Protocol {
                            code: err.code().to_owned(),
                            message: err.to_string(),
                        })?;
                    match envelope.kind.as_str() {
                        // 子进程健康检查（spec §12.2 允许双向 ping）：回 pong，不打断业务。
                        kind::PROCESS_PING => {
                            let pong =
                                Envelope::new(kind::PROCESS_PONG).with_generation(self.generation);
                            self.send(&pong)?;
                            continue;
                        }
                        // 流式 chunk：本切片只记诊断，不阻塞结算（spec §12.2）。
                        kind::TOOL_CHUNK => continue,
                        _ => return Ok(envelope),
                    }
                }
                Ok(ChildMessage::Exited) => {
                    return Err(ExchangeError::Exited {
                        reason: self.exit_reason(),
                    })
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(ExchangeError::Timeout { timeout_ms })
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ExchangeError::Exited {
                        reason: "child stdout reader stopped".to_owned(),
                    })
                }
            }
        }
    }

    fn exchange(
        &mut self,
        request: Envelope,
        timeout: Duration,
    ) -> Result<Envelope, ExchangeError> {
        let request_id = request.request_id.clone().unwrap_or_default();
        self.send(&request)?;
        let deadline = Instant::now() + timeout;
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        loop {
            let envelope = self.next_envelope(deadline, timeout_ms)?;
            let matches_request = envelope.request_id.as_deref() == Some(request_id.as_str());
            let unattributed_error = envelope.error.is_some() && envelope.request_id.is_none();
            if matches_request || unattributed_error {
                return Ok(envelope);
            }
            // 迟到的旧响应/无关消息：丢弃继续等本次请求。
        }
    }

    fn exit_reason(&mut self) -> String {
        let status = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.to_string())
            .unwrap_or_else(|| "unknown status".to_owned());
        let stderr = self.stderr_tail();
        let hint = stderr
            .iter()
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        if hint.is_empty() {
            format!("exit status {status}")
        } else {
            format!("exit status {status}; stderr: {hint}")
        }
    }

    fn notify_cancel(&mut self, request_id: &str) {
        let cancel = Envelope::with_payload(
            kind::TOOL_CANCEL,
            dshr_plugin_sdk::CancelPayload {
                reason: "host timeout watchdog".to_owned(),
            },
        )
        .map(|envelope| envelope.with_request_id(request_id));
        if let Ok(cancel) = cancel {
            let _ = self.send(&cancel);
        }
    }

    /// 标记 generation 失效并杀掉 Job（幂等）；后续调用统一报 `E_INTERNAL`。
    fn mark_failed(&mut self, _reason: &str) {
        self.failed = true;
        self.kill();
    }

    fn kill(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn host_error(&self, reason: String) -> HostError {
        HostError::External {
            plugin_id: self.plugin_id.clone(),
            reason,
        }
    }
}

impl Plugin for ExternalPlugin {
    fn id(&self) -> &'static str {
        self.id_static
    }

    fn version(&self) -> &'static str {
        self.version_static
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for decl in &self.ready.tools {
            let mut definition = ToolDefinition::new(
                decl.name.clone(),
                decl.description.clone(),
                ToolOrigin::Native {
                    plugin_id: self.plugin_id.clone(),
                },
            );
            definition.input_schema = decl.input_schema.clone();
            definition.output_schema = decl.output_schema.clone();
            definition.presentation = decl.presentation.clone();
            definition.timeout_ms = decl.timeout_ms;
            ctx.register_tool(definition)?;
        }
        for panel in &self.ready.panels {
            ctx.register_panel(
                PanelContribution::new(panel.id.clone(), panel.label.clone(), panel.slot.clone())
                    .with_view(panel.view.clone()),
            );
        }
        Ok(())
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let Some(decl) = self.ready.tool(tool) else {
            return Err(PluginError::UnknownTool {
                plugin: self.plugin_id.clone(),
                tool: tool.to_owned(),
            });
        };
        if self.failed || self.disposed {
            return Err(PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!(
                    "E_INTERNAL: plugin process generation {} is not alive",
                    self.generation
                ),
            });
        }
        let timeout_ms = decl.timeout_ms;
        let cancelable = decl.cancelable;
        let request_id = self.next_request_id();
        let request = Envelope::with_payload(
            kind::TOOL_INVOKE,
            InvokePayload {
                tool: tool.to_owned(),
                arguments: arguments.clone(),
                timeout_ms,
                cancelable,
                generation: Some(self.generation),
            },
        )
        .map_err(|err| PluginError::ToolFailed {
            tool: tool.to_owned(),
            reason: format!("E_PROTOCOL: {err}"),
        })?
        .with_request_id(request_id.clone())
        .with_generation(self.generation);

        match self.exchange(request, Duration::from_millis(timeout_ms)) {
            Ok(envelope) => {
                if let Some(error) = envelope.error {
                    return Err(ExchangeError::Child {
                        code: error.code,
                        message: error.message,
                    }
                    .into_plugin_error(tool));
                }
                let result: ToolResult =
                    serde_json::from_value(envelope.payload).map_err(|err| {
                        PluginError::ToolFailed {
                            tool: tool.to_owned(),
                            reason: format!("E_SCHEMA: invalid tool.result payload: {err}"),
                        }
                    })?;
                result.validate().map_err(|err| PluginError::ToolFailed {
                    tool: tool.to_owned(),
                    reason: format!("E_INTERNAL: invalid tool result: {err}"),
                })?;
                result.to_json().map_err(|err| PluginError::ToolFailed {
                    tool: tool.to_owned(),
                    reason: format!("E_INTERNAL: cannot serialize tool result: {err}"),
                })
            }
            Err(error @ ExchangeError::Timeout { .. }) => {
                // 看门狗：先发 cancel，再杀 Job（spec §12.4 / §7.4）。
                self.notify_cancel(&request_id);
                self.mark_failed(&error.to_string());
                Err(error.into_plugin_error(tool))
            }
            Err(error) => {
                self.mark_failed(&error.to_string());
                Err(error.into_plugin_error(tool))
            }
        }
    }

    /// `Plugin::render_view` 的 override：把子进程 `ui.render` 错误映射成插件错误，
    /// 这样 `PluginHost::render_panel` 对所有插件来源都返回同一种错误类型。
    fn render_view(&mut self, view: &str) -> Result<serde_json::Value, PluginError> {
        ExternalPlugin::render_view(self, view).map_err(|error| {
            PluginError::Invalid(format!(
                "external plugin `{}` view `{view}`: {error}",
                self.plugin_id
            ))
        })
    }

    fn unload(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        if self.failed {
            self.kill();
            return;
        }
        let shutdown = Envelope::with_payload(
            kind::PROCESS_SHUTDOWN,
            ShutdownPayload {
                timeout_ms: SHUTDOWN_TIMEOUT_MS,
            },
        );
        if let Ok(shutdown) = shutdown {
            if self.send(&shutdown).is_ok() {
                let deadline = Instant::now() + Duration::from_millis(SHUTDOWN_TIMEOUT_MS);
                loop {
                    match self.next_envelope(deadline, SHUTDOWN_TIMEOUT_MS) {
                        Ok(envelope) if envelope.kind == kind::PROCESS_BYE => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
        // 即使收到 bye 也终止 Job：确保不残留子进程/句柄（spec §12.5）。
        self.kill();
    }
}

impl Drop for ExternalPlugin {
    fn drop(&mut self) {
        if !self.disposed {
            self.disposed = true;
            self.kill();
        }
    }
}

fn read_stdout(stdout: ChildStdout, sender: mpsc::Sender<ChildMessage>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(ChildMessage::Exited);
                break;
            }
            Ok(_) => {
                if sender.send(ChildMessage::Line(line)).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = sender.send(ChildMessage::Exited);
                break;
            }
        }
    }
}

fn read_stderr(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut reader = BufReader::new(stderr);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let mut guard = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if guard.len() >= STDERR_TAIL_LIMIT {
                    guard.pop_front();
                }
                guard.push_back(line.trim_end().to_owned());
            }
        }
    }
}

/// manifest → Job Object 配额（ADR-0010 §1 的五维映射中进程侧可表达的四维）。
fn guard_limits(manifest: &Manifest) -> GuardLimits {
    let memory_mb = manifest
        .runtime
        .max_memory_mb
        .unwrap_or(DEFAULT_MEMORY_MB)
        .clamp(1, 512);
    let cpu_ms = manifest
        .runtime
        .fuel
        .map(|fuel| (fuel / FUEL_PER_CPU_MS).max(1_000))
        .unwrap_or(DEFAULT_CPU_MS);
    GuardLimits {
        max_processes: Some(MAX_PROCESSES_PER_PLUGIN),
        max_memory_bytes: Some(memory_mb * 1024 * 1024),
        max_cpu_time: Some(Duration::from_millis(cpu_ms)),
    }
}

/// 解析并校验 `runtime.entry`：包内相对路径、拒绝 `..`/绝对路径/符号链接（spec §12.6）。
fn resolve_entry(package_dir: &Path, entry: &str) -> Result<PathBuf, String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err("runtime.entry must not be empty for `external-process`".to_owned());
    }
    if entry.contains('\\') {
        return Err(format!(
            "runtime.entry `{entry}` must use `/` separators (no backslashes)"
        ));
    }
    let relative = Path::new(entry);
    if relative.is_absolute() {
        return Err(format!("runtime.entry `{entry}` must be relative"));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "runtime.entry `{entry}` must stay inside the package directory"
                ))
            }
        }
    }
    let joined = package_dir.join(relative);
    let metadata = fs::symlink_metadata(&joined)
        .map_err(|err| format!("runtime.entry `{entry}` is not accessible: {err}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("runtime.entry `{entry}` must not be a symlink"));
    }
    if !metadata.is_file() {
        return Err(format!("runtime.entry `{entry}` is not a regular file"));
    }
    let canonical_package = package_dir
        .canonicalize()
        .map_err(|err| format!("package directory is invalid: {err}"))?;
    let canonical_entry = joined
        .canonicalize()
        .map_err(|err| format!("runtime.entry `{entry}` cannot be resolved: {err}"))?;
    if !canonical_entry.starts_with(&canonical_package) {
        return Err(format!(
            "runtime.entry `{entry}` resolves outside the package directory"
        ));
    }
    #[cfg(windows)]
    if canonical_entry
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| !extension.eq_ignore_ascii_case(EXECUTABLE_HINT.trim_start_matches('.')))
        .unwrap_or(true)
    {
        return Err(format!(
            "runtime.entry `{entry}` must be a Windows executable (`{EXECUTABLE_HINT}`)"
        ));
    }
    Ok(canonical_entry)
}

/// `Plugin::id/version` 返回 `&'static str`（trait 冻结的既有限制），
/// 这里把 manifest 字符串泄漏一次：每进程 O(1)，换来 trait 零改动。
fn leak_plugin_str(value: &str) -> &'static str {
    Box::leak(value.to_owned().into_boxed_str())
}
