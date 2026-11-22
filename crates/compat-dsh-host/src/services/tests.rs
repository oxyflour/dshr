#![cfg(test)]

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
        call_id: None,
        tool_request_id: None,
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

/// RPC-L2-228（M4 W6）：`events.append` 写一条事件并返回 seq；projections 仍只读。
#[test]
fn rpc_l2_228_events_append_writes_and_projections_stay_read_only() {
    let access = FakeAccess::with_session(SESSION, Some("C:\\ws"));
    let mut service = SessionLogService::new(dyn_access(&access));
    let reply = service
        .dispatch(
            "events.append",
            &json!({ "event_type": "todo/write", "data": { "todos": [] } }),
            &sl_ctx(),
        )
        .expect("append");
    assert_eq!(reply["seq"], 1);
    let events = access.events(SESSION);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "todo/write");

    let invalid = service
        .dispatch(
            "events.append",
            &json!({ "event_type": "bad type!", "data": {} }),
            &sl_ctx(),
        )
        .unwrap_err();
    assert_eq!(invalid.code, "E_SERVICE_ARGS");

    let mut projections = SessionProjectionsService::new(dyn_access(&access));
    let read_only = projections
        .dispatch(
            "events.append",
            &json!({ "event_type": "todo/write", "data": {} }),
            &sl_ctx(),
        )
        .unwrap_err();
    assert_eq!(read_only.code, "E_SERVICE_METHOD");
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

// ---- session-projections/v1（Sidecar 投影注册表数据面）：RPC-L2-202..208 ----

fn projections_service(access: &Arc<FakeAccess>) -> SessionProjectionsService {
    SessionProjectionsService::new(dyn_access(access))
}

/// RPC-L2-202：`session.describe` 给投影 `init(header)` 的 header 投影。
#[test]
fn rpc_l2_202_session_projections_describe_header() {
    let access = seeded_access(1);
    let mut service = projections_service(&access);
    assert_eq!(service.table().service_key, SESSION_PROJECTIONS_V1);
    assert!(service.table().validate().is_ok());
    let reply = service
        .dispatch("session.describe", &json!({}), &sl_ctx())
        .expect("describe");
    assert_eq!(reply["header"]["id"], SESSION);
    assert_eq!(reply["header"]["cwd"], "C:\\ws");
    assert_eq!(reply["header"]["version"], SESSION_HEADER_VERSION);
    assert_eq!(reply["header"]["delegation_depth"], 0);
}

/// RPC-L2-203：`events.read` 第一页：seq 递增、next_seq=3、eof=false。
#[test]
fn rpc_l2_203_session_projections_events_first_page() {
    let access = seeded_access(5);
    let mut service = projections_service(&access);
    let page = service
        .dispatch(
            "events.read",
            &json!({ "from_seq": 1, "max_events": 2 }),
            &sl_ctx(),
        )
        .expect("page 1");
    let seqs: Vec<u64> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(page["next_seq"], 3);
    assert_eq!(page["eof"], false);
}

/// RPC-L2-204：续页从 `next_seq` 继续、最后一页 eof=true。
#[test]
fn rpc_l2_204_session_projections_events_next_page() {
    let access = seeded_access(2);
    let mut service = projections_service(&access);
    let page = service
        .dispatch(
            "events.read",
            &json!({ "from_seq": 2, "max_events": 10 }),
            &sl_ctx(),
        )
        .expect("page 2");
    assert_eq!(page["events"].as_array().unwrap().len(), 2);
    assert_eq!(page["next_seq"], 4);
    assert_eq!(page["eof"], true);
}

/// RPC-L2-205：未知会话 → `E_SERVICE_UNAVAILABLE`。
#[test]
fn rpc_l2_205_session_projections_unknown_session() {
    let access = FakeAccess::with_session(SESSION, None);
    let mut service = projections_service(&access);
    let mut context = sl_ctx();
    context.session_id = Some("s-unknown".to_owned());
    let error = service
        .dispatch(
            "events.read",
            &json!({ "from_seq": 1, "max_events": 10 }),
            &context,
        )
        .expect_err("unknown session");
    assert_eq!(error.code, "E_SERVICE_UNAVAILABLE");
}

/// RPC-L2-206：`max_events > 500` → `E_TOO_LARGE`（分页边界）。
#[test]
fn rpc_l2_206_session_projections_page_count_limit() {
    let access = seeded_access(1);
    let mut service = projections_service(&access);
    let error = service
        .dispatch(
            "events.read",
            &json!({ "from_seq": 1, "max_events": 501 }),
            &sl_ctx(),
        )
        .expect_err("page count limit");
    assert_eq!(error.code, "E_TOO_LARGE");
}

/// RPC-L2-207：单页 > 512 KiB → `E_TOO_LARGE`（不返回半页）。
#[test]
fn rpc_l2_207_session_projections_page_byte_limit() {
    let access = FakeAccess::with_session(SESSION, None);
    access.push_event(
        SESSION,
        "message.user",
        json!({ "text": "x".repeat(600_000) }),
    );
    let mut service = projections_service(&access);
    let error = service
        .dispatch(
            "events.read",
            &json!({ "from_seq": 1, "max_events": 10 }),
            &sl_ctx(),
        )
        .expect_err("page bytes limit");
    assert_eq!(error.code, "E_TOO_LARGE");
}

/// RPC-L2-208：投影方法都是会话级（requires_session）；缺 session_id 拒绝。
#[test]
fn rpc_l2_208_session_projections_requires_session() {
    let access = seeded_access(0);
    let mut service = projections_service(&access);
    assert!(service
        .table()
        .methods
        .iter()
        .all(|method| method.requires_session));
    let error = service
        .dispatch("session.describe", &json!({}), &ctx(None))
        .expect_err("missing session");
    assert_eq!(error.code, "E_SERVICE_ARGS");
    assert_eq!(
        error.detail.as_ref().expect("detail")["path"],
        "/session_id"
    );
}
