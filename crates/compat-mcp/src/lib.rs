//! MCP（Model Context Protocol）最小客户端：stdio NDJSON + JSON-RPC 2.0。
//!
//! 定位（`docs/PLAN.md` §6.3）：MCP 用于接入外部工具/服务，工具发现、Schema、
//! 请求/响应、权限与审计统一进入 Tool Registry；这里只做**传输与协议适配**，
//! 权限裁决仍由 `dshr-tool-runtime` 的 Policy 负责，执行超时/取消由本 crate 裁决。
//!
//! 与官方 MCP stdio 传输的差异（有意为之，文档化）：
//! - **不用 `Content-Length` 头**：一行一条 JSON（NDJSON），与主流 MCP stdio 实现一致；
//! - `protocolVersion` 固定协商 `2024-11-05`，只接受 [`SUPPORTED_PROTOCOL_VERSIONS`]
//!   里的版本；服务端回不支持版本时按规范断开（客户端置 `Broken`，需 `restart()`）；
//! - 客户端是**同步单线程**模型：同时最多一个在途请求；超时后发
//!   `notifications/cancelled` 并丢弃迟到答复（迟到答复计数进 [`McpStats`]）；
//! - `tools/call` 的 `isError` 不映射成 `Err`：`Ok(McpCallResult { is_error: true, .. })`
//!   保留工具级错误的结构化载荷（对应 DSH `ToolResult.is_error` 语义）；只有
//!   JSON-RPC / 传输 / 生命周期错误才是 [`McpError`]。
//!
//! 生命周期：`spawn` → `initialize` → `list_tools`/`call_tool` → `shutdown`（幂等）；
//! 进程崩溃后客户端置 `Broken`，在途请求返回 [`McpError::ProcessExited`]，可 `restart()`。

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dshr_tool_runtime::{RegistryError, ToolDefinition, ToolOrigin, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// 客户端支持的 MCP 协议版本（`initialize` 的 `protocolVersion`）。
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
/// 可接受的协议版本集合；服务端返回其它版本即断开（规范要求）。
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION];
/// 单次 JSON-RPC 请求的默认超时。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// `tools/call` 结果内联上限，默认 1 MiB（超出按 [`truncate_value`] 裁剪）。
pub const DEFAULT_MAX_RESULT_BYTES: usize = 1024 * 1024;
/// `tools/list` 分页的防御性页数上限。
pub const MAX_LIST_PAGES: usize = 64;
/// 工具描述进 `ToolDefinition` 前的截断长度（字符数），对齐 DSH 的 1024。
pub const MAX_TOOL_DESCRIPTION_CHARS: usize = 1024;
/// `shutdown` 等服务端退出的宽限。
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// 请求等待的轮询片段：保证及时检查超时/断链。
const WAIT_SLICE: Duration = Duration::from_millis(50);
/// `shutdown` 请求本身的超时上限（服务端可能不实现 shutdown，不能卡 30s）。
const SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// MCP server 的启动描述（`command` + `args` + `env` + `cwd`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    /// 逻辑名：进入 `ToolOrigin::Mcp { server }`，也用于日志前缀。
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// 覆盖/追加的子进程环境变量。
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// 每次 JSON-RPC 请求的默认超时；也作为 `ToolDefinition.timeout_ms`。
    pub timeout: Duration,
    /// `tools/call` 结果内联上限（字节）。
    pub max_result_bytes: usize,
}

impl McpServerSpec {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            timeout: DEFAULT_REQUEST_TIMEOUT,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_result_bytes(mut self, max_result_bytes: usize) -> Self {
        self.max_result_bytes = max_result_bytes;
        self
    }
}

/// MCP 工具描述（`tools/list` 的条目）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 原样保留的 JSON Schema（DSH 侧只做最小子集校验）。
    #[serde(default = "default_input_schema")]
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

fn default_input_schema() -> Value {
    json!({ "type": "object" })
}

impl McpTool {
    /// 把 MCP 工具转成 Tool Registry 的定义（origin = `Mcp { server }`）。
    pub fn as_tool_definition(&self, server: &str, timeout_ms: u64) -> ToolDefinition {
        let description: String = self
            .description
            .chars()
            .take(MAX_TOOL_DESCRIPTION_CHARS)
            .collect();
        let mut definition = ToolDefinition::new(
            self.name.clone(),
            description,
            ToolOrigin::Mcp {
                server: server.to_owned(),
            },
        )
        .with_schema(self.input_schema.clone());
        definition.timeout_ms = timeout_ms.max(1);
        if let Some(output_schema) = &self.output_schema {
            definition = definition.with_output_schema(output_schema.clone());
        }
        definition
    }

    fn from_value(value: &Value) -> Result<Self, McpError> {
        let object = value
            .as_object()
            .ok_or_else(|| McpError::BadTool("tools/list entry must be an object".to_owned()))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| McpError::BadTool("tools/list entry is missing `name`".to_owned()))?
            .to_owned();
        let description = object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let input_schema = object
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(default_input_schema);
        let output_schema = object
            .get("outputSchema")
            .cloned()
            .filter(|schema| !schema.is_null());
        Ok(Self {
            name,
            description,
            input_schema,
            output_schema,
        })
    }
}

/// `initialize` 的结果（协商版本 + 服务端信息/能力）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpInitializeResult {
    pub protocol_version: String,
    pub server_info: Value,
    pub capabilities: Value,
}

/// `tools/call` 的结果投影。
///
/// - `value`：优先 `structuredContent`；否则 `content[]` 全为 text 时拼接字符串；
///   含 image/resource 时投影为数组（image → `attachment://mcp/<server>/<tool>/<i>`
///   引用 + mime/bytes，resource → `{type:"resource", uri, text}`）；
/// - `is_error`：MCP 的 `isError`（不映射成 `Err`，见模块文档）；
/// - `truncated`：结果超过 [`McpServerSpec::max_result_bytes`] 被裁剪。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpCallResult {
    pub value: Value,
    pub is_error: bool,
    pub truncated: bool,
}

/// 客户端与 MCP server 的会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpClientState {
    /// 已 spawn，尚未完成 `initialize`。
    Spawned,
    /// `initialize` 完成，可发请求。
    Ready,
    /// 进程崩溃 / 协议版本不兼容 / 传输断开；需 `restart()`。
    Broken,
    /// 已 `shutdown`（或 `Drop` 兜底），不可再用。
    Shutdown,
}

impl McpClientState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "spawned",
            Self::Ready => "ready",
            Self::Broken => "broken",
            Self::Shutdown => "shutdown",
        }
    }
}

/// 客户端统计（诊断/测试证据）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpStats {
    /// 收到的通知总数（含 `notifications/message`）。
    pub notifications: u64,
    /// `notifications/message`（日志）条数。
    pub log_notifications: u64,
    /// 无法解析为 JSON 的 stdout 行数（诊断；客户端不因此断开）。
    pub invalid_lines: u64,
    /// 与当前在途请求 ID 不匹配、被丢弃的迟到响应数。
    pub late_responses: u64,
    /// 服务端 → 客户端的请求数（最小客户端不实现，直接忽略）。
    pub server_requests_ignored: u64,
    /// 超时的请求数。
    pub timed_out_requests: u64,
    /// 被裁剪的结果数。
    pub truncated_results: u64,
}

#[derive(Debug, Default)]
struct StatsInner {
    notifications: AtomicU64,
    log_notifications: AtomicU64,
    invalid_lines: AtomicU64,
    late_responses: AtomicU64,
    server_requests_ignored: AtomicU64,
    timed_out_requests: AtomicU64,
    truncated_results: AtomicU64,
}

impl StatsInner {
    fn snapshot(&self) -> McpStats {
        McpStats {
            notifications: self.notifications.load(Ordering::Relaxed),
            log_notifications: self.log_notifications.load(Ordering::Relaxed),
            invalid_lines: self.invalid_lines.load(Ordering::Relaxed),
            late_responses: self.late_responses.load(Ordering::Relaxed),
            server_requests_ignored: self.server_requests_ignored.load(Ordering::Relaxed),
            timed_out_requests: self.timed_out_requests.load(Ordering::Relaxed),
            truncated_results: self.truncated_results.load(Ordering::Relaxed),
        }
    }
}

/// MCP 客户端错误（工具级 `isError` 不在此列，见 [`McpCallResult::is_error`]）。
#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to spawn MCP server `{server}`: {source}")]
    Spawn {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("mcp transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP server `{server}` is not ready (state: {state})")]
    NotReady { server: String, state: &'static str },
    #[error("mcp protocol error: {0}")]
    Protocol(String),
    #[error("mcp json-rpc error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("request `{method}` timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("MCP server process exited (code: {code:?})")]
    ProcessExited { code: Option<i32> },
    #[error("unsupported MCP protocol version `{received}`; client supports `{supported}`")]
    ProtocolVersion {
        received: String,
        supported: &'static str,
    },
    #[error("malformed MCP tool: {0}")]
    BadTool(String),
    #[error("tool registry: {0}")]
    Registry(#[from] RegistryError),
}

/// 子进程 + 管道句柄（spawn 与 restart 共用）。
struct ProcessHandles {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
}

/// stdio MCP 客户端（同步、单在途请求）。
pub struct McpClient {
    spec: McpServerSpec,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Option<Receiver<Value>>,
    state: McpClientState,
    next_request_id: u64,
    /// 当前在途请求 ID（reader 线程据此丢弃迟到响应）。
    pending: Arc<AtomicU64>,
    stats: Arc<StatsInner>,
    timeout: Duration,
    max_result_bytes: usize,
    initialize: Option<McpInitializeResult>,
}

impl McpClient {
    /// 启动子进程（stdout/stdin 管道，stderr 转发日志）；不自动 `initialize`。
    pub fn spawn(spec: McpServerSpec) -> Result<Self, McpError> {
        let pending = Arc::new(AtomicU64::new(0));
        let stats = Arc::new(StatsInner::default());
        let handles = spawn_process(&spec, Arc::clone(&pending), Arc::clone(&stats))?;
        let timeout = spec.timeout;
        let max_result_bytes = spec.max_result_bytes;
        Ok(Self {
            spec,
            child: Some(handles.child),
            stdin: Some(handles.stdin),
            rx: Some(handles.rx),
            state: McpClientState::Spawned,
            next_request_id: 0,
            pending,
            stats,
            timeout,
            max_result_bytes,
            initialize: None,
        })
    }

    pub fn spec(&self) -> &McpServerSpec {
        &self.spec
    }

    pub fn server_name(&self) -> &str {
        &self.spec.name
    }

    pub fn state(&self) -> McpClientState {
        self.state
    }

    pub fn stats(&self) -> McpStats {
        self.stats.snapshot()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 调整后续请求的超时（已发出的请求不受影响）。
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn negotiated_protocol(&self) -> Option<&str> {
        self.initialize
            .as_ref()
            .map(|initialize| initialize.protocol_version.as_str())
    }

    pub fn server_info(&self) -> Option<&Value> {
        self.initialize
            .as_ref()
            .map(|initialize| &initialize.server_info)
    }

    pub fn server_capabilities(&self) -> Option<&Value> {
        self.initialize
            .as_ref()
            .map(|initialize| &initialize.capabilities)
    }

    /// 子进程是否仍存活（已退出/未启动/已 shutdown → false）。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// 完成 MCP 握手：`initialize` → 校验版本 → `notifications/initialized`。
    ///
    /// 已处于 `Ready` 时幂等返回缓存结果；`Broken`/`Shutdown` → [`McpError::NotReady`]。
    pub fn initialize(&mut self) -> Result<McpInitializeResult, McpError> {
        match self.state {
            McpClientState::Ready => {
                return self
                    .initialize
                    .clone()
                    .ok_or_else(|| McpError::Protocol("ready without initialize result".into()));
            }
            McpClientState::Spawned => {}
            other => return Err(self.not_ready(other)),
        }

        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "dshr",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let result = self.request("initialize", params)?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::Protocol("initialize result is missing `protocolVersion`".to_owned())
            })?
            .to_owned();
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol_version.as_str()) {
            // 规范：服务端回不支持的版本 → 客户端必须断开。
            self.mark_broken();
            return Err(McpError::ProtocolVersion {
                received: protocol_version,
                supported: MCP_PROTOCOL_VERSION,
            });
        }
        self.initialize = Some(McpInitializeResult {
            protocol_version,
            server_info: result.get("serverInfo").cloned().unwrap_or(Value::Null),
            capabilities: result
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| json!({})),
        });
        self.send_notification("notifications/initialized", json!({}))?;
        self.state = McpClientState::Ready;
        Ok(self
            .initialize
            .clone()
            .expect("initialize result set before ready"))
    }

    /// `tools/list`：跟随 `nextCursor` 聚合全部页（防御性上限 [`MAX_LIST_PAGES`]）。
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        self.ensure_ready()?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    McpError::Protocol("tools/list result is missing `tools` array".to_owned())
                })?;
            for entry in page {
                tools.push(McpTool::from_value(entry)?);
            }
            pages += 1;
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            if pages >= MAX_LIST_PAGES {
                return Err(McpError::Protocol(format!(
                    "tools/list pagination exceeded {MAX_LIST_PAGES} pages"
                )));
            }
        }
        Ok(tools)
    }

    /// `tools/call`：`arguments` 必须是 object；`isError` 按键返回、JSON-RPC 错误才是 `Err`。
    pub fn call_tool(
        &mut self,
        name: impl Into<String>,
        arguments: Value,
    ) -> Result<McpCallResult, McpError> {
        self.ensure_ready()?;
        if !arguments.is_object() {
            return Err(McpError::Protocol(
                "tools/call `arguments` must be a JSON object".to_owned(),
            ));
        }
        let name = name.into();
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )?;
        Ok(self.project_call_result(&result, &name))
    }

    /// 发现并注册全部 MCP 工具；返回注册成功的工具名（按 `tools/list` 顺序）。
    pub fn register_tools(&mut self, registry: &mut ToolRegistry) -> Result<Vec<String>, McpError> {
        let server = self.spec.name.clone();
        let timeout_ms = self.timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut names = Vec::new();
        for tool in self.list_tools()? {
            let definition = tool.as_tool_definition(&server, timeout_ms);
            names.push(definition.name.clone());
            registry.register(definition)?;
        }
        Ok(names)
    }

    /// 优雅关闭：`shutdown` 请求 → `exit` 通知 → 等退出（最多 [`SHUTDOWN_WAIT`]）→ kill。
    ///
    /// 幂等：重复调用 / `Broken` 状态下调用都返回 `Ok`，并保证进程被回收。
    pub fn shutdown(&mut self) -> Result<(), McpError> {
        if self.state == McpClientState::Shutdown {
            return Ok(());
        }
        if self.state == McpClientState::Ready {
            let previous = self.timeout;
            self.timeout = previous.min(SHUTDOWN_REQUEST_TIMEOUT);
            // 服务端可能不实现 shutdown；失败也只降级为 kill，不让调用方卡住。
            let _ = self.request("shutdown", json!({}));
            self.timeout = previous;
            let _ = self.send_notification("exit", json!({}));
        }
        self.close_and_wait_process();
        self.state = McpClientState::Shutdown;
        Ok(())
    }

    /// 崩溃/关闭后重启：清理旧进程 → 重新 spawn → 重新 `initialize`。
    pub fn restart(&mut self) -> Result<McpInitializeResult, McpError> {
        if self.state != McpClientState::Shutdown {
            let _ = self.shutdown();
        }
        self.close_and_wait_process();
        let handles = spawn_process(
            &self.spec,
            Arc::clone(&self.pending),
            Arc::clone(&self.stats),
        )?;
        self.child = Some(handles.child);
        self.stdin = Some(handles.stdin);
        self.rx = Some(handles.rx);
        self.initialize = None;
        self.state = McpClientState::Spawned;
        self.initialize()
    }

    fn ensure_ready(&mut self) -> Result<(), McpError> {
        match self.state {
            McpClientState::Ready => {}
            other => return Err(self.not_ready(other)),
        }
        if !self.is_alive() {
            let code = self.wait_exit_code(Duration::from_secs(1));
            self.mark_broken();
            return Err(McpError::ProcessExited { code });
        }
        Ok(())
    }

    fn not_ready(&self, state: McpClientState) -> McpError {
        McpError::NotReady {
            server: self.spec.name.clone(),
            state: state.as_str(),
        }
    }

    fn mark_broken(&mut self) {
        self.pending.store(0, Ordering::SeqCst);
        self.stdin.take();
        self.state = McpClientState::Broken;
    }

    /// 关闭 stdin 并等待子进程退出；超时则 kill。所有路径都回收进程（无残留）。
    fn close_and_wait_process(&mut self) {
        self.pending.store(0, Ordering::SeqCst);
        self.stdin.take();
        self.rx.take();
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }

    fn wait_exit_code(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            match self.child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(status))) => return status.code(),
                Some(Ok(None)) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                _ => return None,
            }
        }
    }

    fn write_message(&mut self, message: &Value) -> Result<(), McpError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.not_ready(self.state));
        };
        let line = serde_json::to_string(message)
            .map_err(|error| McpError::Protocol(format!("serialize outbound message: {error}")))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Err(error) = self.write_message(&message) {
            self.mark_broken();
            return Err(error);
        }
        Ok(())
    }

    /// 发一条 JSON-RPC 请求并等待匹配的 response（同步、单在途）。
    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        self.pending.store(request_id, Ordering::SeqCst);
        let message = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_message(&message) {
            self.pending.store(0, Ordering::SeqCst);
            self.mark_broken();
            return Err(error);
        }

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.pending.store(0, Ordering::SeqCst);
                // 取消是建议性的：先通知，迟到答复由 reader 线程丢弃并计数。
                let _ = self.write_message(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": request_id, "reason": "timeout" },
                }));
                self.stats
                    .timed_out_requests
                    .fetch_add(1, Ordering::Relaxed);
                return Err(McpError::Timeout {
                    method: method.to_owned(),
                    timeout: self.timeout,
                });
            }
            let wait = remaining.min(WAIT_SLICE);
            let received = match self.rx.as_ref() {
                Some(rx) => rx.recv_timeout(wait),
                None => return Err(self.not_ready(self.state)),
            };
            match received {
                Ok(response) => {
                    self.pending.store(0, Ordering::SeqCst);
                    return parse_response(&response);
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.pending.store(0, Ordering::SeqCst);
                    // 子进程退出（崩溃/被 kill）：在途请求以 ProcessExited 结算。
                    let code = self.wait_exit_code(Duration::from_secs(1));
                    self.mark_broken();
                    return Err(McpError::ProcessExited { code });
                }
            }
        }
    }

    fn project_call_result(&self, result: &Value, tool: &str) -> McpCallResult {
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let value = result
            .get("structuredContent")
            .filter(|structured| !structured.is_null())
            .cloned()
            .or_else(|| {
                result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| project_content(content, &self.spec.name, tool))
            })
            .unwrap_or_else(|| result.clone());
        let (value, truncated) = truncate_value(value, self.max_result_bytes);
        if truncated {
            self.stats.truncated_results.fetch_add(1, Ordering::Relaxed);
        }
        McpCallResult {
            value,
            is_error,
            truncated,
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // 兜底：不阻塞太久的收尾，直接关管道 + kill/wait（优雅路径用 shutdown）。
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.rx.take();
    }
}

/// 把 MCP `content[]` 投影成有限 JSON（image/resource 不内联二进制）。
fn project_content(content: &[Value], server: &str, tool: &str) -> Value {
    let mut texts: Vec<String> = Vec::new();
    let mut parts: Vec<Value> = Vec::new();
    let mut has_non_text = false;
    for (index, item) in content.iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    texts.push(text.to_owned());
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("image") => {
                has_non_text = true;
                let bytes = item.get("data").and_then(Value::as_str).map(str::len);
                parts.push(json!({
                    "type": "attachment",
                    "kind": "image",
                    "mime_type": item.get("mimeType").cloned().unwrap_or(Value::Null),
                    // v2 不做附件存储：只记录可复核的引用形态与字节数。
                    "reference": format!("attachment://mcp/{server}/{tool}/{index}"),
                    "bytes": bytes,
                }));
            }
            Some("resource") => {
                has_non_text = true;
                let resource = item.get("resource").cloned().unwrap_or(Value::Null);
                let uri = resource.get("uri").cloned().unwrap_or(Value::Null);
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    texts.push(text.to_owned());
                }
                parts.push(json!({
                    "type": "resource",
                    "uri": uri,
                    "text": resource.get("text").cloned().unwrap_or(Value::Null),
                }));
            }
            _ => {
                // 未知内容类型：原样保留（向前兼容），不丢结果。
                has_non_text = true;
                parts.push(item.clone());
            }
        }
    }
    if has_non_text {
        Value::Array(parts)
    } else if !texts.is_empty() {
        Value::String(texts.concat())
    } else {
        Value::Null
    }
}

/// 超过内联上限的裁剪策略：字符串取前缀；结构化结果替换为 `{truncated,...}` 摘要。
fn truncate_value(value: Value, max_bytes: usize) -> (Value, bool) {
    let serialized_bytes = serde_json::to_vec(&value)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if serialized_bytes <= max_bytes {
        return (value, false);
    }
    match value {
        Value::String(text) => {
            let prefix = prefix_bytes(&text, max_bytes);
            (Value::String(prefix), true)
        }
        other => {
            let preview_limit = max_bytes.saturating_sub(64).max(1);
            let preview = serde_json::to_string(&other)
                .map(|serialized| prefix_bytes(&serialized, preview_limit))
                .unwrap_or_default();
            (
                json!({
                    "truncated": true,
                    "original_bytes": serialized_bytes,
                    "preview": preview,
                }),
                true,
            )
        }
    }
}

fn prefix_bytes(text: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn parse_response(response: &Value) -> Result<Value, McpError> {
    let object = response
        .as_object()
        .ok_or_else(|| McpError::Protocol("json-rpc response must be an object".to_owned()))?;
    if let Some(error) = object.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown JSON-RPC error")
            .to_owned();
        return Err(McpError::Rpc {
            code,
            message,
            data: error.get("data").cloned(),
        });
    }
    object.get("result").cloned().ok_or_else(|| {
        McpError::Protocol("json-rpc response has neither `result` nor `error`".into())
    })
}

fn spawn_process(
    spec: &McpServerSpec,
    pending: Arc<AtomicU64>,
    stats: Arc<StatsInner>,
) -> Result<ProcessHandles, McpError> {
    let mut command = Command::new(&spec.command);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.envs(spec.env.iter().map(|(key, value)| (key, value)));
    hide_console_window(&mut command);
    let mut child = command.spawn().map_err(|source| McpError::Spawn {
        server: spec.name.clone(),
        source,
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::Protocol("child stdin pipe unavailable".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpError::Protocol("child stdout pipe unavailable".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| McpError::Protocol("child stderr pipe unavailable".to_owned()))?;
    let rx = spawn_reader(stdout, stats, pending);
    spawn_stderr_logger(stderr, spec.name.clone());
    Ok(ProcessHandles { child, stdin, rx })
}

/// stdout 读线程：NDJSON 逐行解析；通知计数、迟到响应丢弃、非法行计数。
///
/// 只把**匹配当前在途 ID 的 response** 转发给客户端；其余全部就地处理，
/// 这样超时后的迟到答复不会串到下一个请求上。
fn spawn_reader(
    stdout: ChildStdout,
    stats: Arc<StatsInner>,
    pending: Arc<AtomicU64>,
) -> Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                stats.invalid_lines.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let Some(object) = message.as_object() else {
                stats.invalid_lines.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let has_id = object.contains_key("id");
            let has_method = object.contains_key("method");
            match (has_id, has_method) {
                (true, true) => {
                    // 服务端 → 客户端的请求（sampling/roots 等）：最小客户端不实现。
                    stats
                        .server_requests_ignored
                        .fetch_add(1, Ordering::Relaxed);
                }
                (false, true) => {
                    stats.notifications.fetch_add(1, Ordering::Relaxed);
                    if object.get("method").and_then(Value::as_str) == Some("notifications/message")
                    {
                        stats.log_notifications.fetch_add(1, Ordering::Relaxed);
                    }
                }
                (true, false) => {
                    let expected = pending.load(Ordering::SeqCst);
                    let matches = object
                        .get("id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| expected != 0 && id == expected);
                    if !matches {
                        stats.late_responses.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if tx.send(message).is_err() {
                        break;
                    }
                }
                (false, false) => {
                    stats.invalid_lines.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    rx
}

fn spawn_stderr_logger(stderr: ChildStderr, server: String) {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(line) if !line.trim().is_empty() => eprintln!("[mcp:{server}] {line}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

/// Windows：GUI 父进程 spawn console 子进程时避免弹窗（仅创建标志，不涉及 unsafe）。
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`（winbase.h）。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// 便捷入口：发现并注册 MCP 工具（等价于 [`McpClient::register_tools`]）。
pub fn register_tools(
    client: &mut McpClient,
    registry: &mut ToolRegistry,
) -> Result<Vec<String>, McpError> {
    client.register_tools(registry)
}

/// 按名字索引工具（诊断/查找用；重复名保留首个）。
pub fn tools_by_name(tools: &[McpTool]) -> BTreeMap<&str, &McpTool> {
    let mut index = BTreeMap::new();
    for tool in tools {
        index.entry(tool.name.as_str()).or_insert(tool);
    }
    index
}
// Intentional code-quality length regression.
