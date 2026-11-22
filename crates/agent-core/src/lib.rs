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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// 取消原因字符串：与 `session.interrupted` / sidecar `tool.cancel` 的 reason 对齐。
const CANCEL_REASON: &str = "cancel";

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
    ReasoningDelta(String),
    ToolCall(ToolCallRequest),
    ToolResult {
        call_id: String,
        outcome: ToolOutcome,
    },
    /// 工具调用被取消（**未执行**，或执行中被打断）。每个 `ToolCall` 要么有配对的
    /// `ToolResult`，要么有配对的 `ToolCancelled`，UI/会话日志不会出现孤立调用。
    ToolCancelled {
        call_id: String,
        reason: String,
    },
    TurnFailed(String),
    /// 本轮因取消而结束（不是失败）：已产生的文本/工具结果保留，不再发起新请求。
    TurnCancelled {
        reason: String,
    },
    TurnEnded {
        text: String,
    },
}

pub struct Agent<P> {
    provider: P,
    turn: u64,
    budget: ToolBudget,
    /// 覆盖 `maxParallelToolCalls`（None = 读 env / 默认 10；测试注入确定性值用）。
    max_parallel_tool_calls: Option<usize>,
    /// 重复工具调用软提醒（默认开启，与 DSH `repeat-tool-reminder` 一致）。
    repeat_reminders: RepeatToolReminderConfig,
    reminder_log: Vec<RepeatToolReminder>,
}

impl<P: ModelProvider> Agent<P> {
    /// 默认预算来自环境变量；默认（未设置）与 DSH 一致：不限制工具调用次数/step 数。
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            turn: 0,
            budget: ToolBudget::from_env(),
            max_parallel_tool_calls: None,
            repeat_reminders: RepeatToolReminderConfig::from_env(),
            reminder_log: Vec::new(),
        }
    }

    /// 显式注入预算：测试与需要确定性上限的调用方用。
    pub fn with_budget(provider: P, budget: ToolBudget) -> Self {
        Self {
            provider,
            turn: 0,
            budget,
            max_parallel_tool_calls: None,
            repeat_reminders: RepeatToolReminderConfig::from_env(),
            reminder_log: Vec::new(),
        }
    }

    /// 覆盖每步并行工具数（`None` = 回到 env/默认 10；`0` 视为 1）。
    pub fn with_max_parallel_tool_calls(mut self, max_parallel: Option<usize>) -> Self {
        self.max_parallel_tool_calls = max_parallel.filter(|value| *value > 0);
        self
    }

    /// 当前生效的并行上限：显式覆盖 > `DSHR_MAX_PARALLEL_TOOL_CALLS` > 默认 10。
    pub fn max_parallel_tool_calls(&self) -> usize {
        self.max_parallel_tool_calls
            .or_else(|| parse_budget_env(MAX_PARALLEL_TOOL_CALLS_ENV))
            .unwrap_or(DEFAULT_MAX_PARALLEL_TOOL_CALLS)
            .max(1)
    }

    /// 显式注入重复提醒配置（测试 / 静默运行）。
    pub fn with_repeat_reminders(mut self, config: RepeatToolReminderConfig) -> Self {
        self.repeat_reminders = config;
        self
    }

    pub fn repeat_reminder_config(&self) -> &RepeatToolReminderConfig {
        &self.repeat_reminders
    }

    /// 已触发的软提醒（累计；宿主可在每轮结束后落成 `tool.repeat` 诊断）。
    pub fn repeat_reminder_log(&self) -> &[RepeatToolReminder] {
        &self.reminder_log
    }

    /// 取走已触发的软提醒（调用方负责写 Session Log）。
    pub fn take_repeat_reminders(&mut self) -> Vec<RepeatToolReminder> {
        std::mem::take(&mut self.reminder_log)
    }

    pub fn turn_count(&self) -> u64 {
        self.turn
    }

    pub fn budget(&self) -> ToolBudget {
        self.budget
    }

    /// 访问 provider：上下文压缩需要在 Agent 之外调用一次模型（摘要）。
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// 跑一轮（无历史版本，测试与单轮场景）。
    ///
    /// step 数只受 [`ToolBudget::max_steps`] 约束（默认无限）；工具调用总数只受
    /// [`ToolBudget::max_tool_calls`] 约束（默认无限，与官方 DSH 一致）。
    ///
    /// 事件顺序保证：`TurnStarted` → (`StepStarted` → `TextDelta`* →
    /// (`ToolCall`* → (`ToolResult` | `ToolCancelled`)*)?) → `TurnEnded` /
    /// `TurnFailed` / `TurnCancelled`。同一 step 的工具调用按批执行：先按原顺序发
    /// `ToolCall`，再按原顺序发 paired 终态（M3 并行工具，DSH `maxParallelToolCalls=10`）。
    /// 预算命中是**降级**，不会产生 `TurnFailed`；取消产生 `TurnCancelled`（含未执行
    /// 调用的 `ToolCancelled`），同样不是失败。
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
    ///
    /// 取消语义（M2 review P1-2；M3 并行批）：UI 通过 provider 的
    /// [`ModelProvider::cancel_token`] 置位 token；本函数在四个检查点读它
    /// （`Relaxed` 足够：只关心最终可见性）：
    /// 1. 每步开始前（发 `StepStarted` 之前）；
    /// 2. 拿到模型回复之后、执行本步工具之前；
    /// 3. 本批工具执行前（batch 一个都还没发出时）；
    /// 4. 本批执行后（batch 内已发出的调用允许完成，但不再发起下一步）。
    ///
    /// 命中取消时：未发出的调用逐个发 `ToolCall` + `ToolCancelled`（事件配对），
    /// 已完成的调用保留其 `ToolResult`，最后以 `TurnCancelled` 收尾——**不会**产生
    /// `TurnFailed`。预算命中仍是"降级"（`ToolResult(is_error)`），与取消无关。
    pub fn run_turn_streaming(
        &mut self,
        history: &[ChatMessage],
        user_text: &str,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        self.run_turn_streaming_message(history, ChatMessage::user(user_text), executor, on_event);
    }

    pub fn run_turn_streaming_message(
        &mut self,
        history: &[ChatMessage],
        user_message: ChatMessage,
        executor: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(&AgentEvent),
    ) {
        debug_assert!(user_message.user_text().is_some());
        self.turn += 1;
        let turn_id = self.turn;
        on_event(&AgentEvent::TurnStarted { turn_id });

        // 取消句柄只读：清零由 UI 在每轮开始时做（见 trait 注释）。
        // 测试/Mock provider 没暴露句柄时，取消检查恒为 false。
        let cancel = self.provider.cancel_token();

        let mut messages: Vec<ChatMessage> = history.to_vec();
        let user_text = user_message.user_text().map(str::to_owned);
        messages.push(user_message);
        let mut turn_instructions = Vec::new();

        let mut step: u32 = 1;
        let mut calls = 0usize;
        let mut text = String::new();
        // 命中 max_tool_calls 后置位：本步剩余调用全部标记失败，本步结束后直接收尾。
        let mut budget_exhausted = false;
        // 重复工具调用软提醒：key = (tool, canonical arguments)，计数按“本轮”累计。
        let mut repeat_counts: HashMap<(String, String), usize> = HashMap::new();
        let mut pending_reminders: Vec<RepeatToolReminder> = Vec::new();

        loop {
            // 取消检查点 1：步与步之间，StepStarted 之前。
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }
            if let Some(max_steps) = self.budget.max_steps {
                if step as usize > max_steps {
                    // 优雅结束：不再发起模型请求，也不把整轮判失败。
                    break;
                }
            }
            on_event(&AgentEvent::StepStarted { turn_id, step });

            let mut tool_specs = executor.prepare_step();
            let context = executor.prepare_step_context(
                step,
                if step == 1 {
                    user_text.as_deref()
                } else {
                    None
                },
            );
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }
            tool_specs.retain(|tool| !context.unavailable_tools.contains(&tool.name));
            turn_instructions.extend(
                context
                    .turn_instructions
                    .into_iter()
                    .map(ChatMessage::System),
            );
            let request = CompletionRequest {
                messages: context
                    .system_sections
                    .into_iter()
                    .map(ChatMessage::System)
                    .chain(turn_instructions.iter().cloned())
                    .chain(messages.iter().cloned())
                    .collect(),
                tool_specs,
            };

            // provider 边流边回调；这里只负责把 delta 转成事件并累计全文。
            let mut streamed_any = false;
            let mut streamed_reasoning = false;
            let reply = {
                let mut forward = |delta: ModelDelta<'_>| match delta {
                    ModelDelta::Text(delta) if !delta.is_empty() => {
                        streamed_any = true;
                        text.push_str(delta);
                        on_event(&AgentEvent::TextDelta(delta.to_owned()));
                    }
                    ModelDelta::Reasoning(delta) if !delta.is_empty() => {
                        streamed_reasoning = true;
                        on_event(&AgentEvent::ReasoningDelta(delta.to_owned()));
                    }
                    _ => {}
                };
                match self
                    .provider
                    .complete_streaming_with_reasoning(&request, &mut forward)
                {
                    Ok(reply) => reply,
                    Err(err) => {
                        // provider 在取消后返回 Err（如 openai 非流式的 cancelled）不算失败：
                        // 统一收敛为 TurnCancelled，避免 UI 显示"处理失败：cancelled"（P2-10）。
                        if cancel_requested(cancel.as_ref()) {
                            on_event(&AgentEvent::TurnCancelled {
                                reason: CANCEL_REASON.to_owned(),
                            });
                        } else {
                            on_event(&AgentEvent::TurnFailed(err));
                        }
                        return;
                    }
                }
            };

            if !streamed_reasoning && !reply.reasoning.is_empty() {
                on_event(&AgentEvent::ReasoningDelta(reply.reasoning.clone()));
            }
            // provider 没走流式（或实现有 bug 只返回值）：补发一次，保证 UI 不空白。
            if !streamed_any && !reply.text.is_empty() {
                text.push_str(&reply.text);
                on_event(&AgentEvent::TextDelta(reply.text.clone()));
            }

            // 无论有没有工具调用，assistant 这一步都要进历史（协议要求配对）。
            if !reply.text.is_empty() || !reply.tool_calls.is_empty() {
                messages.push(ChatMessage::Assistant {
                    reasoning: reply.reasoning,
                    text: reply.text.clone(),
                    tool_calls: reply.tool_calls.clone(),
                });
            }

            // 取消检查点 2：模型回复之后、执行工具之前。本步所有调用都不执行，
            // 但每个调用仍发 ToolCall + ToolCancelled，保持事件配对。
            if cancel_requested(cancel.as_ref()) {
                cancel_calls(&reply.tool_calls, on_event);
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }

            if reply.tool_calls.is_empty() {
                break;
            }

            // 本 step 的工具调用按批执行（DSH `maxParallelToolCalls`，默认 10）：
            // 1) 按原顺序发 ToolCall、裁决预算/取消，收集"真正要执行"的批；
            // 2) 一次 execute_batch（执行器决定并发程度，默认串行）；
            // 3) 再按原顺序发 ToolResult / 写 tool 消息。
            let calls_in_step = reply.tool_calls.len();
            let mut batch_indices: Vec<usize> = Vec::with_capacity(calls_in_step);
            let mut batch_calls: Vec<ToolCallRequest> = Vec::with_capacity(calls_in_step);
            let mut executed = vec![false; calls_in_step];
            let mut outcomes: Vec<Option<ToolOutcome>> = (0..calls_in_step).map(|_| None).collect();

            // 阶段 1：按原顺序发 ToolCall + 预算/取消裁决。
            for (index, tool_call) in reply.tool_calls.iter().enumerate() {
                let call = tool_call_request(tool_call);
                // 预算耗尽也是"事件完整"的：调用与结果都要发，模型/UI 才能看到配对。
                on_event(&AgentEvent::ToolCall(call.clone()));

                // 取消检查点 3：执行前。当前调用不执行，发配对的 ToolCancelled；
                // 本步剩余调用同样补 ToolCall + ToolCancelled（配平事件流）。
                if cancel_requested(cancel.as_ref()) {
                    on_event(&AgentEvent::ToolCancelled {
                        call_id: call.call_id.clone(),
                        reason: CANCEL_REASON.to_owned(),
                    });
                    cancel_calls(&reply.tool_calls[index + 1..], on_event);
                    on_event(&AgentEvent::TurnCancelled {
                        reason: CANCEL_REASON.to_owned(),
                    });
                    return;
                }

                if let Some(max) = self.budget.max_tool_calls.filter(|max| calls >= *max) {
                    budget_exhausted = true;
                    outcomes[index] = Some(ToolOutcome::failure(format!(
                        "tool call budget exhausted ({max}): 本次调用未执行"
                    )));
                } else {
                    calls += 1;
                    executed[index] = true;
                    batch_indices.push(index);
                    batch_calls.push(call);
                }
            }

            // 阶段 2：一次批量执行（sidecar 可在 max_parallel 内并发）。
            if !batch_calls.is_empty() {
                let batch_outcomes =
                    executor.execute_batch(&batch_calls, self.max_parallel_tool_calls());
                for (slot, index) in batch_indices.iter().enumerate() {
                    outcomes[*index] =
                        Some(batch_outcomes.get(slot).cloned().unwrap_or_else(|| {
                            ToolOutcome::failure(
                                "batch executor returned fewer outcomes than calls",
                            )
                        }));
                }
            }

            // 阶段 3：按原顺序发 ToolResult / 写 tool 消息 / 统计重复提醒。
            let mut images = Vec::new();
            let mut image_bytes = 0;
            for index in 0..calls_in_step {
                let tool_call = &reply.tool_calls[index];
                let call = tool_call_request(tool_call);
                let outcome = outcomes[index]
                    .take()
                    .unwrap_or_else(|| ToolOutcome::failure("tool outcome missing for this call"));
                on_event(&AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    outcome: outcome.clone(),
                });
                messages.push(ChatMessage::Tool {
                    call_id: call.call_id.clone(),
                    content: tool_content_for_model(&outcome),
                });
                if !outcome.is_error {
                    for image in executor.tool_images(&outcome) {
                        let size = image.bytes.as_ref().map_or(0, |bytes| bytes.len());
                        if size > 0 && image_bytes + size <= 24 * 1024 * 1024 {
                            image_bytes += size;
                            images.push(image);
                        }
                    }
                }

                // 重复工具调用软提醒：只统计**真正执行**的调用；命中阈值先排队，
                // 本步工具都执行完后统一注入 System 消息（不打断配对协议）。
                if executed[index] && self.repeat_reminders.enabled {
                    let key = (call.tool.clone(), canonical_arguments(&call.arguments));
                    let count = repeat_counts.entry(key).or_insert(0);
                    *count += 1;
                    if self.repeat_reminders.thresholds.contains(count) {
                        pending_reminders.push(RepeatToolReminder::new(
                            &call.tool,
                            &call.arguments,
                            *count,
                        ));
                    }
                }
            }
            if !images.is_empty() {
                let mut remaining = 24 * 1024 * 1024 - image_bytes;
                for message in messages.iter_mut().rev() {
                    if let ChatMessage::ToolImages(previous) = message {
                        previous.retain(|image| {
                            let size = image.bytes.as_ref().map_or(0, |bytes| bytes.len());
                            if size > remaining {
                                return false;
                            }
                            remaining -= size;
                            true
                        });
                    }
                }
                messages.retain(|message| !matches!(message, ChatMessage::ToolImages(images) if images.is_empty()));
                messages.push(ChatMessage::ToolImages(images));
            }
            // 本步结束：把命中的软提醒注入历史（system 消息，软提醒不失败）。
            if !pending_reminders.is_empty() {
                for reminder in pending_reminders.drain(..) {
                    messages.push(ChatMessage::System(reminder.text.clone()));
                    self.reminder_log.push(reminder);
                }
            }

            // 取消检查点 4：批执行后。本批已发出的调用允许完成（结果已保留），
            // 但不再发起下一 step，直接 TurnCancelled 收尾。
            if cancel_requested(cancel.as_ref()) {
                on_event(&AgentEvent::TurnCancelled {
                    reason: CANCEL_REASON.to_owned(),
                });
                return;
            }
            step += 1;
            if budget_exhausted {
                // 不再给模型"继续发工具调用"的 step：预算已用尽，直接收尾。
                break;
            }
        }

        on_event(&AgentEvent::TurnEnded { text });
    }
}

/// 取消 token 是否已置位。无句柄（Mock/测试 provider）= 永不取消。
fn cancel_requested(cancel: Option<&Arc<AtomicBool>>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Relaxed))
}

/// `ModelToolCall` → 事件用请求（id 是配对依据，必须原样保留）。
fn tool_call_request(tool_call: &ModelToolCall) -> ToolCallRequest {
    ToolCallRequest {
        call_id: tool_call.id.clone(),
        tool: tool_call.name.clone(),
        arguments: tool_call.arguments.clone(),
    }
}

/// 参数规范化：`serde_json::Value` 的数字/对象序列化是确定性的（对象键有序），
/// 用来判断“同一工具 + 同一参数”。
fn canonical_arguments(arguments: &Value) -> String {
    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string())
}

/// 取消后本步剩余的调用：只发 `ToolCall` + `ToolCancelled`，一个都不执行。
fn cancel_calls(calls: &[ModelToolCall], on_event: &mut dyn FnMut(&AgentEvent)) {
    for tool_call in calls {
        on_event(&AgentEvent::ToolCall(tool_call_request(tool_call)));
        on_event(&AgentEvent::ToolCancelled {
            call_id: tool_call.id.clone(),
            reason: CANCEL_REASON.to_owned(),
        });
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

mod mock;
mod model;
mod tool_limits;
mod tools;

pub use mock::MockProvider;
pub use model::*;
use tool_limits::parse_budget_env;
pub use tool_limits::*;
pub use tools::*;

#[cfg(test)]
mod tests;
