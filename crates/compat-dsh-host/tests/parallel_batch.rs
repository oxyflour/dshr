//! `SidecarHost::invoke_batch` 集成测试（M3 并行工具）：真实 probe sidecar + NDJSON。
//!
//! 覆盖：多请求关联、原顺序、并发墙钟、per-call timeout、迟到结果丢弃、取消、空批。
//! 测试 ID：`M3-PARALLEL-101..107`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_host::{BatchInvokeCall, HostError, HostOptions, SidecarHost};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("workspace root")
}

fn sidecar_paths() -> (PathBuf, PathBuf) {
    let root = repo_root();
    (
        root.join("sidecar/probe-sidecar.mjs"),
        root.join("sidecar/probe-plugin.mjs"),
    )
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn spawn_probe() -> Option<SidecarHost> {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return None;
    }
    let (script, plugin) = sidecar_paths();
    let options = HostOptions::default().with_registration_window(Duration::from_millis(1500));
    match SidecarHost::spawn_with(script, plugin, options) {
        Ok(host) => Some(host),
        Err(error) => {
            eprintln!("SKIP: probe sidecar 启动失败: {error}");
            None
        }
    }
}

fn echo_call(call_id: &str, index: usize) -> BatchInvokeCall {
    BatchInvokeCall::new(
        call_id,
        "probe_echo",
        json!({ "i": index }),
        Duration::from_secs(5),
    )
}

fn value_of(outcome: &dshr_compat_dsh_host::InvokeOutcome) -> &Value {
    &outcome.terminal.payload["value"]
}

/// M3-PARALLEL-101：一批 3 个 probe_echo：结果按原顺序关联，chunk 不串线。
#[test]
fn m3_parallel_101_batch_correlates_results_and_chunks() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let calls = vec![
        echo_call("call-b1", 0),
        echo_call("call-b2", 1),
        echo_call("call-b3", 2),
    ];
    let results = host.invoke_batch(&calls, 3, None);
    assert_eq!(results.len(), 3);

    for (index, result) in results.iter().enumerate() {
        let outcome = result
            .as_ref()
            .unwrap_or_else(|error| panic!("call {index} failed: {error}"));
        assert_eq!(outcome.terminal.kind, "tool.result", "{index}: {outcome:?}");
        assert_eq!(
            outcome.terminal.call_id.as_deref(),
            Some(calls[index].call_id.as_str()),
            "结果必须按 call_id 关联"
        );
        assert_eq!(value_of(outcome)["echo"]["i"], index);
        assert_eq!(outcome.chunks.len(), 3, "probe_echo 的 3 个 chunk 都应归位");
    }
}

/// M3-PARALLEL-102：3 个 2s 的 probe_slow 并发墙钟接近单个耗时（串行应 ≈6s）。
#[test]
fn m3_parallel_102_batch_runs_concurrently() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    // 单个基线：probe_slow = 10 × 200ms。
    let baseline_started = Instant::now();
    let baseline = host
        .invoke("probe_slow", json!({}), Duration::from_secs(10))
        .expect("baseline probe_slow");
    assert_eq!(baseline.terminal.kind, "tool.result");
    let baseline_elapsed = baseline_started.elapsed();

    let calls: Vec<BatchInvokeCall> = (0..3)
        .map(|index| {
            BatchInvokeCall::new(
                format!("call-s{index}"),
                "probe_slow",
                json!({}),
                Duration::from_secs(10),
            )
        })
        .collect();
    let started = Instant::now();
    let results = host.invoke_batch(&calls, 3, None);
    let elapsed = started.elapsed();

    assert_eq!(results.len(), 3);
    for result in &results {
        let outcome = result.as_ref().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(outcome.terminal.kind, "tool.result");
    }
    assert!(
        elapsed < baseline_elapsed.mul_f64(1.6),
        "并行墙钟 {elapsed:?} 应接近单个 {baseline_elapsed:?}（串行应 ≈3×）"
    );
    assert!(
        elapsed < Duration::from_millis(3_500),
        "并行批不应串行到 6s: {elapsed:?}"
    );
}

/// M3-PARALLEL-103：per-call timeout 只影响慢调用，快调用成功且 host 仍可用。
#[test]
fn m3_parallel_103_per_call_timeout_isolated() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let calls = vec![
        BatchInvokeCall::new(
            "call-fast",
            "probe_immediate",
            json!({}),
            Duration::from_secs(5),
        ),
        BatchInvokeCall::new(
            "call-slow",
            "probe_slow",
            json!({}),
            Duration::from_millis(250),
        ),
    ];
    let started = Instant::now();
    let results = host.invoke_batch(&calls, 2, None);
    let elapsed = started.elapsed();

    let fast = results[0].as_ref().expect("fast call succeeds");
    assert_eq!(fast.terminal.kind, "tool.result");
    assert_eq!(value_of(fast)["ok"], true);
    match &results[1] {
        Err(HostError::Timeout(_)) => {}
        other => panic!("慢调用应超时: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_millis(1_500),
        "超时后应尽快返回（不等 probe_slow 2s）: {elapsed:?}"
    );

    // 批结束后 host 仍可用（读循环未被破坏）。
    let outcome = host
        .invoke("probe_immediate", json!({}), Duration::from_secs(5))
        .expect("host usable after batch timeout");
    assert_eq!(value_of(&outcome)["ok"], true);
}

/// M3-PARALLEL-104：超时后的迟到结果被丢弃，不干扰后续调用。
#[test]
fn m3_parallel_104_late_result_is_dropped() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let calls = vec![BatchInvokeCall::new(
        "call-late",
        "probe_slow",
        json!({}),
        Duration::from_millis(200),
    )];
    let results = host.invoke_batch(&calls, 1, None);
    assert!(
        matches!(results[0], Err(HostError::Timeout(_))),
        "{:?}",
        results[0]
    );

    // probe_slow 的迟到 tool.result 会在 ~2s 后到达；随后单调用必须正常。
    thread::sleep(Duration::from_millis(2_200));
    let outcome = host
        .invoke("probe_echo", json!({ "i": 7 }), Duration::from_secs(5))
        .expect("invoke after late result");
    assert_eq!(value_of(&outcome)["echo"]["i"], 7);
}

/// M3-PARALLEL-105：取消置位 → 对在途调用发 tool.cancel，批快速返回 aborted 终态。
#[test]
fn m3_parallel_105_cancel_aborts_in_flight_calls() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&cancel);
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        setter.store(true, Ordering::Relaxed);
    });
    let calls: Vec<BatchInvokeCall> = (0..2)
        .map(|index| {
            BatchInvokeCall::new(
                format!("call-c{index}"),
                "probe_slow",
                json!({}),
                Duration::from_secs(10),
            )
        })
        .collect();
    let started = Instant::now();
    let results = host.invoke_batch(&calls, 2, Some(cancel.as_ref()));
    let elapsed = started.elapsed();
    let _ = canceller.join();

    assert_eq!(results.len(), 2);
    for result in &results {
        let outcome = result
            .as_ref()
            .unwrap_or_else(|error| panic!("取消后仍应结算: {error}"));
        assert_eq!(
            outcome.terminal.kind, "tool.aborted",
            "probe_slow 的取消终态是 tool.aborted: {outcome:?}"
        );
    }
    assert!(
        elapsed < Duration::from_secs(2),
        "取消应尽快返回（不等 2s）: {elapsed:?}"
    );
}

/// M3-PARALLEL-106：`max_parallel=1` 时按波次串行，结果仍按原顺序。
#[test]
fn m3_parallel_106_max_parallel_one_preserves_order() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let calls: Vec<BatchInvokeCall> = (0..3)
        .map(|index| echo_call(&format!("call-o{index}"), index))
        .collect();
    let results = host.invoke_batch(&calls, 1, None);
    for (index, result) in results.iter().enumerate() {
        let outcome = result.as_ref().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value_of(outcome)["echo"]["i"], index, "顺序必须保持");
        assert_eq!(
            outcome.terminal.call_id.as_deref(),
            Some(calls[index].call_id.as_str())
        );
    }
}

/// M3-PARALLEL-107：空批返回空 vec（不碰 sidecar）。
#[test]
fn m3_parallel_107_empty_batch_is_noop() {
    let Some(mut host) = spawn_probe() else {
        return;
    };
    let results = host.invoke_batch(&[], 10, None);
    assert!(results.is_empty());
    // 空批后 host 仍可用。
    let outcome = host
        .invoke("probe_immediate", json!({}), Duration::from_secs(5))
        .expect("host usable");
    assert_eq!(value_of(&outcome)["ok"], true);
}

// ---------------------------------------------------------------------------
// batch fixture：取消 2s 宽限 + session_id 透传（P1-7 / P2-10）
// ---------------------------------------------------------------------------

fn batch_fixture_paths() -> (PathBuf, PathBuf) {
    let dir = std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
        .expect("fixtures dir");
    let script = dir.join("batch-sidecar.mjs");
    (script.clone(), script)
}

fn spawn_batch_fixture() -> Option<SidecarHost> {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return None;
    }
    let (script, plugin) = batch_fixture_paths();
    let options = HostOptions::default().with_registration_window(Duration::from_millis(1500));
    match SidecarHost::spawn_with(script, plugin, options) {
        Ok(host) => Some(host),
        Err(error) => {
            eprintln!("SKIP: batch fixture 启动失败: {error}");
            None
        }
    }
}

/// M3-PARALLEL-108：sidecar 忽略 `tool.cancel` 时，批在 2s 宽限内以 `Cancelled` 结算，
/// 不能等到 per-call timeout（120s）。
#[test]
fn m3_parallel_108_cancel_grace_with_ignoring_sidecar() {
    let Some(mut host) = spawn_batch_fixture() else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = Arc::clone(&cancel);
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        setter.store(true, Ordering::Relaxed);
    });
    let calls: Vec<BatchInvokeCall> = (0..2)
        .map(|index| {
            BatchInvokeCall::new(
                format!("call-ignore-{index}"),
                "batch_ignore_cancel",
                json!({ "ms": 10_000 }),
                Duration::from_secs(30),
            )
        })
        .collect();
    let started = Instant::now();
    let results = host.invoke_batch(&calls, 2, Some(cancel.as_ref()));
    let elapsed = started.elapsed();
    let _ = canceller.join();

    assert_eq!(results.len(), 2);
    for result in &results {
        assert!(
            matches!(result, Err(HostError::Cancelled(_))),
            "忽略 cancel 的 sidecar 也必须在宽限后结算为 Cancelled: {result:?}"
        );
    }
    assert!(
        elapsed >= Duration::from_millis(1_800),
        "应给 sidecar 2s 宽限: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(5_000),
        "不能等到 per-call 30s/120s: {elapsed:?}"
    );
}

/// M3-PARALLEL-109：批内 `session_id` 透传到 `tool.invoke` 信封（L2 会话方法的前提）。
#[test]
fn m3_parallel_109_batch_passes_session_id() {
    let Some(mut host) = spawn_batch_fixture() else {
        return;
    };
    let calls = vec![
        BatchInvokeCall::new("call-s1", "batch_fast", json!({}), Duration::from_secs(5))
            .with_session("s-l2-test"),
        BatchInvokeCall::new("call-s2", "batch_fast", json!({}), Duration::from_secs(5))
            .with_session("s-l2-test"),
    ];
    let results = host.invoke_batch(&calls, 2, None);
    for result in &results {
        let outcome = result.as_ref().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            value_of(outcome)["session_id"],
            "s-l2-test",
            "批调用必须透传 session_id"
        );
    }
}

/// M3-PARALLEL-110：单调用 `invoke_in_session` 同样透传 session_id；`None` 保持旧占位。
#[test]
fn m3_parallel_110_single_invoke_passes_session_id() {
    let Some(mut host) = spawn_batch_fixture() else {
        return;
    };
    let outcome = host
        .invoke_in_session(
            "call-single",
            Some("s-l2-single"),
            "batch_fast",
            json!({}),
            Duration::from_secs(5),
        )
        .expect("single invoke in session");
    assert_eq!(value_of(&outcome)["session_id"], "s-l2-single");

    let legacy = host
        .invoke("batch_fast", json!({}), Duration::from_secs(5))
        .expect("legacy invoke");
    assert_eq!(
        value_of(&legacy)["session_id"],
        "s-probe",
        "None 保持旧 s-probe 占位（探针/旧测试兼容）"
    );
}
