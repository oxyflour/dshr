#![cfg(test)]

use super::*;
use serde_json::json;

fn ts() -> &'static str {
    "2026-11-09T10:00:00.000Z"
}

fn log_with_call() -> SessionLog {
    let mut log = SessionLog::new("s-1");
    log.append(EventSpec::durable(
        "tool.call",
        ts(),
        json!({ "call_id": "call-1", "tool": "bash" }),
    ))
    .unwrap();
    log
}

#[test]
fn append_assigns_contiguous_seq() {
    let mut log = SessionLog::new("s-1");
    assert_eq!(
        log.append(EventSpec::durable("turn.started", ts(), json!({})))
            .unwrap(),
        1
    );
    assert_eq!(
        log.append(EventSpec::durable("turn.ended", ts(), json!({})))
            .unwrap(),
        2
    );
    assert_eq!(log.len(), 2);
}

#[test]
fn rejects_second_settlement_for_same_call() {
    let mut log = log_with_call();
    log.append(EventSpec::durable(
        "tool.result",
        ts(),
        json!({ "call_id": "call-1", "is_error": false }),
    ))
    .unwrap();

    let err = log
        .append(EventSpec::durable(
            "tool.aborted",
            ts(),
            json!({ "call_id": "call-1", "reason": "crash" }),
        ))
        .unwrap_err();

    assert_eq!(
        err,
        LogError::DuplicateSettlement {
            call_id: "call-1".to_owned()
        }
    );
    assert_eq!(log.len(), 2, "failed append must not mutate the log");
}

#[test]
fn rejects_duplicate_assistant_settlement() {
    let mut log = SessionLog::new("s-1");
    log.append(EventSpec::durable(
        "message.assistant.settled",
        ts(),
        json!({ "message_id": "m-1", "text": "hi" }),
    ))
    .unwrap();

    let err = log
        .append(EventSpec::durable(
            "message.assistant.settled",
            ts(),
            json!({ "message_id": "m-1", "text": "hi again" }),
        ))
        .unwrap_err();
    assert!(matches!(err, LogError::DuplicateMessage { .. }));
}

#[test]
fn rejects_settlement_without_call_id() {
    let mut log = SessionLog::new("s-1");
    let err = log
        .append(EventSpec::durable("tool.result", ts(), json!({})))
        .unwrap_err();
    assert_eq!(
        err,
        LogError::MissingField {
            event_type: "tool.result".to_owned(),
            field: "call_id".to_owned(),
        }
    );
}

#[test]
fn replay_rebuilds_messages_tools_and_turns() {
    let mut log = SessionLog::new("s-1");
    log.append(EventSpec::durable("turn.started", ts(), json!({})))
        .unwrap();
    log.append(EventSpec::durable(
        "message.user",
        ts(),
        json!({ "message_id": "m-1", "text": "run it" }),
    ))
    .unwrap();
    log.append(EventSpec::ephemeral(
        "message.assistant.delta",
        ts(),
        json!({ "message_id": "m-2", "text": "partial" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "message.assistant.settled",
        ts(),
        json!({ "message_id": "m-2", "text": "done" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "tool.call",
        ts(),
        json!({ "call_id": "call-1", "tool": "bash" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "tool.result",
        ts(),
        json!({ "call_id": "call-1", "is_error": true }),
    ))
    .unwrap();
    log.append(EventSpec::durable("some.future.event", ts(), json!({})))
        .unwrap();

    let view = log.replay();
    assert_eq!(view.turns, 1);
    assert_eq!(view.messages.len(), 2);
    assert_eq!(view.messages[1].role, Role::Assistant);
    assert_eq!(view.messages[1].text, "done");
    assert_eq!(
        view.tool_calls[0].state,
        ToolState::Settled { is_error: true }
    );
    assert_eq!(view.unmodeled_events, 1);
    assert_eq!(view.ephemeral_skipped, 1);
}

#[test]
fn replay_marks_aborted_tool_and_skips_ephemeral() {
    let mut log = log_with_call();
    log.append(EventSpec::ephemeral(
        "tool.chunk",
        ts(),
        json!({ "call_id": "call-1", "text": "..." }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "tool.aborted",
        ts(),
        json!({ "call_id": "call-1", "reason": "crash" }),
    ))
    .unwrap();

    let view = log.replay();
    assert_eq!(view.ephemeral_skipped, 1);
    assert_eq!(
        view.tool_calls[0].state,
        ToolState::Aborted {
            reason: "crash".to_owned()
        }
    );
}

#[test]
fn jsonl_round_trip_preserves_events_and_view() {
    let mut log = log_with_call();
    log.append(EventSpec::durable(
        "tool.result",
        ts(),
        json!({ "call_id": "call-1", "is_error": false }),
    ))
    .unwrap();

    let jsonl = log.to_jsonl();
    let restored = SessionLog::from_jsonl("s-1", &jsonl).unwrap();

    assert_eq!(restored.events(), log.events());
    assert_eq!(
        restored.replay().tool_calls[0].state,
        ToolState::Settled { is_error: false }
    );
}

#[test]
fn from_jsonl_rejects_seq_gap_and_wrong_session() {
    let mut log = log_with_call();
    log.append(EventSpec::durable(
        "tool.result",
        ts(),
        json!({ "call_id": "call-1" }),
    ))
    .unwrap();
    let mut jsonl = log.to_jsonl();

    let err = SessionLog::from_jsonl("s-2", &jsonl).unwrap_err();
    assert!(matches!(err, LogError::SessionMismatch { .. }));

    let first_line_end = jsonl.find('\n').unwrap();
    jsonl.remove(first_line_end - 1); // 破坏第一条事件的字符
    let err = SessionLog::from_jsonl("s-1", &jsonl).unwrap_err();
    assert!(
        matches!(err, LogError::Invalid(_) | LogError::SeqGap { .. }),
        "corrupted line must be rejected, got {err:?}"
    );
}

// ------------------------------------------------------------------
// 会话树（规范 §3.6 / §7）
// ------------------------------------------------------------------

#[test]
fn created_must_be_first_and_carries_parent_and_source() {
    let mut log = SessionLog::new("s-child");
    let created =
        SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent).with_preset("reviewer");
    assert_eq!(log.append_created(ts(), &created).unwrap(), 1);

    assert_eq!(log.parent_session_id(), Some("s-parent"));
    assert_eq!(log.spawn_source(), Some(SpawnSource::Agent));
    assert_eq!(log.created().unwrap().preset.as_deref(), Some("reviewer"));

    // 不是第一条 → 拒绝
    let mut later = SessionLog::new("s-x");
    later
        .append(EventSpec::durable("turn.started", ts(), json!({})))
        .unwrap();
    let err = later.append_created(ts(), &created).unwrap_err();
    assert!(matches!(err, LogError::Invalid(_)));

    // 自引用 → 拒绝
    let mut self_parent = SessionLog::new("s-self");
    let err = self_parent
        .append_created(
            ts(),
            &SessionCreated::child("ws", "s-self", SpawnSource::Agent),
        )
        .unwrap_err();
    assert!(matches!(err, LogError::Invalid(_)));
}

#[test]
fn replay_rebuilds_tree_fields_from_session_created() {
    let mut log = SessionLog::new("s-root");
    log.append_created(ts(), &SessionCreated::root("ws-1"))
        .unwrap();
    log.append_child_spawned(ts(), "s-child", "修窗口", Some("call-1"))
        .unwrap();
    log.append_child_finished(ts(), "s-child", "completed")
        .unwrap();
    log.append_interrupted(ts(), "user_stop").unwrap();

    let view = log.replay();
    assert_eq!(view.created.as_ref().unwrap().workspace_id, "ws-1");
    assert!(!view.created.as_ref().unwrap().is_child());
    assert_eq!(view.children.len(), 1);
    assert_eq!(view.children[0].child_session_id, "s-child");
    assert_eq!(view.children[0].prompt, "修窗口");
    assert_eq!(view.children[0].outcome.as_deref(), Some("completed"));
    assert!(view.is_interrupted());
    assert!(!view.interrupted_by_parent());
    assert!(!view.was_killed());
    assert_eq!(view.unmodeled_events, 0);
}

#[test]
fn replay_marks_interrupted_by_parent_and_kill_escalation() {
    let mut child = SessionLog::new("s-child");
    child
        .append_created(
            ts(),
            &SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent),
        )
        .unwrap();
    child
        .append_interrupted_by_parent(ts(), "s-parent", "parent_interrupt")
        .unwrap();
    child.append_killed(ts(), "interrupt_timeout").unwrap();

    let view = child.replay();
    assert_eq!(
        view.created.as_ref().unwrap().parent_session_id.as_deref(),
        Some("s-parent")
    );
    assert_eq!(view.interruptions.len(), 2);
    assert!(view.interrupted_by_parent());
    assert!(view.was_killed());
    assert!(child.interrupted_by_parent());
    assert!(child.was_killed());
}

#[test]
fn tree_events_survive_jsonl_round_trip() {
    let mut log = SessionLog::new("s-child");
    log.append_created(
        ts(),
        &SessionCreated::child("ws-1", "s-parent", SpawnSource::Agent),
    )
    .unwrap();
    log.append_interrupted_by_parent(ts(), "s-parent", "parent_interrupt")
        .unwrap();

    let restored = SessionLog::from_jsonl("s-child", &log.to_jsonl()).unwrap();
    assert_eq!(restored.parent_session_id(), Some("s-parent"));
    assert_eq!(restored.spawn_source(), Some(SpawnSource::Agent));
    assert!(restored.replay().interrupted_by_parent());
}

#[test]
fn spawn_source_round_trips_and_tolerates_unknown_values() {
    assert_eq!(SpawnSource::parse("user"), Some(SpawnSource::User));
    assert_eq!(SpawnSource::parse("agent"), Some(SpawnSource::Agent));
    assert_eq!(SpawnSource::parse("acp"), None);
    assert_eq!(SpawnSource::User.as_str(), "user");
    assert_eq!(SpawnSource::Agent.as_str(), "agent");
}
