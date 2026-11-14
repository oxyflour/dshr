//! CPP-v1 `external-process` 插件作者 SDK（`cpp-process/v1`，spec/cpp-v1.md §12）。
//!
//! 这是**插件侧**的唯一依赖：作者用它在子进程里实现一个 `ChildSession`，
//! 通过 NDJSON over stdio 与 Rust 宿主（`dshr-plugin-host::external`）说话：
//!
//! ```no_run
//! use dshr_plugin_sdk::{
//!     CancelFlag, ChildSession, ReadyDecls, ToolCall, ToolDecl, ToolResult,
//! };
//!
//! # fn main() -> Result<(), dshr_plugin_sdk::SessionError> {
//! let tool = ToolDecl::new("echo", "echo back the input text");
//! let decls = ReadyDecls::new(vec![tool], Vec::new());
//! ChildSession::serve(
//!     decls,
//!     |call: &ToolCall, _cancel: &CancelFlag| {
//!         let text = call
//!             .argument("text")
//!             .and_then(|value| value.as_str())
//!             .unwrap_or_default();
//!         Ok(ToolResult::text(text))
//!     },
//! )
//! # }
//! ```
//!
//! 约束（与 spec §12 一致）：
//! - stdout **只写协议**（一行一条 JSON，≤ [`MAX_FRAME_BYTES`]）；日志走 stderr；
//! - `process.hello` → `process.ready`，`tool.invoke` → `tool.result`，
//!   `tool.cancel` 置取消标记，`process.shutdown` → `process.bye` 并退出；
//! - 未知 kind / 非法帧返回带 `E_PROTOCOL` 的 error envelope，不 panic。
//!
//! ## 与宿主/运行时替换的缝隙
//!
//! 本 crate 只描述线上形状（envelope/声明/结果），不依赖 `plugin-host`、不引入执行器。
//! 将来 `external-process` 退场、WASM 主机上位时（ADR-0010），作者侧这些类型可以
//! 直接映射到 WIT 世界；宿主侧换成 wasmtime 也不影响本 SDK 的协议层。

#![forbid(unsafe_code)]

pub mod protocol;
pub mod session;

pub use dshr_plugin_api::{ContentPart, ToolResult};
pub use protocol::{
    error_code, kind, CancelPayload, Envelope, EnvelopeError, HelloPayload, InvokePayload,
    PanelDecl, ProtocolError, ReadyDecls, ReadyPayload, RenderPayload, ShutdownPayload, ToolDecl,
    DEFAULT_SHUTDOWN_TIMEOUT_MS, DEFAULT_TOOL_TIMEOUT_MS, MAX_FRAME_BYTES, MAX_SCHEMA_BYTES,
    MAX_TOOL_TIMEOUT_MS, PROTOCOL_VERSION,
};
pub use session::{
    CancelFlag, ChildSession, SessionError, ToolCall, ToolError, ToolHandler, ViewHandler,
};
