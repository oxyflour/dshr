//! 首批 L2 Host Service 的 Rust 实现：`working-directory/v1` 与 `session-log/v1`。
//!
//! 数据权威在 Rust Session Log（`docs/spec/session-log.md`），这里只做方法级的
//! **adapter 投影**：
//! - `SessionAccess` 是会话存储的抽象，由 `chat.rs` 注入真实实现（`session-store` +
//!   `SessionHandle`），测试用内存 fake；`append_event` 用 `&self` + 内部可变性，
//!   这样两个 service 可以共享同一个 `Arc<dyn SessionAccess>`；
//! - `WorkingDirectoryService`（spec §13.2 / m3-l2-services §3.1）：`get/ensure/set`，
//!   目录权威是 Session Log 的 `working-directory/change` 事件；`get` 不碰 FS；
//! - `SessionLogService`（m3-l2-services §3.2）：`session.describe` / `events.read`，
//!   分页 `max_events ≤ 500`、单页 ≤ 512 KiB，`eof/next_seq` 语义。
//!
//! 会话身份只从信封 `session_id` 来（`HostServiceMethod.requires_session = true`，
//! 由 `HostServiceRouter` 与各方法再校验一次），args 里不重复。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dshr_compat_dsh_protocol::{HostServiceKind, HostServiceMethod, HostServiceTable};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::service::{HostService, ServiceCallContext, ServiceError};

/// `working-directory/v1` 服务键。
pub const WORKING_DIRECTORY_V1: &str = "working-directory/v1";
/// `session-log/v1` 服务键。
pub const SESSION_LOG_V1: &str = "session-log/v1";
/// `session-projections/v1` 服务键（Sidecar 投影注册表的数据面）。
pub const SESSION_PROJECTIONS_V1: &str = "session-projections/v1";
/// 工作目录变更事件类型（官方 `dsh-working-directory` 同名事件）。
pub const WORKING_DIRECTORY_CHANGED: &str = "working-directory/change";
/// `events.read` 单页事件数上限（m3-l2-services §3.2）。
pub const MAX_EVENTS_PER_PAGE: u64 = 500;
/// `events.read` 单页序列化上限（512 KiB）。
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// `events.append` 单条事件数据上限（64 KiB；官方 todo/write 等远小于此）。
pub const MAX_APPEND_BYTES: usize = 64 * 1024;
/// `events.append` 事件类型名上限。
pub const MAX_EVENT_TYPE_CHARS: usize = 128;
/// `session.describe` 返回的会话头 schema 版本（`session-log.md` §2）。
pub const SESSION_HEADER_VERSION: u32 = 1;

/// 会话头投影（只含 Sidecar 需要的字段，不含 `Agent`/`Session` 对象）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeaderView {
    pub id: String,
    /// 会话创建时的目录；`working-directory/change` 事件优先。
    pub cwd: Option<String>,
    /// RFC 3339 UTC。
    pub created_at: String,
    pub parent_session_id: Option<String>,
    /// 根会话 0，子会话递增（`session-log.md` §7）。
    pub delegation_depth: u32,
}

/// 事件投影：wire 形状 `{seq, type, data}`（m3-l2-services §3.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventView {
    pub seq: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
}

/// `events.read` 一页。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<SessionEventView>,
    /// 下一页起点（`最后一条事件 seq + 1`；无事件时等于请求的 `from_seq`）。
    pub next_seq: u64,
    /// 是否已到 Session Log 末尾。
    pub eof: bool,
}

/// 会话存储抽象：由 `chat.rs` 注入真实实现（测试用 fake）。
///
/// 约定：未知/已销毁会话必须返回 `E_SERVICE_UNAVAILABLE`；内部 I/O 失败返回
/// `E_INTERNAL`。`append_event` 取 `&self` 是为了让两个 service 共享一个
/// `Arc<dyn SessionAccess>`（真实实现内部加锁串行化，符合 `session-log.md` I2）。
pub trait SessionAccess: Send + Sync {
    fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError>;

    /// 从 `from_seq` 起读至多 `max_events` 条；实现负责 `next_seq`/`eof`。
    fn read_events(
        &self,
        session_id: &str,
        from_seq: u64,
        max_events: usize,
    ) -> Result<EventPage, ServiceError>;

    /// 追加一条事件并返回分配的 `seq`（唯一提交者串行化）。
    fn append_event(
        &self,
        session_id: &str,
        event_type: &str,
        data: Value,
    ) -> Result<u64, ServiceError>;

    /// 最后一条 `working-directory/change` 的 `cwd`（没有则 `None`）。
    fn latest_working_directory(&self, session_id: &str) -> Option<String>;
}

/// `working-directory/v1`（插件可见名 `ctx.workingDirectory`）。
pub struct WorkingDirectoryService {
    access: Arc<dyn SessionAccess>,
    /// 宿主配置的默认目录（通常是 workspace root，`DSHR_WORKSPACE`）。
    default_directory: PathBuf,
    table: HostServiceTable,
}

impl WorkingDirectoryService {
    pub fn new(access: Arc<dyn SessionAccess>, default_directory: impl Into<PathBuf>) -> Self {
        Self {
            access,
            default_directory: default_directory.into(),
            table: working_directory_table(),
        }
    }

    fn get(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        parse_empty_args(args)?;
        let session_id = require_session(ctx)?;
        let header = self.access.header(session_id)?;
        let cwd = self
            .access
            .latest_working_directory(session_id)
            .or(header.cwd)
            .unwrap_or_else(|| self.default_directory_string());
        Ok(json!({ "cwd": cwd }))
    }

    fn ensure(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        parse_empty_args(args)?;
        let session_id = require_session(ctx)?;
        let header = self.access.header(session_id)?;
        let current = self
            .access
            .latest_working_directory(session_id)
            .or_else(|| header.cwd.clone())
            .unwrap_or_else(|| self.default_directory_string());

        if let Some(normalized) = normalize_existing_directory(Path::new(&current)) {
            return Ok(json!({ "cwd": normalized, "recovered": false }));
        }

        // 目录消失：回落到 header.cwd ?? default_directory，并 append 变更事件。
        let fallback = header
            .cwd
            .clone()
            .unwrap_or_else(|| self.default_directory_string());
        let recovered = normalize_existing_directory(Path::new(&fallback)).ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "working directory and fallback directory are not accessible",
            )
            .with_detail(json!({
                "reason": "working-directory-unavailable",
                "cwd": current,
                "fallback": fallback,
            }))
        })?;
        if !same_directory(&current, &recovered) {
            self.access.append_event(
                session_id,
                WORKING_DIRECTORY_CHANGED,
                json!({ "cwd": recovered }),
            )?;
        }
        Ok(json!({ "cwd": recovered, "recovered": true }))
    }

    fn set(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        let args: SetArgs = parse_args(args)?;
        if args.path.trim().is_empty() {
            return Err(ServiceError::args("/args/path", "path must not be empty"));
        }
        let session_id = require_session(ctx)?;
        let header = self.access.header(session_id)?;
        let current = self
            .access
            .latest_working_directory(session_id)
            .or_else(|| header.cwd.clone())
            .unwrap_or_else(|| self.default_directory_string());

        let candidate = if Path::new(&args.path).is_absolute() {
            PathBuf::from(&args.path)
        } else {
            Path::new(&current).join(&args.path)
        };
        let normalized = normalize_existing_directory(&candidate).ok_or_else(|| {
            ServiceError::args(
                "/args/path",
                "path must exist and be a directory (`not-a-directory`)",
            )
            .with_detail(json!({
                "reason": "not-a-directory",
                "path": args.path,
            }))
        })?;

        if same_directory(&current, &normalized) {
            // 幂等：set 到相同目录不产生事件（与官方 dsh-working-directory 一致）。
            return Ok(json!({ "cwd": normalized }));
        }
        self.access.append_event(
            session_id,
            WORKING_DIRECTORY_CHANGED,
            json!({ "cwd": normalized }),
        )?;
        Ok(json!({ "cwd": normalized }))
    }

    fn default_directory_string(&self) -> String {
        display_path(&self.default_directory)
    }
}

impl HostService for WorkingDirectoryService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        match method {
            "get" => self.get(args, ctx),
            "ensure" => self.ensure(args, ctx),
            "set" => self.set(args, ctx),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown working-directory method `{other}`"),
            )),
        }
    }
}

/// `session-log/v1`（配 `ctx.sessionProjections` 适配器；宿主只提供数据面）。
pub struct SessionLogService {
    access: Arc<dyn SessionAccess>,
    table: HostServiceTable,
}

impl SessionLogService {
    pub fn new(access: Arc<dyn SessionAccess>) -> Self {
        Self {
            access,
            table: session_log_table(),
        }
    }
}

impl HostService for SessionLogService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        match method {
            "session.describe" => describe_payload(self.access.as_ref(), args, ctx),
            "events.read" => read_events_payload(self.access.as_ref(), args, ctx),
            "events.append" => append_event_payload(self.access.as_ref(), args, ctx),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown session-log method `{other}`"),
            )),
        }
    }
}

/// `session-projections/v1`：Sidecar 的投影注册表（`ctx.sessionProjections`）使用的
/// 数据面读服务。语义与 `session-log/v1` 一致（`session.describe` 给 `init` 的 header，
/// `events.read` 给分页事件流），但 service_key 独立，便于白名单按投影需求单独授权。
pub struct SessionProjectionsService {
    access: Arc<dyn SessionAccess>,
    table: HostServiceTable,
}

impl SessionProjectionsService {
    pub fn new(access: Arc<dyn SessionAccess>) -> Self {
        Self {
            access,
            table: session_projections_table(),
        }
    }
}

impl HostService for SessionProjectionsService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        match method {
            "session.describe" => describe_payload(self.access.as_ref(), args, ctx),
            "events.read" => read_events_payload(self.access.as_ref(), args, ctx),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown session-projections method `{other}`"),
            )),
        }
    }
}

/// `session.describe` 的共享实现（session-log / session-projections 两个 service key 共用）。
fn describe_payload(
    access: &dyn SessionAccess,
    args: &Value,
    ctx: &ServiceCallContext,
) -> Result<Value, ServiceError> {
    parse_empty_args(args)?;
    let session_id = require_session(ctx)?;
    let header = access.header(session_id)?;
    Ok(json!({
        "header": {
            "id": header.id,
            "cwd": header.cwd,
            "created_at": header.created_at,
            "version": SESSION_HEADER_VERSION,
            "parent": header.parent_session_id,
            "delegation_depth": header.delegation_depth,
        }
    }))
}

/// `events.read` 的共享实现：分页 + 单页字节上限（m3-l2-services §3.2）。
fn read_events_payload(
    access: &dyn SessionAccess,
    args: &Value,
    ctx: &ServiceCallContext,
) -> Result<Value, ServiceError> {
    let args: EventsReadArgs = parse_args(args)?;
    let session_id = require_session(ctx)?;
    if args.max_events == 0 {
        return Err(ServiceError::args(
            "/args/max_events",
            "max_events must be >= 1",
        ));
    }
    if args.max_events > MAX_EVENTS_PER_PAGE {
        return Err(ServiceError::new(
            "E_TOO_LARGE",
            format!(
                "max_events {} exceeds {MAX_EVENTS_PER_PAGE}",
                args.max_events
            ),
        ));
    }
    // 未知/已销毁会话 → E_SERVICE_UNAVAILABLE（来自 SessionAccess 的约定）。
    access.header(session_id)?;
    let page = access.read_events(session_id, args.from_seq, args.max_events as usize)?;
    if page.events.len() as u64 > MAX_EVENTS_PER_PAGE {
        return Err(ServiceError::new(
            "E_TOO_LARGE",
            "session access returned more events than the page limit",
        ));
    }
    let reply = json!({
        "events": page.events,
        "next_seq": page.next_seq,
        "eof": page.eof,
    });
    let bytes = serde_json::to_vec(&reply)
        .map_err(|error| ServiceError::new("E_INTERNAL", format!("serialize page: {error}")))?
        .len();
    if bytes > MAX_PAGE_BYTES {
        // m3-l2-services §3.2：单页超限整页失败，不返回半页。
        return Err(ServiceError::new(
            "E_TOO_LARGE",
            format!("events page serialized size {bytes} exceeds {MAX_PAGE_BYTES} bytes"),
        ));
    }
    Ok(reply)
}

/// `events.append`：官方工具 `exec.agent.session.append(type, data)` 的宿主端。
///
/// 只允许 `session-log/v1`；事件类型与数据大小先在本层拒绝，再交给
/// [`SessionAccess::append_event`]（唯一提交者负责 seq 与落盘）。
fn append_event_payload(
    access: &dyn SessionAccess,
    args: &Value,
    ctx: &ServiceCallContext,
) -> Result<Value, ServiceError> {
    let args: EventsAppendArgs = parse_args(args)?;
    let session_id = require_session(ctx)?;
    let event_type = args.event_type.trim().to_owned();
    if event_type.is_empty() || event_type.chars().count() > MAX_EVENT_TYPE_CHARS {
        return Err(ServiceError::args(
            "/args/event_type",
            format!("event_type must be 1..={MAX_EVENT_TYPE_CHARS} characters"),
        ));
    }
    if !event_type
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/'))
    {
        return Err(ServiceError::args(
            "/args/event_type",
            "event_type may only contain ASCII letters, digits, `.`, `_`, `-`, `/`",
        ));
    }
    let bytes = serde_json::to_vec(&args.data)
        .map_err(|error| ServiceError::new("E_INTERNAL", format!("serialize event data: {error}")))?
        .len();
    if bytes > MAX_APPEND_BYTES {
        return Err(ServiceError::new(
            "E_TOO_LARGE",
            format!("event data {bytes} bytes exceeds {MAX_APPEND_BYTES}"),
        ));
    }
    // 未知/已销毁会话 → E_SERVICE_UNAVAILABLE（来自 SessionAccess 的约定）。
    access.header(session_id)?;
    let seq = access.append_event(session_id, &event_type, args.data)?;
    Ok(json!({ "seq": seq }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsReadArgs {
    from_seq: u64,
    max_events: u64,
}

/// `events.append`：官方工具通过 `exec.agent.session.append(type, data)` 写入会话事件。
/// 仅 `session-log/v1` 暴露写方法；`session-projections/v1` 保持只读。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsAppendArgs {
    event_type: String,
    data: Value,
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(args.clone())
        .map_err(|error| ServiceError::args("/args", format!("invalid args: {error}")))
}

fn parse_empty_args(args: &Value) -> Result<(), ServiceError> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(()),
        _ => Err(ServiceError::args(
            "/args",
            "this method takes an empty args object",
        )),
    }
}

fn require_session(ctx: &ServiceCallContext) -> Result<&str, ServiceError> {
    ctx.session_id.as_deref().ok_or_else(|| {
        ServiceError::args(
            "/session_id",
            "session-level method requires the envelope session_id",
        )
    })
}

/// 显示用路径：去掉 Windows canonicalize 的 `\\?\` 前缀。
pub(crate) fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return rest.to_owned();
        }
    }
    text
}

/// 目录存在 → 返回规范化绝对路径；不存在/不是目录 → `None`。
fn normalize_existing_directory(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    Some(display_path(&canonical))
}

fn same_directory(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn method(
    name: &str,
    kind: HostServiceKind,
    args_schema: Value,
    result_schema: Value,
    timeout_ms: u64,
    requires_session: bool,
) -> HostServiceMethod {
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        args_schema,
        result_schema,
        timeout_ms,
        idempotent: true,
        requires_session,
    }
}

fn object_schema(properties: Value, required: Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn cwd_result_schema() -> Value {
    object_schema(json!({ "cwd": { "type": "string" } }), json!(["cwd"]))
}

fn session_projections_table() -> HostServiceTable {
    HostServiceTable::new(
        SESSION_PROJECTIONS_V1,
        vec![
            method(
                "session.describe",
                HostServiceKind::Read,
                object_schema(json!({}), json!([])),
                object_schema(json!({ "header": { "type": "object" } }), json!(["header"])),
                5_000,
                true,
            ),
            method(
                "events.read",
                HostServiceKind::Read,
                object_schema(
                    json!({
                        "from_seq": { "type": "integer", "minimum": 0 },
                        "max_events": { "type": "integer", "minimum": 1 },
                    }),
                    json!(["from_seq", "max_events"]),
                ),
                object_schema(
                    json!({
                        "events": { "type": "array" },
                        "next_seq": { "type": "integer", "minimum": 0 },
                        "eof": { "type": "boolean" },
                    }),
                    json!(["events", "next_seq", "eof"]),
                ),
                10_000,
                true,
            ),
        ],
    )
}

fn working_directory_table() -> HostServiceTable {
    HostServiceTable::new(
        WORKING_DIRECTORY_V1,
        vec![
            method(
                "get",
                HostServiceKind::Read,
                object_schema(json!({}), json!([])),
                cwd_result_schema(),
                5_000,
                true,
            ),
            method(
                "ensure",
                HostServiceKind::Write,
                object_schema(json!({}), json!([])),
                object_schema(
                    json!({
                        "cwd": { "type": "string" },
                        "recovered": { "type": "boolean" },
                    }),
                    json!(["cwd", "recovered"]),
                ),
                30_000,
                true,
            ),
            method(
                "set",
                HostServiceKind::Write,
                object_schema(json!({ "path": { "type": "string" } }), json!(["path"])),
                cwd_result_schema(),
                30_000,
                true,
            ),
        ],
    )
}

fn session_log_table() -> HostServiceTable {
    HostServiceTable::new(
        SESSION_LOG_V1,
        vec![
            method(
                "events.append",
                HostServiceKind::Write,
                object_schema(
                    json!({
                        "event_type": { "type": "string" },
                        "data": {},
                    }),
                    json!(["event_type", "data"]),
                ),
                object_schema(
                    json!({ "seq": { "type": "integer", "minimum": 1 } }),
                    json!(["seq"]),
                ),
                10_000,
                false,
            ),
            method(
                "session.describe",
                HostServiceKind::Read,
                object_schema(json!({}), json!([])),
                object_schema(json!({ "header": { "type": "object" } }), json!(["header"])),
                5_000,
                true,
            ),
            method(
                "events.read",
                HostServiceKind::Read,
                object_schema(
                    json!({
                        "from_seq": { "type": "integer", "minimum": 0 },
                        "max_events": { "type": "integer", "minimum": 1 },
                    }),
                    json!(["from_seq", "max_events"]),
                ),
                object_schema(
                    json!({
                        "events": { "type": "array" },
                        "next_seq": { "type": "integer", "minimum": 0 },
                        "eof": { "type": "boolean" },
                    }),
                    json!(["events", "next_seq", "eof"]),
                ),
                10_000,
                true,
            ),
        ],
    )
}

/// 供集成测试/chat 接线复用的形状提醒已移除；路由层负责把 `Value` 包装成
/// `ServiceReply`/`host.service.result`。
#[cfg(test)]
mod tests;
