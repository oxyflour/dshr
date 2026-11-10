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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
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
    pub content: Value,
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

pub trait ModelProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String>;
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
        self.turn += 1;
        let turn_id = self.turn;
        let mut events = vec![AgentEvent::TurnStarted { turn_id }];

        let mut step: u32 = 1;
        let mut previous: Option<ToolOutcome> = None;
        let mut calls = 0usize;
        let mut text = String::new();

        loop {
            events.push(AgentEvent::StepStarted { turn_id, step });

            let request = CompletionRequest {
                user_text: user_text.to_owned(),
                tool_specs: executor.specs(),
                previous: previous.clone(),
            };

            let reply = match self.provider.complete(&request) {
                Ok(reply) => reply,
                Err(err) => {
                    events.push(AgentEvent::TurnFailed(err));
                    return events;
                }
            };

            for delta in chunks(&reply.text, 16) {
                events.push(AgentEvent::TextDelta(delta));
            }
            text.push_str(&reply.text);

            let Some((tool, arguments)) = reply.tool_call else {
                break;
            };
            if calls >= MAX_TOOL_CALLS {
                events.push(AgentEvent::TurnFailed(format!(
                    "tool call budget exhausted ({MAX_TOOL_CALLS})"
                )));
                return events;
            }
            calls += 1;

            let call = ToolCallRequest {
                call_id: format!("call-{turn_id}-{calls}"),
                tool,
                arguments,
            };
            events.push(AgentEvent::ToolCall(call.clone()));
            let outcome = executor.execute(&call);
            events.push(AgentEvent::ToolResult {
                call_id: call.call_id.clone(),
                outcome: outcome.clone(),
            });
            previous = Some(outcome);
            step += 1;
        }

        events.push(AgentEvent::TurnEnded { text });
        events
    }
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
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
                },
                ToolSpec {
                    name: "probe_echo".to_owned(),
                    description: "sidecar".to_owned(),
                },
            ]
        }

        fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
            self.calls.push(call.clone());
            ToolOutcome {
                is_error: false,
                content: json!({ "echo": call.arguments }),
            }
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
}
