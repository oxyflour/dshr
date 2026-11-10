//! L1 端到端：真实 Node 进程 + NDJSON 协议。
//!
//! 这些测试就是"实验 C 的单位成本表"的来源：它们不仅断言行为，
//! 还打印每次 invoke 的耗时，用来回答"一个 adapter 到底要几天"。

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use dshr_compat_dsh_host::SidecarHost;
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
