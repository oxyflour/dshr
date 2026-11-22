#![cfg(test)]

use super::*;
use std::sync::{Arc, Mutex};

struct EchoExecutor {
    calls: Vec<ToolCallRequest>,
}

impl EchoExecutor {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }
}

impl ToolExecutor for EchoExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "probe_native".to_owned(),
                description: "native".to_owned(),
                parameters: json!({ "type": "object" }),
            },
            ToolSpec {
                name: "probe_echo".to_owned(),
                description: "sidecar".to_owned(),
                parameters: json!({ "type": "object" }),
            },
        ]
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.calls.push(call.clone());
        ToolOutcome::success(
            json!([{ "type": "json", "value": call.arguments }]),
            json!({ "echo": call.arguments }),
        )
    }
}

#[test]
fn plain_turn_produces_deltas_and_final_text() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("你好", &mut executor);

    assert!(matches!(events[0], AgentEvent::TurnStarted { turn_id: 1 }));
    assert!(matches!(events[1], AgentEvent::StepStarted { step: 1, .. }));
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnEnded { .. }
    ));
    assert!(executor.calls.is_empty());

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta(delta) => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains("收到：你好"));
}

#[test]
fn tool_turn_calls_executor_then_answers() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("用工具试一下", &mut executor);

    assert_eq!(executor.calls.len(), 1);
    assert_eq!(executor.calls[0].tool, "probe_echo");
    assert_eq!(executor.calls[0].arguments["text"], "用工具试一下");

    let tool_call_pos = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolCall(_)))
        .expect("tool call event");
    let tool_result_pos = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
        .expect("tool result event");
    assert!(tool_call_pos < tool_result_pos);

    // 工具之后必须再来一个 step 才能给最终回答
    let steps = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::StepStarted { .. }))
        .count();
    assert_eq!(steps, 2, "调用工具后应当有第二步");

    let turn_text = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::TurnEnded { text } => Some(text.clone()),
            _ => None,
        })
        .expect("turn ended");
    assert!(turn_text.contains("工具返回"));
}

#[test]
fn native_keyword_selects_native_tool() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    agent.run_turn("用原生工具", &mut executor);
    assert_eq!(executor.calls[0].tool, "probe_native");
}

/// 每次回复都发工具调用（可限制次数），用来验证预算语义。
struct LoopProvider {
    /// `None` = 永远发工具调用；`Some(n)` = 前 n 次回复发工具调用，之后回纯文本。
    tool_steps: Option<usize>,
    /// 每次回复里发几个工具调用。
    per_reply: usize,
    invocations: Arc<Mutex<usize>>,
}

impl ModelProvider for LoopProvider {
    fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
        let index = {
            let mut invocations = self
                .invocations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let index = *invocations;
            *invocations += 1;
            index
        };
        let emit_tools = self.tool_steps.is_none_or(|steps| index < steps);
        if !emit_tools {
            return Ok(ModelReply::text_only("done"));
        }
        Ok(ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: (0..self.per_reply)
                .map(|call| ModelToolCall {
                    id: format!("call-{index}-{call}"),
                    name: "probe_native".to_owned(),
                    arguments: json!({}),
                })
                .collect(),
        })
    }
}

fn loop_provider(tool_steps: Option<usize>, per_reply: usize) -> LoopProvider {
    LoopProvider {
        tool_steps,
        per_reply,
        invocations: Arc::new(Mutex::new(0)),
    }
}

fn tool_call_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCall(call) => Some(call.call_id.clone()),
            _ => None,
        })
        .collect()
}

fn tool_results(events: &[AgentEvent]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { call_id, outcome } => {
                Some((call_id.clone(), outcome.is_error))
            }
            _ => None,
        })
        .collect()
}

fn step_count(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, AgentEvent::StepStarted { .. }))
        .count()
}

fn has_turn_failed(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnFailed(_)))
}

fn has_turn_cancelled(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnCancelled { .. }))
}

fn tool_cancelled_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCancelled { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

/// 显式预算：命中后**降级**而不是整轮失败（PLAN §6.4 第 3 条）。
#[test]
fn tool_budget_is_enforced() {
    let mut agent = Agent::with_budget(
        loop_provider(None, 1),
        ToolBudget {
            max_tool_calls: Some(2),
            max_steps: None,
        },
    );
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("loop", &mut executor);

    assert!(
        !has_turn_failed(&events),
        "预算耗尽必须是降级，不能 TurnFailed: {events:?}"
    );
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnEnded { .. }
    ));
    assert_eq!(executor.calls.len(), 2, "预算内只执行 2 次");

    // 事件完整：第 3 次调用也要发 ToolCall + 配对的失败 ToolResult。
    let calls = tool_call_ids(&events);
    let results = tool_results(&events);
    assert_eq!(calls.len(), 3, "超预算的调用仍要发 ToolCall: {events:?}");
    assert_eq!(results.len(), 3, "每个 ToolCall 必须有配对结果");
    for (index, call_id) in calls.iter().enumerate() {
        assert_eq!(&results[index].0, call_id, "结果顺序必须与调用一致");
    }
    assert!(!results[0].1 && !results[1].1, "预算内调用应成功");
    assert!(results[2].1, "超出预算的调用必须 is_error");
}

/// 默认预算（未设置 env）与 DSH 一致：20 次循环工具调用全部执行，不失败。
#[test]
fn default_budget_does_not_cap_tool_calls() {
    let mut agent = Agent::with_budget(loop_provider(Some(20), 1), ToolBudget::default());
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("loop", &mut executor);

    assert!(!has_turn_failed(&events), "{events:?}");
    assert_eq!(executor.calls.len(), 20, "默认预算下 20 次调用必须全部执行");
    assert_eq!(tool_results(&events).len(), 20);
    assert_eq!(step_count(&events), 21, "20 个工具 step + 1 个收尾 step");
    assert_eq!(turn_text(&events), "done");
}

/// 用户现场场景：一个 step 里 10 个工具调用（原实现第 9 个就 TurnFailed）。
#[test]
fn many_tool_calls_in_one_turn_are_not_capped() {
    let mut agent = Agent::with_budget(loop_provider(Some(1), 10), ToolBudget::default());
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("从 1 数到 10", &mut executor);

    assert!(!has_turn_failed(&events), "{events:?}");
    assert_eq!(executor.calls.len(), 10, "一步里的 10 个调用都要执行");
    assert_eq!(tool_call_ids(&events).len(), 10);
    let results = tool_results(&events);
    assert_eq!(results.len(), 10);
    assert!(results.iter().all(|(_, is_error)| !is_error));
    assert_eq!(step_count(&events), 2);
    assert_eq!(turn_text(&events), "done");
}

/// 一步内超出预算：剩余调用逐条标记失败，仍然发事件并结束本步。
#[test]
fn over_budget_calls_in_one_step_are_marked_failed() {
    let mut agent = Agent::with_budget(
        loop_provider(Some(1), 5),
        ToolBudget {
            max_tool_calls: Some(2),
            max_steps: None,
        },
    );
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("parallel", &mut executor);

    assert!(!has_turn_failed(&events), "{events:?}");
    assert_eq!(executor.calls.len(), 2);
    let results = tool_results(&events);
    assert_eq!(results.len(), 5, "5 个调用都要有配对结果");
    assert_eq!(
        results.iter().filter(|(_, is_error)| *is_error).count(),
        3,
        "超出预算的 3 个调用标失败"
    );
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnEnded { .. }
    ));
}

// -----------------------------------------------------------------------
// M3 并行工具（DSH `maxParallelToolCalls = 10`）：M3-PARALLEL-001..010
// -----------------------------------------------------------------------

/// 只记录本批看得到的调用及其 `max_parallel` 参数。
struct BatchRecordingExecutor {
    calls: Vec<ToolCallRequest>,
}

impl BatchRecordingExecutor {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }
}

impl ToolExecutor for BatchRecordingExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.calls.push(call.clone());
        ToolOutcome::success(
            json!([{ "type": "text", "text": call.tool.clone() }]),
            Value::Null,
        )
    }
}

/// M3-PARALLEL-001：默认 `execute_batch` 逐个 `execute`；事件按原顺序配对
/// （先 ToolCall×N，再 ToolResult×N）。
#[test]
fn m3_parallel_001_batch_default_is_sequential_and_ordered() {
    let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(3));
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("同时跑三个", &mut executor);

    // 默认 execute_batch 走 3 次 execute，顺序 = 调用顺序。
    let executed: Vec<String> = executor
        .calls
        .iter()
        .map(|call| call.call_id.clone())
        .collect();
    assert_eq!(executed, tool_call_ids(&events));
    // 事件顺序：3 个 ToolCall 都在第一个 ToolResult 之前。
    let first_result = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
        .expect("tool result");
    let call_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, AgentEvent::ToolCall(_)))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(call_positions.len(), 3);
    assert!(call_positions
        .iter()
        .all(|position| *position < first_result));
    // 结果顺序 = 发出顺序。
    let result_ids: Vec<String> = tool_results(&events)
        .into_iter()
        .map(|(call_id, _)| call_id)
        .collect();
    assert_eq!(result_ids, tool_call_ids(&events));
}

/// M3-PARALLEL-002：执行器覆盖 `execute_batch` 时可真正并发；墙钟显著小于串行。
struct ConcurrentExecutor {
    max_active: Arc<std::sync::atomic::AtomicUsize>,
}

impl ToolExecutor for ConcurrentExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::failure(format!("execute should not be used for {}", call.call_id))
    }

    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        assert_eq!(max_parallel, 3, "max_parallel 应透传给执行器");
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for call in calls {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&self.max_active);
            let call = call.clone();
            handles.push(std::thread::spawn(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(80));
                active.fetch_sub(1, Ordering::SeqCst);
                ToolOutcome::success(
                    json!([{ "type": "json", "value": { "call": call.call_id } }]),
                    json!({ "call": call.call_id }),
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect()
    }
}

#[test]
fn m3_parallel_002_execute_batch_runs_concurrently() {
    let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(3));
    let mut executor = ConcurrentExecutor {
        max_active: Arc::clone(&max_active),
    };
    let started = std::time::Instant::now();
    let events = agent.run_turn("并行", &mut executor);
    let elapsed = started.elapsed();

    assert_eq!(max_active.load(Ordering::SeqCst), 3, "3 个调用应同时在跑");
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "并行墙钟应远小于串行 240ms，实际 {elapsed:?}"
    );
    assert_eq!(tool_results(&events).len(), 3);
    assert_eq!(
        tool_results(&events)
            .iter()
            .filter(|(_, is_error)| *is_error)
            .count(),
        0
    );
}

/// M3-PARALLEL-003：预算截断只把允许的调用放进 batch；超预算的按原位置降级。
#[test]
fn m3_parallel_003_budget_limits_the_batch() {
    let mut agent = Agent::with_budget(
        loop_provider(Some(1), 4),
        ToolBudget {
            max_tool_calls: Some(2),
            max_steps: None,
        },
    )
    .with_max_parallel_tool_calls(Some(4));
    let mut executor = BatchRecordingExecutor::new();
    let events = agent.run_turn("预算", &mut executor);

    assert_eq!(executor.calls.len(), 2, "batch 只收到预算内的 2 个调用");
    let call_ids = tool_call_ids(&events);
    let results = tool_results(&events);
    assert_eq!(results.len(), 4, "4 个调用都要有配对结果");
    assert_eq!(results[0].0, call_ids[0]);
    assert!(!results[0].1);
    assert_eq!(results[1].0, call_ids[1]);
    assert!(!results[1].1);
    assert_eq!(results[2].0, call_ids[2]);
    assert!(results[2].1, "超预算调用标失败");
    assert_eq!(results[3].0, call_ids[3]);
    assert!(results[3].1, "超预算调用标失败");
}

/// M3-PARALLEL-004：batch 内单个调用失败不影响其它调用（错误隔离、顺序保留）。
struct PartialFailureExecutor;
impl ToolExecutor for PartialFailureExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }
    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::failure("unused")
    }
    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        _max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                if index == 1 {
                    ToolOutcome::failure(format!("boom {}", call.call_id))
                } else {
                    ToolOutcome::success(
                        json!([{ "type": "text", "text": call.call_id.clone() }]),
                        json!(call.call_id.clone()),
                    )
                }
            })
            .collect()
    }
}

#[test]
fn m3_parallel_004_batch_error_is_isolated() {
    let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(3));
    let mut executor = PartialFailureExecutor;
    let events = agent.run_turn("隔离", &mut executor);

    let results = tool_results(&events);
    assert_eq!(
        results
            .iter()
            .map(|(_, is_error)| *is_error)
            .collect::<Vec<_>>(),
        vec![false, true, false],
        "中间失败不影响两侧: {results:?}"
    );
    assert_eq!(
        results
            .iter()
            .map(|(call_id, _)| call_id.clone())
            .collect::<Vec<_>>(),
        tool_call_ids(&events)
    );
    assert!(!has_turn_failed(&events), "{events:?}");
}

/// M3-PARALLEL-005：`max_parallel` 取值优先级（显式覆盖 > env > 默认 10）与透传。
#[test]
fn m3_parallel_005_max_parallel_resolution_and_passthrough() {
    let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(2));
    assert_eq!(agent.max_parallel_tool_calls(), 2);
    let mut recorder = MaxParallelRecorder::default();
    let _ = agent.run_turn("parallel", &mut recorder);
    assert_eq!(recorder.seen, vec![2], "显式覆盖必须透传给执行器");

    let mut recorder = MaxParallelRecorder::default();
    let mut agent = Agent::with_budget(loop_provider(Some(1), 10), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(10));
    let _ = agent.run_turn("parallel", &mut recorder);
    assert_eq!(recorder.seen, vec![10]);

    if std::env::var(MAX_PARALLEL_TOOL_CALLS_ENV).is_err() {
        let agent = Agent::new(MockProvider);
        assert_eq!(
            agent.max_parallel_tool_calls(),
            DEFAULT_MAX_PARALLEL_TOOL_CALLS
        );
    }
}

#[derive(Default)]
struct MaxParallelRecorder {
    seen: Vec<usize>,
}

impl ToolExecutor for MaxParallelRecorder {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }
    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::failure("unused")
    }
    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        self.seen.push(max_parallel);
        calls
            .iter()
            .map(|call| ToolOutcome::success(json!([]), json!(call.call_id.clone())))
            .collect()
    }
}

/// M3-PARALLEL-006：batch 的重复工具提醒统计仍然只算真正执行的调用。
#[test]
fn m3_parallel_006_repeat_reminder_counts_batch_calls() {
    let mut agent = Agent::with_budget(loop_provider(Some(1), 3), ToolBudget::default())
        .with_max_parallel_tool_calls(Some(3))
        .with_repeat_reminders(RepeatToolReminderConfig {
            enabled: true,
            thresholds: vec![3],
        });
    let mut executor = BatchRecordingExecutor::new();
    let _ = agent.run_turn("重复", &mut executor);
    let reminders = agent.take_repeat_reminders();
    assert_eq!(reminders.len(), 1, "{reminders:?}");
    assert_eq!(reminders[0].count, 3);
    assert_eq!(reminders[0].tool, "probe_native");
}

/// 命中 max_steps：优雅结束 TurnEnded，不是 TurnFailed。
#[test]
fn max_steps_ends_turn_without_failure() {
    let mut agent = Agent::with_budget(
        loop_provider(None, 1),
        ToolBudget {
            max_tool_calls: None,
            max_steps: Some(3),
        },
    );
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("loop", &mut executor);

    assert!(!has_turn_failed(&events), "{events:?}");
    assert_eq!(step_count(&events), 3);
    assert_eq!(executor.calls.len(), 3);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnEnded { .. }
    ));
}

/// 空 / 0 / 非法 env 都表示"不限制"（与 DSH 默认一致）；不依赖全局 env。
#[test]
fn zero_or_unset_env_means_unlimited() {
    assert_eq!(
        ToolBudget::default(),
        ToolBudget {
            max_tool_calls: None,
            max_steps: None,
        }
    );
    assert_eq!(parse_positive_limit(""), None);
    assert_eq!(parse_positive_limit("   "), None);
    assert_eq!(parse_positive_limit("0"), None);
    assert_eq!(parse_positive_limit("abc"), None);
    assert_eq!(parse_positive_limit("-1"), None);
    assert_eq!(parse_positive_limit("12"), Some(12));
    assert_eq!(parse_positive_limit(" 7 "), Some(7));

    // Agent::new 的默认预算就是环境读取结果（两者同一时刻读，无写入竞争）。
    let agent = Agent::new(MockProvider);
    assert_eq!(agent.budget(), ToolBudget::from_env());
}

#[test]
fn turn_ids_increment_across_turns() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    let first = agent.run_turn("a", &mut executor);
    let second = agent.run_turn("b", &mut executor);

    assert!(matches!(first[0], AgentEvent::TurnStarted { turn_id: 1 }));
    assert!(matches!(second[0], AgentEvent::TurnStarted { turn_id: 2 }));
}

#[test]
fn provider_errors_become_turn_failed() {
    struct FailingProvider;
    impl ModelProvider for FailingProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            Err("upstream 503".to_owned())
        }
    }

    let mut agent = Agent::new(FailingProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("x", &mut executor);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(message) if message.contains("503")
    ));
}

/// 每次回复都发工具调用、且暴露取消 token 的 provider（取消语义测试专用）。
struct CancelAwareProvider {
    token: Arc<AtomicBool>,
    per_reply: usize,
    /// 返回 reply 之前先置位取消：模拟"模型流式回复期间按了停止"。
    cancel_before_reply: bool,
    invocations: Arc<Mutex<usize>>,
}

impl CancelAwareProvider {
    fn new(token: Arc<AtomicBool>, per_reply: usize) -> Self {
        Self {
            token,
            per_reply,
            cancel_before_reply: false,
            invocations: Arc::new(Mutex::new(0)),
        }
    }

    fn cancel_before_reply(mut self) -> Self {
        self.cancel_before_reply = true;
        self
    }
}

impl ModelProvider for CancelAwareProvider {
    fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
        let index = {
            let mut invocations = self
                .invocations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let index = *invocations;
            *invocations += 1;
            index
        };
        if self.cancel_before_reply {
            self.token.store(true, Ordering::Relaxed);
        }
        Ok(ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: (0..self.per_reply)
                .map(|call| ModelToolCall {
                    id: format!("cancel-call-{index}-{call}"),
                    name: "probe_native".to_owned(),
                    arguments: json!({}),
                })
                .collect(),
        })
    }

    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        Some(self.token.clone())
    }
}

/// 执行第一个工具时置位取消：覆盖"执行后取消 → 已执行结果保留、剩余调用配平"。
struct CancellingExecutor {
    token: Arc<AtomicBool>,
    calls: Vec<ToolCallRequest>,
}

impl CancellingExecutor {
    fn new(token: Arc<AtomicBool>) -> Self {
        Self {
            token,
            calls: Vec::new(),
        }
    }
}

impl ToolExecutor for CancellingExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.calls.push(call.clone());
        if self.calls.len() == 1 {
            // 模拟"用户在这个工具执行完之后按了停止"。
            self.token.store(true, Ordering::Relaxed);
        }
        ToolOutcome::success(
            json!([{ "type": "text", "text": "ok" }]),
            json!({ "ok": true }),
        )
    }
}

/// M3-PARALLEL-007：turn 中途取消（发生在 batch 执行内）——已发出的批允许完成、
/// 结果保留；不再发起下一个 step，以 TurnCancelled 收尾（不是 TurnFailed）；
/// batch 内不再有"未执行调用"，因此不产生 ToolCancelled。
#[test]
fn cancel_during_batch_keeps_results_then_stops() {
    let token = Arc::new(AtomicBool::new(false));
    let provider = CancelAwareProvider::new(token.clone(), 3);
    let mut agent = Agent::new(provider).with_max_parallel_tool_calls(Some(3));
    let mut executor = CancellingExecutor::new(token.clone());
    let events = agent.run_turn("loop", &mut executor);

    assert!(
        !has_turn_failed(&events),
        "取消不是失败，不能 TurnFailed: {events:?}"
    );
    assert!(
        has_turn_cancelled(&events),
        "必须发 TurnCancelled: {events:?}"
    );
    assert_eq!(
        executor.calls.len(),
        3,
        "batch 内已发出的调用允许完成（M3 并行语义）"
    );
    assert_eq!(step_count(&events), 1, "取消后不应再发起模型请求");
    assert_eq!(tool_call_ids(&events).len(), 3, "{events:?}");
    assert_eq!(
        tool_results(&events).len(),
        3,
        "批内 3 个调用都要有配对结果: {events:?}"
    );
    assert!(
        tool_cancelled_ids(&events).is_empty(),
        "batch 内没有未执行调用: {events:?}"
    );
}

/// 第一轮开始前 token 已置位：不发 StepStarted、不请求模型，只有 TurnCancelled。
#[test]
fn cancel_before_first_step() {
    let token = Arc::new(AtomicBool::new(true));
    let provider = CancelAwareProvider::new(token.clone(), 1);
    let invocations = provider.invocations.clone();
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("x", &mut executor);

    assert_eq!(
        events.len(),
        2,
        "只有 TurnStarted + TurnCancelled: {events:?}"
    );
    assert!(matches!(events[0], AgentEvent::TurnStarted { turn_id: 1 }));
    assert!(matches!(
        events[1],
        AgentEvent::TurnCancelled { ref reason } if reason == "cancel"
    ));
    assert!(!has_turn_failed(&events));
    assert!(executor.calls.is_empty());
    assert_eq!(
        *invocations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        0,
        "取消后不应发起模型请求"
    );
}

/// 模型回复之后、执行工具之前置位取消：本步所有调用发 ToolCall + ToolCancelled，
/// 但一个都不执行。
#[test]
fn cancel_after_reply_pairs_calls_without_executing() {
    let token = Arc::new(AtomicBool::new(false));
    let provider = CancelAwareProvider::new(token.clone(), 2).cancel_before_reply();
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("loop", &mut executor);

    assert!(!has_turn_failed(&events), "{events:?}");
    assert!(has_turn_cancelled(&events), "{events:?}");
    assert!(executor.calls.is_empty(), "取消后不得执行任何工具");
    let calls = tool_call_ids(&events);
    assert_eq!(calls.len(), 2, "{events:?}");
    assert_eq!(tool_cancelled_ids(&events), calls, "每个 ToolCall 都要配对");
    assert!(tool_results(&events).is_empty());
}

/// provider 在取消状态下返回 Err：收敛为 TurnCancelled，不是 TurnFailed（P2-10）。
#[test]
fn cancelled_provider_error_is_not_a_failure() {
    struct CancelledThenFailingProvider {
        token: Arc<AtomicBool>,
    }

    impl ModelProvider for CancelledThenFailingProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            self.token.store(true, Ordering::Relaxed);
            Err("cancelled".to_owned())
        }

        fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
            Some(self.token.clone())
        }
    }

    let mut agent = Agent::new(CancelledThenFailingProvider {
        token: Arc::new(AtomicBool::new(false)),
    });
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("x", &mut executor);

    assert!(!has_turn_failed(&events), "取消不能报失败: {events:?}");
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnCancelled { .. }
    ));
}

fn text_deltas(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta(delta) => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

fn turn_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::TurnEnded { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn streaming_deltas_are_forwarded_in_order() {
    struct StreamingProvider;
    impl ModelProvider for StreamingProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            unreachable!("流式 provider 不应回退到 complete")
        }
        fn complete_streaming(
            &mut self,
            _request: &CompletionRequest,
            on_delta: &mut dyn FnMut(&str),
        ) -> Result<ModelReply, String> {
            on_delta("你");
            on_delta("好");
            on_delta("呀");
            Ok(ModelReply::text_only("你好呀"))
        }
    }

    let mut agent = Agent::new(StreamingProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("hi", &mut executor);

    assert_eq!(text_deltas(&events), vec!["你", "好", "呀"]);
    assert_eq!(turn_text(&events), "你好呀");
}

#[test]
fn provider_ignoring_on_delta_still_gets_one_delta() {
    struct SilentProvider;
    impl ModelProvider for SilentProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            Ok(ModelReply::text_only("整段返回"))
        }
    }

    let mut agent = Agent::new(SilentProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("hi", &mut executor);

    assert_eq!(text_deltas(&events), vec!["整段返回"]);
    assert_eq!(turn_text(&events), "整段返回");
}

#[test]
fn mock_provider_streams_multiple_deltas() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("你好", &mut executor);

    let deltas = text_deltas(&events);
    assert!(deltas.len() > 1, "mock 也应切成多个 delta: {deltas:?}");
    assert_eq!(deltas.concat(), turn_text(&events));
}

#[test]
fn streaming_tool_turn_keeps_call_before_result() {
    let mut agent = Agent::new(MockProvider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("用工具试一下", &mut executor);

    let call_pos = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolCall(_)))
        .expect("tool call");
    let result_pos = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolResult { .. }))
        .expect("tool result");
    assert!(call_pos < result_pos);
    // 工具调用那一步没有文本，所以 delta 只能来自调用之后的第二步。
    let first_delta_pos = events
        .iter()
        .position(|event| matches!(event, AgentEvent::TextDelta(_)))
        .expect("streamed delta");
    assert!(first_delta_pos > result_pos);
}

/// 记录每次请求的消息序列，用于断言协议形状。
struct RecordingProvider {
    script: Vec<ModelReply>,
    requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl ModelProvider for RecordingProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.messages.clone());
        if self.script.is_empty() {
            Ok(ModelReply::text_only("done"))
        } else {
            Ok(self.script.remove(0))
        }
    }
}

fn recording(script: Vec<ModelReply>) -> (RecordingProvider, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    (
        RecordingProvider {
            script,
            requests: requests.clone(),
        },
        requests,
    )
}

fn recorded(requests: &Arc<Mutex<Vec<Vec<ChatMessage>>>>) -> Vec<Vec<ChatMessage>> {
    requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[test]
fn previous_turn_tool_results_do_not_look_like_current_step() {
    let request = CompletionRequest {
        messages: vec![
            ChatMessage::user("跑一下工具"),
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![ModelToolCall {
                    id: "c1".to_owned(),
                    name: "probe_echo".to_owned(),
                    arguments: json!({}),
                }],
            },
            ChatMessage::Tool {
                call_id: "c1".to_owned(),
                content: "旧结果".to_owned(),
            },
            ChatMessage::Assistant {
                reasoning: String::new(),
                text: "做完了".to_owned(),
                tool_calls: Vec::new(),
            },
            ChatMessage::user("新问题"),
        ],
        tool_specs: Vec::new(),
    };
    assert_eq!(request.last_user_text(), "新问题");
    assert_eq!(
        request.last_tool_content(),
        None,
        "上一轮的工具结果不能误判"
    );
}

#[test]
fn history_is_sent_before_the_new_user_message() {
    let history = vec![
        ChatMessage::user("我叫小张"),
        ChatMessage::Assistant {
            reasoning: String::new(),
            text: "记住了".to_owned(),
            tool_calls: Vec::new(),
        },
    ];
    let (provider, requests) = recording(Vec::new());
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    agent.run_turn_with_history(&history, "我叫什么名字", &mut executor);

    let requests = recorded(&requests);
    let sent = &requests[0];
    assert_eq!(sent[0], history[0]);
    assert_eq!(sent[1], history[1]);
    assert_eq!(sent[2], ChatMessage::user("我叫什么名字"));
    assert_eq!(sent.len(), 3);
}

#[test]
fn assistant_tool_calls_and_tool_role_are_paired_by_id() {
    let script = vec![ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: vec![ModelToolCall {
            id: "call-abc".to_owned(),
            name: "probe_native".to_owned(),
            arguments: json!({ "text": "hi" }),
        }],
    }];
    let (provider, requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    let events = agent.run_turn("用原生工具", &mut executor);
    assert!(!turn_text(&events).is_empty(), "第二步应有回答");

    let requests = recorded(&requests);
    // 第二次请求 = 用户 + assistant(tool_calls) + tool(结果)
    let second = &requests[1];
    assert_eq!(second.len(), 3);
    assert!(matches!(
        &second[1],
        ChatMessage::Assistant { text, tool_calls, .. }
            if text.is_empty() && tool_calls.len() == 1 && tool_calls[0].id == "call-abc"
    ));
    assert!(matches!(
        &second[2],
        ChatMessage::Tool { call_id, content }
            if call_id == "call-abc" && content.contains("\"text\"")
    ));
}

#[test]
fn multiple_tool_calls_in_one_reply_are_all_executed() {
    let script = vec![ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: vec![
            ModelToolCall {
                id: "c1".to_owned(),
                name: "probe_native".to_owned(),
                arguments: json!({ "text": "a" }),
            },
            ModelToolCall {
                id: "c2".to_owned(),
                name: "probe_echo".to_owned(),
                arguments: json!({ "text": "b" }),
            },
        ],
    }];
    let (provider, requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    agent.run_turn("两个工具", &mut executor);

    assert_eq!(executor.calls.len(), 2);
    assert_eq!(executor.calls[0].call_id, "c1");
    assert_eq!(executor.calls[1].call_id, "c2");

    let requests = recorded(&requests);
    let second = &requests[1];
    assert_eq!(second.len(), 4, "user + assistant + 两个 tool");
    assert!(matches!(
        &second[2],
        ChatMessage::Tool { call_id, .. } if call_id == "c1"
    ));
    assert!(matches!(
        &second[3],
        ChatMessage::Tool { call_id, .. } if call_id == "c2"
    ));
}

#[test]
fn tool_error_content_is_prefixed_for_the_model() {
    struct FailingExecutor;
    impl ToolExecutor for FailingExecutor {
        fn specs(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
            ToolOutcome::failure("permission denied")
        }
    }

    let script = vec![ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: vec![ModelToolCall {
            id: "c1".to_owned(),
            name: "bash".to_owned(),
            arguments: json!({}),
        }],
    }];
    let (provider, requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = FailingExecutor;
    agent.run_turn("试试", &mut executor);

    let requests = recorded(&requests);
    assert!(matches!(
        &requests[1][2],
        ChatMessage::Tool { content, .. } if content.starts_with("ERROR: ")
    ));
}

// ------------------------------------------------------------------
// M3-DEFAULT-001..004：reasoning effort（共享档位类型）
// ------------------------------------------------------------------

#[test]
fn m3_default_001_reasoning_effort_parses_dsh_levels() {
    assert_eq!(ReasoningEffort::parse("off"), Some(ReasoningEffort::Off));
    assert_eq!(ReasoningEffort::parse("LOW"), Some(ReasoningEffort::Low));
    assert_eq!(
        ReasoningEffort::parse(" high "),
        Some(ReasoningEffort::High)
    );
    assert_eq!(ReasoningEffort::parse("max"), Some(ReasoningEffort::Max));
    assert_eq!(ReasoningEffort::parse("medium"), None);
    assert_eq!(ReasoningEffort::parse(""), None);
}

#[test]
fn m3_default_002_reasoning_effort_maps_off_to_no_wire_value() {
    assert_eq!(ReasoningEffort::Off.wire_value(), None);
    assert_eq!(ReasoningEffort::Low.wire_value(), Some("low"));
    assert_eq!(ReasoningEffort::High.wire_value(), Some("high"));
    assert_eq!(ReasoningEffort::Max.wire_value(), Some("max"));
    for effort in [
        ReasoningEffort::Off,
        ReasoningEffort::Low,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ] {
        assert_eq!(ReasoningEffort::parse(effort.as_str()), Some(effort));
    }
}

#[test]
fn m3_default_003_invalid_reasoning_effort_is_ignored_not_fatal() {
    assert_eq!(ReasoningEffort::resolve(None), None);
    assert_eq!(ReasoningEffort::resolve(Some("  ")), None);
    assert_eq!(ReasoningEffort::resolve(Some("turbo")), None);
    assert_eq!(
        ReasoningEffort::resolve(Some("max")),
        Some(ReasoningEffort::Max)
    );
}

#[test]
fn m3_default_004_reasoning_effort_env_name_is_stable() {
    assert_eq!(ReasoningEffort::ENV, "DSHR_REASONING_EFFORT");
}

// ------------------------------------------------------------------
// M3-DEFAULT-010..015：重复工具调用软提醒
// ------------------------------------------------------------------

fn repeated_calls(count: usize, arguments: Value) -> ModelReply {
    ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: (0..count)
            .map(|index| ModelToolCall {
                id: format!("repeat-call-{index}"),
                name: "probe_echo".to_owned(),
                arguments: arguments.clone(),
            })
            .collect(),
    }
}

#[test]
fn m3_default_010_repeat_reminder_fires_at_3_5_8() {
    let script = vec![repeated_calls(8, json!({ "text": "same" }))];
    let (provider, requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    agent.run_turn("重复调用", &mut executor);

    let reminders = agent.take_repeat_reminders();
    let counts: Vec<usize> = reminders.iter().map(|reminder| reminder.count).collect();
    assert_eq!(counts, vec![3, 5, 8], "thresholds [3,5,8]：{reminders:?}");
    assert!(reminders
        .iter()
        .all(|reminder| reminder.tool == "probe_echo"));
    assert!(reminders
        .iter()
        .all(|reminder| reminder.text.contains("重复工具调用提醒")));
    assert!(agent.repeat_reminder_log().is_empty(), "take 后清空");

    // 提醒作为 system 消息注入到了下一次请求，且不打断 tool 配对。
    let requests = recorded(&requests);
    let second = &requests[1];
    let system_reminders = second
        .iter()
        .filter(|message| {
            matches!(message, ChatMessage::System(text) if text.contains("重复工具调用提醒"))
        })
        .count();
    assert_eq!(system_reminders, 3);
    assert_eq!(
        second
            .iter()
            .filter(|message| matches!(message, ChatMessage::Tool { .. }))
            .count(),
        8
    );
}

#[test]
fn m3_default_011_different_arguments_do_not_trigger() {
    let calls: Vec<ModelToolCall> = (0..8)
        .map(|index| ModelToolCall {
            id: format!("diff-call-{index}"),
            name: "probe_echo".to_owned(),
            arguments: json!({ "text": format!("value-{index}") }),
        })
        .collect();
    let script = vec![ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: calls,
    }];
    let (provider, _requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    agent.run_turn("不同参数", &mut executor);
    assert!(agent.take_repeat_reminders().is_empty(), "参数不同不得提醒");
}

#[test]
fn m3_default_012_different_tools_with_same_arguments_do_not_trigger() {
    // 3 个不同工具 × 各 2 次 + 相同参数：每个 (tool, args) 计数都 < 3。
    let names = ["probe_echo", "probe_native", "probe_extra"];
    let calls: Vec<ModelToolCall> = (0..6)
        .map(|index| ModelToolCall {
            id: format!("tool-call-{index}"),
            name: names[index % names.len()].to_owned(),
            arguments: json!({ "text": "same" }),
        })
        .collect();
    let script = vec![ModelReply {
        reasoning: String::new(),
        text: String::new(),
        tool_calls: calls,
    }];
    let (provider, _requests) = recording(script);
    let mut agent = Agent::new(provider);
    let mut executor = EchoExecutor::new();
    agent.run_turn("不同工具", &mut executor);
    assert!(agent.take_repeat_reminders().is_empty(), "工具不同不得提醒");
}

#[test]
fn m3_default_013_repeat_reminder_can_be_disabled() {
    let script = vec![repeated_calls(8, json!({ "text": "same" }))];
    let (provider, requests) = recording(script);
    let mut agent =
        Agent::new(provider).with_repeat_reminders(RepeatToolReminderConfig::disabled());
    let mut executor = EchoExecutor::new();
    agent.run_turn("重复调用", &mut executor);
    assert!(agent.take_repeat_reminders().is_empty());
    let requests = recorded(&requests);
    assert!(
        !requests[1]
            .iter()
            .any(|message| matches!(message, ChatMessage::System(text) if text.contains("重复工具调用提醒"))),
        "关闭后不得注入提醒"
    );
}

#[test]
fn m3_default_014_repeat_reminder_custom_thresholds() {
    let script = vec![repeated_calls(4, json!({ "text": "same" }))];
    let (provider, _requests) = recording(script);
    let mut agent = Agent::new(provider)
        .with_repeat_reminders(RepeatToolReminderConfig::default().with_thresholds([2, 4]));
    let mut executor = EchoExecutor::new();
    agent.run_turn("自定义阈值", &mut executor);
    let counts: Vec<usize> = agent
        .take_repeat_reminders()
        .iter()
        .map(|reminder| reminder.count)
        .collect();
    assert_eq!(counts, vec![2, 4]);
}

#[test]
fn m3_default_015_repeat_threshold_config_parsing() {
    assert_eq!(
        parse_repeat_thresholds("3,5,8"),
        Some(vec![3, 5, 8]),
        "DSH 默认阈值"
    );
    assert_eq!(
        parse_repeat_thresholds("8, 3, 5, 3"),
        Some(vec![3, 5, 8]),
        "排序去重"
    );
    assert_eq!(parse_repeat_thresholds("3,x,8"), None);
    assert_eq!(parse_repeat_thresholds("0,3"), None);
    assert_eq!(parse_repeat_thresholds("  "), None);
    assert_eq!(
        RepeatToolReminderConfig::default().thresholds,
        REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec()
    );
    assert!(RepeatToolReminderConfig::disabled().thresholds.is_empty());
}
