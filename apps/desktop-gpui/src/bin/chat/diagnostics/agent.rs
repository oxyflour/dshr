use crate::*;

/// 自检用的"重复同一工具调用" provider：前 3 步都发同一个工具+参数，第 4 步收尾。
///
/// 只用于 `--selftest-defaults` 验证 `agent-core` 的重复工具软提醒阈值（3/5/8）。
struct RepeatProbeProvider;

impl ModelProvider for RepeatProbeProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        let executed = request
            .messages
            .iter()
            .filter(|message| matches!(message, ChatMessage::Tool { .. }))
            .count();
        if executed >= 3 {
            return Ok(ModelReply::text_only("done"));
        }
        Ok(ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: format!("repeat-{executed}"),
                name: "probe_repeat".to_owned(),
                arguments: json!({ "text": "same" }),
            }],
        })
    }
}

/// 自检用的执行器：无论调用什么都返回成功。
struct ProbeRepeatExecutor;

impl ToolExecutor for ProbeRepeatExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "probe_repeat".to_owned(),
            description: "repeat probe".to_owned(),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::success(
            json!([{ "type": "json", "value": { "ok": true } }]),
            json!({ "ok": true }),
        )
    }
}

/// `--selftest-defaults`：M3 默认差异接线的无网络自检。
///
/// 覆盖：approval 三种权限模式（`DSHR_PERMISSION_MODE`）、重复工具软提醒在真实 agent
/// 序列里触发并经 `tool.repeat` 落 durable 日志、sidecar per-tool timeout 往返、
/// retainRatio 与 reasoning effort 的 env 解析。全部断言走纯函数/内存日志，不联网。
pub(crate) fn selftest_defaults() -> i32 {
    let mut ok = true;

    // 1) approval 模式：env → ApprovalPolicy（缺省 workspace-write = 原 desktop 行为）。
    let write_tool = ToolDefinition::new(
        "write",
        "写文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("diff");
    let read_tool = ToolDefinition::new(
        "read",
        "读文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("read");

    std::env::remove_var(PermissionMode::ENV);
    let default_mode_ok = PermissionMode::from_env() == PermissionMode::WorkspaceWrite;
    let default_policy_ok = matches!(
        ApprovalPolicy::from_env().check(&write_tool, &json!({})),
        PolicyDecision::Ask
    ) && matches!(
        ApprovalPolicy::from_env().check(&read_tool, &json!({})),
        PolicyDecision::Allow
    );

    std::env::set_var(PermissionMode::ENV, "read-only");
    let read_only_mode = PermissionMode::from_env();
    let read_only_policy = ApprovalPolicy::from_env();
    let read_only_ok = read_only_mode == PermissionMode::ReadOnly
        && matches!(
            read_only_policy.check(&write_tool, &json!({})),
            PolicyDecision::Deny(_)
        )
        && matches!(
            read_only_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );

    std::env::set_var(PermissionMode::ENV, "danger-full-access");
    let danger_mode = PermissionMode::from_env();
    let danger_policy = ApprovalPolicy::from_env();
    let danger_ok = danger_mode == PermissionMode::DangerFullAccess
        && matches!(
            danger_policy.check(&write_tool, &json!({})),
            PolicyDecision::Allow
        )
        && matches!(
            danger_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );
    std::env::remove_var(PermissionMode::ENV);
    ok &= default_mode_ok && default_policy_ok && read_only_ok && danger_ok;

    // 2) 重复工具软提醒：默认阈值 3 在真实 agent 序列里触发，并落成 `tool.repeat`。
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_CONFIG_ENV);
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_THRESHOLDS_ENV);
    let mut agent = Agent::new(RepeatProbeProvider);
    let default_thresholds_ok = agent.repeat_reminder_config().enabled
        && agent.repeat_reminder_config().thresholds == vec![3, 5, 8];
    let mut executor = ProbeRepeatExecutor;
    let events = agent.run_turn("repeat", &mut executor);
    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    let mut log = SessionLog::new("s-selftest-defaults");
    let ts = now_ts();
    let _ = log.append(EventSpec::durable(
        "session.created",
        &ts,
        SessionCreated::root(".").payload(),
    ));
    let written = append_repeat_reminders(&mut agent, &mut log, &ts);
    let reminder = log
        .events()
        .iter()
        .find(|event| event.event_type == "tool.repeat");
    let repeat_event_ok = reminder.is_some_and(|event| {
        event.payload["tool"] == json!("probe_repeat")
            && event.payload["count"] == json!(3)
            && event.payload["threshold"] == json!(3)
            && event.payload["message"]
                .as_str()
                .is_some_and(|text| text.contains("重复工具调用提醒"))
    });
    let repeat_ok = default_thresholds_ok && tool_calls == 3 && written == 1 && repeat_event_ok;
    ok &= repeat_ok;

    // 3) sidecar per-tool timeout：`tool.register.timeout_ms` 往返 + 归一化。
    let registration = parse_tool_registration(
        &json!({"name": "bash", "description": "run", "timeout_ms": 5_000}),
        "dsh-official",
        "reg-selftest-1",
    );
    let definition = registration.to_tool_definition("gen-selftest");
    let timeout_ok = registration.timeout_ms == Some(5_000)
        && definition.timeout_ms == 5_000
        && definition.effective_timeout_ms() == 5_000
        && definition.origin.generation() == Some("gen-selftest")
        && parse_tool_registration(
            &json!({"name": "slow", "timeout_ms": 50_000_000u64}),
            "p",
            "r",
        )
        .timeout_ms
            == Some(600_000)
        && parse_tool_registration(&json!({"name": "read"}), "p", "r")
            .timeout_ms
            .is_none();
    ok &= timeout_ok;

    // 4) retainRatio：默认 DSH 0.16，env 可回退旧行为 0.6。
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    let retain_default = dshr_agent_context::retain_ratio_from_env();
    let retain_default_ok = (retain_default - dshr_agent_context::DSH_RETAIN_RATIO).abs() < 1e-6;
    std::env::set_var(dshr_agent_context::RETAIN_RATIO_ENV, "0.6");
    let retain_override = dshr_agent_context::retain_ratio_from_env();
    let retain_override_ok = (retain_override - 0.6).abs() < 1e-6;
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    ok &= retain_default_ok && retain_override_ok;

    // 5) reasoning effort：provider 构造时读 env，chat 不覆盖。
    std::env::remove_var(ReasoningEffort::ENV);
    let reasoning_unset_ok = DeepSeekProvider::new("selftest-key")
        .reasoning_effort()
        .is_none();
    std::env::set_var(ReasoningEffort::ENV, "high");
    let reasoning_high_ok = DeepSeekProvider::new("selftest-key").reasoning_effort()
        == Some(ReasoningEffort::High)
        && OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::High);
    std::env::set_var(ReasoningEffort::ENV, "max");
    let reasoning_max_ok =
        OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::Max);
    std::env::remove_var(ReasoningEffort::ENV);
    ok &= reasoning_unset_ok && reasoning_high_ok && reasoning_max_ok;

    println!(
        "[selftest-defaults] approval_default={default_policy_ok} approval_read_only={read_only_ok} \
         approval_danger={danger_ok} repeat_reminders={tool_calls} tool_repeat_events={written} \
         repeat_event_ok={repeat_event_ok} timeout_roundtrip={timeout_ok} \
         retain_default={retain_default:.2} retain_env={retain_override:.2} \
         reasoning_env={reasoning_high_ok}"
    );
    if ok {
        println!("[selftest-defaults] PASS");
        0
    } else {
        println!("[selftest-defaults] FAIL");
        1
    }
}

/// 重试/故障注入自检：每第 3 次请求返回 503，20 轮工具调用全部应成功。
struct FlakyProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail_every: usize,
}

impl ModelProvider for FlakyProvider {
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<dshr_agent_core::ModelReply, String> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if self.fail_every > 0 && call.is_multiple_of(self.fail_every) {
            return Err(String::from("HTTP 503 temporarily unavailable"));
        }
        // 第二轮（已看到工具结果）返回文本收尾；第一轮返回一次工具调用。
        let saw_tool_result = matches!(request.messages.last(), Some(ChatMessage::Tool { .. }));
        if saw_tool_result {
            Ok(dshr_agent_core::ModelReply {
                reasoning: String::new(),
                text: format!("ok-{call}"),
                tool_calls: Vec::new(),
            })
        } else {
            Ok(dshr_agent_core::ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![dshr_agent_core::ModelToolCall {
                    id: format!("call-{call}"),
                    name: String::from("probe_native"),
                    arguments: json!({ "text": "ping" }),
                }],
            })
        }
    }
}

struct CountingExecutor {
    executed: usize,
}

impl ToolExecutor for CountingExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: String::from("probe_native"),
            description: String::from("自检用"),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        self.executed += 1;
        ToolOutcome::success(
            json!([{ "type": "text", "text": "ok" }]),
            json!({ "ok": true }),
        )
    }
}

pub(crate) fn selftest_retry() -> i32 {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flaky = FlakyProvider {
        calls: calls.clone(),
        fail_every: 3,
    };
    let mut agent = Agent::new(RetryingProvider::new(
        Box::new(flaky),
        RetryPolicy::immediate(4),
    ));
    let mut executor = CountingExecutor { executed: 0 };
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for _ in 0..20 {
        let events = agent.run_turn("继续", &mut executor);
        if events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed(_)))
        {
            failed += 1;
        } else {
            succeeded += 1;
        }
    }
    let total_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "[retry] turns=20 succeeded={succeeded} failed={failed} tool_executions={} \
         provider_calls={total_calls} (fault every 3rd call, max attempts 4)",
        executor.executed
    );
    let ok = failed == 0 && succeeded == 20 && executor.executed == 20 && total_calls > 40;
    if ok {
        println!("[selftest-retry] PASS");
        0
    } else {
        println!("[selftest-retry] FAIL");
        1
    }
}
