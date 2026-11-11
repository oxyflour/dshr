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

pub mod session_tree;

pub use session_tree::{
    spawn_child_spec, DrainMode, DrainWait, InterruptAction, InterruptPlan, InterruptTarget,
    PolicyTightening, SessionPolicy, SessionStatus, SessionTreeRuntime, SpawnChildArgs,
    SpawnContext, SpawnError, SpawnedChild, TreeSession, DEFAULT_MAX_DEPTH, SPAWN_CHILD_TOOL,
};

use serde_json::{json, Value};

/// 一次 run_turn 内允许的最大工具调用次数（防止模型自激循环）。
///
/// 8 是"够用且能兜住死循环"的经验值：真实多工具任务常见 2–6 次。
pub const MAX_TOOL_CALLS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema（原样交给模型；MockProvider 忽略它）。
    pub parameters: Value,
}

/// 模型返回的工具调用（OpenAI `tool_calls[]` 的形状：id 用来对结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolCall {
    /// Provider 分配的工具调用 id；`role: tool` 消息必须回填同一个 id。
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// 发给模型的一条消息。**这里是严格 OpenAI 协议的形状**：
/// assistant 的工具调用与 `tool` 结果必须按顺序配对，否则模型无法把结果关联回调用。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ModelToolCall>,
    },
    Tool {
        /// 对应 `ModelToolCall::id`。
        call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self::User(text.into())
    }
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
    /// 完整对话历史（含本轮用户消息、以及本轮已发生的 assistant/tool 消息）。
    pub messages: Vec<ChatMessage>,
    pub tool_specs: Vec<ToolSpec>,
}

impl CompletionRequest {
    /// 最后一条用户消息（MockProvider 用它做关键词判定）。
    pub fn last_user_text(&self) -> &str {
        self.messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ChatMessage::User(text) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// 当前这一步里的最后一条工具结果（只看最后一条用户消息之后的消息）。
    ///
    /// 不能扫全历史：上一轮的工具结果会误判成"本轮刚跑完工具"。
    pub fn last_tool_content(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .take_while(|message| !matches!(message, ChatMessage::User(_)))
            .find_map(|message| match message {
                ChatMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelReply {
    pub text: String,
    /// 一次回复可以有多个工具调用（OpenAI 的 parallel tool calls）。
    pub tool_calls: Vec<ModelToolCall>,
}

impl ModelReply {
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
        }
    }
}

pub trait ModelProvider: Send {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String>;

    /// 停止生成的句柄：UI 无需持有 provider 就能请求中断。
    ///
    /// 约定：provider 在每次请求开始时清零，读到 true 后尽快返回**已生成的部分**，
    /// 由 Agent 按正常轮次收尾（会话日志里会是一条不完整的回复）。
    fn cancel_token(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        None
    }

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

    fn cancel_token(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        (**self).cancel_token()
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

    /// 访问 provider：上下文压缩需要在 Agent 之外调用一次模型（摘要）。
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// 跑一轮：最多 `1 + MAX_TOOL_CALLS` 个 step。无历史版本（测试与单轮场景）。
    ///
    /// 事件顺序保证：`TurnStarted` → (`StepStarted` → `TextDelta`* → (`ToolCall` → `ToolResult`)*)? →
    /// `TurnEnded` 或 `TurnFailed`。
    pub fn run_turn(
        &mut self,
        user_text: &str,
        executor: &mut dyn ToolExecutor,
    ) -> Vec<AgentEvent> {
        self.run_turn_with_history(&[], user_text, executor)
    }

    pub fn run_turn_with_history(
        &mut self,
        history: &[ChatMessage],
        user_text: &str,
        executor: &mut dyn ToolExecutor,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        self.run_turn_streaming(history, user_text, executor, &mut |event| {
            events.push(event.clone())
        });
        events
    }

    /// 流式版本：事件一到就回调，不攒到轮次结束。
    ///
    /// `history` 由调用方从 Session Log 重建（`agent-context`），Agent 自己只维护**本轮**
    /// 产生的 assistant/tool 消息，保证 `role: tool` 与 `tool_call_id` 严格配对。
    pub fn run_turn_streaming(
        &mut self,
        history: &[ChatMessage],
        user_text: &str,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        self.turn += 1;
        let turn_id = self.turn;
        on_event(&AgentEvent::TurnStarted { turn_id });

        let mut messages: Vec<ChatMessage> = history.to_vec();
        messages.push(ChatMessage::User(user_text.to_owned()));

        let mut step: u32 = 1;
        let mut calls = 0usize;
        let mut text = String::new();

        loop {
            on_event(&AgentEvent::StepStarted { turn_id, step });

            let request = CompletionRequest {
                messages: messages.clone(),
                tool_specs: executor.specs(),
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

            // 无论有没有工具调用，assistant 这一步都要进历史（协议要求配对）。
            if !reply.text.is_empty() || !reply.tool_calls.is_empty() {
                messages.push(ChatMessage::Assistant {
                    text: reply.text.clone(),
                    tool_calls: reply.tool_calls.clone(),
                });
            }

            if reply.tool_calls.is_empty() {
                break;
            }

            for tool_call in &reply.tool_calls {
                if calls >= MAX_TOOL_CALLS {
                    on_event(&AgentEvent::TurnFailed(format!(
                        "tool call budget exhausted ({MAX_TOOL_CALLS})"
                    )));
                    return;
                }
                calls += 1;

                let call = ToolCallRequest {
                    call_id: tool_call.id.clone(),
                    tool: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                };
                on_event(&AgentEvent::ToolCall(call.clone()));
                let outcome = executor.execute(&call);
                on_event(&AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    outcome: outcome.clone(),
                });
                messages.push(ChatMessage::Tool {
                    call_id: call.call_id.clone(),
                    content: tool_content_for_model(&outcome),
                });
            }
            step += 1;
        }

        on_event(&AgentEvent::TurnEnded { text });
    }
}

/// 工具结果喂给模型时的文本投影：优先 text block，其次 `value` 的 JSON。
///
/// `role: tool` 的 content 在 OpenAI 协议里是字符串；结构化值已经通过 UI 的 `value`
/// 承载，模型侧只需要可读文本。
fn tool_content_for_model(outcome: &ToolOutcome) -> String {
    let text = if let Some(blocks) = outcome.content.as_array() {
        let parts: Vec<String> = blocks
            .iter()
            .map(|block| {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text.to_owned()
                } else if let Some(value) = block.get("value") {
                    value.to_string()
                } else {
                    String::new()
                }
            })
            .filter(|part| !part.is_empty())
            .collect();
        if parts.is_empty() {
            outcome.content.to_string()
        } else {
            parts.join("\n")
        }
    } else {
        outcome.content.to_string()
    };
    if outcome.is_error {
        format!("ERROR: {text}")
    } else {
        text
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
        // 已经有工具结果：生成第二步回答（模拟"看结果再说话"）。
        if let Some(content) = request.last_tool_content() {
            return Ok(ModelReply::text_only(summarize(content)));
        }

        let user_text = request.last_user_text().to_owned();
        let lower = user_text.to_lowercase();
        if user_text.contains("原生") || lower.contains("native") {
            return Ok(tool_reply(request, "probe_native", &user_text));
        }

        if user_text.contains("工具") || lower.contains("tool") {
            if let Some(spec) = request
                .tool_specs
                .iter()
                .find(|spec| spec.name != "probe_native")
            {
                return Ok(tool_reply(request, &spec.name, &user_text));
            }
        }

        Ok(ModelReply::text_only(format!(
            "收到：{user_text}。当前是 mock agent，尚未接入 LLM。"
        )))
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

/// 构造一个工具调用。id 用消息条数做序号：同一轮内唯一、跨轮不冲突。
fn tool_reply(request: &CompletionRequest, name: &str, user_text: &str) -> ModelReply {
    ModelReply {
        text: String::new(),
        tool_calls: vec![ModelToolCall {
            id: format!("mock-call-{}-{name}", request.messages.len()),
            name: name.to_owned(),
            arguments: json!({ "text": user_text }),
        }],
    }
}

fn summarize(content: &str) -> String {
    if let Some(reason) = content.strip_prefix("ERROR: ") {
        return format!("工具执行失败：{reason}。当前是 mock agent，尚未接入 LLM。");
    }
    format!("工具返回 {content}。当前是 mock agent，尚未接入 LLM。")
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn tool_budget_is_enforced() {
        struct LoopProvider;
        impl ModelProvider for LoopProvider {
            fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
                Ok(ModelReply {
                    text: String::new(),
                    tool_calls: vec![ModelToolCall {
                        id: "loop-call".to_owned(),
                        name: "probe_native".to_owned(),
                        arguments: json!({}),
                    }],
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

    fn recording(
        script: Vec<ModelReply>,
    ) -> (RecordingProvider, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
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
            ChatMessage::Assistant { text, tool_calls }
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
}
