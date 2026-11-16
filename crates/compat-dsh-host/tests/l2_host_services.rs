//! SidecarHost × HostServiceRouter 集成测试（v2 握手/读循环/取消/generation）。
//!
//! 用 `tests/fixtures/l2-sidecar.mjs` 这个真实 Node 子进程驱动协议：它按场景主动发
//! `host.service.request` / `capabilities.list`，把答复摘要放进 `tool.result.value`。
//! 覆盖：v2 握手、v1 回退、storage-kv 往返、working-directory 往返、未知 key、
//! 取消、迟到 result、generation 变化与恢复。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dshr_compat_dsh_host::{
    EventPage, HostOptions, HostServiceRouter, ServiceError, SessionAccess, SessionEventView,
    SessionHeaderView, SessionLogService, SidecarHost, StorageKvService, WorkingDirectoryService,
    SESSION_LOG_V1, STORAGE_KV_V1, WORKING_DIRECTORY_V1,
};
use dshr_compat_dsh_protocol::{CAP_SERVICE_PROXY, PROTOCOL_MAX};
use serde_json::{json, Value};

const FIXTURE_PLUGIN: &str = "l2.fixture";

fn fixture_paths() -> (PathBuf, PathBuf) {
    let dir = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
        .expect("fixtures dir");
    let script = dir.join("l2-sidecar.mjs");
    (script.clone(), script)
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
        "dshr-l2-host-{label}-{}-{unique}",
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

#[derive(Clone)]
struct FakeSession {
    header: SessionHeaderView,
    events: Vec<SessionEventView>,
}

#[derive(Default)]
struct FakeAccess {
    sessions: Mutex<BTreeMap<String, FakeSession>>,
    appends: Mutex<Vec<(String, String, Value)>>,
}

impl FakeAccess {
    fn with_session(session_id: &str, cwd: &str) -> Arc<Self> {
        let access = Arc::new(Self::default());
        access.sessions.lock().expect("sessions").insert(
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
        access
    }

    fn appended_types(&self) -> Vec<String> {
        self.appends
            .lock()
            .expect("appends")
            .iter()
            .map(|(_, event_type, _)| event_type.clone())
            .collect()
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
            data: data.clone(),
        });
        self.appends.lock().expect("appends").push((
            session_id.to_owned(),
            event_type.to_owned(),
            data,
        ));
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

fn make_router(
    access: Arc<FakeAccess>,
    storage_root: &Path,
    workspace: &Path,
) -> Arc<Mutex<HostServiceRouter>> {
    let mut router = HostServiceRouter::new(
        "node-placeholder",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .expect("router");
    router
        .register(Box::new(
            StorageKvService::open(storage_root).expect("storage service"),
        ))
        .expect("register storage-kv");
    router
        .register(Box::new(WorkingDirectoryService::new(
            access.clone(),
            workspace,
        )))
        .expect("register working-directory");
    router
        .register(Box::new(SessionLogService::new(access)))
        .expect("register session-log");
    router.register_plugin_inject(
        FIXTURE_PLUGIN,
        vec![
            STORAGE_KV_V1.to_owned(),
            WORKING_DIRECTORY_V1.to_owned(),
            SESSION_LOG_V1.to_owned(),
        ],
    );
    Arc::new(Mutex::new(router))
}

fn spawn_host(
    scenario: &str,
    router: Option<Arc<Mutex<HostServiceRouter>>>,
    env: &[(&str, String)],
) -> SidecarHost {
    let (script, plugin) = fixture_paths();
    let mut options = HostOptions::default()
        .with_env("L2_SCENARIO", scenario)
        .with_registration_window(Duration::from_secs(3));
    for (key, value) in env {
        options = options.with_env(*key, value.clone());
    }
    if let Some(router) = router {
        options = options.with_host_services(router);
    }
    SidecarHost::spawn_with(script, plugin, options).expect("fixture sidecar starts")
}

fn invoke_probe(host: &mut SidecarHost) -> Value {
    let outcome = host
        .invoke("l2_probe", json!({}), Duration::from_secs(15))
        .expect("l2_probe invoke");
    assert_eq!(outcome.terminal.kind, "tool.result");
    assert_eq!(
        outcome.terminal.payload["is_error"], false,
        "l2_probe failed: {}",
        outcome.terminal.payload
    );
    outcome.terminal.payload["value"].clone()
}

fn step(summary: &Value, index: usize) -> &Value {
    &summary["steps"][index]
}

fn step_error(step: &Value) -> Option<&str> {
    step["error"].as_str()
}

/// RPC-L2-401：v2 握手（caps + protocol 2 + sidecar 回声 service.proxy），
/// `capabilities.list` 返回 3 张白名单表，且 router generation 已同步。
#[test]
fn rpc_l2_401_v2_handshake_and_capabilities_list() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("401-ws");
    let storage = temp_dir("401-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host("capabilities", Some(router.clone()), &[]);

    assert!(host.host_caps().iter().any(|cap| cap == CAP_SERVICE_PROXY));
    assert_eq!(host.negotiated_protocol(), 2);
    assert!(host.host_services_active());
    assert_eq!(
        router.lock().expect("router").generation(),
        host.generation(),
        "握手后必须把 generation 同步给 router"
    );

    let tables = host.host_services_payload().expect("host services payload");
    let tables = tables.as_array().expect("host_services array");
    assert_eq!(tables.len(), 3);
    let keys: Vec<&str> = tables
        .iter()
        .map(|table| table["service_key"].as_str().unwrap_or_default())
        .collect();
    assert!(keys.contains(&STORAGE_KV_V1));
    assert!(keys.contains(&WORKING_DIRECTORY_V1));
    assert!(keys.contains(&SESSION_LOG_V1));

    let summary = invoke_probe(&mut host);
    assert_eq!(summary["protocol"], 2);
    assert_eq!(summary["service_proxy"], true);
    let capabilities_step = step(&summary, 0);
    assert!(
        step_error(capabilities_step).is_none(),
        "{capabilities_step:?}"
    );
    assert_eq!(
        capabilities_step["payload"]["host_services"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-402：不配置 router = M2 行为：协商 1、无 service.proxy、capabilities 回 E_VERSION。
#[test]
fn rpc_l2_402_without_router_stays_v1() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let mut host = spawn_host("capabilities", None, &[]);
    assert_eq!(host.negotiated_protocol(), 1);
    assert!(!host.host_services_active());
    assert!(
        !host.host_caps().iter().any(|cap| cap == CAP_SERVICE_PROXY),
        "没有 router 时绝不能让 sidecar 认为有 service.proxy"
    );
    assert!(host.host_services_payload().is_none());

    let summary = invoke_probe(&mut host);
    let capabilities_step = step(&summary, 0);
    assert_eq!(step_error(capabilities_step), Some("E_VERSION"));
    drop(host);
}

/// RPC-L2-403：宿主配置了 router、但 sidecar 只支持 v1 → 安全回退 v1（不启用服务）。
#[test]
fn rpc_l2_403_sidecar_v1_falls_back() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("403-ws");
    let storage = temp_dir("403-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host(
        "capabilities",
        Some(router),
        &[("L2_PROTOCOL_MAX", "1".to_owned())],
    );
    assert_eq!(host.negotiated_protocol(), 1);
    assert!(!host.host_services_active());
    let summary = invoke_probe(&mut host);
    assert_eq!(step_error(step(&summary, 0)), Some("E_VERSION"));
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-404：`storage-kv/v1` 经真实 sidecar 往返：open → putRecord → loadAll。
#[test]
fn rpc_l2_404_storage_kv_round_trip_through_sidecar() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("404-ws");
    let storage = temp_dir("404-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host("storage", Some(router), &[]);
    assert!(host.host_services_active());

    let summary = invoke_probe(&mut host);
    let open = step(&summary, 0);
    assert!(step_error(open).is_none(), "unit.open failed: {open:?}");
    let unit_id = open["result"]["unit_id"].as_str().expect("unit_id");
    assert!(!unit_id.is_empty());
    assert!(step_error(step(&summary, 1)).is_none(), "putRecord failed");
    let load = step(&summary, 2);
    assert!(step_error(load).is_none(), "loadAll failed: {load:?}");
    assert_eq!(load["result"]["tables"]["items"]["a"]["n"], 1);
    assert!(
        storage.join(FIXTURE_PLUGIN).join("todos.json").exists(),
        "数据必须落在宿主侧 storage 目录"
    );
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-405：`working-directory/v1` 往返：get → set(绝对目录) → ensure；
/// set 会往 Session Log 追加 `working-directory/change`。
#[test]
fn rpc_l2_405_working_directory_round_trip_through_sidecar() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("405-ws");
    let target = temp_dir("405-target");
    let storage = temp_dir("405-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access.clone(), &storage, &workspace);
    let mut host = spawn_host(
        "working-directory",
        Some(router),
        &[("L2_TARGET_DIR", norm(&target))],
    );

    let summary = invoke_probe(&mut host);
    let get = step(&summary, 0);
    assert!(step_error(get).is_none(), "get failed: {get:?}");
    assert_eq!(get["result"]["cwd"], norm(&workspace));

    let set = step(&summary, 1);
    assert!(step_error(set).is_none(), "set failed: {set:?}");
    assert_eq!(set["result"]["cwd"], norm(&target));

    let ensure = step(&summary, 2);
    assert!(step_error(ensure).is_none(), "ensure failed: {ensure:?}");
    assert_eq!(ensure["result"]["cwd"], norm(&target));
    assert_eq!(ensure["result"]["recovered"], false);

    assert_eq!(
        access.appended_types(),
        vec!["working-directory/change".to_owned()],
        "set 必须恰好追加一条变更事件"
    );
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&target);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-406：未知 service_key → `E_SERVICE_UNAVAILABLE`（sidecar 收到结构化 error）。
#[test]
fn rpc_l2_406_unknown_service_key_is_reported_to_sidecar() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("406-ws");
    let storage = temp_dir("406-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host("unknown", Some(router), &[]);
    let summary = invoke_probe(&mut host);
    let missing = step(&summary, 0);
    assert_eq!(step_error(missing), Some("E_SERVICE_UNAVAILABLE"));
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-407：`host.service.cancel` 先到 → 请求以 `E_CANCELLED` 结算且后续调用可用。
#[test]
fn rpc_l2_407_cancel_prevents_dispatch() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("407-ws");
    let storage = temp_dir("407-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access.clone(), &storage, &workspace);
    let mut host = spawn_host("cancel", Some(router), &[]);

    let summary = invoke_probe(&mut host);
    let cancelled = step(&summary, 0);
    assert_eq!(step_error(cancelled), Some("E_CANCELLED"));
    let after = step(&summary, 1);
    assert!(
        step_error(after).is_none(),
        "cancel must not break later calls: {after:?}"
    );
    assert_eq!(after["result"]["cwd"], norm(&workspace));
    assert_eq!(access.appended_types().len(), 0, "取消请求不得写事件");
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-408：迟到的 `host.service.result` 被忽略，不阻塞后续 capabilities 请求。
#[test]
fn rpc_l2_408_late_result_is_ignored() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("408-ws");
    let storage = temp_dir("408-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host("late-result", Some(router), &[]);
    let summary = invoke_probe(&mut host);
    let capabilities_step = step(&summary, 0);
    assert!(
        step_error(capabilities_step).is_none(),
        "{capabilities_step:?}"
    );
    assert_eq!(
        capabilities_step["payload"]["host_services"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-409：generation 变化 → 在途/新请求 `E_SERVICE_UNAVAILABLE`；恢复 generation 后可用。
#[test]
fn rpc_l2_409_generation_change_settles_unavailable_then_recovers() {
    assert!(
        node_available(),
        "node is required for this integration test (no silent SKIP)"
    );
    let workspace = temp_dir("409-ws");
    let storage = temp_dir("409-storage");
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let router = make_router(access, &storage, &workspace);
    let mut host = spawn_host("working-directory", Some(router.clone()), &[]);
    let original_generation = host.generation().to_owned();

    host.set_generation("node-9999").expect("set generation");
    assert_eq!(router.lock().expect("router").generation(), "node-9999");
    let stale = invoke_probe(&mut host);
    assert_eq!(step_error(step(&stale, 0)), Some("E_SERVICE_UNAVAILABLE"));

    // 恢复 sidecar 当前 generation 后，服务调用重新可用。
    host.set_generation(original_generation)
        .expect("restore generation");
    let fresh = invoke_probe(&mut host);
    assert!(
        step_error(step(&fresh, 0)).is_none(),
        "generation restored, call must succeed: {:?}",
        step(&fresh, 0)
    );
    drop(host);
    let _ = std::fs::remove_dir_all(&workspace);
    let _ = std::fs::remove_dir_all(&storage);
}

/// RPC-L2-410：`HostOptions` 默认不协商 service.proxy（M2 兼容的硬约束）。
#[test]
fn rpc_l2_410_default_options_advertise_no_service_proxy() {
    let options = HostOptions::default();
    assert!(options.host_services.is_none());
    let caps = options.effective_caps();
    assert_eq!(
        caps,
        vec!["tool.stream".to_owned(), "tool.cancel".to_owned()]
    );
}
