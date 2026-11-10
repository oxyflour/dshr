//! Agent Loop：Turn / Step / 工具调用事件流。
//!
//! 这一层是 PLAN §6.1 的最小实现，也是 M2 的入口：
//! **模型只负责产出文本与工具调用意图，执行与记录由宿主完成**。
//!
//! 两个可替换的接口：
//! - [`ModelProvider`]：目前是 [`MockProvider`]（关键词决定行为），M2 换成真实 LLM；
//! - [`ToolExecutor`]：由应用实现（chat 里接的是 PluginHost + Sidecar）。
//!
//! `Agent` 只负责编排与事件产出，不碰 Session Log、不碰 UI——这让它可以被单测。

#![forbid(unsafe_code)]

use serde_json::{json, Value};

/// 一次 run_turn 内允许的最大工具调用次数（防止模型自激循环）。
pub const MAX_TOOL_CALLS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema（原样交给模型；MockProvider 忽略它）。
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub is_error: bool,
    /// 模型可见内容（DSH: `content: ContentBlock[]`）。
    pub content: Value,
    /// 结构化返回值（DSH: `value`）。展示与回放用它，不要再从 content 里挖。
    pub value: Value,
    /// 工具自带的元数据（DSH: `meta`）。
    pub meta: Option<Value>,
}

impl ToolOutcome {
    pub fn success(content: Value, value: Value) -> Self {
        Self {
            is_error: false,
            content,
            value,
            meta: None,
        }
    }

    pub fn failure(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            is_error: true,
            content: json!([{ "type": "text", "text": reason }]),
            value: Value::Null,
            meta: None,
        }
    }
}

pub trait ToolExecutor {
    fn specs(&self) -> Vec<ToolSpec>;
    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome;
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub user_text: String,
    pub tool_specs: Vec<ToolSpec>,
    /// 上一步工具结果（如果有），供模型据此生成最终回答。
    pub previous: Option<ToolOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelReply {
    pub text: String,
    /// (tool name, arguments)。有工具调用时 `text` 通常为空。
    pub tool_call: Option<(String, Value)>,
}

pub trait ModelProvider: Send {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String>;

    /// 流式补全：每收到一段文本就回调一次，返回完整回复。
    ///
    /// 默认实现回退到 [`ModelProvider::complete`] 并把整段文本作为**一个** delta 回放，
    /// 这样不实现流式的 provider 不需要额外代码。真正流式的 provider（SSE）应覆盖它。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let reply = self.complete(request)?;
        if !reply.text.is_empty() {
            on_delta(&reply.text);
        }
        Ok(reply)
    }
}

/// 让 `Agent<Box<dyn ModelProvider>>` 成为可用类型（运行时选择 provider）。
impl ModelProvider for Box<dyn ModelProvider> {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        (**self).complete(request)
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        (**self).complete_streaming(request, on_delta)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: u64,
    },
    StepStarted {
        turn_id: u64,
        step: u32,
    },
    TextDelta(String),
    ToolCall(ToolCallRequest),
    ToolResult {
        call_id: String,
        outcome: ToolOutcome,
    },
    TurnFailed(String),
    TurnEnded {
        text: String,
    },
}

pub struct Agent<P> {
    provider: P,
    turn: u64,
}

impl<P: ModelProvider> Agent<P> {
    pub fn new(provider: P) -> Self {
        Self { provider, turn: 0 }
    }

    pub fn turn_count(&self) -> u64 {
        self.turn
    }

    /// 跑一轮：最多 `1 + MAX_TOOL_CALLS` 个 step。
    ///
    /// 事件顺序保证：`TurnStarted` → (`StepStarted` → `TextDelta`* → (`ToolCall` → `ToolResult`)?)+ →
    /// `TurnEnded` 或 `TurnFailed`。
    pub fn run_turn(
        &mut self,
        user_text: &str,
        executor: &mut dyn ToolExecutor,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        self.run_turn_streaming(user_text, executor, &mut |event| events.push(event.clone()));
        events
    }

    /// 流式版本：事件一到就回调，不攒到轮次结束。
    ///
    /// UI 用这个接口才能"边收边显示"；测试与无 UI 场景用 [`Agent::run_turn`] 收集。
    pub fn run_turn_streaming(
        &mut self,
        user_text: &str,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        self.turn += 1;
        let turn_id = self.turn;
        on_event(&AgentEvent::TurnStarted { turn_id });

        let mut step: u32 = 1;
        let mut previous: Option<ToolOutcome> = None;
        let mut calls = 0usize;
        let mut text = String::new();

        loop {
            on_event(&AgentEvent::StepStarted { turn_id, step });

            let request = CompletionRequest {
                user_text: user_text.to_owned(),
                tool_specs: executor.specs(),
                previous: previous.clone(),
            };

            // provider 边流边回调；这里只负责把 delta 转成事件并累计全文。
            let mut streamed_any = false;
            let reply = {
                let mut forward = |delta: &str| {
                    if delta.is_empty() {
                        return;
                    }
                    streamed_any = true;
                    text.push_str(delta);
                    on_event(&AgentEvent::TextDelta(delta.to_owned()));
                };
                match self.provider.complete_streaming(&request, &mut forward) {
                    Ok(reply) => reply,
                    Err(err) => {
                        on_event(&AgentEvent::TurnFailed(err));
                        return;
                    }
                }
            };

            // provider 没走流式（或实现有 bug 只返回值）：补发一次，保证 UI 不空白。
            if !streamed_any && !reply.text.is_empty() {
                text.push_str(&reply.text);
                on_event(&AgentEvent::TextDelta(reply.text.clone()));
            }

            let Some((tool, arguments)) = reply.tool_call else {
                break;
            };
            if calls >= MAX_TOOL_CALLS {
                on_event(&AgentEvent::TurnFailed(format!(
                    "tool call budget exhausted ({MAX_TOOL_CALLS})"
                )));
                return;
            }
            calls += 1;

            let call = ToolCallRequest {
                call_id: format!("call-{turn_id}-{calls}"),
                tool,
                arguments,
            };
            on_event(&AgentEvent::ToolCall(call.clone()));
            let outcome = executor.execute(&call);
            on_event(&AgentEvent::ToolResult {
                call_id: call.call_id.clone(),
                outcome: outcome.clone(),
            });
            previous = Some(outcome);
            step += 1;
        }

        on_event(&AgentEvent::TurnEnded { text });
    }
}

/// M1 的替代模型：按关键词决定"回文本"还是"调工具"。
///
/// 它只依赖 `CompletionRequest`（不看历史、不保存状态），所以同一请求必然得到同一输出，
/// 这让 Agent Loop 的测试是确定性的。M2 会被真实 provider 替换。
#[derive(Debug, Default)]
pub struct MockProvider;

impl ModelProvider for MockProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        if let Some(previous) = &request.previous {
            return Ok(ModelReply {
                text: summarize(previous),
                tool_call: None,
            });
        }

        let lower = request.user_text.to_lowercase();
        if request.user_text.contains("原生") || lower.contains("native") {
            return Ok(ModelReply {
                text: String::new(),
                tool_call: Some((
                    "probe_native".to_owned(),
                    json!({ "text": request.user_text }),
                )),
            });
        }

        if request.user_text.contains("工具") || lower.contains("tool") {
            if let Some(spec) = request
                .tool_specs
                .iter()
                .find(|spec| spec.name != "probe_native")
            {
                return Ok(ModelReply {
                    text: String::new(),
                    tool_call: Some((spec.name.clone(), json!({ "text": request.user_text }))),
                });
            }
        }

        Ok(ModelReply {
            text: format!(
                "收到：{}。当前是 mock agent，尚未接入 LLM。",
                request.user_text
            ),
            tool_call: None,
        })
    }

    /// 把回复切成 8 字符的 delta 依次回调：让流式路径在 mock 下也可确定性地被测。
    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let reply = self.complete(request)?;
        let chars: Vec<char> = reply.text.chars().collect();
        for chunk in chars.chunks(8) {
            let piece: String = chunk.iter().collect();
            on_delta(&piece);
        }
        Ok(reply)
    }
}

fn summarize(outcome: &ToolOutcome) -> String {
    if outcome.is_error {
        return format!(
            "工具执行失败：{}。当前是 mock agent，尚未接入 LLM。",
            readable(&outcome.content)
        );
    }
    format!(
        "工具返回 {}。当前是 mock agent，尚未接入 LLM。",
        readable(&outcome.content)
    )
}

/// 工具结果是 content block 数组时，只展示第一个块的 value/text，避免把 JSON 包装样例甩给用户。
fn readable(content: &Value) -> String {
    if let Some(blocks) = content.as_array() {
        if let Some(first) = blocks.first() {
            if let Some(value) = first.get("value") {
                return value.to_string();
            }
            if let Some(text) = first.get("text").and_then(Value::as_str) {
                return text.to_owned();
            }
        }
    }
    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn tool_budget_is_enforced() {
        struct LoopProvider;
        impl ModelProvider for LoopProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                Ok(ModelReply {
                    text: String::new(),
                    tool_call: Some(("probe_native".to_owned(), json!({}))),
                })
            }
        }

        let mut agent = Agent::new(LoopProvider);
        let mut executor = EchoExecutor::new();
        let events = agent.run_turn("loop", &mut executor);

        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed(_))));
        assert_eq!(executor.calls.len(), MAX_TOOL_CALLS);
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
                Ok(ModelReply {
                    text: "你好呀".to_owned(),
                    tool_call: None,
                })
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
                Ok(ModelReply {
                    text: "整段返回".to_owned(),
                    tool_call: None,
                })
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
}
