//! 子进程侧的会话循环：读 stdin NDJSON、分发、写 stdout（spec/cpp-v1.md §12）。
//!
//! 并发模型（最小但可用）：
//! - 一个 **stdin 读线程**把每一行送进 mpsc；
//! - 主循环处理 `process.hello/ready`、`process.ping`、`process.shutdown`、`tool.cancel`、
//!   `ui.render`，并把每个 `tool.invoke` 交给一个 worker 线程（共享 `Mutex<handler>`，
//!   因此同一时刻只有一个工具在跑，符合"一进程一 generation"的最小语义）；
//! - `tool.cancel` 通过 `CancelFlag`（`Arc<AtomicBool>`）通知在途调用；handler 自己轮询；
//! - stdout 只写协议（每行 flush），日志由作者写 stderr。

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use thiserror::Error;

use crate::protocol::{
    error_code, kind, ActionPayload, ChatNodeContext, Envelope, FileViewContext, HelloPayload,
    InvokePayload, ProtocolError, ReadyDecls, ReadyPayload, RenderRequest, ToolViewContext,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use crate::{ToolResult, MAX_TOOL_TIMEOUT_MS};

/// 写端：stdout 或测试用内存 sink；统一包成 trait object 便于 worker 线程共享。
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// 取消标记：宿主发 `tool.cancel` 后由子进程置位，handler 自行轮询。
#[derive(Debug, Clone, Default)]
pub struct CancelFlag {
    inner: Arc<AtomicBool>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位；返回本次调用是否完成了 `未取消 -> 已取消` 迁移。
    pub fn cancel(&self) -> bool {
        !self.inner.swap(true, Ordering::SeqCst)
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

/// 一次工具调用的入参（`tool.invoke` 的 child 侧视图）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub timeout_ms: u64,
    pub cancelable: bool,
    pub generation: Option<u64>,
}

impl ToolCall {
    /// 取一个命名参数。
    pub fn argument(&self, name: &str) -> Option<&Value> {
        self.arguments.get(name)
    }

    fn from_invoke(call_id: String, invoke: InvokePayload) -> Self {
        Self {
            call_id,
            tool: invoke.tool,
            arguments: invoke.arguments,
            timeout_ms: invoke.timeout_ms.min(MAX_TOOL_TIMEOUT_MS),
            cancelable: invoke.cancelable,
            generation: invoke.generation,
        }
    }
}

/// 工具执行失败（模型/宿主可见）；`code()` 映射到 envelope 错误码。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolError {
    #[error("{0}")]
    Failed(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("cancelled")]
    Cancelled,
    #[error("result too large: {0}")]
    TooLarge(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ToolError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::InvalidArguments(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// 稳定的错误码（spec/cpp-v1.md §12.2）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Failed(_) => error_code::E_INTERNAL,
            Self::InvalidArguments(_) => error_code::E_SCHEMA,
            Self::Cancelled => error_code::E_CANCELLED,
            Self::TooLarge(_) => error_code::E_TOO_LARGE,
            Self::Internal(_) => error_code::E_INTERNAL,
        }
    }
}

/// 工具处理器。闭包 `FnMut(&ToolCall, &CancelFlag) -> Result<ToolResult, ToolError>` 自动实现。
pub trait ToolHandler: Send + 'static {
    fn call(&mut self, call: &ToolCall, cancel: &CancelFlag) -> Result<ToolResult, ToolError>;
}

impl<F> ToolHandler for F
where
    F: FnMut(&ToolCall, &CancelFlag) -> Result<ToolResult, ToolError> + Send + 'static,
{
    fn call(&mut self, call: &ToolCall, cancel: &CancelFlag) -> Result<ToolResult, ToolError> {
        self(call, cancel)
    }
}

/// 声明式视图处理器（`ui.render`）；返回 ADC-v1 元素树 JSON。
///
/// 渲染本身由宿主/`ui-gpui` 负责，SDK 只负责把树送过去。
pub trait ViewHandler: Send + 'static {
    fn render(&mut self, view: &str, state_seq: u64) -> Result<Value, ToolError>;

    fn render_tool(
        &mut self,
        view: &str,
        state_seq: u64,
        _context: &ToolViewContext,
    ) -> Result<Value, ToolError> {
        self.render(view, state_seq)
    }

    fn render_file(
        &mut self,
        view: &str,
        state_seq: u64,
        _context: &FileViewContext,
    ) -> Result<Value, ToolError> {
        self.render(view, state_seq)
    }

    fn render_node(
        &mut self,
        view: &str,
        state_seq: u64,
        _context: &ChatNodeContext,
    ) -> Result<Value, ToolError> {
        self.render(view, state_seq)
    }
}

impl<F> ViewHandler for F
where
    F: FnMut(&str, u64) -> Result<Value, ToolError> + Send + 'static,
{
    fn render(&mut self, view: &str, state_seq: u64) -> Result<Value, ToolError> {
        self(view, state_seq)
    }
}

/// 没有注册任何 view 的默认 `ViewHandler`（`ui.render` 返回 `E_INTERNAL`）。
struct NoViews;

impl ViewHandler for NoViews {
    fn render(&mut self, view: &str, _state_seq: u64) -> Result<Value, ToolError> {
        Err(ToolError::internal(format!(
            "no view handler registered for `{view}`"
        )))
    }
}

/// ADC 交互动作处理器（`ui.action`，M4 W3）。
///
/// `action` 为 `call` / `command`，`payload` 为其参数。命令槽位传 `run` 方法名作为
/// `view`，并在 `payload.id` 中传注册 ID。fire-and-forget，返回错误只写
/// stderr，不影响协议 stdout。
pub trait ActionHandler: Send + 'static {
    fn action(&mut self, view: &str, action: &str, payload: &Value) -> Result<(), ToolError>;
}

impl<F> ActionHandler for F
where
    F: FnMut(&str, &str, &Value) -> Result<(), ToolError> + Send + 'static,
{
    fn action(&mut self, view: &str, action: &str, payload: &Value) -> Result<(), ToolError> {
        self(view, action, payload)
    }
}

/// 默认动作处理器：接受但忽略（保持 M3 作者的 `serve_with_views` 行为）。
struct NoActions;

impl ActionHandler for NoActions {
    fn action(&mut self, _view: &str, _action: &str, _payload: &Value) -> Result<(), ToolError> {
        Ok(())
    }
}

/// 会话错误：I/O 或协议。
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

enum InputEvent {
    Line(String),
    Eof,
    ReaderError(String),
}

/// 子进程会话入口。
pub struct ChildSession;

impl ChildSession {
    /// 用 stdin/stdout 运行会话（生产路径）。
    pub fn serve<H: ToolHandler>(decls: ReadyDecls, handler: H) -> Result<(), SessionError> {
        Self::serve_with_views(decls, NoViews, handler)
    }

    /// 与 [`ChildSession::serve`] 相同，但额外注册 `ui.render` 处理器。
    pub fn serve_with_views<H: ToolHandler, V: ViewHandler>(
        decls: ReadyDecls,
        views: V,
        handler: H,
    ) -> Result<(), SessionError> {
        Self::serve_with_views_and_actions(decls, views, NoActions, handler)
    }

    /// 同时注册 `ui.render` 与 `ui.action` 处理器（M4 W3 面板交互）。
    pub fn serve_with_views_and_actions<H: ToolHandler, V: ViewHandler, A: ActionHandler>(
        decls: ReadyDecls,
        views: V,
        actions: A,
        handler: H,
    ) -> Result<(), SessionError> {
        let reader = std::io::BufReader::new(std::io::stdin());
        let writer = std::io::stdout();
        Self::serve_with_io_views_actions(reader, writer, decls, views, actions, handler)
    }

    /// 可注入 I/O 的会话（集成/单元测试用）。
    pub fn serve_with_io<R, W, H>(
        reader: R,
        writer: W,
        decls: ReadyDecls,
        handler: H,
    ) -> Result<(), SessionError>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
        H: ToolHandler,
    {
        Self::serve_with_io_and_views(reader, writer, decls, NoViews, handler)
    }

    /// 完整入口：可注入 I/O + views。
    pub fn serve_with_io_and_views<R, W, H, V>(
        reader: R,
        writer: W,
        decls: ReadyDecls,
        views: V,
        handler: H,
    ) -> Result<(), SessionError>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
        H: ToolHandler,
        V: ViewHandler,
    {
        Self::serve_with_io_views_actions(reader, writer, decls, views, NoActions, handler)
    }

    /// 完整入口：可注入 I/O + views + actions。
    pub fn serve_with_io_views_actions<R, W, H, V, A>(
        reader: R,
        writer: W,
        decls: ReadyDecls,
        views: V,
        actions: A,
        handler: H,
    ) -> Result<(), SessionError>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
        H: ToolHandler,
        V: ViewHandler,
        A: ActionHandler,
    {
        let (events, receiver) = mpsc::channel::<InputEvent>();
        thread::Builder::new()
            .name("cpp-process-stdin".to_owned())
            .spawn(move || read_loop(reader, events))
            .map_err(SessionError::Io)?;

        let writer: SharedWriter = Arc::new(Mutex::new(Box::new(writer)));
        let handler: Arc<Mutex<Box<dyn ToolHandler>>> = Arc::new(Mutex::new(Box::new(handler)));
        let views: Arc<Mutex<Box<dyn ViewHandler>>> = Arc::new(Mutex::new(Box::new(views)));
        let actions: Arc<Mutex<Box<dyn ActionHandler>>> = Arc::new(Mutex::new(Box::new(actions)));
        let cancels: Arc<Mutex<HashMap<String, CancelFlag>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let mut hello_seen = false;

        while let Ok(event) = receiver.recv() {
            let line = match event {
                InputEvent::Eof => break,
                InputEvent::ReaderError(error) => {
                    return Err(SessionError::Io(std::io::Error::other(error)))
                }
                InputEvent::Line(line) => line,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let envelope = match Envelope::parse(line) {
                Ok(envelope) => envelope,
                Err(error) => {
                    write_error(&writer, kind::PROCESS_PONG, None, &error)?;
                    continue;
                }
            };
            match envelope.kind.as_str() {
                kind::PROCESS_HELLO => {
                    let hello: HelloPayload = match serde_json::from_value(envelope.payload.clone())
                    {
                        Ok(hello) => hello,
                        Err(error) => {
                            write_error(
                                &writer,
                                kind::PROCESS_READY,
                                envelope.request_id.clone(),
                                &ProtocolError::InvalidFrame(format!(
                                    "invalid process.hello payload: {error}"
                                )),
                            )?;
                            continue;
                        }
                    };
                    if hello.protocol != PROTOCOL_VERSION {
                        write_error(
                            &writer,
                            kind::PROCESS_READY,
                            envelope.request_id.clone(),
                            &ProtocolError::UnsupportedVersion {
                                actual: hello.protocol,
                                expected: PROTOCOL_VERSION.to_owned(),
                            },
                        )?;
                        continue;
                    }
                    hello_seen = true;
                    let ready = ReadyPayload {
                        protocol: PROTOCOL_VERSION.to_owned(),
                        plugin_id: hello.plugin_id,
                        generation: Some(hello.generation),
                        tools: decls.tools.clone(),
                        panels: decls.panels.clone(),
                        slots: decls.slots.clone(),
                    };
                    write_envelope(
                        &writer,
                        &Envelope::with_payload(kind::PROCESS_READY, ready)?,
                    )?;
                }
                kind::PROCESS_PING => {
                    let mut pong = Envelope::new(kind::PROCESS_PONG);
                    pong.request_id = envelope.request_id.clone();
                    write_envelope(&writer, &pong)?;
                }
                kind::TOOL_INVOKE => {
                    let Some(request_id) = envelope
                        .request_id
                        .clone()
                        .filter(|request_id| !request_id.is_empty())
                    else {
                        write_error(
                            &writer,
                            kind::TOOL_RESULT,
                            None,
                            &ProtocolError::InvalidFrame(
                                "tool.invoke requires a non-empty request_id".to_owned(),
                            ),
                        )?;
                        continue;
                    };
                    let invoke: InvokePayload =
                        match serde_json::from_value(envelope.payload.clone()) {
                            Ok(invoke) => invoke,
                            Err(error) => {
                                write_tool_error(
                                    &writer,
                                    &request_id,
                                    &ToolError::invalid_arguments(format!(
                                        "invalid tool.invoke payload: {error}"
                                    )),
                                )?;
                                continue;
                            }
                        };
                    let Some(decl) = decls.tool(&invoke.tool) else {
                        write_tool_error(
                            &writer,
                            &request_id,
                            &ToolError::invalid_arguments(format!(
                                "unknown tool `{}`",
                                invoke.tool
                            )),
                        )?;
                        continue;
                    };
                    if !hello_seen {
                        write_tool_error(
                            &writer,
                            &request_id,
                            &ToolError::internal("process.hello was not received"),
                        )?;
                        continue;
                    }
                    let cancelable = invoke.cancelable && decl.cancelable;
                    let cancel = CancelFlag::new();
                    if cancelable {
                        cancels
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(request_id.clone(), cancel.clone());
                    }
                    let call = ToolCall::from_invoke(request_id.clone(), invoke);
                    workers.push(spawn_tool_worker(
                        call,
                        cancel,
                        Arc::clone(&writer),
                        Arc::clone(&handler),
                        Arc::clone(&cancels),
                        request_id,
                    )?);
                }
                kind::TOOL_CANCEL => {
                    if let Some(request_id) = envelope.request_id.as_deref() {
                        let flag = cancels
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .get(request_id)
                            .cloned();
                        if let Some(flag) = flag {
                            flag.cancel();
                        }
                    }
                }
                kind::UI_RENDER => {
                    write_envelope(&writer, &render_result(&views, &envelope))?;
                }
                kind::UI_ACTION => {
                    // fire-and-forget：没有 result envelope；错误只写 stderr。
                    match serde_json::from_value::<ActionPayload>(envelope.payload.clone()) {
                        Ok(payload) => {
                            let mut guard = actions
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if let Err(error) =
                                guard.action(&payload.view, &payload.action, &payload.payload)
                            {
                                eprintln!(
                                    "[cpp-process] ui.action `{}` on `{}` failed: {error}",
                                    payload.action, payload.view
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("[cpp-process] invalid ui.action payload: {error}");
                        }
                    }
                }
                kind::PROCESS_SHUTDOWN => {
                    write_envelope(&writer, &Envelope::new(kind::PROCESS_BYE))?;
                    break;
                }
                other => {
                    write_error(
                        &writer,
                        kind::PROCESS_PONG,
                        envelope.request_id.clone(),
                        &ProtocolError::InvalidFrame(format!("unsupported kind `{other}`")),
                    )?;
                }
            }
        }

        // 优雅退出前等待在途 worker（真实进程里宿主 5 s 后会 TerminateJobObject 兜底）。
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

fn render_result(views: &Arc<Mutex<Box<dyn ViewHandler>>>, envelope: &Envelope) -> Envelope {
    let request_id = envelope.request_id.clone();
    let payload: RenderRequest = match serde_json::from_value(envelope.payload.clone()) {
        Ok(payload) => payload,
        Err(error) => {
            return Envelope::with_error(
                kind::UI_RESULT,
                request_id,
                error_code::E_PROTOCOL,
                format!("invalid ui.render payload: {error}"),
            )
        }
    };
    if usize::from(payload.tool_context.is_some())
        + usize::from(payload.file_context.is_some())
        + usize::from(payload.node_context.is_some())
        > 1
    {
        return Envelope::with_error(
            kind::UI_RESULT,
            request_id,
            error_code::E_PROTOCOL,
            "ui.render must specify at most one context",
        );
    }
    let rendered = {
        let mut guard = views
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        catch_unwind(AssertUnwindSafe(|| {
            match (
                payload.tool_context.as_ref(),
                payload.file_context.as_ref(),
                payload.node_context.as_ref(),
            ) {
                (Some(context), _, _) => {
                    guard.render_tool(&payload.view, payload.state_seq, context)
                }
                (_, Some(context), _) => {
                    guard.render_file(&payload.view, payload.state_seq, context)
                }
                (_, _, Some(context)) => {
                    guard.render_node(&payload.view, payload.state_seq, context)
                }
                _ => guard.render(&payload.view, payload.state_seq),
            }
        }))
    };
    match rendered {
        Ok(Ok(tree)) => {
            let mut envelope =
                Envelope::with_payload(kind::UI_RESULT, serde_json::json!({ "tree": tree }))
                    .unwrap_or_else(|_| Envelope::new(kind::UI_RESULT));
            envelope.request_id = request_id;
            envelope
        }
        Ok(Err(error)) => {
            Envelope::with_error(kind::UI_RESULT, request_id, error.code(), error.to_string())
        }
        Err(payload) => Envelope::with_error(
            kind::UI_RESULT,
            request_id,
            error_code::E_INTERNAL,
            panic_message(payload),
        ),
    }
}

fn spawn_tool_worker(
    call: ToolCall,
    cancel: CancelFlag,
    writer: SharedWriter,
    handler: Arc<Mutex<Box<dyn ToolHandler>>>,
    cancels: Arc<Mutex<HashMap<String, CancelFlag>>>,
    request_id: String,
) -> Result<JoinHandle<()>, SessionError> {
    thread::Builder::new()
        .name("cpp-process-tool".to_owned())
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let mut guard = handler
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.call(&call, &cancel)
            }));
            let envelope = match outcome {
                Ok(Ok(result)) => result_envelope(&request_id, result),
                Ok(Err(error)) => Envelope::with_error(
                    kind::TOOL_RESULT,
                    Some(request_id.clone()),
                    error.code(),
                    error.to_string(),
                ),
                Err(payload) => Envelope::with_error(
                    kind::TOOL_RESULT,
                    Some(request_id.clone()),
                    error_code::E_INTERNAL,
                    panic_message(payload),
                ),
            };
            let _ = write_envelope(&writer, &envelope);
            if let Ok(mut map) = cancels.lock() {
                map.remove(&request_id);
            }
        })
        .map_err(SessionError::Io)
}

/// 把 `ToolResult` 包装成 `tool.result`；超过帧上限时改发 `E_TOO_LARGE`。
fn result_envelope(request_id: &str, result: ToolResult) -> Envelope {
    let payload = match serde_json::to_value(&result) {
        Ok(payload) => payload,
        Err(error) => {
            return Envelope::with_error(
                kind::TOOL_RESULT,
                Some(request_id.to_owned()),
                error_code::E_INTERNAL,
                format!("cannot serialize tool result: {error}"),
            )
        }
    };
    let mut envelope = Envelope::new(kind::TOOL_RESULT);
    envelope.request_id = Some(request_id.to_owned());
    envelope.payload = payload;
    match envelope.encoded_len() {
        Ok(size) if size <= MAX_FRAME_BYTES => envelope,
        _ => Envelope::with_error(
            kind::TOOL_RESULT,
            Some(request_id.to_owned()),
            error_code::E_TOO_LARGE,
            format!("tool result exceeds the {MAX_FRAME_BYTES} byte frame limit"),
        ),
    }
}

fn read_loop<R: BufRead>(mut reader: R, events: mpsc::Sender<InputEvent>) {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = events.send(InputEvent::Eof);
                break;
            }
            Ok(_) => {
                if events.send(InputEvent::Line(line)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = events.send(InputEvent::ReaderError(error.to_string()));
                break;
            }
        }
    }
}

fn write_error(
    writer: &SharedWriter,
    response_kind: &str,
    request_id: Option<String>,
    error: &ProtocolError,
) -> Result<(), SessionError> {
    let envelope = Envelope::with_error(response_kind, request_id, error.code(), error.to_string());
    write_envelope(writer, &envelope)
}

fn write_tool_error(
    writer: &SharedWriter,
    request_id: &str,
    error: &ToolError,
) -> Result<(), SessionError> {
    let envelope = Envelope::with_error(
        kind::TOOL_RESULT,
        Some(request_id.to_owned()),
        error.code(),
        error.to_string(),
    );
    write_envelope(writer, &envelope)
}

fn write_envelope(writer: &SharedWriter, envelope: &Envelope) -> Result<(), SessionError> {
    let line = match envelope.encode_line() {
        Ok(line) => line,
        Err(error) => {
            // 兜底：连错误 envelope 都编不出来时，发一条固定的小错误，保证协议仍然闭合。
            let fallback = Envelope::with_error(
                kind::TOOL_RESULT,
                envelope.request_id.clone(),
                error_code::E_INTERNAL,
                error.to_string(),
            );
            fallback.encode_line().unwrap_or_else(|_| {
                "{\"version\":\"cpp-process/v1\",\"kind\":\"tool.result\",\"payload\":null}\n"
                    .to_owned()
            })
        }
    };
    let mut guard = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.write_all(line.as_bytes())?;
    guard.flush()?;
    Ok(())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "tool handler panicked".to_owned()
    }
}

#[cfg(test)]
mod tests;
