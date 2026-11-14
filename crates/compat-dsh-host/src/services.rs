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
/// 工作目录变更事件类型（官方 `dsh-working-directory` 同名事件）。
pub const WORKING_DIRECTORY_CHANGED: &str = "working-directory/change";
/// `events.read` 单页事件数上限（m3-l2-services §3.2）。
pub const MAX_EVENTS_PER_PAGE: u64 = 500;
/// `events.read` 单页序列化上限（512 KiB）。
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
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

    fn describe(&mut self, args: &Value, ctx: &ServiceCallContext) -> Result<Value, ServiceError> {
        parse_empty_args(args)?;
        let session_id = require_session(ctx)?;
        let header = self.access.header(session_id)?;
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

    fn read_events(
        &mut self,
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
        self.access.header(session_id)?;
        let page = self
            .access
            .read_events(session_id, args.from_seq, args.max_events as usize)?;
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
            "session.describe" => self.describe(args, ctx),
            "events.read" => self.read_events(args, ctx),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown session-log method `{other}`"),
            )),
        }
    }
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
fn display_path(path: &Path) -> String {
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
mod tests {
    use super::*;
    use crate::service::HostServiceRouter;
    use dshr_compat_dsh_protocol::{
        Envelope, CAP_SERVICE_PROXY, KIND_HOST_SERVICE_REQUEST, PROTOCOL_MAX,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    const PLUGIN: &str = "example.dsh.tool";
    const SESSION: &str = "s-0001";

    struct FakeSession {
        header: SessionHeaderView,
        events: Vec<SessionEventView>,
        next_seq: u64,
    }

    #[derive(Default)]
    struct FakeAccess {
        sessions: Mutex<BTreeMap<String, FakeSession>>,
        append_calls: AtomicUsize,
    }

    impl FakeAccess {
        fn with_session(session_id: &str, cwd: Option<&str>) -> Arc<Self> {
            let access = Arc::new(Self::default());
            access.add_session(session_id, cwd);
            access
        }

        fn add_session(&self, session_id: &str, cwd: Option<&str>) {
            let mut sessions = self.sessions.lock().expect("fake sessions");
            sessions.insert(
                session_id.to_owned(),
                FakeSession {
                    header: SessionHeaderView {
                        id: session_id.to_owned(),
                        cwd: cwd.map(str::to_owned),
                        created_at: "2026-11-14T00:00:00Z".to_owned(),
                        parent_session_id: None,
                        delegation_depth: 0,
                    },
                    events: Vec::new(),
                    next_seq: 1,
                },
            );
        }

        fn push_event(&self, session_id: &str, event_type: &str, data: Value) -> u64 {
            let mut sessions = self.sessions.lock().expect("fake sessions");
            let session = sessions.get_mut(session_id).expect("session");
            let seq = session.next_seq;
            session.next_seq += 1;
            session.events.push(SessionEventView {
                seq,
                event_type: event_type.to_owned(),
                data,
            });
            seq
        }

        fn events(&self, session_id: &str) -> Vec<SessionEventView> {
            self.sessions
                .lock()
                .expect("fake sessions")
                .get(session_id)
                .map(|session| session.events.clone())
                .unwrap_or_default()
        }

        fn append_calls(&self) -> usize {
            self.append_calls.load(Ordering::SeqCst)
        }
    }

    impl SessionAccess for FakeAccess {
        fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError> {
            self.sessions
                .lock()
                .expect("fake sessions")
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
            let sessions = self.sessions.lock().expect("fake sessions");
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
            let next_seq = events.last().map(|event| event.seq + 1).unwrap_or_else(|| {
                session
                    .events
                    .last()
                    .map(|event| event.seq + 1)
                    .unwrap_or(from_seq)
            });
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
            self.append_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.push_event(session_id, event_type, data))
        }

        fn latest_working_directory(&self, session_id: &str) -> Option<String> {
            let sessions = self.sessions.lock().expect("fake sessions");
            let session = sessions.get(session_id)?;
            session
                .events
                .iter()
                .rev()
                .find(|event| event.event_type == WORKING_DIRECTORY_CHANGED)
                .and_then(|event| event.data.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dshr-services-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn ctx(session_id: Option<&str>) -> ServiceCallContext {
        ServiceCallContext {
            plugin_id: PLUGIN.to_owned(),
            generation: "node-0001".to_owned(),
            session_id: session_id.map(str::to_owned),
            depth: 1,
        }
    }

    fn wd_service(access: Arc<FakeAccess>, default_dir: &Path) -> WorkingDirectoryService {
        let access: Arc<dyn SessionAccess> = access;
        WorkingDirectoryService::new(access, default_dir)
    }

    fn dyn_access(access: &Arc<FakeAccess>) -> Arc<dyn SessionAccess> {
        let cloned: Arc<FakeAccess> = Arc::clone(access);
        cloned
    }

    fn wd_request(method: &str, args: Value, request_id: &str) -> Envelope {
        Envelope::host(
            KIND_HOST_SERVICE_REQUEST,
            request_id,
            json!({ "method": method, "args": args }),
        )
        .with_version(PROTOCOL_MAX)
        .with_generation("node-0001")
        .with_plugin(PLUGIN)
        .with_service_key(WORKING_DIRECTORY_V1)
        .with_session(SESSION)
        .with_depth(1)
    }

    fn router_with(service: Box<dyn HostService>, service_key: &str) -> HostServiceRouter {
        let mut router = HostServiceRouter::new(
            "node-0001",
            PROTOCOL_MAX,
            vec![CAP_SERVICE_PROXY.to_owned()],
        )
        .expect("router");
        router.register(service).expect("register service");
        router.register_plugin_inject(PLUGIN, vec![service_key.to_owned()]);
        router
    }

    fn error_code(envelope: &Envelope) -> &str {
        envelope
            .error
            .as_ref()
            .unwrap_or_else(|| panic!("expected error envelope: {envelope:?}"))
            .code
            .as_str()
    }

    /// RPC-L2-121：`get` 链路优先级：latest change 事件 > header.cwd > 默认目录。
    #[test]
    fn rpc_l2_121_working_directory_get_precedence() {
        let default_dir = temp_dir("121-default");
        let access = FakeAccess::with_session(SESSION, None);
        let mut service = wd_service(Arc::clone(&access), &default_dir);

        let default_cwd = service
            .dispatch("get", &json!({}), &ctx(Some(SESSION)))
            .expect("get default");
        assert_eq!(
            default_cwd["cwd"],
            display_path(&default_dir),
            "未配置 cwd 时回默认目录"
        );

        access.add_session("s-header", Some("C:\\header"));
        let header_cwd = service
            .dispatch("get", &json!({}), &ctx(Some("s-header")))
            .expect("get header");
        assert_eq!(header_cwd["cwd"], "C:\\header");

        access.push_event(
            SESSION,
            WORKING_DIRECTORY_CHANGED,
            json!({ "cwd": "C:\\latest" }),
        );
        let latest_cwd = service
            .dispatch("get", &json!({}), &ctx(Some(SESSION)))
            .expect("get latest");
        assert_eq!(latest_cwd["cwd"], "C:\\latest");
        let _ = std::fs::remove_dir_all(&default_dir);
    }

    /// RPC-L2-122：`set` 存在目录 → append 恰好一条事件；重复 set 幂等不新增。
    #[test]
    fn rpc_l2_122_working_directory_set_appends_once_and_is_idempotent() {
        let default_dir = temp_dir("122-default");
        let target = temp_dir("122-target");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&default_dir)));
        let mut service = wd_service(Arc::clone(&access), &default_dir);

        let reply = service
            .dispatch(
                "set",
                &json!({ "path": target.to_string_lossy() }),
                &ctx(Some(SESSION)),
            )
            .expect("set");
        let normalized = display_path(&std::fs::canonicalize(&target).unwrap());
        assert_eq!(reply["cwd"], normalized);
        assert_eq!(access.append_calls(), 1);
        assert_eq!(access.events(SESSION).len(), 1);
        assert_eq!(
            access.events(SESSION)[0].event_type,
            WORKING_DIRECTORY_CHANGED
        );

        // 幂等：set 到相同目录不再 append。
        service
            .dispatch(
                "set",
                &json!({ "path": target.to_string_lossy() }),
                &ctx(Some(SESSION)),
            )
            .expect("set again");
        assert_eq!(access.append_calls(), 1, "同目录 set 必须幂等");
        let _ = std::fs::remove_dir_all(&default_dir);
        let _ = std::fs::remove_dir_all(&target);
    }

    /// RPC-L2-123：`set` 不存在/不是目录/空串 → `E_SERVICE_ARGS` 且无事件写入。
    #[test]
    fn rpc_l2_123_working_directory_set_rejects_bad_paths() {
        let default_dir = temp_dir("123-default");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&default_dir)));
        let mut service = wd_service(Arc::clone(&access), &default_dir);
        let missing = default_dir.join("does-not-exist");

        for path in [
            missing.to_string_lossy().into_owned(),
            String::new(),
            "   ".to_owned(),
        ] {
            let error = service
                .dispatch("set", &json!({ "path": path }), &ctx(Some(SESSION)))
                .expect_err("invalid path");
            assert_eq!(error.code, "E_SERVICE_ARGS", "path `{path}`: {error:?}");
        }
        // 普通文件也不是目录。
        let file = default_dir.join("file.txt");
        std::fs::write(&file, "x").expect("write file");
        let error = service
            .dispatch(
                "set",
                &json!({ "path": file.to_string_lossy() }),
                &ctx(Some(SESSION)),
            )
            .expect_err("file is not a directory");
        assert_eq!(error.code, "E_SERVICE_ARGS");
        assert_eq!(
            error.detail.as_ref().expect("detail")["reason"],
            "not-a-directory"
        );
        assert_eq!(access.append_calls(), 0, "非法路径不得写事件");
        let _ = std::fs::remove_dir_all(&default_dir);
    }

    /// RPC-L2-124：相对路径以当前目录（header.cwd）为基准解析。
    #[test]
    fn rpc_l2_124_working_directory_set_resolves_relative_paths() {
        let root = temp_dir("124-root");
        let child = root.join("src");
        std::fs::create_dir_all(&child).expect("child dir");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&root)));
        let mut service = wd_service(Arc::clone(&access), &root);

        let reply = service
            .dispatch("set", &json!({ "path": "src" }), &ctx(Some(SESSION)))
            .expect("set relative");
        let expected = display_path(&std::fs::canonicalize(&child).unwrap());
        assert_eq!(reply["cwd"], expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-125：缺信封 `session_id` → `E_SERVICE_ARGS`；未知会话 → `E_SERVICE_UNAVAILABLE`。
    #[test]
    fn rpc_l2_125_working_directory_session_gates() {
        let default_dir = temp_dir("125-default");
        let access = FakeAccess::with_session(SESSION, None);
        let mut service = wd_service(Arc::clone(&access), &default_dir);

        let error = service
            .dispatch("get", &json!({}), &ctx(None))
            .expect_err("missing session");
        assert_eq!(error.code, "E_SERVICE_ARGS");
        assert_eq!(
            error.detail.as_ref().expect("detail")["path"],
            "/session_id"
        );

        let error = service
            .dispatch("get", &json!({}), &ctx(Some("s-unknown")))
            .expect_err("unknown session");
        assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
        let _ = std::fs::remove_dir_all(&default_dir);
    }

    /// RPC-L2-126：`ensure` 原目录消失 → recovered=true、回退 header.cwd 并 append 事件。
    #[test]
    fn rpc_l2_126_working_directory_ensure_recovers() {
        let fallback = temp_dir("126-fallback");
        let missing = fallback.join("gone");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&fallback)));
        access.push_event(
            SESSION,
            WORKING_DIRECTORY_CHANGED,
            json!({ "cwd": missing.to_string_lossy() }),
        );
        let mut service = wd_service(Arc::clone(&access), &fallback);

        let reply = service
            .dispatch("ensure", &json!({}), &ctx(Some(SESSION)))
            .expect("ensure");
        let expected = display_path(&std::fs::canonicalize(&fallback).unwrap());
        assert_eq!(reply["cwd"], expected);
        assert_eq!(reply["recovered"], true);
        assert_eq!(access.append_calls(), 1, "恢复必须 append 新事件");
        let _ = std::fs::remove_dir_all(&fallback);
    }

    /// RPC-L2-127：`ensure` 目录存在 → recovered=false 且不写事件。
    #[test]
    fn rpc_l2_127_working_directory_ensure_keeps_existing() {
        let root = temp_dir("127-root");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&root)));
        let mut service = wd_service(Arc::clone(&access), &root);

        let reply = service
            .dispatch("ensure", &json!({}), &ctx(Some(SESSION)))
            .expect("ensure");
        assert_eq!(
            reply["cwd"],
            display_path(&std::fs::canonicalize(&root).unwrap())
        );
        assert_eq!(reply["recovered"], false);
        assert_eq!(access.append_calls(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// RPC-L2-128：Router 集成：白名单/会话/取消；取消先到不产生事件副作用。
    #[test]
    fn rpc_l2_128_working_directory_router_integration() {
        let root = temp_dir("128-root");
        let access = FakeAccess::with_session(SESSION, Some(&display_path(&root)));
        let mut router = router_with(
            Box::new(wd_service(Arc::clone(&access), &root)),
            WORKING_DIRECTORY_V1,
        );

        let reply = router.handle_request(&wd_request("get", json!({}), "req-128a"));
        assert!(reply.error.is_none(), "get failed: {reply:?}");
        assert_eq!(reply.version, PROTOCOL_MAX);
        assert_eq!(reply.service_key.as_deref(), Some(WORKING_DIRECTORY_V1));

        // 会话级方法缺 session_id → E_SERVICE_ARGS（Router 的 requires_session 门）。
        let mut no_session = wd_request("get", json!({}), "req-128b");
        no_session.session_id = None;
        assert_eq!(
            error_code(&router.handle_request(&no_session)),
            "E_SERVICE_ARGS"
        );

        // 取消先到：不 dispatch、不写事件、回报 E_CANCELLED。
        router.cancel("req-128c");
        let cancelled = router.handle_request(&wd_request("ensure", json!({}), "req-128c"));
        assert_eq!(error_code(&cancelled), "E_CANCELLED");
        assert_eq!(access.append_calls(), 0, "取消不得产生事件副作用");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- session-log/v1 ----

    fn sl_ctx() -> ServiceCallContext {
        ctx(Some(SESSION))
    }

    fn sl_request(method: &str, args: Value, request_id: &str) -> Envelope {
        Envelope::host(
            KIND_HOST_SERVICE_REQUEST,
            request_id,
            json!({ "method": method, "args": args }),
        )
        .with_version(PROTOCOL_MAX)
        .with_generation("node-0001")
        .with_plugin(PLUGIN)
        .with_service_key(SESSION_LOG_V1)
        .with_session(SESSION)
        .with_depth(1)
    }

    fn seeded_access(event_count: usize) -> Arc<FakeAccess> {
        let access = FakeAccess::with_session(SESSION, Some("C:\\ws"));
        access.push_event(SESSION, "session.created", json!({ "workspace_id": "ws" }));
        for index in 0..event_count {
            access.push_event(SESSION, "message.user", json!({ "n": index }));
        }
        access
    }

    /// RPC-L2-221：`session.describe` 返回 header 投影（含 version/parent/depth）。
    #[test]
    fn rpc_l2_221_session_describe_projects_header() {
        let access = FakeAccess::with_session(SESSION, Some("C:\\ws"));
        let mut service = SessionLogService::new(dyn_access(&access));
        let reply = service
            .dispatch("session.describe", &json!({}), &sl_ctx())
            .expect("describe");
        assert_eq!(reply["header"]["id"], SESSION);
        assert_eq!(reply["header"]["cwd"], "C:\\ws");
        assert_eq!(reply["header"]["created_at"], "2026-11-14T00:00:00Z");
        assert_eq!(reply["header"]["version"], SESSION_HEADER_VERSION);
        assert_eq!(reply["header"]["delegation_depth"], 0);
        assert!(reply["header"]["parent"].is_null());
    }

    /// RPC-L2-222：`events.read` 分页：seq 递增、next_seq/eof 正确。
    #[test]
    fn rpc_l2_222_events_read_paginates() {
        let access = seeded_access(5);
        let mut service = SessionLogService::new(dyn_access(&access));

        let first = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 1, "max_events": 2 }),
                &sl_ctx(),
            )
            .expect("page 1");
        let seqs: Vec<u64> = first["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(first["next_seq"], 3);
        assert_eq!(first["eof"], false);
        assert_eq!(first["events"][0]["type"], "session.created");
        assert_eq!(first["events"][0]["data"]["workspace_id"], "ws");

        let last = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 5, "max_events": 10 }),
                &sl_ctx(),
            )
            .expect("page 2");
        assert_eq!(last["next_seq"], 7);
        assert_eq!(last["eof"], true);
    }

    /// RPC-L2-223：`max_events > 500` → `E_TOO_LARGE`；`0` → `E_SERVICE_ARGS`。
    #[test]
    fn rpc_l2_223_events_read_page_limits() {
        let access = seeded_access(1);
        let mut service = SessionLogService::new(dyn_access(&access));

        let error = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 1, "max_events": 501 }),
                &sl_ctx(),
            )
            .expect_err("max_events too large");
        assert_eq!(error.code, "E_TOO_LARGE");

        let error = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 1, "max_events": 0 }),
                &sl_ctx(),
            )
            .expect_err("zero max_events");
        assert_eq!(error.code, "E_SERVICE_ARGS");
    }

    /// RPC-L2-224：单页序列化 > 512 KiB → `E_TOO_LARGE`（不返回半页）。
    #[test]
    fn rpc_l2_224_events_read_page_byte_limit() {
        let access = FakeAccess::with_session(SESSION, None);
        access.push_event(
            SESSION,
            "message.user",
            json!({ "text": "x".repeat(600_000) }),
        );
        let mut service = SessionLogService::new(dyn_access(&access));
        let error = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 1, "max_events": 10 }),
                &sl_ctx(),
            )
            .expect_err("page too large");
        assert_eq!(error.code, "E_TOO_LARGE");
    }

    /// RPC-L2-225：未知会话 → `E_SERVICE_UNAVAILABLE`。
    #[test]
    fn rpc_l2_225_session_log_unknown_session() {
        let access = FakeAccess::with_session(SESSION, None);
        let mut service = SessionLogService::new(dyn_access(&access));
        let mut context = sl_ctx();
        context.session_id = Some("s-unknown".to_owned());
        let error = service
            .dispatch("session.describe", &json!({}), &context)
            .expect_err("unknown session");
        assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
    }

    /// RPC-L2-226：Router 集成：events.read 成功 + args schema 拒绝非法 from_seq。
    #[test]
    fn rpc_l2_226_session_log_router_integration() {
        let access = seeded_access(2);
        let mut router = router_with(
            Box::new(SessionLogService::new(dyn_access(&access))),
            SESSION_LOG_V1,
        );

        let reply = router.handle_request(&sl_request(
            "events.read",
            json!({ "from_seq": 1, "max_events": 10 }),
            "req-226a",
        ));
        assert!(reply.error.is_none(), "events.read failed: {reply:?}");
        assert_eq!(
            reply.payload["result"]["events"].as_array().unwrap().len(),
            3
        );
        assert_eq!(reply.payload["result"]["eof"], true);

        let invalid = router.handle_request(&sl_request(
            "events.read",
            json!({ "from_seq": -1, "max_events": 10 }),
            "req-226b",
        ));
        assert_eq!(error_code(&invalid), "E_SERVICE_ARGS");
    }

    /// RPC-L2-227：直接 dispatch 也会校验会话门（args 里不重复 session_id）。
    #[test]
    fn rpc_l2_227_session_log_session_gate() {
        let access = seeded_access(0);
        let mut service = SessionLogService::new(dyn_access(&access));
        let error = service
            .dispatch("session.describe", &json!({}), &ctx(None))
            .expect_err("missing session");
        assert_eq!(error.code, "E_SERVICE_ARGS");
        assert_eq!(
            error.detail.as_ref().expect("detail")["path"],
            "/session_id"
        );

        let error = service
            .dispatch(
                "events.read",
                &json!({ "from_seq": 1, "max_events": 10, "extra": true }),
                &sl_ctx(),
            )
            .expect_err("unknown args field");
        assert_eq!(error.code, "E_SERVICE_ARGS");
    }
}
