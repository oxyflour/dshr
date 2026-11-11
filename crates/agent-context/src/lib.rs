//! 上下文工程：token 计量、超长工具结果裁剪、按预算截断历史。
//!
//! 两条硬约束：
//! 1. **回放一致**：历史完全从 durable 事件重建，不读内存状态、不读插件；
//!    同一份 JSONL 必须得到同一串消息。
//! 2. **协议合法**：`tool.call` / `tool.result` 折叠成
//!    `assistant{tool_calls}` + `tool{call_id, content}`，配对关系逐条可断言。
//!
//! 预算策略（M2）：
//! - 估算：CJK 约 1 token/字，其余约 1 token/4 字符（偏保守，不依赖 tokenizer）；
//! - 触发：历史 + 本轮用户消息 > `window * 0.8`；
//! - 裁剪：先把超过 800 token 的工具结果截成"头部 + 尾部"；
//! - 截断：仍超预算则从最旧的对话开始丢，直到降到 `window * 0.6`，且绝不从
//!   `Tool` 消息开始（那会拆散 `assistant.tool_calls` 配对）。
//!
//! 真正的"摘要压缩"（`context.compacted` + LLM 摘要）是下一步，当前是**有损裁剪**：
//! 被丢掉的消息仍保留在 Session Log 里，只是不再进模型历史。
//!
//! 外部化（spill）见 [`spill`] 模块：超过阈值（默认 16 KiB）的工具结果落盘为
//! 内容寻址引用，模型只见引用 + 开头预览。**既有裁剪/丢弃逻辑保持不变**：
//! `plan_with_window` 在日志没有 `spill.created` 时与引入外部化前逐字节一致，
//! 低于 spill 阈值的超长结果仍走原来的 head/tail 裁剪。

#![forbid(unsafe_code)]

mod sha256;
pub mod spill;

pub use spill::{PendingSpill, RecordedSpill, Retention, SpillPolicy, SpillRef, SpillStore};

use std::collections::{HashMap, HashSet};

use dshr_agent_core::{ChatMessage, ModelToolCall};
use dshr_session_store::{Durability, SessionLog};
use serde_json::Value;

/// 默认保留的历史消息条数（无预算信息时的兜底）。
pub const DEFAULT_MAX_MESSAGES: usize = 60;

/// 默认上下文窗口（token）。DeepSeek `deepseek-chat` 为 64K，可用
/// `DSHR_CONTEXT_WINDOW` 覆盖。
pub const DEFAULT_CONTEXT_WINDOW: usize = 64_000;

/// 触发裁剪的窗口占比。
pub const TRIGGER_RATIO: f32 = 0.8;

/// 裁剪后希望降到窗口占比（留出输出与工具 schema 余量）。
pub const TARGET_RATIO: f32 = 0.6;

/// 单条工具结果超过这个 token 数就做头尾裁剪。
pub const TOOL_RESULT_TOKEN_LIMIT: usize = 800;

/// 裁剪时保留的头部/尾部字符数。
const TOOL_RESULT_HEAD_CHARS: usize = 1_200;
const TOOL_RESULT_TAIL_CHARS: usize = 400;

/// 从环境变量读上下文窗口。
pub fn context_window_from_env() -> usize {
    std::env::var("DSHR_CONTEXT_WINDOW")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|window| *window >= 4_000)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// 估算一段文本的 token 数。CJK 按字计，其它按 4 字符/token，再加少量消息开销。
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4) + 4
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF
    )
}

/// 估算整段历史的 token 数。
pub fn estimate_history_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| match message {
            ChatMessage::System(text) | ChatMessage::User(text) => estimate_tokens(text) + 4,
            ChatMessage::Assistant { text, tool_calls } => {
                estimate_tokens(text)
                    + tool_calls
                        .iter()
                        .map(|call| {
                            estimate_tokens(&call.name)
                                + estimate_tokens(&call.arguments.to_string())
                        })
                        .sum::<usize>()
                    + 8
            }
            ChatMessage::Tool { content, .. } => estimate_tokens(content) + 4,
        })
        .sum()
}

/// 一次请求的上下文计划：实际发给模型的历史 + 计量结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextPlan {
    pub messages: Vec<ChatMessage>,
    /// 计量窗口（token）。
    pub window: usize,
    /// 计划前的估算（历史 + 本轮用户消息）。
    pub tokens_before: usize,
    /// 计划后的估算。
    pub tokens_after: usize,
    /// 被头尾裁剪的工具结果条数。
    pub pruned_tool_results: usize,
    /// 因超预算丢弃的历史消息条数。
    pub dropped_messages: usize,
    /// 被丢弃的前缀消息（用于生成压缩摘要）。
    pub dropped: Vec<ChatMessage>,
    /// 被丢弃消息覆盖的事件区间 `[from_seq, to_seq]`；写 `context.compacted` 用。
    pub dropped_range: Option<(u64, u64)>,
    /// 本次计划**新产生**的外部化（调用方负责落盘并写 `spill.created` /
    /// `tool.result.pruned`；已留痕的外部化不会再出现在这里）。
    pub spilled: Vec<PendingSpill>,
}

impl ContextPlan {
    pub fn ratio(&self) -> f64 {
        if self.window == 0 {
            return 0.0;
        }
        self.tokens_after as f64 / self.window as f64
    }

    pub fn triggered(&self) -> bool {
        self.pruned_tool_results > 0 || self.dropped_messages > 0
    }

    /// 本次计划刚外部化的结果条数（已留痕的不重复计数）。
    pub fn spilled_tool_results(&self) -> usize {
        self.spilled.len()
    }
}

/// 按默认策略为下一轮请求生成上下文计划。
pub fn plan_context(log: &SessionLog, next_user_text: &str) -> ContextPlan {
    plan_with_window(log, next_user_text, context_window_from_env())
}

/// 带外部化的默认策略版本：超过 [`SpillPolicy::threshold_bytes`] 的工具结果落盘为
/// 内容寻址引用，模型只看到引用 + 开头预览。
///
/// 调用方顺序（`chat` 接线）：
/// 1. `plan_context_with_spill(&log, "", &policy)`；
/// 2. 对 `plan.spilled` 逐条 `SpillStore::put_pending` → `spill::record_spill` → `save`；
/// 3. 重新 `plan_context_with_spill`（此时 `spilled` 为空，历史从事件确定性还原）。
pub fn plan_context_with_spill(
    log: &SessionLog,
    next_user_text: &str,
    policy: &SpillPolicy,
) -> ContextPlan {
    plan_with_window_and_spill(log, next_user_text, context_window_from_env(), policy)
}

/// 可注入窗口的版本（测试/自检用）。行为与引入外部化前完全一致。
pub fn plan_with_window(log: &SessionLog, next_user_text: &str, window: usize) -> ContextPlan {
    plan_with_window_and_spill(log, next_user_text, window, &SpillPolicy::disabled())
}

/// 可注入窗口 + 外部化策略的版本（测试/自检用）。
///
/// 日志中已留痕的 `spill.created` / `tool.result.pruned` 始终参与历史重建，即使传
/// `disabled()` 策略；只有"新产生"的外部化受 `policy` 控制。
pub fn plan_with_window_and_spill(
    log: &SessionLog,
    next_user_text: &str,
    window: usize,
    policy: &SpillPolicy,
) -> ContextPlan {
    let window = window.max(1);
    let recorded = spill::recorded_spills(log);
    let policy = (policy.threshold_bytes != usize::MAX).then_some(policy);
    let needs_externalization = policy.is_some() || !recorded.is_empty();
    let mut pending: Vec<PendingSpill> = Vec::new();
    let full = if needs_externalization {
        history_with_seqs_spilled(
            log,
            Some(SpillStage {
                policy,
                recorded: &recorded,
                pending: &mut pending,
            }),
        )
    } else {
        history_with_seqs(log)
    };
    let full_messages: Vec<ChatMessage> =
        full.iter().map(|wrapped| wrapped.message.clone()).collect();
    // `tokens_before` 始终表示"未外部化前"的估算，方便对比外部化省下的量。
    let tokens_before = if needs_externalization {
        let raw: Vec<ChatMessage> = history_with_seqs(log)
            .into_iter()
            .map(|wrapped| wrapped.message)
            .collect();
        estimate_history_tokens(&raw) + estimate_tokens(next_user_text)
    } else {
        estimate_history_tokens(&full_messages) + estimate_tokens(next_user_text)
    };
    let mut working = full.clone();
    let mut tokens = estimate_history_tokens(&full_messages) + estimate_tokens(next_user_text);

    let trigger = (window as f32 * TRIGGER_RATIO) as usize;
    if tokens <= trigger {
        return ContextPlan {
            messages: full_messages,
            window,
            tokens_before,
            tokens_after: tokens,
            pruned_tool_results: 0,
            dropped_messages: 0,
            dropped: Vec::new(),
            dropped_range: None,
            spilled: pending,
        };
    }

    // 1. 超长工具结果做头尾裁剪（未外部化的长结果，行为与引入 spill 前一致）。
    let mut pruned_tool_results = 0usize;
    for wrapped in &mut working {
        if let ChatMessage::Tool { content, .. } = &mut wrapped.message {
            if estimate_tokens(content) > TOOL_RESULT_TOKEN_LIMIT {
                *content = prune_tool_result(content);
                pruned_tool_results += 1;
            }
        }
    }
    tokens = estimate_history_tokens(
        &working
            .iter()
            .map(|wrapped| wrapped.message.clone())
            .collect::<Vec<_>>(),
    ) + estimate_tokens(next_user_text);

    // 2. 仍超预算：从最旧的对话丢起，直到降到目标占比。
    let mut dropped = Vec::new();
    let mut dropped_range = None;
    if tokens > trigger {
        let target = (window as f32 * TARGET_RATIO) as usize;
        let (kept, removed) = trim_to_budget(&working, target, estimate_tokens(next_user_text));
        if !removed.is_empty() {
            dropped_range = Some((
                removed
                    .first()
                    .map(|wrapped| wrapped.start_seq)
                    .unwrap_or(0),
                removed.last().map(|wrapped| wrapped.end_seq).unwrap_or(0),
            ));
            dropped = removed
                .iter()
                .map(|wrapped| wrapped.message.clone())
                .collect();
        }
        working = kept;
        tokens = estimate_history_tokens(
            &working
                .iter()
                .map(|wrapped| wrapped.message.clone())
                .collect::<Vec<_>>(),
        ) + estimate_tokens(next_user_text);
    }

    ContextPlan {
        messages: working
            .iter()
            .map(|wrapped| wrapped.message.clone())
            .collect(),
        window,
        tokens_before,
        tokens_after: tokens,
        pruned_tool_results,
        dropped_messages: dropped.len(),
        dropped,
        dropped_range,
        spilled: pending,
    }
}

/// 头尾裁剪：保留开头与结尾，中间标注被裁掉多少字符。
fn prune_tool_result(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let total = chars.len();
    if total <= TOOL_RESULT_HEAD_CHARS + TOOL_RESULT_TAIL_CHARS {
        return content.to_owned();
    }
    let omitted = total - TOOL_RESULT_HEAD_CHARS - TOOL_RESULT_TAIL_CHARS;
    let head: String = chars[..TOOL_RESULT_HEAD_CHARS].iter().collect();
    let tail: String = chars[total - TOOL_RESULT_TAIL_CHARS..].iter().collect();
    format!("{head}\n…[已裁剪 {omitted} 字符；原结果保留在 Session Log 中]…\n{tail}")
}

/// 从尾部往前累计到预算内，并把起点调到合法的 `User` 消息。
/// 返回 `(保留, 丢弃)`；丢弃的是一段前缀，可直接映射成事件区间。
fn trim_to_budget(
    messages: &[SeqMessage],
    target_tokens: usize,
    user_text_tokens: usize,
) -> (Vec<SeqMessage>, Vec<SeqMessage>) {
    let budget = target_tokens.saturating_sub(user_text_tokens);
    let mut used = 0usize;
    let mut start = messages.len();
    while start > 0 {
        let cost = single_message_tokens(&messages[start - 1].message);
        if used + cost > budget && start < messages.len() {
            break;
        }
        used += cost;
        start -= 1;
    }
    // 不从 Tool 开始：它对应的 assistant.tool_calls 被丢掉了。
    while start < messages.len() && matches!(messages[start].message, ChatMessage::Tool { .. }) {
        start += 1;
    }
    // 尽量从 User 开始，避免历史以孤立的 assistant 开头。
    if let Some(user_index) = messages[start..]
        .iter()
        .position(|wrapped| matches!(wrapped.message, ChatMessage::User(_)))
    {
        start += user_index;
    }
    let dropped = messages[..start].to_vec();
    (messages[start..].to_vec(), dropped)
}

fn single_message_tokens(message: &ChatMessage) -> usize {
    estimate_history_tokens(std::slice::from_ref(message))
}

/// 带事件区间的一条消息：压缩时用 `start_seq..end_seq` 标记被替换的原始事件。
#[derive(Debug, Clone, PartialEq)]
struct SeqMessage {
    start_seq: u64,
    end_seq: u64,
    message: ChatMessage,
}

/// 历史重建时的外部化上下文：已留痕的记录（始终生效）+ 新策略（可关闭）+ 待落盘队列。
struct SpillStage<'a> {
    policy: Option<&'a SpillPolicy>,
    recorded: &'a HashMap<String, spill::RecordedSpill>,
    pending: &'a mut Vec<PendingSpill>,
}

/// 从日志重建**带 seq 区间**的消息（不截断、不应用外部化，原始视图）。
fn history_with_seqs(log: &SessionLog) -> Vec<SeqMessage> {
    history_with_seqs_spilled(log, None)
}

/// 同 [`history_with_seqs`]，但按 `spill` 把超大工具结果替换成引用 + 预览。
///
/// - 已留痕的 `spill.created` / `tool.result.pruned`：确定性还原引用预览；
/// - 未留痕且超过策略阈值：生成 [`PendingSpill`]，模型消息先用引用（调用方随后落盘 + 写事件）。
fn history_with_seqs_spilled(
    log: &SessionLog,
    mut spill: Option<SpillStage<'_>>,
) -> Vec<SeqMessage> {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut summary_ids: HashSet<String> = HashSet::new();
    for event in log.events() {
        if event.durability == Durability::Ephemeral {
            continue;
        }
        if event.event_type != "context.compacted" {
            continue;
        }
        let from = num_of(&event.payload, "from_seq");
        let to = num_of(&event.payload, "to_seq");
        if from > 0 && to >= from {
            ranges.push((from, to));
        }
        let summary_id = str_of(&event.payload, "summary_message_id");
        if !summary_id.is_empty() {
            summary_ids.insert(summary_id);
        }
    }

    let mut summary_text: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for event in log.events() {
        if event.event_type != "message.assistant.settled" {
            continue;
        }
        let message_id = str_of(&event.payload, "message_id");
        if summary_ids.contains(&message_id) {
            summary_text.insert(message_id, text_of(&event.payload));
        }
    }

    let in_range = |seq: u64| ranges.iter().any(|(from, to)| seq >= *from && seq <= *to);

    let mut messages: Vec<SeqMessage> = Vec::new();
    let mut pending: HashSet<String> = HashSet::new();
    let mut settled: HashSet<String> = HashSet::new();

    for event in log.events() {
        if event.durability == Durability::Ephemeral || in_range(event.seq) {
            continue;
        }
        match event.event_type.as_str() {
            "message.user" => {
                let text = text_of(&event.payload);
                if !text.is_empty() {
                    messages.push(SeqMessage {
                        start_seq: event.seq,
                        end_seq: event.seq,
                        message: ChatMessage::user(text),
                    });
                }
            }
            "message.assistant.settled" => {
                let message_id = str_of(&event.payload, "message_id");
                // 摘要消息只经 context.compacted 还原，不重复出现。
                if summary_ids.contains(&message_id) {
                    continue;
                }
                // 空文本（例如纯工具轮次）不产生消息：空 assistant 只会干扰模型。
                let text = text_of(&event.payload);
                if !text.is_empty() {
                    messages.push(SeqMessage {
                        start_seq: event.seq,
                        end_seq: event.seq,
                        message: ChatMessage::Assistant {
                            text,
                            tool_calls: Vec::new(),
                        },
                    });
                }
            }
            "tool.call" => {
                let call_id = str_of(&event.payload, "call_id");
                if call_id.is_empty() {
                    continue;
                }
                let call = ModelToolCall {
                    id: call_id.clone(),
                    name: str_of(&event.payload, "tool"),
                    arguments: event
                        .payload
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Null),
                };
                pending.insert(call_id);
                // 工具调用必须挂在 assistant 消息上；若上一条不是 assistant，补一条空的。
                match messages.last_mut() {
                    Some(wrapped) => match &mut wrapped.message {
                        ChatMessage::Assistant { tool_calls, .. } => {
                            tool_calls.push(call);
                            wrapped.end_seq = event.seq;
                        }
                        _ => messages.push(SeqMessage {
                            start_seq: event.seq,
                            end_seq: event.seq,
                            message: ChatMessage::Assistant {
                                text: String::new(),
                                tool_calls: vec![call],
                            },
                        }),
                    },
                    None => messages.push(SeqMessage {
                        start_seq: event.seq,
                        end_seq: event.seq,
                        message: ChatMessage::Assistant {
                            text: String::new(),
                            tool_calls: vec![call],
                        },
                    }),
                }
            }
            "tool.result" | "tool.aborted" => {
                let call_id = str_of(&event.payload, "call_id");
                // 没有对应调用（日志残缺）就跳过：宁可少一条，也不要非法序列。
                if !pending.contains(&call_id) || settled.contains(&call_id) {
                    continue;
                }
                settled.insert(call_id.clone());
                let content = if event.event_type == "tool.aborted" {
                    format!("ERROR: 工具已中止（{}）", str_of(&event.payload, "reason"))
                } else {
                    let content_value =
                        event.payload.get("content").cloned().unwrap_or(Value::Null);
                    let text = content_text(&content_value);
                    if event
                        .payload
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        format!("ERROR: {text}")
                    } else {
                        externalize_tool_content(&call_id, &text, &content_value, spill.as_mut())
                    }
                };
                messages.push(SeqMessage {
                    start_seq: event.seq,
                    end_seq: event.seq,
                    message: ChatMessage::Tool { call_id, content },
                });
            }
            "context.compacted" => {
                let summary_id = str_of(&event.payload, "summary_message_id");
                let text = summary_text
                    .get(&summary_id)
                    .cloned()
                    .unwrap_or_else(|| String::from("（历史已压缩，摘要缺失）"));
                messages.push(SeqMessage {
                    start_seq: event.seq,
                    end_seq: event.seq,
                    message: ChatMessage::System(format!("以下是早前对话的摘要：\n{text}")),
                });
            }
            _ => {}
        }
    }

    messages
}

/// 模型可见的工具结果文本：已留痕的外部化按事件还原引用；否则按策略判断是否新外部化。
fn externalize_tool_content(
    call_id: &str,
    text: &str,
    content: &Value,
    stage: Option<&mut SpillStage<'_>>,
) -> String {
    let Some(stage) = stage else {
        return text.to_owned();
    };
    if let Some(recorded) = stage.recorded.get(call_id) {
        let preview = spill::preview_of(text, recorded.preview_chars);
        return spill::render_reference(&recorded.spill, &preview);
    }
    if let Some(policy) = stage.policy {
        if let Some((spill_ref, payload)) = policy.classify(content) {
            let preview = spill::preview_of(text, policy.preview_chars);
            stage.pending.push(PendingSpill {
                call_id: call_id.to_owned(),
                spill: spill_ref.clone(),
                payload,
                preview_chars: policy.preview_chars,
            });
            return spill::render_reference(&spill_ref, &preview);
        }
    }
    text.to_owned()
}

fn num_of(payload: &Value, field: &str) -> u64 {
    payload.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// 从日志重建全部历史消息（不截断，原始视图：已外部化的结果仍是完整内容）。
pub fn history_from_log(log: &SessionLog) -> Vec<ChatMessage> {
    history_with_seqs(log)
        .into_iter()
        .map(|wrapped| wrapped.message)
        .collect()
}

/// 从日志重建**模型可见**的全部历史：已留痕的外部化结果替换成引用 + 预览。
///
/// 与 `plan_with_window(..)` 的重建规则一致（都不新产生外部化），供 UI/自检
/// 展示"模型当时实际看到的历史"。
pub fn history_from_log_with_spill(log: &SessionLog) -> Vec<ChatMessage> {
    let recorded = spill::recorded_spills(log);
    let mut pending = Vec::new();
    history_with_seqs_spilled(
        log,
        Some(SpillStage {
            policy: None,
            recorded: &recorded,
            pending: &mut pending,
        }),
    )
    .into_iter()
    .map(|wrapped| wrapped.message)
    .collect()
}

/// 对话历史 + 截断，供 Agent 直接使用。
pub fn history_for_model(log: &SessionLog) -> Vec<ChatMessage> {
    trim_history(history_from_log(log), DEFAULT_MAX_MESSAGES)
}

/// 截断到最近 `max_messages` 条，且绝不从一条 `Tool` 消息开始
/// （那意味着它对应的 `assistant.tool_calls` 被截掉了，是非法序列）。
pub fn trim_history(messages: Vec<ChatMessage>, max_messages: usize) -> Vec<ChatMessage> {
    if max_messages == 0 {
        return Vec::new();
    }
    let mut messages = messages;
    if messages.len() <= max_messages {
        return messages;
    }
    let mut start = messages.len() - max_messages;
    while start < messages.len() && matches!(messages[start], ChatMessage::Tool { .. }) {
        start += 1;
    }
    messages.split_off(start)
}

fn str_of(payload: &Value, field: &str) -> String {
    payload
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn text_of(payload: &Value) -> String {
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    content_text(&payload.get("content").cloned().unwrap_or(Value::Null))
}

fn content_text(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().map(str::to_owned).unwrap_or_default();
    };
    blocks
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
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
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
    fn demo_session_rebuilds_a_protocol_legal_history() {
        let log =
            SessionLog::from_jsonl("s-demo", dshr_conversation_model_session()).expect("demo");
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
        assert!(messages.iter().any(
            |message| matches!(message, ChatMessage::System(text) if text.contains("摘要缺失"))
        ));
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
        assert!(
            matches!(tools[1], ChatMessage::Tool { content, .. } if content.contains("timeout"))
        );
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
}
