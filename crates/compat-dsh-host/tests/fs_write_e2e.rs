//! 官方写工具离线 E2E：真实 sidecar + 未修改的 `@deepseek-ai/dsh-tool-fs`，
//! 经 Rust `FsWriteService` 的审批 + 沙箱裁决落盘。
//!
//! 前置：`node tools/dsh-vendor.mjs` 已把官方依赖抽到 `sidecar/dsh-official/node_modules`；
//! 不存在时整个测试 SKIP（CI 无本机 DSH 安装也能跑）。
//!
//! 覆盖：
//! 1. 未协商 `fs.write`：即使 profile 显式打开 write 开关，也**不得**注册 write/edit/apply_patch；
//! 2. 协商 `fs.write`：注册 write/edit/apply_patch，官方 write/edit/apply_patch 经审批落盘；
//! 3. 审批 Deny：工具返回错误、文件不变；
//! 4. 更新走写前快照（`.dshr/snapshots/`）。

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{HostOptions, SidecarHost};
use dshr_tool_runtime::{ApprovalBroker, ApprovalOutcome};
use serde_json::json;

/// 只装载 dsh-tool-fs，避免依赖本机 primary-runtime（离线可跑）。
const PLUGINS_JSON: &str = r#"[
  {
    "name": "@deepseek-ai/dsh-tool-fs",
    "inject": ["tools", "fs", "systemPrompt"],
    "config": {
      "read": true,
      "readImage": false,
      "write": true,
      "edit": true,
      "applyPatch": true
    }
  }
]"#;

fn repo_root() -> PathBuf {
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("workspace root")
}

fn sidecar_paths() -> (PathBuf, PathBuf) {
    let dir = repo_root().join("sidecar/dsh-official");
    let script = dir.join("sidecar-dsh.mjs");
    (script.clone(), script)
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn vendored() -> bool {
    repo_root()
        .join("sidecar/dsh-official/node_modules/@deepseek-ai/dsh-tool-fs")
        .exists()
}

fn skip_if_unavailable(test: &str) -> bool {
    if !node_available() {
        eprintln!("SKIP {test}: node not found in PATH");
        return true;
    }
    if !vendored() {
        eprintln!("SKIP {test}: 官方依赖未准备（先运行 `node tools/dsh-vendor.mjs`）");
        return true;
    }
    false
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dshr-official-write-{name}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp workspace");
    dir
}

fn registrations(host: &SidecarHost) -> Vec<String> {
    let mut names: Vec<String> = host
        .registrations()
        .iter()
        .map(|registration| registration.name.clone())
        .collect();
    names.sort();
    names
}

fn spawn(
    name: &str,
    fs_write: Option<Arc<FsWriteService>>,
) -> (SidecarHost, Arc<ApprovalBroker>, PathBuf) {
    let workspace = temp_root(name);
    let broker = ApprovalBroker::new();
    let options = HostOptions::default()
        .with_env("DSHR_DSH_PLUGINS", PLUGINS_JSON)
        .with_registration_window(Duration::from_secs(3));
    let options = match fs_write {
        Some(service) => options.with_fs_write(service),
        None => options,
    };
    let (script, plugin) = sidecar_paths();
    let host = SidecarHost::spawn_with(script, plugin, options).expect("sidecar starts");
    (host, broker, workspace)
}

/// 在后台等审批 pending 出现并决定；调用结束后 `finish()` 立刻收尾。
struct Resolver {
    done: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Resolver {
    fn spawn(broker: Arc<ApprovalBroker>, decision: ApprovalOutcome) -> Self {
        let (done, done_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if done_rx.try_recv().is_ok() {
                    return;
                }
                let pending = broker.pending();
                if !pending.is_empty() {
                    for item in pending {
                        broker.resolve(&item.call_id, decision);
                    }
                    return;
                }
                if Instant::now() >= deadline {
                    eprintln!("[w3-e2e] 审批 pending 未出现（可能工具在审批前失败）");
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        Self {
            done,
            handle: Some(handle),
        }
    }

    fn finish(mut self) {
        let _ = self.done.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn read_only_when_fs_write_not_negotiated() {
    if skip_if_unavailable("read_only_when_fs_write_not_negotiated") {
        return;
    }
    let (host, _broker, workspace) = spawn("read-only", None);
    assert!(
        !host.host_caps().iter().any(|cap| cap == "fs.write"),
        "未提供 FsWriteService 时不得协商 fs.write"
    );
    assert!(!host.fs_write_enabled());
    let names = registrations(&host);
    println!("[w3-e2e] read-only registrations={names:?}");
    assert_eq!(names, vec!["read"], "只读模式只能注册 read");
    assert!(
        !names
            .iter()
            .any(|name| matches!(name.as_str(), "write" | "edit" | "apply_patch")),
        "host_caps 不含 fs.write 时绝不能装载写工具"
    );
    drop(host);
    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn negotiated_write_capability_edits_and_patches_via_approval() {
    if skip_if_unavailable("negotiated_write_capability_edits_and_patches_via_approval") {
        return;
    }
    let workspace = temp_root("write");
    let broker = ApprovalBroker::new();
    let service =
        Arc::new(FsWriteService::open(&workspace, broker.clone()).expect("fs write service opens"));
    let (mut host, _broker, workspace2) = {
        let options = HostOptions::default()
            .with_env("DSHR_DSH_PLUGINS", PLUGINS_JSON)
            .with_registration_window(Duration::from_secs(3))
            .with_fs_write(service.clone());
        let (script, plugin) = sidecar_paths();
        (
            SidecarHost::spawn_with(script, plugin, options).expect("sidecar starts"),
            broker.clone(),
            workspace.clone(),
        )
    };

    assert!(host.fs_write_enabled());
    assert!(host.host_caps().iter().any(|cap| cap == "fs.write"));
    let names = registrations(&host);
    println!("[w3-e2e] writable registrations={names:?}");
    for expected in ["read", "write", "edit", "apply_patch"] {
        assert!(names.iter().any(|name| name == expected), "缺少 {expected}");
    }

    // 1) 官方 write：创建文件。
    let resolver = Resolver::spawn(broker.clone(), ApprovalOutcome::AllowOnce);
    let outcome = host
        .invoke_as(
            "call-w1",
            "write",
            json!({
                "file_path": "hello.txt",
                "content": "hello from official write\n"
            }),
            Duration::from_secs(20),
        )
        .expect("write invoke");
    resolver.finish();
    assert_eq!(outcome.terminal.kind, "tool.result");
    assert_eq!(
        outcome.terminal.payload["is_error"], false,
        "write failed: {}",
        outcome.terminal.payload
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("hello.txt")).unwrap(),
        "hello from official write\n"
    );
    assert!(
        outcome.terminal.payload["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Created file"),
        "官方 write 的 render 应显示 Created file: {}",
        outcome.terminal.payload["content"]
    );

    // 2) 官方 edit：替换并触发写前快照。
    let resolver = Resolver::spawn(broker.clone(), ApprovalOutcome::AllowOnce);
    let outcome = host
        .invoke_as(
            "call-e1",
            "edit",
            json!({
                "file_path": "hello.txt",
                "old_string": "official",
                "new_string": "rust"
            }),
            Duration::from_secs(20),
        )
        .expect("edit invoke");
    resolver.finish();
    assert_eq!(
        outcome.terminal.payload["is_error"], false,
        "edit failed: {}",
        outcome.terminal.payload
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("hello.txt")).unwrap(),
        "hello from rust write\n"
    );
    let snapshots: Vec<_> = std::fs::read_dir(workspace.join(".dshr/snapshots"))
        .expect("snapshots dir")
        .collect();
    assert!(!snapshots.is_empty(), "edit 覆盖前必须留写前快照");

    // 3) 官方 apply_patch：整文件替换一行。
    let patch = "*** Begin Patch\n*** Update File: hello.txt\n@@\n-hello from rust write\n+hello patched\n*** End Patch\n";
    let resolver = Resolver::spawn(broker.clone(), ApprovalOutcome::AllowOnce);
    let outcome = host
        .invoke_as(
            "call-p1",
            "apply_patch",
            json!({ "input": patch }),
            Duration::from_secs(20),
        )
        .expect("apply_patch invoke");
    resolver.finish();
    assert_eq!(
        outcome.terminal.payload["is_error"], false,
        "apply_patch failed: {}",
        outcome.terminal.payload
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("hello.txt")).unwrap(),
        "hello patched\n"
    );

    // 4) 审批 Deny：工具报错、文件不变。
    let resolver = Resolver::spawn(broker.clone(), ApprovalOutcome::Deny);
    let outcome = host
        .invoke_as(
            "call-w2",
            "write",
            json!({ "file_path": "denied.txt", "content": "must not land" }),
            Duration::from_secs(20),
        )
        .expect("denied write invoke");
    resolver.finish();
    assert_eq!(outcome.terminal.payload["is_error"], true);
    assert!(!workspace.join("denied.txt").exists(), "拒绝后不得落盘");
    let text = outcome.terminal.payload["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("denied") || text.contains("approval"),
        "拒绝信息应结构化可见: {text}"
    );

    // 5) 路径穿越：官方 write 也不能写到工作区外，且不应触发审批。
    let escape_name = format!("escape-{}.txt", std::process::id());
    let outside = workspace
        .parent()
        .expect("workspace parent")
        .join(&escape_name);
    let _ = std::fs::remove_file(&outside);
    let outcome = host
        .invoke_as(
            "call-w3",
            "write",
            json!({ "file_path": format!("../{escape_name}"), "content": "escape" }),
            Duration::from_secs(20),
        )
        .expect("escape write invoke");
    assert_eq!(outcome.terminal.payload["is_error"], true);
    assert!(!outside.exists(), "越界路径绝不能落盘");
    let text = outcome.terminal.payload["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("workspace") || text.contains("escape"),
        "越界错误应说明路径边界: {text}"
    );

    drop(host);
    std::fs::remove_dir_all(&workspace2).ok();
}
