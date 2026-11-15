//! G3 官方包验收（RPC-L2-501..）：**未修改官方包**在真实 v2 sidecar 下的装载/往返/错误面。
//!
//! 组合（真实 `sidecar/dsh-official/sidecar-dsh.mjs` + vendored node_modules）：
//! - `@deepseek-ai/dsh-tool-working-directory`（来自 `.tmp/dsh-tools-all`，测试时复制进
//!   sidecar node_modules，源码不改）：装载绿 + `execute` 的 `exec.agent` 债务证据；
//! - `@deepseek-ai/dsh-storage-domain`（未修改）：`defineDomain`/`domainTable`(zod) +
//!   `DomainFacility` → 我们的 `storage-kv` backend（Rust 介质）真实往返；
//! - 测试专用 driver（`sidecar/dsh-official/lib/official-l2-driver.mjs`）：按官方签名调用
//!   `workingDirectory` 与 `storageDomain`，把证据作为 tool.result 返回。
//!
//! 缺 node / vendored node_modules / `.tmp/dsh-tools-all` 时 SKIP（打印原因），不失败。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dshr_compat_dsh_host::{
    EventPage, HostOptions, HostServiceRouter, ServiceError, SessionAccess, SessionEventView,
    SessionHeaderView, SessionLogService, SidecarHost, StorageKvService, WorkingDirectoryService,
    SESSION_LOG_V1, STORAGE_KV_V1, WORKING_DIRECTORY_V1,
};
use dshr_compat_dsh_protocol::{Envelope, CAP_SERVICE_PROXY, PROTOCOL_MAX};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).expect("repo root")
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
        "dshr-official-l2-{label}-{}-{unique}",
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

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 把 `.tmp/dsh-tools-all` 里的官方工具包放进 sidecar 的 node_modules（仅运行期，gitignored）。
/// 返回 `false` = 该验收路径不可用（调用方 SKIP）。
fn ensure_official_tool_package() -> bool {
    let source = repo_root().join(".tmp/dsh-tools-all/dsh-tool-working-directory");
    if !source.is_dir() {
        return false;
    }
    let destination = repo_root()
        .join("sidecar/dsh-official/node_modules/@deepseek-ai/dsh-tool-working-directory");
    if destination.join("lib/index.js").exists() {
        return true;
    }
    match copy_dir_all(&source, &destination) {
        Ok(()) => destination.join("lib/index.js").exists(),
        Err(error) => {
            eprintln!("SKIP official packages: 无法复制官方工具包到 node_modules: {error}");
            false
        }
    }
}

fn file_url(path: &Path) -> String {
    let absolute = std::path::absolute(path).expect("absolute path");
    let text = absolute.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
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
            data,
        });
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

struct Fixture {
    host: SidecarHost,
    workspace: PathBuf,
    target: PathBuf,
    storage: PathBuf,
}

impl Fixture {
    fn cleanup(self) {
        drop(self.host);
        let _ = std::fs::remove_dir_all(&self.workspace);
        let _ = std::fs::remove_dir_all(&self.target);
        let _ = std::fs::remove_dir_all(&self.storage);
    }
}

fn official_plugins(driver_url: &str) -> String {
    json!([
        {
            "name": "@deepseek-ai/dsh-storage-domain",
            "config": { "backend": "storage-kv" },
        },
        {
            "name": "@deepseek-ai/dsh-tool-working-directory",
            "config": {},
        },
        { "name": driver_url },
    ])
    .to_string()
}

fn spawn_official_sidecar(label: &str) -> Option<Fixture> {
    if !node_available() {
        eprintln!("SKIP {label}: node not found");
        return None;
    }
    let sidecar_dir = repo_root().join("sidecar/dsh-official");
    if !sidecar_dir.join("sidecar-dsh.mjs").exists() || !sidecar_dir.join("node_modules").exists() {
        eprintln!("SKIP {label}: 未准备 vendored 官方 sidecar（node tools/dsh-vendor.mjs）");
        return None;
    }
    if !ensure_official_tool_package() {
        eprintln!("SKIP {label}: 缺少 .tmp/dsh-tools-all/dsh-tool-working-directory");
        return None;
    }
    let driver = sidecar_dir.join("lib/official-l2-driver.mjs");
    if !driver.exists() {
        eprintln!("SKIP {label}: 缺少 official-l2-driver.mjs");
        return None;
    }

    let workspace = temp_dir(&format!("{label}-ws"));
    let target = temp_dir(&format!("{label}-target"));
    let storage = temp_dir(&format!("{label}-storage"));
    let access = FakeAccess::with_session("s-probe", &norm(&workspace));
    let mut router = HostServiceRouter::new(
        "node-placeholder",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .expect("router");
    router
        .register(Box::new(
            StorageKvService::open(&storage).expect("storage service"),
        ))
        .expect("register storage-kv");
    router
        .register(Box::new(WorkingDirectoryService::new(
            access.clone(),
            &workspace,
        )))
        .expect("register working-directory");
    router
        .register(Box::new(SessionLogService::new(access)))
        .expect("register session-log");
    router.register_plugin_inject(
        "dsh-official",
        vec![
            STORAGE_KV_V1.to_owned(),
            WORKING_DIRECTORY_V1.to_owned(),
            SESSION_LOG_V1.to_owned(),
        ],
    );
    let options = HostOptions::default()
        .with_env("DSHR_DSH_PLUGINS", official_plugins(&file_url(&driver)))
        .with_registration_window(Duration::from_secs(8))
        .with_host_services(Arc::new(Mutex::new(router)));
    let script = sidecar_dir.join("sidecar-dsh.mjs");
    match SidecarHost::spawn_with(&script, &script, options) {
        Ok(host) => Some(Fixture {
            host,
            workspace,
            target,
            storage,
        }),
        Err(error) => {
            eprintln!("SKIP {label}: sidecar 启动失败: {error}");
            let _ = std::fs::remove_dir_all(&workspace);
            let _ = std::fs::remove_dir_all(&target);
            let _ = std::fs::remove_dir_all(&storage);
            None
        }
    }
}

fn registration_names(host: &SidecarHost) -> Vec<String> {
    let mut names: Vec<String> = host
        .registrations()
        .iter()
        .map(|registration| registration.name.clone())
        .collect();
    names.sort();
    names
}

fn invoke(host: &mut SidecarHost, tool: &str, arguments: Value) -> Envelope {
    let outcome = host
        .invoke(tool, arguments, Duration::from_secs(20))
        .unwrap_or_else(|error| panic!("invoke `{tool}` failed: {error}"));
    assert_eq!(
        outcome.terminal.kind, "tool.result",
        "{tool} did not settle: {:?}",
        outcome.terminal
    );
    outcome.terminal
}

/// RPC-L2-501：官方工具包装载绿（v2）——`working_directory` 与 driver 都收到 tool.register，
/// 无 plugin load 失败；握手协商到 v2。
#[test]
fn rpc_l2_501_official_working_directory_tool_loads_green() {
    let Some(fixture) = spawn_official_sidecar("501") else {
        return;
    };
    assert_eq!(fixture.host.negotiated_protocol(), 2);
    assert!(fixture.host.host_services_active());
    let names = registration_names(&fixture.host);
    assert!(
        names.iter().any(|name| name == "working_directory"),
        "官方 dsh-tool-working-directory 未注册工具: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "l2_official_roundtrip"),
        "L2 driver 未注册: {names:?}"
    );
    let load_errors: Vec<&String> = fixture
        .host
        .boot_errors()
        .iter()
        .filter(|message| message.contains("failed to load"))
        .collect();
    assert!(load_errors.is_empty(), "官方包装载失败: {load_errors:?}");
    fixture.cleanup();
}

/// RPC-L2-502：官方工具的 `execute` 依赖 `exec.agent`（M3 明确不计入闭环）——
/// 未提供 agent 时返回结构化错误，不是挂起/崩溃。
#[test]
fn rpc_l2_502_official_tool_execute_is_blocked_by_exec_agent() {
    let Some(mut fixture) = spawn_official_sidecar("502") else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "working_directory",
        json!({ "cd": fixture.target.to_string_lossy() }),
    );
    assert_eq!(
        envelope.error.as_ref().map(|error| error.code.as_str()),
        Some("E_TOOL"),
        "结构化错误码应可判定: {envelope:?}"
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], true, "官方工具不应成功: {payload}");
    let text = payload["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("requires an agent Session"),
        "应命中 exec.agent 债务原文: {text}"
    );
    assert_eq!(
        fixture.host.negotiated_protocol(),
        2,
        "错误路径不能破坏 v2 会话"
    );
    fixture.cleanup();
}

/// RPC-L2-503：官方 `dsh-working-directory` 签名的服务调用（get/ensure/set）经 shim →
/// host.service.request 真实往返（driver 触发，不依赖 execute/agent）。
#[test]
fn rpc_l2_503_working_directory_official_signature_round_trip() {
    let Some(mut fixture) = spawn_official_sidecar("503") else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "l2_official_roundtrip",
        json!({ "target": fixture.target.to_string_lossy() }),
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "driver 失败: {payload}");
    let evidence = &payload["value"]["working_directory"];
    assert_eq!(evidence["get"], norm(&fixture.workspace));
    assert_eq!(evidence["set"], norm(&fixture.target));
    assert_eq!(evidence["get_after_set"], norm(&fixture.target));
    assert_eq!(evidence["ensure"], norm(&fixture.target));
    assert_eq!(evidence["recovered"], false);
    fixture.cleanup();
}

/// RPC-L2-504：未修改官方 `dsh-storage-domain`（defineDomain/zod + DomainFacility）经
/// storage-kv backend → Rust `StorageKvService` 的真实 kv facet 往返 + `already-open` 错误面。
#[test]
fn rpc_l2_504_official_storage_domain_round_trip() {
    let Some(mut fixture) = spawn_official_sidecar("504") else {
        return;
    };
    let envelope = invoke(
        &mut fixture.host,
        "l2_official_roundtrip",
        json!({ "target": fixture.target.to_string_lossy() }),
    );
    let payload = &envelope.payload;
    assert_eq!(payload["is_error"], false, "driver 失败: {payload}");
    let storage = &payload["value"]["storage"];
    assert_eq!(
        storage["initial_global"],
        json!({ "theme": "dark" }),
        "未写 global 时必须回落到 schema initial: {storage}"
    );
    assert_eq!(storage["get"], json!({ "n": 1 }), "put/get 往返: {storage}");
    assert_eq!(storage["delete"], true);
    assert_eq!(storage["size_after_delete"], 1);
    assert_eq!(storage["global"], json!({ "theme": "light" }));
    assert_eq!(
        storage["duplicate_open_code"], "already-open",
        "官方 domain 错误面: {storage}"
    );
    assert_eq!(storage["closed"], true);

    // 数据落在宿主侧 Rust 介质（plugin_id=dsh-official），而不是 Sidecar 内存。
    let medium = fixture
        .storage
        .join("dsh-official")
        .join("official_acceptance.json");
    assert!(
        medium.exists(),
        "宿主侧 storage 介质缺失: {}",
        medium.display()
    );
    let file: Value =
        serde_json::from_slice(&std::fs::read(&medium).expect("read medium")).expect("medium json");
    assert_eq!(file["tables"]["items"]["b"]["n"], 2);
    assert_eq!(file["global"], json!({ "theme": "light" }));
    fixture.cleanup();
}

/// RPC-L2-505：`@deepseek-ai/dsh-storage` 是 **Cordis Service class**（没有插件 apply/inject），
/// 不能作为普通 Cordis plugin 装载；官方 kv facet 契约由它的消费者（storage-domain）验证。
/// 这条用例固化"官方 hub + Rust backend adapter"的边界证据。
#[test]
fn rpc_l2_505_official_storage_is_a_service_class_not_a_plugin() {
    if !node_available() {
        eprintln!("SKIP rpc_l2_505: node not found");
        return;
    }
    let sidecar_dir = repo_root().join("sidecar/dsh-official");
    if !sidecar_dir
        .join("node_modules/@deepseek-ai/dsh-storage")
        .exists()
    {
        eprintln!("SKIP rpc_l2_505: dsh-storage 未 vendored");
        return;
    }
    let script = "import('@deepseek-ai/dsh-storage').then((m) => { console.log(JSON.stringify({ hasApply: typeof m.apply === 'function', hasInject: Array.isArray(m.inject), hasDefaultClass: typeof m.default === 'function' })) })";
    let output = std::process::Command::new("node")
        .args(["-e", script])
        .current_dir(&sidecar_dir)
        .output()
        .expect("run node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let shape: Value = serde_json::from_str(text.trim()).expect("node JSON output");
    assert_eq!(shape["hasApply"], false);
    assert_eq!(shape["hasInject"], false);
    assert_eq!(shape["hasDefaultClass"], true);
}
