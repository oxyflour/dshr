use crate::*;

/// 上下文预算自检：塞一份超预算的历史，断言
/// (1) 写了 `context.measured` 事件；(2) 工具结果被裁剪或旧消息被丢弃；
/// (3) 实际发给模型的历史仍然是合法配对（不会以 Tool 开头）。
pub(crate) fn selftest_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    // 直接写日志模拟长会话：CJK 文本 + 大工具结果，确保超过 8000 的 80%。
    for index in 0..20 {
        let user = format!("历史问题 {index}：{}", "长内容".repeat(60));
        let _ = state.log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("h-u{index}"), "text": user }),
        ));
        let call_id = format!("h-c{index}");
        let _ = state.log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": call_id, "tool": "probe_echo", "arguments": {} }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": call_id, "is_error": false,
                "content": [{ "type": "text", "text": "输出".repeat(800) }]
            }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("h-a{index}"), "text": "收到" }),
        ));
    }
    state.save();

    let reply = state.stream_turn("继续", &mut |_| {});
    let measured = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.measured")
        .cloned();
    let Some(measured) = measured else {
        println!("[selftest-context] FAIL: 没有 context.measured 事件");
        return 1;
    };
    let ratio = measured
        .payload
        .get("ratio")
        .and_then(|value| value.as_f64())
        .unwrap_or(1.0);
    let pruned = measured
        .payload
        .get("pruned_tool_results")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let dropped = measured
        .payload
        .get("dropped_messages")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    // 压缩事件 + 摘要消息 + 重建出的 System 摘要都应存在。
    let compacted = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.compacted")
        .cloned();
    let summary_id = compacted
        .as_ref()
        .and_then(|event| event.payload.get("summary_message_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let summary_event = state.log.events().iter().find(|event| {
        event.event_type == "message.assistant.settled"
            && event.payload.get("message_id").and_then(|v| v.as_str()) == Some(&summary_id)
    });
    let history = history_from_log(&state.log);
    let has_system_summary = history
        .iter()
        .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
    println!(
        "[context] ratio={ratio:.3} pruned={pruned} dropped={dropped} compacted={} summary_chars={:?} window={:?}",
        compacted.is_some(),
        summary_event
            .and_then(|event| event.payload.get("text"))
            .and_then(|value| value.as_str())
            .map(str::chars)
            .map(Iterator::count),
        measured.payload.get("window")
    );

    // 用同一份日志重建计划，断言配对合法且压缩后不再触发丢弃。
    let plan = dshr_agent_context::plan_context(&state.log, "");
    let legal = !matches!(
        plan.messages.first(),
        Some(dshr_agent_core::ChatMessage::Tool { .. })
    );

    let ok = reply.is_some()
        && ratio <= 0.8
        && compacted.is_some()
        && !summary_id.is_empty()
        && summary_event.is_some()
        && has_system_summary
        && legal
        && plan.dropped_messages == 0
        && plan.tokens_after <= plan.window;
    if ok {
        println!("[selftest-context] PASS");
        0
    } else {
        println!("[selftest-context] FAIL");
        1
    }
}

/// 长会话压缩自检（Gate 3）：连续多轮注入超预算历史，验证
/// (1) 每次超预算都累积一条 `context.compacted`（可叠加多次压缩）；
/// (2) 每轮压缩后 `context.measured.ratio ≤ 0.8`、不再丢消息、配对合法；
/// (3) 重新读 JSONL 的 UI 行与模型历史与在线日志逐行一致（回放一致）。
pub(crate) fn selftest_long_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-long-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let mut failures = 0usize;
    let mut compactions = 0usize;
    // 每轮写入的历史量是窗口的 2 倍以上，保证"每轮都触发一次压缩"。
    for round in 0..8 {
        for index in 0..20 {
            let user = format!("长会话第 {round}-{index} 轮：{}", "长内容".repeat(60));
            let _ = state.log.append(EventSpec::durable(
                "message.user",
                "t0",
                json!({ "message_id": format!("lc-u{round}-{index}"), "text": user }),
            ));
            let call_id = format!("lc-c{round}-{index}");
            let _ = state.log.append(EventSpec::durable(
                "tool.call",
                "t0",
                json!({ "call_id": call_id, "tool": "probe_echo", "arguments": {} }),
            ));
            let _ = state.log.append(EventSpec::durable(
                "tool.result",
                "t0",
                json!({
                    "call_id": call_id, "is_error": false,
                    "content": [{ "type": "text", "text": "输出".repeat(800) }]
                }),
            ));
            let _ = state.log.append(EventSpec::durable(
                "message.assistant.settled",
                "t0",
                json!({ "message_id": format!("lc-a{round}-{index}"), "text": "收到" }),
            ));
        }
        let reply = state.stream_turn("继续", &mut |_| {});
        if reply.is_none() {
            failures += 1;
        }

        let latest_compacted = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.compacted")
            .cloned();
        let Some(compacted) = latest_compacted else {
            println!("[long-context] round={round} FAIL: 没有 context.compacted");
            return 1;
        };
        compactions = state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == "context.compacted")
            .count();

        let measured_ratio = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("ratio"))
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0);
        let measured_window = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("window"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let plan = plan_context(&state.log, "");
        let legal = !matches!(plan.messages.first(), Some(ChatMessage::Tool { .. }));

        // 回放一致性：JSONL → 事件 → 行/历史，必须与在线状态逐项一致。
        let rebuilt =
            SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay log");
        let live_rows = rows_snapshot(&state.log, None);
        let replay_rows = rows_snapshot(&rebuilt, None);
        let rows_equal = live_rows.len() == replay_rows.len()
            && live_rows
                .iter()
                .zip(replay_rows.iter())
                .all(|(live, replay)| live.main_text() == replay.main_text());
        let live_history = history_from_log(&state.log);
        let replay_history = history_from_log(&rebuilt);
        let history_equal = live_history.len() == replay_history.len()
            && live_history
                .iter()
                .zip(replay_history.iter())
                .all(|(live, replay)| format!("{live:?}") == format!("{replay:?}"));
        let has_summary = live_history
            .iter()
            .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
        let compaction_rows = live_rows
            .iter()
            .filter(|row| row.main_text().contains("上下文已压缩"))
            .count();
        let ok = measured_ratio <= 0.8
            && measured_window > 0
            && plan.tokens_after <= plan.window
            && plan.dropped_messages == 0
            && legal
            && rows_equal
            && history_equal
            && has_summary
            // UI 只显示"最近一次压缩"标记（更早的压缩事件被后续压缩区间覆盖，属预期），
            // 一致性由 rows_equal/history_equal 保证。
            && compaction_rows >= 1
            && compacted.payload.get("tokens_after").is_some();
        if !ok {
            failures += 1;
        }
        println!(
            "[long-context] round={round} compactions={compactions} ratio={measured_ratio:.3} \
             tokens={}/{} dropped={} rows={} history={} rows_equal={rows_equal} \
             history_equal={history_equal} summary={has_summary} ok={ok}",
            plan.tokens_after,
            plan.window,
            plan.dropped_messages,
            live_rows.len(),
            live_history.len(),
        );
    }

    let ok = failures == 0 && compactions >= 5;
    if ok {
        println!("[selftest-long-context] PASS compactions={compactions}");
        0
    } else {
        println!("[selftest-long-context] FAIL failures={failures} compactions={compactions}");
        1
    }
}

/// 上下文外部化自检（PLAN §6.4）：超大工具结果 → 内容寻址对象 + 两条事件留痕；
/// 模型历史只剩引用 + 预览；对象可无损取回；重复计划不重复外部化；回放一致。
pub(crate) fn selftest_spill() -> i32 {
    let path = PathBuf::from(".tmp/chat-spill.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let spill_dir = PathBuf::from(".tmp/chat-spill-objects");
    let _ = std::fs::remove_dir_all(&spill_dir);
    std::env::set_var("DSHR_SPILL_DIR", &spill_dir);
    // 窗口放大，避免压缩/丢弃与外部化互相干扰；阈值用默认 16 KiB。
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let big = format!("{}结尾唯一标记XYZ", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "sp-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "sp-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();

    let plan = state.plan_with_spill();
    let created = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();
    let pruned = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.result.pruned")
        .count();
    let refs = dshr_agent_context::spill::spill_refs_in_log(&state.log);
    let object = refs
        .first()
        .and_then(|spill| state.spill.get_verified(spill).ok());
    let lossless = object
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).contains("外部化内容"))
        .unwrap_or(false);
    // 模型看到的是 plan.messages（由 plan_context_with_spill 生成）；history_from_log
    // 不含外部化替换，不能用来断言"模型看到什么"。
    let history_text = format!("{:?}", plan.messages);
    let model_sees_reference = history_text.contains("工具结果已外部化");
    // 预览允许出现开头 512 字符；结尾标记只能由"完整内容泄漏"带进模型历史。
    let model_body_hidden = !history_text.contains("结尾唯一标记XYZ");

    // 重复计划：事件不重复，且第二次不再产生待外部化项。
    let plan_again = state.plan_with_spill();
    let created_again = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();

    // 回放一致：从 JSONL 重建后外部化已生效、模型历史条数一致。
    let rebuilt =
        SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay spill log");
    let policy = SpillPolicy::from_env();
    let replay_plan = plan_context_with_spill(&rebuilt, "", &policy);
    let live_plan = plan_context_with_spill(&state.log, "", &policy);
    let replay_consistent = replay_plan.spilled.is_empty()
        && replay_plan.messages.len() == live_plan.messages.len()
        && replay_plan.spilled_tool_results() == 0;

    let ok = created == 1
        && pruned == 1
        && refs.len() == 1
        && lossless
        && model_sees_reference
        && model_body_hidden
        && plan.spilled.is_empty()
        && plan_again.spilled.is_empty()
        && created_again == created
        && replay_consistent
        && plan.tokens_after < plan.tokens_before;
    println!(
        "[spill] created={created} pruned={pruned} objects={} lossless={lossless} \
         reference={model_sees_reference} hidden={model_body_hidden} \
         tokens={}->{} replay={replay_consistent} history_chars={}",
        refs.len(),
        plan.tokens_before,
        plan.tokens_after,
        history_text.chars().count(),
    );
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-spill] PASS");
        0
    } else {
        println!("[selftest-spill] FAIL");
        1
    }
}
