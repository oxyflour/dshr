#![cfg(test)]

use super::*;
use dshr_session_store::EventSpec;
use serde_json::json;

fn log_with(events: &[(&str, Value)]) -> SessionLog {
    let mut log = SessionLog::new("s-t");
    for (kind, payload) in events {
        log.append(EventSpec::durable(*kind, "t0", payload.clone()))
            .expect("append");
    }
    log
}

/// 断言每条 Tool 消息都能在前一条 assistant 的 tool_calls 里找到同一个 id。
fn assert_paired(messages: &[ChatMessage]) {
    for (index, message) in messages.iter().enumerate() {
        if let ChatMessage::Tool { call_id, .. } = message {
            let previous = &messages[index - 1];
            assert!(
                matches!(previous, ChatMessage::Assistant { tool_calls, .. }
                    if tool_calls.iter().any(|call| &call.id == call_id)),
                "位置 {index} 的 Tool({call_id}) 没有配对的 assistant.tool_calls"
            );
        }
    }
}

#[test]
fn model_names_select_context_window_defaults() {
    // DSH 官方 1M 档。
    for model in ["deepseek-flash", "deepseek-v4-pro", "deepseek-v4.1"] {
        assert_eq!(
            resolve_context_window(None, None, Some(model), None),
            DSH_OFFICIAL_CONTEXT_WINDOW,
            "{model} 应使用 DSH 官方 1M 窗口"
        );
    }
    // DeepSeek 现行 128K API 上限。
    for model in ["deepseek-chat", "deepseek-reasoner"] {
        assert_eq!(
            resolve_context_window(None, None, Some(model), None),
            DEEPSEEK_API_CONTEXT_WINDOW,
            "{model} 应使用 128K"
        );
    }
    // OpenAI 128K 档。
    for model in ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o3", "o4-mini"] {
        assert_eq!(
            resolve_context_window(None, None, None, Some(model)),
            OPENAI_CONTEXT_WINDOW,
            "{model} 应使用 128K"
        );
    }
    // 未知 / 未配置 → 默认 128K 兜底（两家默认模型都是 128K）。
    assert_eq!(
        resolve_context_window(None, None, Some("some-unknown-model"), None),
        DEFAULT_CONTEXT_WINDOW
    );
    assert_eq!(
        resolve_context_window(None, None, None, None),
        DEFAULT_CONTEXT_WINDOW
    );
    assert_eq!(
        resolve_context_window(None, Some("  "), Some(""), None),
        DEFAULT_CONTEXT_WINDOW
    );
}

#[test]
fn generic_model_env_wins_over_provider_specific() {
    assert_eq!(
        resolve_context_window(
            None,
            Some("deepseek-flash"),
            Some("deepseek-chat"),
            Some("gpt-4o"),
        ),
        DSH_OFFICIAL_CONTEXT_WINDOW,
        "DSHR_MODEL 应优先于 DEEPSEEK_MODEL / OPENAI_MODEL"
    );
    assert_eq!(
        resolve_context_window(None, None, Some("deepseek-chat"), Some("gpt-4o")),
        DEEPSEEK_API_CONTEXT_WINDOW,
        "没有 DSHR_MODEL 时 DeepSeek 优先"
    );
}

#[test]
fn explicit_dshr_context_window_wins_and_invalid_falls_through() {
    assert_eq!(
        resolve_context_window(Some("8000"), Some("deepseek-flash"), None, None),
        8_000,
        "Gate 3 自检显式 DSHR_CONTEXT_WINDOW=8000 必须原样生效"
    );
    // < 4000 视为无效（沿用既有语义），回退到模型档位。
    assert_eq!(
        resolve_context_window(Some("100"), Some("deepseek-flash"), None, None),
        DSH_OFFICIAL_CONTEXT_WINDOW
    );
    assert_eq!(
        resolve_context_window(Some("not-a-number"), None, Some("deepseek-chat"), None),
        DEEPSEEK_API_CONTEXT_WINDOW
    );
}

#[test]
fn context_window_from_env_reads_the_four_variables() {
    // 本 crate 只有这个测试读写这些变量，并行安全；结束后恢复。
    const KEYS: [&str; 4] = [
        "DSHR_CONTEXT_WINDOW",
        "DSHR_MODEL",
        "DEEPSEEK_MODEL",
        "OPENAI_MODEL",
    ];
    let previous: Vec<Option<String>> = KEYS.iter().map(|key| std::env::var(key).ok()).collect();

    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    std::env::set_var("DEEPSEEK_MODEL", "deepseek-flash");
    assert_eq!(context_window_from_env(), 8_000, "显式窗口优先");

    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    assert_eq!(context_window_from_env(), DSH_OFFICIAL_CONTEXT_WINDOW);

    std::env::set_var("DEEPSEEK_MODEL", "deepseek-chat");
    assert_eq!(context_window_from_env(), DEEPSEEK_API_CONTEXT_WINDOW);

    for (key, value) in KEYS.iter().zip(previous) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

#[test]
fn demo_session_rebuilds_a_protocol_legal_history() {
    let log = SessionLog::from_jsonl("s-demo", dshr_conversation_model_session()).expect("demo");
    let messages = history_from_log(&log);
    assert_paired(&messages);
    // demo 里 seq 1..3 已被 context.compacted 压缩，摘要消息缺失（fixture 只带引用），
    // 因此先看到工具轮次，中间是"摘要缺失"的 System，最后是最终回复。
    assert_paired(&messages);
    assert!(
        messages.iter().any(|message| matches!(
            message,
            ChatMessage::Assistant { tool_calls, .. } if tool_calls.len() == 1
        )),
        "messages={messages:?}"
    );
    assert!(messages
        .iter()
        .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要缺失"))));
    assert!(messages.iter().any(|message| matches!(
        message,
        ChatMessage::Assistant { text, .. } if text.contains("Rust 工具成功")
    )));
}

// 让测试不依赖 conversation-model（避免循环依赖）：直接读同一份 fixture 路径。
fn dshr_conversation_model_session() -> &'static str {
    include_str!("../../conversation-model/src/demo_session.jsonl")
}

#[test]
fn tool_result_is_paired_and_carries_content() {
    let log = log_with(&[
        (
            "message.user",
            json!({ "message_id": "m1", "text": "跑一下" }),
        ),
        (
            "tool.call",
            json!({ "call_id": "c1", "tool": "bash", "arguments": { "command": "ls" } }),
        ),
        (
            "tool.result",
            json!({
                "call_id": "c1", "is_error": false,
                "content": [{ "type": "text", "text": "main.rs" }]
            }),
        ),
        (
            "message.assistant.settled",
            json!({ "message_id": "m2", "text": "目录里只有 main.rs" }),
        ),
    ]);
    let messages = history_from_log(&log);
    assert_eq!(
        messages,
        vec![
            ChatMessage::user("跑一下"),
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: "c1".to_owned(),
                    name: "bash".to_owned(),
                    arguments: json!({ "command": "ls" }),
                }],
            },
            ChatMessage::Tool {
                call_id: "c1".to_owned(),
                content: "main.rs".to_owned()
            },
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: "目录里只有 main.rs".to_owned(),
                tool_calls: Vec::new()
            },
        ]
    );
    assert_paired(&messages);
}

#[test]
fn aborted_and_error_results_are_marked_for_the_model() {
    let log = log_with(&[
        (
            "message.user",
            json!({ "message_id": "m1", "text": "试试" }),
        ),
        (
            "tool.call",
            json!({ "call_id": "c1", "tool": "a", "arguments": {} }),
        ),
        (
            "tool.call",
            json!({ "call_id": "c2", "tool": "b", "arguments": {} }),
        ),
        (
            "tool.aborted",
            json!({ "call_id": "c1", "reason": "crash" }),
        ),
        (
            "tool.aborted",
            json!({ "call_id": "c2", "reason": "timeout" }),
        ),
    ]);
    let messages = history_from_log(&log);
    let tools: Vec<&ChatMessage> = messages
        .iter()
        .filter(|message| matches!(message, ChatMessage::Tool { .. }))
        .collect();
    assert!(
        matches!(tools[0], ChatMessage::Tool { content, .. } if content.starts_with("ERROR: "))
    );
    assert!(matches!(tools[1], ChatMessage::Tool { content, .. } if content.contains("timeout")));
}

#[test]
fn result_without_call_is_skipped() {
    let log = log_with(&[(
        "tool.result",
        json!({ "call_id": "ghost", "is_error": false, "content": [] }),
    )]);
    assert!(history_from_log(&log).is_empty());
}

#[test]
fn trim_never_starts_with_a_tool_message() {
    let mut messages = vec![
        ChatMessage::user("u0"),
        ChatMessage::Assistant {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: "c1".to_owned(),
                name: "a".to_owned(),
                arguments: Value::Null,
            }],
        },
        ChatMessage::Tool {
            call_id: "c1".to_owned(),
            content: "ok".to_owned(),
        },
        ChatMessage::user("u1"),
    ];
    // 保留 2 条时理论起点正好落在 Tool 上：必须往后挪，从 user("u1") 开始。
    let trimmed = trim_history(messages.clone(), 2);
    assert_eq!(trimmed, vec![ChatMessage::user("u1")]);

    messages.clear();
    assert!(trim_history(messages, 2).is_empty());
    assert!(trim_history(vec![ChatMessage::user("a")], 0).is_empty());
}

#[test]
fn history_for_model_caps_at_default_limit() {
    let mut log = SessionLog::new("s-t");
    for index in 0..DEFAULT_MAX_MESSAGES + 10 {
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("m{index}"), "text": format!("msg-{index}") }),
        ))
        .expect("append");
    }
    let messages = history_for_model(&log);
    assert_eq!(messages.len(), DEFAULT_MAX_MESSAGES);
    assert!(matches!(&messages[0], ChatMessage::User(text) if text == "msg-10"));
}

#[test]
fn empty_assistant_settlement_is_skipped() {
    let log = log_with(&[
        ("message.user", json!({ "message_id": "m1", "text": "hi" })),
        (
            "message.assistant.settled",
            json!({ "message_id": "m2", "text": "" }),
        ),
    ]);
    assert_eq!(history_from_log(&log), vec![ChatMessage::user("hi")]);
}

#[test]
fn token_estimate_counts_cjk_per_char() {
    let cjk = estimate_tokens("窗口尺寸问题");
    let ascii = estimate_tokens("window size issue");
    assert!(cjk > ascii, "cjk={cjk} ascii={ascii}");
    assert!(cjk >= 6, "cjk={cjk}");
    assert!(estimate_tokens("") > 0);
}

#[test]
fn plan_under_budget_keeps_everything() {
    let log = log_with(&[
        (
            "message.user",
            json!({ "message_id": "m1", "text": "你好" }),
        ),
        (
            "message.assistant.settled",
            json!({ "message_id": "m2", "text": "你好，有什么可以帮你？" }),
        ),
    ]);
    let plan = plan_with_window(&log, "继续", 64_000);
    assert!(!plan.triggered());
    assert_eq!(plan.messages, history_from_log(&log));
    assert_eq!(plan.pruned_tool_results, 0);
    assert_eq!(plan.dropped_messages, 0);
}

#[test]
fn long_tool_result_is_pruned_head_and_tail() {
    let long = "A".repeat(8_000);
    let log = log_with(&[
        (
            "message.user",
            json!({ "message_id": "m1", "text": "跑一下" }),
        ),
        (
            "tool.call",
            json!({ "call_id": "c1", "tool": "bash", "arguments": {} }),
        ),
        (
            "tool.result",
            json!({
                "call_id": "c1", "is_error": false,
                "content": [{ "type": "text", "text": long }]
            }),
        ),
        (
            "message.assistant.settled",
            json!({ "message_id": "m2", "text": "完成" }),
        ),
    ]);
    let plan = plan_with_window(&log, "", 2_000);
    assert_eq!(plan.pruned_tool_results, 1);
    let ChatMessage::Tool { content, .. } = plan
        .messages
        .iter()
        .find(|message| matches!(message, ChatMessage::Tool { .. }))
        .expect("tool message kept")
    else {
        unreachable!()
    };
    assert!(content.contains("已裁剪"), "{content}");
    assert!(content.starts_with('A'));
    assert!(content.ends_with('A'));
    assert!(plan.tokens_after < plan.tokens_before);
    assert!(plan.tokens_after <= 2_000);
}

#[test]
fn over_budget_drops_oldest_and_keeps_protocol_legal() {
    let mut log = SessionLog::new("s-t");
    for index in 0..40 {
        let user = format!("很长的用户消息 {index}：{}", "内容".repeat(80));
        let assistant = format!("很长的助手回复 {index}：{}", "回答".repeat(80));
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": user }),
        ))
        .expect("append user");
        if index % 5 == 0 {
            let call_id = format!("c{index}");
            log.append(EventSpec::durable(
                "tool.call",
                "t0",
                json!({ "call_id": call_id, "tool": "bash", "arguments": {} }),
            ))
            .expect("append call");
            log.append(EventSpec::durable(
                "tool.result",
                "t0",
                json!({
                    "call_id": call_id, "is_error": false,
                    "content": [{ "type": "text", "text": "输出".repeat(500) }]
                }),
            ))
            .expect("append result");
        }
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": assistant }),
        ))
        .expect("append assistant");
    }

    let window = 8_000;
    let plan = plan_with_window(&log, "继续", window);
    assert!(plan.triggered(), "{plan:?}");
    assert!(plan.dropped_messages > 0, "{plan:?}");
    assert!(!matches!(
        plan.messages.first(),
        Some(ChatMessage::Tool { .. })
    ));
    assert!(
        matches!(plan.messages.first(), Some(ChatMessage::User(_))),
        "历史应从 user 开始: {:?}",
        plan.messages.first()
    );
    assert_paired(&plan.messages);
    assert!(
        (plan.tokens_after as f64) <= (window as f64 * 0.8),
        "tokens_after={} window={window}",
        plan.tokens_after
    );
    // 被丢掉的旧消息仍在日志里，只是不进模型历史。
    assert!(plan.dropped_messages < history_from_log(&log).len());
}

#[test]
fn plan_never_returns_a_dangling_tool_first() {
    let mut log = SessionLog::new("s-t");
    for index in 0..30 {
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": "长".repeat(200) }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": format!("c{index}"), "tool": "bash", "arguments": {} }),
        ))
        .expect("append call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": format!("c{index}"), "is_error": false,
                "content": [{ "type": "text", "text": "长".repeat(200) }]
            }),
        ))
        .expect("append result");
    }
    let plan = plan_with_window(&log, "继续", 3_000);
    assert!(!matches!(
        plan.messages.first(),
        Some(ChatMessage::Tool { .. })
    ));
    assert_paired(&plan.messages);
}

#[test]
fn compaction_event_rebuilds_system_summary() {
    let mut log = SessionLog::new("s-t");
    log.append(EventSpec::durable(
        "message.user",
        "t0",
        json!({ "message_id": "u-old", "text": "旧问题" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "message.assistant.settled",
        "t0",
        json!({ "message_id": "a-old", "text": "旧回答" }),
    ))
    .unwrap();
    // 摘要消息（内容要被隐藏，只通过 compacted 引用还原）
    log.append(EventSpec::durable(
        "message.assistant.settled",
        "t0",
        json!({ "message_id": "m-sum1", "text": "用户问了旧问题，助手回答了旧回答。" }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "context.compacted",
        "t0",
        json!({
            "from_seq": 1, "to_seq": 2,
            "summary_message_id": "m-sum1",
            "tokens_before": 100, "tokens_after": 40
        }),
    ))
    .unwrap();
    log.append(EventSpec::durable(
        "message.user",
        "t0",
        json!({ "message_id": "u-new", "text": "新问题" }),
    ))
    .unwrap();

    let history = history_from_log(&log);
    assert_eq!(
        history,
        vec![
            ChatMessage::System(
                "以下是早前对话的摘要：\n用户问了旧问题，助手回答了旧回答。".to_owned()
            ),
            ChatMessage::user("新问题"),
        ]
    );
    // 压缩后历史很小，不应再次触发裁剪。
    let plan = plan_with_window(&log, "", 64_000);
    assert!(!plan.triggered());
    assert_eq!(plan.messages.len(), 2);
}

#[test]
fn plan_reports_dropped_range_for_compaction() {
    let mut log = SessionLog::new("s-t");
    for index in 0..20 {
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": "长".repeat(300) }),
        ))
        .unwrap();
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": "答".repeat(300) }),
        ))
        .unwrap();
    }
    let plan = plan_with_window(&log, "继续", 4_000);
    assert!(plan.dropped_messages > 0, "{plan:?}");
    let (from, to) = plan.dropped_range.expect("dropped_range");
    assert_eq!(from, 1);
    assert!(to < 40, "to={to}");
    assert_eq!(plan.dropped.len(), plan.dropped_messages);
    assert!(plan.dropped_range.is_some());
}

// ------------------------------------------------------------------
// M3-DEFAULT-040..045：压缩保留比对齐 DSH retainRatio=0.16
// ------------------------------------------------------------------

fn long_log(rounds: usize) -> SessionLog {
    let mut log = SessionLog::new("s-retain");
    for index in 0..rounds {
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": "长用户消息".repeat(120) }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": "长助手回复".repeat(120) }),
        ))
        .expect("append assistant");
    }
    log
}

#[test]
fn m3_default_040_retain_ratio_defaults_to_dsh_016() {
    assert_eq!(DSH_RETAIN_RATIO, 0.16, "DSH DEFAULT_RETAIN_RATIO = .16");
    assert_eq!(TRIGGER_RATIO, 0.8, "DSH thresholdRatio = .8");
    assert_eq!(resolve_retain_ratio(None), 0.16);
    assert_eq!(TARGET_RATIO, 0.6, "旧行为仅作 env 回退值保留");
}

#[test]
fn m3_default_041_retain_ratio_env_parse_and_clamp() {
    assert_eq!(resolve_retain_ratio(Some("0.6")), 0.6);
    assert_eq!(resolve_retain_ratio(Some(" 0.16 ")), 0.16);
    assert_eq!(resolve_retain_ratio(Some("garbage")), 0.16);
    assert_eq!(resolve_retain_ratio(Some("2.0")), 0.16, "越界回退默认");
    assert_eq!(resolve_retain_ratio(Some("0.01")), 0.16);
    assert_eq!(resolve_retain_ratio(Some("1.0")), 1.0);
    assert_eq!(resolve_retain_ratio(Some("0.05")), 0.05);
    assert_eq!(RETAIN_RATIO_ENV, "DSHR_CONTEXT_RETAIN_RATIO");
}

#[test]
fn m3_default_042_compaction_targets_16_percent_of_window() {
    let log = long_log(40);
    let window = 8_000;
    let plan = plan_with_window(&log, "继续", window);
    assert!(plan.triggered(), "{plan:?}");
    assert!(plan.dropped_messages > 0, "{plan:?}");
    let target = (window as f32 * DSH_RETAIN_RATIO) as usize;
    assert!(
        plan.tokens_after <= target,
        "压缩后 tokens={} 应 <= 16% 窗口 {target}",
        plan.tokens_after
    );
    assert!(
        (plan.tokens_after as f64) <= (window as f64 * 0.16) + 1.0,
        "保留比应接近 16%: tokens_after={}",
        plan.tokens_after
    );
}

#[test]
fn m3_default_043_legacy_ratio_keeps_more_messages() {
    let log = long_log(40);
    let window = 8_000;
    let dsh = plan_with_window_and_retain_ratio(&log, "继续", window, DSH_RETAIN_RATIO);
    let legacy = plan_with_window_and_retain_ratio(&log, "继续", window, TARGET_RATIO);
    assert!(dsh.triggered() && legacy.triggered());
    assert!(
        dsh.messages.len() <= legacy.messages.len(),
        "0.16 保留的消息应 <= 0.6: {} vs {}",
        dsh.messages.len(),
        legacy.messages.len()
    );
    assert!(
        dsh.tokens_after <= legacy.tokens_after,
        "0.16 tokens_after 应更小: {} vs {}",
        dsh.tokens_after,
        legacy.tokens_after
    );
}

#[test]
fn m3_default_044_dsh_ratio_still_produces_legal_history() {
    let log = long_log(40);
    let plan = plan_with_window_and_retain_ratio(&log, "继续", 8_000, DSH_RETAIN_RATIO);
    assert!(!matches!(
        plan.messages.first(),
        Some(ChatMessage::Tool { .. })
    ));
    assert_paired(&plan.messages);
    assert_eq!(plan.dropped.len(), plan.dropped_messages);
    assert!(plan.dropped_range.is_some());
}

#[test]
fn m3_default_045_retain_ratio_out_of_range_falls_back() {
    let log = long_log(20);
    let weird = plan_with_window_and_retain_ratio(&log, "继续", 8_000, 9.5);
    let default = plan_with_window_and_retain_ratio(&log, "继续", 8_000, DSH_RETAIN_RATIO);
    assert_eq!(weird.messages, default.messages);
    assert_eq!(weird.tokens_after, default.tokens_after);
}
