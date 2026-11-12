//! L1 端到端：真实 Node 进程 + NDJSON 协议。
//!
//! 这些测试就是"实验 C 的单位成本表"的来源：它们不仅断言行为，
//! 还打印每次 invoke 的耗时，用来回答"一个 adapter 到底要几天"。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::sleep;
use std::time::Duration;

use dshr_compat_dsh_host::{HostError, HostOptions, SidecarHost};
use serde_json::json;

fn sidecar_paths() -> (PathBuf, PathBuf) {
    // 不要用 canonicalize：Windows 上它会返回 \\?\C:\... 前缀，Node 会把它解析成
    // "C:" 这样的一级路径并报 EISDIR。absolute 只做规范化，不加前缀。
    let root = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("workspace root");
    (
        root.join("sidecar/probe-sidecar.mjs"),
        root.join("sidecar/probe-plugin.mjs"),
    )
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn handshake_and_registration() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    println!(
        "[l1] ready={:?} generation={}",
        host.ready_latency(),
        host.generation()
    );
    for reg in host.registrations() {
        println!("[l1] registered {} -> {}", reg.name, reg.registration_id);
    }

    assert!(host.ready_latency() < Duration::from_secs(10));
    assert_eq!(
        host.registrations().len(),
        3,
        "probe plugin registers three tools"
    );
    assert_eq!(host.registrations()[0].name, "probe_echo");
    assert_eq!(host.registrations()[1].name, "probe_immediate");
    assert_eq!(host.registrations()[2].name, "probe_slow");
    assert!(host.generation().starts_with("node-"));

    // 对齐 DSH 的 output.schema / presentResult 卡片类型：必须能穿过注册链路。
    let definitions = host.tool_definitions();
    assert!(
        definitions[0].output_schema.is_some(),
        "output schema 必须保留"
    );
    assert_eq!(definitions[0].presentation.as_deref(), Some("generic"));
}

#[test]
fn invoke_streams_chunks_and_settles() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    let mut latencies = Vec::new();
    const RUNS: usize = 50;
    for i in 0..RUNS {
        let outcome = host
            .invoke("probe_echo", json!({ "i": i }), Duration::from_secs(5))
            .expect("invoke succeeds");
        latencies.push(outcome.elapsed);
        assert_eq!(outcome.chunks.len(), 3, "three streamed chunks");
        assert_eq!(outcome.terminal.kind, "tool.result");
        assert_eq!(outcome.terminal.payload["is_error"], false);
        assert_eq!(
            outcome.terminal.payload["content"][0]["value"]["echo"]["i"],
            i
        );
    }

    latencies.sort();
    let p50 = latencies[RUNS / 2];
    let p95 = latencies[(RUNS * 95) / 100];
    println!(
        "[l1] invoke x{} p50={:?} p95={:?} max={:?}",
        RUNS,
        p50,
        p95,
        latencies.last().unwrap()
    );
    assert!(
        p95 < Duration::from_millis(200),
        "L1 round trip stays sub-200ms"
    );
}

#[test]
fn invoke_immediate_measures_protocol_only_cost() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    let mut latencies = Vec::new();
    const RUNS: usize = 50;
    for _ in 0..RUNS {
        let outcome = host
            .invoke("probe_immediate", json!({}), Duration::from_secs(5))
            .expect("invoke succeeds");
        latencies.push(outcome.elapsed);
        assert!(outcome.chunks.is_empty(), "no streamed chunks");
        assert_eq!(outcome.terminal.kind, "tool.result");
    }

    latencies.sort();
    let p50 = latencies[RUNS / 2];
    let p95 = latencies[(RUNS * 95) / 100];
    println!(
        "[l1] immediate x{} p50={:?} p95={:?} max={:?}",
        RUNS,
        p50,
        p95,
        latencies.last().unwrap()
    );
    assert!(
        p95 < Duration::from_millis(100),
        "pure protocol round trip stays sub-100ms"
    );
}

#[test]
fn cancel_is_observed_by_the_tool() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    let call_id = host
        .begin_invoke("probe_slow", json!({}))
        .expect("invoke starts");
    sleep(Duration::from_millis(300));
    let cancelled_at = std::time::Instant::now();
    host.cancel(&call_id).expect("cancel sent");

    let outcome = host
        .wait_terminal(&call_id, Duration::from_secs(5))
        .expect("terminal event arrives");
    println!(
        "[l1] cancel observed in {:?} kind={} reason={}",
        cancelled_at.elapsed(),
        outcome.terminal.kind,
        outcome.terminal.payload["reason"]
    );

    assert_eq!(outcome.terminal.kind, "tool.aborted");
    assert_eq!(outcome.terminal.payload["reason"], "cancel");
    assert!(cancelled_at.elapsed() < Duration::from_secs(3));
}

#[test]
fn invoke_as_cancellable_aborts_a_slow_tool() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    // 300ms 后置位取消：宿主应发 tool.cancel 并把 tool.aborted 当终态返回。
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = cancel.clone();
    let handle = thread::spawn(move || {
        sleep(Duration::from_millis(300));
        setter.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let outcome = host
        .invoke_as_cancellable(
            "call-cancel-1",
            "probe_slow",
            json!({}),
            Duration::from_secs(5),
            Some(&cancel),
        )
        .expect("取消后应收到终态（tool.aborted），而不是 Err(Cancelled)");
    let elapsed = started.elapsed();
    handle.join().expect("cancel setter");

    println!(
        "[l1] invoke_as_cancellable settled in {elapsed:?} kind={} reason={}",
        outcome.terminal.kind, outcome.terminal.payload["reason"]
    );
    assert_eq!(outcome.terminal.kind, "tool.aborted");
    assert_eq!(outcome.terminal.payload["reason"], "cancel");
    assert!(
        elapsed < Duration::from_secs(3),
        "取消信号 + 宽限期内必须返回，实际 {elapsed:?}"
    );
}

#[test]
fn killing_the_sidecar_mid_call_reports_closed() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    let call_id = host
        .begin_invoke("probe_slow", json!({}))
        .expect("invoke starts");
    sleep(Duration::from_millis(200));
    host.kill().expect("sidecar killed");

    let err = host
        .wait_terminal(&call_id, Duration::from_secs(3))
        .expect_err("pipe is gone");
    println!("[l1] after kill: {err}");
    assert!(matches!(
        err,
        dshr_compat_dsh_host::HostError::Closed | dshr_compat_dsh_host::HostError::Timeout(_)
    ));
}

/// 一个真实 Node 进程，但**故意不响应** `tool.cancel`：用来覆盖
/// `invoke_as_cancellable` 的兜底路径——发出 `tool.cancel` 后宽限 2s，
/// 仍无终态就返回 `HostError::Cancelled`（而不是拖到原 timeout）。
const FAKE_SIDECAR_JS: &str = r#"
import { createInterface } from 'node:readline'

const send = (kind, payload, requestId) => {
  process.stdout.write(
    JSON.stringify({
      version: 1,
      kind,
      request_id: requestId ?? 'req-fake',
      runtime_generation: 'node-fake',
      depth: 0,
      trace_id: 't-fake',
      ts: '1970-01-01T00:00:00Z',
      payload,
    }) + '\n',
  )
}

const rl = createInterface({ input: process.stdin })
rl.on('line', (line) => {
  let message
  try {
    message = JSON.parse(line)
  } catch {
    return
  }
  switch (message.kind) {
    case 'runtime.hello':
      send('runtime.ready', { generation: 'node-fake' }, message.request_id)
      break
    case 'tool.invoke':
    case 'tool.cancel':
      // 故意忽略取消、永不返回终态。
      break
    default:
      break
  }
})
"#;

#[test]
fn invoke_cancellable_returns_cancelled_when_sidecar_ignores_cancel() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "dshr-fake-sidecar-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("fake sidecar dir");
    let script = dir.join("fake-sidecar.mjs");
    std::fs::write(&script, FAKE_SIDECAR_JS).expect("fake sidecar script");

    let options = HostOptions::default().with_registration_window(Duration::from_millis(200));
    let mut host = SidecarHost::spawn_with(&script, &script, options).expect("fake sidecar starts");

    let cancel = Arc::new(AtomicBool::new(false));
    let setter = cancel.clone();
    let handle = thread::spawn(move || {
        sleep(Duration::from_millis(200));
        setter.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let err = host
        .invoke_as_cancellable(
            "call-ignored",
            "slow_tool",
            json!({}),
            Duration::from_secs(10),
            Some(&cancel),
        )
        .expect_err("不响应 cancel 的 sidecar 必须在宽限后报 Cancelled");
    let elapsed = started.elapsed();
    handle.join().expect("cancel setter");

    println!("[l1] ignored cancel -> {err} after {elapsed:?}");
    assert!(
        matches!(err, HostError::Cancelled(_)),
        "必须返回 HostError::Cancelled，实际 {err}"
    );
    assert!(
        elapsed >= Duration::from_millis(1_900),
        "取消后要留满 2s 宽限，实际 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "不能拖到原来的 10s 超时，实际 {elapsed:?}"
    );

    host.kill().ok();
    std::fs::remove_dir_all(&dir).ok();
}
