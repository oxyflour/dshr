//! M1 闭环：真实 sidecar 调用 → 写入 Session Log → **进程重启** → 回放出一致的界面模型。
//!
//! 这是 M1 退出条件的最后一段：日志必须能独立于 Sidecar 存活。

use std::path::PathBuf;
use std::time::Duration;

use dshr_compat_dsh_host::SidecarHost;
use dshr_conversation_model::{build_from_log, Item};
use dshr_session_store::{EventSpec, SessionLog, ToolState};
use serde_json::json;

const TS: &str = "2026-11-09T12:00:00.000Z";

fn sidecar_paths() -> (PathBuf, PathBuf) {
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
fn sidecar_call_is_recorded_then_replays_after_restart() {
    if !node_available() {
        eprintln!("SKIP: node not found in PATH");
        return;
    }
    let (script, plugin) = sidecar_paths();
    let mut host = SidecarHost::spawn(&script, &plugin).expect("sidecar starts");

    let mut log = SessionLog::new("s-m1");
    let call_id = "call-m1-1";

    log.append(EventSpec::durable(
        "turn.started",
        TS,
        json!({ "turn_id": "turn-1" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "message.user",
        TS,
        json!({ "message_id": "m-1", "text": "用 sidecar 工具跑一次" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "tool.call",
        TS,
        json!({
            "call_id": call_id,
            "tool": "probe_echo",
            "origin": "sidecar",
            "arguments": { "i": 7 }
        }),
    ))
    .unwrap();

    let outcome = host
        .invoke_as(
            call_id,
            "probe_echo",
            json!({ "i": 7 }),
            Duration::from_secs(5),
        )
        .expect("invoke succeeds");

    for (seq, chunk) in outcome.chunks.iter().enumerate() {
        log.append(EventSpec::ephemeral(
            "tool.chunk",
            TS,
            json!({ "call_id": call_id, "seq": seq, "text": chunk }),
        ))
        .unwrap();
    }

    assert_eq!(outcome.terminal.kind, "tool.result", "结算事件必须到达");
    assert_eq!(outcome.terminal.payload["is_error"], false);
    log.append(EventSpec::durable(
        "tool.result",
        TS,
        json!({
            "call_id": call_id,
            "is_error": false,
            "content": outcome.terminal.payload["content"].clone()
        }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "message.assistant.settled",
        TS,
        json!({ "message_id": "m-2", "text": "工具返回了 echo" }),
    ))
    .unwrap();

    let before = build_from_log(&log);
    assert_eq!(before.len(), 3, "user + tool card + assistant");
    match &before[1] {
        Item::Tool(tool) => {
            assert_eq!(tool.tool, "probe_echo");
            assert_eq!(tool.origin, "sidecar");
            assert_eq!(tool.state, ToolState::Settled { is_error: false });
        }
        other => panic!("expected a tool card, got {other:?}"),
    }

    // 落盘，然后**销毁 sidecar 进程**，模拟关掉桌面端再重开。
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("artifact dir");
    let path = dir.join("m1-closure.jsonl");
    std::fs::write(&path, log.to_jsonl()).expect("write log");

    drop(host);

    let reloaded =
        SessionLog::from_jsonl("s-m1", &std::fs::read_to_string(&path).expect("read log"))
            .expect("log reloads without the sidecar");
    let after = build_from_log(&reloaded);

    assert_eq!(before, after, "重启后回放必须逐项一致");
    println!(
        "[closure] events={} items={} state={:?} manifest={}",
        reloaded.len(),
        after.len(),
        after[1],
        path.display()
    );
}
