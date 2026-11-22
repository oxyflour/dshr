use crate::*;

/// 会话树产品面自检：spawn_child 真执行（子会话独立日志 + 父链接 + 父子事件）、
/// 默认单层（子会话不能再派发）、中断传播（interrupted_by_parent）、
/// 侧栏树顺序/深度、重启后 from_logs 重建。
pub(crate) fn selftest_tree() -> i32 {
    let dir = PathBuf::from(".tmp/tree-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let store = SessionIndex::new(&dir);
    let approvals = ApprovalBroker::new();
    let load = |id: &str| -> Option<SessionLog> {
        let meta = store.scan().ok()?.into_iter().find(|meta| meta.id == id)?;
        let text = std::fs::read_to_string(&meta.path).ok()?;
        SessionLog::from_jsonl(&meta.id, &text).ok()
    };

    // 1) 父会话派发一次子会话；子会话同步跑完一轮并回写父子日志。
    let cancel = Arc::new(AtomicBool::new(false));
    let mut parent = ChatState::open_diagnostics(
        dir.join("s-parent.jsonl"),
        "s-parent",
        &cancel,
        approvals.clone(),
    );
    let spawn = parent.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-1"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "你好，子会话只做一句话回答" }),
    });
    let child_id = spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let depth = spawn
        .value
        .get("depth")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let parent_created = parent.log.created().is_some();
    let parent_spawned = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.spawned");
    let parent_finished = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.finished");
    let child_log = load(&child_id);
    let child_parent_link = child_log
        .as_ref()
        .and_then(|log| log.created())
        .and_then(|created| created.parent_session_id.clone())
        .as_deref()
        == Some("s-parent");
    let child_has_reply = child_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "message.assistant.settled")
    });
    let order = session_tree_order(&store);
    let parent_index = order.iter().position(|(id, _)| id == "s-parent");
    let child_index = order.iter().position(|(id, _)| id == &child_id);
    let tree_ok = depth == 1
        && child_parent_link
        && child_has_reply
        && parent_spawned
        && parent_finished
        && parent_created
        && order
            .iter()
            .any(|(id, level)| id == &child_id && *level == 1)
        && parent_index.is_some()
        && child_index.is_some()
        && parent_index < child_index;

    // 2) 子会话不再派发（默认单层）。
    let mut child_state = ChatState::open_diagnostics_with_options(
        store.path_for(&child_id),
        &child_id,
        &cancel,
        approvals.clone(),
        false,
        false,
    );
    let nested = child_state.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-nested"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "孙会话" }),
    });
    let nested_rejected = nested.is_error;

    // 2b) 产品链路：mock 模型选择 spawn_child → 执行器派发 → 子会话回写 → 模型拿到结果。
    let cancel3 = Arc::new(AtomicBool::new(false));
    let mut parent3 = ChatState::open_diagnostics(
        dir.join("s-parent3.jsonl"),
        "s-parent3",
        &cancel3,
        approvals.clone(),
    );
    let loop_reply = parent3.stream_turn("请派发一个子会话，让它回答：子会话你好", &mut |_| {});
    let has_event =
        |log: &SessionLog, kind: &str| log.events().iter().any(|event| event.event_type == kind);
    let loop_ok = loop_reply
        .as_deref()
        .is_some_and(|reply| !reply.is_empty() && !reply.starts_with("处理失败"))
        && parent3.log.events().iter().any(|event| {
            event.event_type == "tool.call"
                && event.payload.get("tool").and_then(|value| value.as_str())
                    == Some(SPAWN_CHILD_TOOL)
        })
        && has_event(&parent3.log, "child.spawned")
        && has_event(&parent3.log, "tool.result")
        && has_event(&parent3.log, "child.finished");

    // 3) 中断传播：父取消后派发 → 父 session.interrupted + 子 interrupted_by_parent。
    let cancel2 = Arc::new(AtomicBool::new(false));
    let mut parent2 = ChatState::open_diagnostics(
        dir.join("s-parent2.jsonl"),
        "s-parent2",
        &cancel2,
        approvals.clone(),
    );
    cancel2.store(true, Ordering::Relaxed);
    let interrupted_spawn = parent2.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-interrupted"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "中断测试" }),
    });
    let child2_id = interrupted_spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let outcome_interrupted = interrupted_spawn
        .value
        .get("outcome")
        .and_then(|value| value.as_str())
        == Some("interrupted");
    let child2_log = load(&child2_id);
    let child_interrupted = child2_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "session.interrupted_by_parent")
    });
    let parent_interrupted = parent2
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");

    // 4) 重启恢复：从磁盘日志重建运行态树，深度与中断状态一致。
    let logs: Vec<SessionLog> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|meta| {
            std::fs::read_to_string(&meta.path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl(&meta.id, &text).ok())
        })
        .collect();
    let rebuilt = SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH);
    let (rebuilt_depth, rebuilt_interrupted, replay_equal) = match &rebuilt {
        Ok(runtime) => (
            runtime.depth(&child_id),
            runtime
                .session(&child2_id)
                .map(|session| session.status.is_interrupted())
                .unwrap_or(false),
            // 父日志回放：新事件不影响对话模型构建（未知事件忽略）。
            child_log.as_ref().is_some_and(|log| {
                let jsonl = log.to_jsonl();
                SessionLog::from_jsonl(log.session_id(), &jsonl)
                    .map(|reloaded| reloaded.len() == log.len())
                    .unwrap_or(false)
            }),
        ),
        Err(_) => (None, false, false),
    };
    let rebuilt_ok = rebuilt_depth == Some(1) && rebuilt_interrupted && replay_equal;

    let ok = !spawn.is_error
        && !child_id.is_empty()
        && tree_ok
        && nested_rejected
        && loop_ok
        && outcome_interrupted
        && child_interrupted
        && parent_interrupted
        && rebuilt_ok;
    println!(
        "[tree] child={child_id} depth={depth} tree_ok={tree_ok} nested_rejected={nested_rejected} \
         loop_ok={loop_ok} interrupted={outcome_interrupted}/{child_interrupted}/{parent_interrupted} \
         rebuilt_ok={rebuilt_ok} parent_created={parent_created} spawned={parent_spawned} \
         finished={parent_finished} link={child_parent_link} reply={child_has_reply} \
         order={:?}",
        order
            .iter()
            .map(|(id, level)| format!("{id}:{level}"))
            .collect::<Vec<_>>()
    );
    if ok {
        println!("[selftest-tree] PASS");
        0
    } else {
        println!("[selftest-tree] FAIL");
        1
    }
}

/// 导出/保留自检：JSONL + 外部化附件 + manifest sha256，导出后可自校验；
/// CLI 入口 `--export-session` 也走同一路径。
pub(crate) fn selftest_export() -> i32 {
    let root = PathBuf::from(".tmp/export-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let sessions = root.join("sessions");
    let _ = std::fs::create_dir_all(&sessions);
    std::env::set_var("DSHR_SESSIONS_DIR", &sessions);
    std::env::set_var("DSHR_SPILL_DIR", root.join("spill"));
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");

    let cancel = Arc::new(AtomicBool::new(false));
    let path = sessions.join("s-export.jsonl");
    let mut state =
        ChatState::open_diagnostics(path.clone(), "s-export", &cancel, ApprovalBroker::new());
    let big = format!("{}导出附件标记", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "ex-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "ex-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();
    let _ = state.plan_with_spill();

    let out = root.join("out");
    let manifest = match export_session(
        &state.log,
        &SpillStoreSource::new(&state.spill),
        &out,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            println!("[selftest-export] FAIL: 导出失败 {err}");
            return 1;
        }
    };
    let verified = match verify_export(&out, "s-export") {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-export] FAIL: 自校验失败 {err}");
            return 1;
        }
    };
    let exported_jsonl = out.join(&manifest.jsonl.path);
    let reloaded = std::fs::read_to_string(&exported_jsonl)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-export", &text).ok());
    let reload_equal = reloaded
        .as_ref()
        .map(|log| log.len() == state.log.len())
        .unwrap_or(false);
    // CLI 入口同样可复现（导出 + 自校验）。
    let cli_out = root.join("out-cli");
    let cli_ok = export_session_cli("s-export", Some(cli_out)) == 0;

    let ok = manifest.event_count == state.log.len()
        && manifest.attachments.len() == 1
        && manifest.missing_blobs.is_empty()
        && verified.is_ok()
        && verified.attachments_checked == 1
        && reload_equal
        && cli_ok
        && manifest.jsonl.size > 0
        && !manifest.jsonl.sha256.is_empty();
    println!(
        "[export] events={} attachments={} missing={} bytes={} verify={} reload_equal={reload_equal} cli={cli_ok}",
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        verified.is_ok()
    );
    // 保留计划：目录里只有本会话、未超限 → 不需要删除。
    let plan_code = retention_plan_cli();
    let ok = ok && plan_code == 0;
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-export] PASS");
        0
    } else {
        println!("[selftest-export] FAIL");
        1
    }
}
