use crate::*;

pub(crate) fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    state.submit("你好");
    state.submit("用工具试一下"); // sidecar

    // 注册表里应当同时有 native 与 sidecar 工具。
    let both_registered = state.plugins.registry().get("probe_native").is_some()
        && state.plugins.registry().get("probe_echo").is_some();

    // 模拟 Sidecar 崩溃：整代撤销，Native 必须仍然可用。
    let killed = state.kill_sidecar().is_some();
    let sidecar_tools_gone = state.plugins.registry().get("probe_echo").is_none()
        && state.plugins.registry().get("probe_native").is_some();
    state.submit("用原生工具");

    // 重启 Sidecar：新 generation 重新注册，sidecar 工具恢复。
    let restarted = state.restart_sidecar();
    let sidecar_tools_back = state.plugins.registry().get("probe_echo").is_some();
    state.submit("再次用工具");

    let items = state.items();
    println!("[selftest] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    // 4 轮 = 4 用户 + 4 助手 + 3 工具卡片（sidecar、native、sidecar）
    let cards_ok = matches!(&items[3], Item::Tool(t)
            if t.presentation.as_deref() == Some("generic")
                && t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic
                    && !card.detail.is_empty()))
        && matches!(&items[6], Item::Tool(t)
            if t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic));
    let ok = items.len() == 11
        && both_registered
        && killed
        && sidecar_tools_gone
        && restarted
        && sidecar_tools_back
        && cards_ok
        && matches!(&items[3], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[6], Item::Tool(t) if t.origin == "native"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[9], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    // 重启：从磁盘重新加载，回放必须一致
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload saved session");
    let replayed = build_from_log(&reloaded);
    let same = replayed == items;
    println!(
        "[selftest] persisted={} replay_equal={same}",
        path.display()
    );

    // 切换时复用真实 Node generation；两个会话可各自调用同一个工具，并能回到旧会话继续。
    const RAW_ID: &str = "model-call-preserved";
    struct FixedToolIdProvider(bool);
    impl ModelProvider for FixedToolIdProvider {
        fn complete(&mut self, _: &CompletionRequest) -> Result<ModelReply, String> {
            self.0 = !self.0;
            if self.0 {
                Ok(ModelReply {
                    reasoning: String::new(),
                    text: String::new(),
                    tool_calls: vec![ModelToolCall {
                        id: RAW_ID.to_owned(),
                        name: "probe_echo".to_owned(),
                        arguments: json!({"text": "same model ID"}),
                    }],
                })
            } else {
                Ok(ModelReply::text_only("done"))
            }
        }
    }
    state.agent = Agent::new(Box::new(FixedToolIdProvider(false)));
    let generation = state.host.as_ref().map(|host| host.generation().to_owned());
    let timed_out = state.host.as_mut().is_some_and(|host| {
        matches!(
            host.invoke_as(RAW_ID, "probe_slow", json!({}), Duration::from_millis(50)),
            Err(dshr_compat_dsh_host::HostError::Timeout(_))
        )
    });
    let other_id = "s-selftest-other";
    let other_path = path.with_file_name(format!("{other_id}.jsonl"));
    reset_session_file(&other_path);
    let created = state.switch_session(other_path.clone(), other_id);
    state.submit("用工具试一下");
    let other_ok = matches!(state.items().as_slice(), [_, Item::Tool(t), _]
        if t.call_id == RAW_ID && t.origin == "sidecar" && t.state == ToolState::Settled { is_error: false });
    let started = std::time::Instant::now();
    let index_changed = state.switch_session(path, SESSION_ID);
    let switch_elapsed = started.elapsed();
    let restored = state.items() == items;
    state.submit("切回来再用工具");
    state.submit("下一轮复用同一个原始 ID");
    let resumed = matches!(state.items().get(items.len()..), Some([_, Item::Tool(a), _, _, Item::Tool(b), _])
        if [a, b].iter().all(|t| t.call_id == RAW_ID && t.state == ToolState::Settled { is_error: false }));
    let replayed_raw_ids = std::fs::read_to_string(&state.path).ok()
        .and_then(|text| SessionLog::from_jsonl(SESSION_ID, &text).ok())
        .is_some_and(|saved| {
            let history = history_from_log(&saved);
            history.iter().filter(|msg| matches!(msg, ChatMessage::Tool { call_id, .. } if call_id == RAW_ID)).count() == 2
                && history.iter().filter(|msg| matches!(msg, ChatMessage::Assistant { tool_calls, .. } if tool_calls.iter().any(|call| call.id == RAW_ID))).count() == 2
        });
    let reused = state
        .host
        .as_mut()
        .is_some_and(|host| !host.has_exited() && generation.as_deref() == Some(host.generation()));
    reset_session_file(&other_path);
    let workspace = tempfile::tempdir().expect("second probe workspace");
    let store = SessionIndex::new(workspace.path().join("sessions"));
    store
        .create_root(
            "s-other-root",
            &workspace.path().to_string_lossy(),
            &now_ts(),
        )
        .expect("create session in second workspace");
    let first_path = state.path.clone();
    state.switch_session(store.path_for("s-other-root"), "s-other-root");
    let used_second_runtime = state
        .host
        .as_ref()
        .is_some_and(|host| generation.as_deref() != Some(host.generation()));
    state.submit("用工具试一下");
    let second_completed = matches!(state.items().as_slice(), [_, Item::Tool(t), _]
        if t.origin == "sidecar" && t.state == ToolState::Settled { is_error: false });
    state.switch_session(first_path, SESSION_ID);
    let resumed_first_runtime = state
        .host
        .as_ref()
        .is_some_and(|host| generation.as_deref() == Some(host.generation()));
    let audited_binding = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.call" && event.payload["call_id"] == RAW_ID)
        .all(|event| {
            event.payload["generation"].as_str() == generation.as_deref()
                && event.payload["registration_id"].as_str().is_some()
        });
    let workspace_switch_ok = used_second_runtime && second_completed && resumed_first_runtime;
    println!(
        "[selftest] switch created={created} other_ok={other_ok} restored={restored} resumed={resumed} reused={reused} timed_out={timed_out} replayed_raw_ids={replayed_raw_ids} workspace_switch_ok={workspace_switch_ok} audited_binding={audited_binding} load={switch_elapsed:?}"
    );

    if ok
        && same
        && created
        && other_ok
        && !index_changed
        && restored
        && resumed
        && reused
        && timed_out
        && replayed_raw_ids
        && workspace_switch_ok
        && audited_binding
    {
        println!("[selftest] PASS");
        0
    } else {
        println!("[selftest] FAIL");
        1
    }
}

/// 单轮真实调用（需要 API key）；用于人工验证 provider 与真流式，不进 CI。
pub(crate) fn live_turn(text: &str) -> i32 {
    let path = PathBuf::from(".tmp/chat-live.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_with_settings(
        path,
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        true,
        true,
        DesktopSettings::from_env(&configured_models()),
    );

    let started = std::time::Instant::now();
    let mut updates = 0usize;
    let mut first_text = None;
    let reply = state.stream_turn(text, &mut |rows| {
        updates += 1;
        if first_text.is_none() && rows.last().is_some_and(is_streaming_row) {
            first_text = Some(started.elapsed());
        }
    });
    let total = started.elapsed();
    let Some(reply) = reply else {
        println!("[live] empty prompt");
        return 1;
    };
    println!(
        "[live] stream_updates={updates} first_text_ms={} total_ms={}",
        first_text.map_or_else(|| "n/a".to_owned(), |d| d.as_millis().to_string()),
        total.as_millis()
    );
    println!("[live] reply={reply}");
    0
}

/// 流式自检：mock provider 把回复切成多段，断言"快照是增量推送的、落库只有一条 settled"。
pub(crate) fn selftest_stream() -> i32 {
    let path = PathBuf::from(".tmp/chat-stream.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let mut snapshots: Vec<Vec<Row>> = Vec::new();
    let reply = state.stream_turn("你好", &mut |rows| snapshots.push(rows));
    let items = state.items();

    let streaming: Vec<&Row> = snapshots
        .iter()
        .filter_map(|rows| rows.last())
        .filter(|row| is_streaming_row(row))
        .collect();
    let final_row = snapshots.last().and_then(|rows| rows.last());
    println!(
        "[stream] snapshots={} streaming_rows={}",
        snapshots.len(),
        streaming.len()
    );
    for row in &streaming {
        println!("  {}", row.main_text());
    }

    let reply = reply.unwrap_or_default();
    let ok = reply.contains("收到：你好")
        && snapshots.len() >= 3
        && streaming.len() >= 2
        // 流式文本单调增长：每个快照都比上一个长。
        && streaming.windows(2).all(|pair| {
            streaming_text(pair[0]).chars().count() < streaming_text(pair[1]).chars().count()
        })
        && items.len() == 2
        && matches!(&items[0], Item::User { .. })
        && matches!(&items[1], Item::Assistant { text } if text == &reply)
        && final_row.is_some_and(|row| {
            matches!(row, Row::Assistant(text) if text.contains(&reply) && !text.contains('▌'))
        });

    if ok {
        println!("[selftest-stream] PASS");
        0
    } else {
        println!("[selftest-stream] FAIL");
        1
    }
}

/// 取消语义自检（P1-2）：turn 开始前置位 cancel → 必须走 TurnCancelled + session.interrupted，
/// 不出现“处理失败”，也不写空的 assistant 消息。
pub(crate) fn selftest_cancel() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_FORCE_MOCK", "1");
    let path = PathBuf::from(".tmp/chat-cancel.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path,
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    cancel.store(true, Ordering::Relaxed);
    let reply = state.stream_turn("你好", &mut |_| {});
    let interrupted = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");
    let failed = reply
        .as_deref()
        .map(|text| text.contains("处理失败"))
        .unwrap_or(false);
    let settled = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "message.assistant.settled");
    let tool_events = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type.starts_with("tool."))
        .count();
    println!(
        "[cancel] interrupted={interrupted} failed={failed} settled={settled} tool_events={tool_events}"
    );
    let ok = interrupted && !failed && !settled && tool_events == 0 && reply.as_deref() == Some("");
    if ok {
        println!("[selftest-cancel] PASS");
        0
    } else {
        println!("[selftest-cancel] FAIL");
        1
    }
}

/// 多轮真实对话（需要 API key）：验证"多轮记忆"而不是单轮应答。
/// 用法：`dshr --live-chat "第一句" "第二句" ...`（每个参数是一轮用户消息）
pub(crate) fn live_chat(script: &[String]) -> i32 {
    let path = PathBuf::from(".tmp/chat-live-chat.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_with_settings(
        path.clone(),
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        true,
        true,
        DesktopSettings::from_env(&configured_models()),
    );

    let mut failed = false;
    for (index, text) in script.iter().enumerate() {
        let started = std::time::Instant::now();
        let mut updates = 0usize;
        let reply = state
            .stream_turn(text, &mut |_| updates += 1)
            .unwrap_or_default();
        if reply.starts_with("处理失败") {
            failed = true;
        }
        println!(
            "[live-chat] turn={} ms={} stream_updates={updates} reply={reply}",
            index + 1,
            started.elapsed().as_millis()
        );
    }

    // 落盘回放：同一份日志重建出的历史必须包含所有轮次。
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload live chat log");
    let history = history_for_model(&reloaded);
    println!(
        "[live-chat] persisted_turns={} history_messages={}",
        script.len(),
        history.len()
    );
    if failed {
        1
    } else {
        0
    }
}

/// Gate 1 真实网络自检：真实 provider 连跑 20 轮，每轮要求模型调用一次本地工具。
///
/// 统计口径：以"每轮一次模型请求 + 至少一次真实工具调用"为一步；一步成功 =
/// 有回复、无失败标记、`tool.call`/`tool.result` 配对且无错误/中止。
/// 需要 `DEEPSEEK_API_KEY` 或 `OPENAI_API_KEY`；两者都没有时返回 2（SKIP，不伪装通过）。
pub(crate) fn selftest_live_20() -> i32 {
    if DeepSeekProvider::from_env().is_none() && OpenAiProvider::from_env().is_none() {
        println!("[selftest-live-20] SKIP: 没有 DEEPSEEK_API_KEY / OPENAI_API_KEY");
        return 2;
    }
    let path = PathBuf::from(".tmp/chat-live-20.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    // 这一步只统计"网络 + 模型 + 工具执行"链路；审批链由 --selftest-approval 覆盖。
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut state =
        ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    let count_errors = |state: &ChatState| {
        state
            .log
            .events()
            .iter()
            .filter(|event| {
                event.event_type == "tool.result"
                    && event
                        .payload
                        .get("is_error")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
            })
            .count()
    };

    let steps = 20usize;
    let mut ok_steps = 0usize;
    let mut failed_steps = 0usize;
    let mut total_calls = 0usize;
    let mut total_results = 0usize;
    let mut total_errors = 0usize;
    let mut latencies = Vec::new();

    for step in 0..steps {
        let prompt = format!(
            "第 {step} 步：只调用一次 probe_native 工具，参数 text 为 \"step-{step}\"，\
             不要调用其它工具；拿到工具结果后用一句中文确认。"
        );
        let before_calls = count(&state, "tool.call");
        let before_results = count(&state, "tool.result");
        let before_errors = count_errors(&state);
        let started = std::time::Instant::now();
        let reply = state.stream_turn(&prompt, &mut |_| {});
        let elapsed_ms = started.elapsed().as_millis() as u64;
        latencies.push(elapsed_ms);

        let calls = count(&state, "tool.call") - before_calls;
        let results = count(&state, "tool.result") - before_results;
        let errors = count_errors(&state) - before_errors;
        let aborted = count(&state, "tool.aborted");
        total_calls += calls;
        total_results += results;
        total_errors += errors;

        let reply_ok = reply
            .as_deref()
            .is_some_and(|text| !text.is_empty() && !text.starts_with("处理失败"));
        let step_ok = reply_ok && calls >= 1 && results >= 1 && errors == 0 && aborted == 0;
        if step_ok {
            ok_steps += 1;
        } else {
            failed_steps += 1;
        }
        println!(
            "[live-20] step={step} ok={step_ok} calls={calls} results={results} \
             errors={errors} ms={elapsed_ms} reply={}",
            reply.unwrap_or_default()
        );
    }

    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let rate = ok_steps as f64 / steps as f64;
    let provider = std::env::var("DSHR_PROVIDER").unwrap_or_else(|_| String::from("auto"));
    println!(
        "[live-20] provider={provider} steps={steps} ok={ok_steps} failed={failed_steps} \
         rate={rate:.4} tool_calls={total_calls} tool_results={total_results} \
         tool_errors={total_errors} p50_ms={p50} p95_ms={p95}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if rate >= 0.99 {
        println!("[selftest-live-20] PASS");
        0
    } else {
        println!("[selftest-live-20] FAIL");
        1
    }
}
