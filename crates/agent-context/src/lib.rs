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

use dshr_agent_core::{
    tool_image_references, AttachmentFormat, ChatMessage, MediaAttachment, ModelToolCall,
};
use dshr_session_store::{Durability, SessionLog, ToolCallKey};
use serde_json::Value;

/// 默认保留的历史消息条数（无预算信息时的兜底）。
pub const DEFAULT_MAX_MESSAGES: usize = 60;

/// 默认上下文窗口（token）的兜底值：模型名无法识别时用它。
///
/// 官方 DSH 0.2.2-rc.1 的 DeepSeek 适配器对 `deepseek-flash` / `deepseek-v4-pro`
/// 使用 `OFFICIAL_CONTEXT_WINDOW = 1_048_576`；dshr 的两个默认模型
/// （`deepseek-chat` / `gpt-4o-mini`）都是 128K，所以未知模型也按 128K 兜底。
/// 更小窗口的模型请显式设置 `DSHR_CONTEXT_WINDOW`。
pub const DEFAULT_CONTEXT_WINDOW: usize = 131_072;

/// DSH 官方 1M 模型窗口（`@deepseek-ai/dsh-llm-deepseek` 的 `OFFICIAL_CONTEXT_WINDOW`）。
pub const DSH_OFFICIAL_CONTEXT_WINDOW: usize = 1_048_576;

/// DeepSeek 现行 API 的 128K 上下文上限（`deepseek-chat` / `deepseek-reasoner`）。
pub const DEEPSEEK_API_CONTEXT_WINDOW: usize = 131_072;

/// OpenAI 主流模型（gpt-4o / gpt-4.1 / o3 / o4）的 128K 窗口。
pub const OPENAI_CONTEXT_WINDOW: usize = 128_000;

/// 触发裁剪的窗口占比（DSH `thresholdRatio=0.8`，已对齐）。
pub const TRIGGER_RATIO: f32 = 0.8;

/// DSH 压缩后的保留比：`retainRatio=0.16`（保留约 16% 窗口的消息）。
///
/// 官方 `@deepseek-ai/dsh-compaction-basic` 用 `DEFAULT_RETAIN_RATIO = .16`：
/// 触发压缩后把历史压到窗口的 16% 以内，再交给 LLM 生成摘要。
pub const DSH_RETAIN_RATIO: f32 = 0.16;

/// 旧 dshr 保留比（裁到 0.6）。仅作为 `DSHR_CONTEXT_RETAIN_RATIO=0.6` 的
/// 兼容回退值保留；正常路径默认取 [`DSH_RETAIN_RATIO`]。
pub const TARGET_RATIO: f32 = 0.6;

/// 覆盖保留比的环境变量（如 `0.6` 回退旧行为；范围 0.05–1.0，非法忽略）。
pub const RETAIN_RATIO_ENV: &str = "DSHR_CONTEXT_RETAIN_RATIO";

/// 单条工具结果超过这个 token 数就做头尾裁剪。
pub const TOOL_RESULT_TOKEN_LIMIT: usize = 800;

/// 裁剪时保留的头部/尾部字符数。
const TOOL_RESULT_HEAD_CHARS: usize = 1_200;
const TOOL_RESULT_TAIL_CHARS: usize = 400;

/// 从环境变量解析上下文窗口：
///
/// 1. 显式 `DSHR_CONTEXT_WINDOW`（>= 4000）优先；
/// 2. 否则按已配置的模型名给默认（`DSHR_MODEL` > `DEEPSEEK_MODEL` > `OPENAI_MODEL`）：
///    `flash` / `v4-pro` / `v4.1` → 1M（DSH 官方窗口），
///    `deepseek-chat` / `deepseek-reasoner` → 128K（DeepSeek API 上限），
///    `gpt-4o` / `gpt-4.1` / `o3` / `o4` → 128K；
/// 3. 其余（含未配置模型）回退 [`DEFAULT_CONTEXT_WINDOW`]（128K，两家当前默认模型口径）。
pub fn context_window_from_env() -> usize {
    let explicit = std::env::var("DSHR_CONTEXT_WINDOW").ok();
    let generic_model = std::env::var("DSHR_MODEL").ok();
    let deepseek_model = std::env::var("DEEPSEEK_MODEL").ok();
    let openai_model = std::env::var("OPENAI_MODEL").ok();
    resolve_context_window(
        explicit.as_deref(),
        generic_model.as_deref(),
        deepseek_model.as_deref(),
        openai_model.as_deref(),
    )
}

/// 纯函数版窗口解析（测试与显式注入用；不读环境）。
pub fn resolve_context_window(
    explicit: Option<&str>,
    generic_model: Option<&str>,
    deepseek_model: Option<&str>,
    openai_model: Option<&str>,
) -> usize {
    if let Some(window) = explicit
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|window| *window >= 4_000)
    {
        return window;
    }
    let model = [generic_model, deepseek_model, openai_model]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|model| !model.is_empty());
    model.map_or(DEFAULT_CONTEXT_WINDOW, context_window_for_model)
}

/// 单个模型名 → 上下文窗口。
fn context_window_for_model(model: &str) -> usize {
    let model = model.to_ascii_lowercase();
    if model.contains("flash") || model.contains("v4-pro") || model.contains("v4.1") {
        DSH_OFFICIAL_CONTEXT_WINDOW
    } else if model.contains("deepseek-chat") || model.contains("deepseek-reasoner") {
        DEEPSEEK_API_CONTEXT_WINDOW
    } else if model.contains("gpt-4o")
        || model.contains("gpt-4.1")
        || model.contains("o3")
        || model.contains("o4")
    {
        OPENAI_CONTEXT_WINDOW
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
}

/// 保留比解析（纯函数，测试与显式注入用）：范围 0.05–1.0，非法/越界回退 DSH 默认。
pub fn resolve_retain_ratio(explicit: Option<&str>) -> f32 {
    explicit
        .and_then(|raw| raw.trim().parse::<f32>().ok())
        .filter(|ratio| (0.05..=1.0).contains(ratio))
        .unwrap_or(DSH_RETAIN_RATIO)
}

/// 从 `DSHR_CONTEXT_RETAIN_RATIO` 读取保留比；未设置/非法回退 DSH 默认 0.16。
pub fn retain_ratio_from_env() -> f32 {
    resolve_retain_ratio(std::env::var(RETAIN_RATIO_ENV).ok().as_deref())
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
            ChatMessage::UserWithAttachments { text, attachments } => {
                estimate_tokens(text)
                    + 4
                    + attachments
                        .iter()
                        .map(|attachment| {
                            estimate_tokens(&attachment.name)
                                + if attachment.format == AttachmentFormat::Pdf {
                                    16_384
                                } else {
                                    1_536
                                }
                        })
                        .sum::<usize>()
            }
            ChatMessage::Assistant {
                reasoning,
                text,
                tool_calls,
            } => {
                estimate_tokens(text)
                    + estimate_tokens(reasoning)
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
            ChatMessage::ToolImages(images) => 4 + 1_536 * images.len(),
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

/// 可注入窗口的版本（测试/自检用）。保留比取自 [`retain_ratio_from_env`]
///（默认 DSH `retainRatio=0.16`；`DSHR_CONTEXT_RETAIN_RATIO=0.6` 可回退旧行为）。
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
    plan_with_window_and_spill_ratio(log, next_user_text, window, policy, retain_ratio_from_env())
}

/// 显式保留比版本（测试/统计用）：`retain_ratio` 是压缩后目标窗口占比。
pub fn plan_with_window_and_retain_ratio(
    log: &SessionLog,
    next_user_text: &str,
    window: usize,
    retain_ratio: f32,
) -> ContextPlan {
    plan_with_window_and_spill_ratio(
        log,
        next_user_text,
        window,
        &SpillPolicy::disabled(),
        retain_ratio,
    )
}

/// 可注入窗口 + 外部化策略 + 保留比的底层实现。
pub fn plan_with_window_and_spill_ratio(
    log: &SessionLog,
    next_user_text: &str,
    window: usize,
    policy: &SpillPolicy,
    retain_ratio: f32,
) -> ContextPlan {
    let window = window.max(1);
    let retain_ratio = if (0.05..=1.0).contains(&retain_ratio) {
        retain_ratio
    } else {
        DSH_RETAIN_RATIO
    };
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

    // 2. 仍超预算：从最旧的对话丢起，直到降到保留比（DSH retainRatio，默认 0.16）。
    let mut dropped = Vec::new();
    let mut dropped_range = None;
    if tokens > trigger {
        let target = (window as f32 * retain_ratio) as usize;
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
    while start < messages.len()
        && matches!(
            messages[start].message,
            ChatMessage::Tool { .. } | ChatMessage::ToolImages(_)
        )
    {
        start += 1;
    }
    // 尽量从 User 开始，避免历史以孤立的 assistant 开头。
    if let Some(user_index) = messages[start..]
        .iter()
        .position(|wrapped| wrapped.message.user_text().is_some())
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
    recorded: &'a HashMap<ToolCallKey, spill::RecordedSpill>,
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
    let mut pending = HashSet::new();
    let mut settled = HashSet::new();
    let mut reasoning = String::new();
    let mut step_images: Option<SeqMessage> = None;

    for event in log.events() {
        if event.durability == Durability::Ephemeral || in_range(event.seq) {
            continue;
        }
        if matches!(
            event.event_type.as_str(),
            "message.user"
                | "message.assistant.reasoning"
                | "message.assistant.settled"
                | "tool.call"
                | "turn.ended"
                | "context.compacted"
        ) {
            messages.extend(step_images.take());
        }
        match event.event_type.as_str() {
            "message.user" => {
                reasoning.clear();
                if let Some(message) = user_message_from_payload(&event.payload) {
                    messages.push(SeqMessage {
                        start_seq: event.seq,
                        end_seq: event.seq,
                        message,
                    });
                }
            }
            "message.assistant.reasoning" => reasoning = str_of(&event.payload, "text"),
            "turn.ended" => reasoning.clear(),
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
                            reasoning: std::mem::take(&mut reasoning),
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
                pending.insert(ToolCallKey::from_payload(&event.payload));
                // 工具调用必须挂在 assistant 消息上；若上一条不是 assistant，补一条空的。
                match messages.last_mut() {
                    Some(wrapped) => match &mut wrapped.message {
                        ChatMessage::Assistant {
                            reasoning: current,
                            tool_calls,
                            ..
                        } => {
                            if !reasoning.is_empty() {
                                *current = std::mem::take(&mut reasoning);
                            }
                            tool_calls.push(call);
                            wrapped.end_seq = event.seq;
                        }
                        _ => messages.push(SeqMessage {
                            start_seq: event.seq,
                            end_seq: event.seq,
                            message: ChatMessage::Assistant {
                                reasoning: std::mem::take(&mut reasoning),
                                text: String::new(),
                                tool_calls: vec![call],
                            },
                        }),
                    },
                    None => messages.push(SeqMessage {
                        start_seq: event.seq,
                        end_seq: event.seq,
                        message: ChatMessage::Assistant {
                            reasoning: std::mem::take(&mut reasoning),
                            text: String::new(),
                            tool_calls: vec![call],
                        },
                    }),
                }
            }
            "tool.result" | "tool.aborted" => {
                let call_id = str_of(&event.payload, "call_id");
                let key = ToolCallKey::from_payload(&event.payload);
                // 没有对应调用（日志残缺）就跳过：宁可少一条，也不要非法序列。
                if !pending.contains(&key) || !settled.insert(key.clone()) {
                    continue;
                }
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
                        let images = tool_image_references(&content_value);
                        if !images.is_empty() {
                            if let Some(SeqMessage {
                                end_seq,
                                message: ChatMessage::ToolImages(existing),
                                ..
                            }) = &mut step_images
                            {
                                *end_seq = event.seq;
                                existing.extend(images);
                            } else {
                                step_images = Some(SeqMessage {
                                    start_seq: event.seq,
                                    end_seq: event.seq,
                                    message: ChatMessage::ToolImages(images),
                                });
                            }
                        }
                        externalize_tool_content(
                            &call_id,
                            &key,
                            &text,
                            &content_value,
                            spill.as_mut(),
                        )
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

    messages.extend(step_images);
    messages
}

/// 模型可见的工具结果文本：已留痕的外部化按事件还原引用；否则按策略判断是否新外部化。
fn externalize_tool_content(
    call_id: &str,
    key: &ToolCallKey,
    text: &str,
    content: &Value,
    stage: Option<&mut SpillStage<'_>>,
) -> String {
    let Some(stage) = stage else {
        return text.to_owned();
    };
    if let Some(recorded) = stage.recorded.get(key) {
        let preview = spill::preview_of(text, recorded.preview_chars);
        return spill::render_reference(&recorded.spill, &preview);
    }
    if let Some(policy) = stage.policy {
        if let Some((spill_ref, payload)) = policy.classify(content) {
            let preview = spill::preview_of(text, policy.preview_chars);
            stage.pending.push(PendingSpill {
                call_id: call_id.to_owned(),
                call_seq: match key {
                    ToolCallKey::Seq(seq) => Some(*seq),
                    ToolCallKey::Id(_) => None,
                },
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
    while start < messages.len()
        && matches!(
            messages[start],
            ChatMessage::Tool { .. } | ChatMessage::ToolImages(_)
        )
    {
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

pub fn user_message_from_payload(payload: &Value) -> Option<ChatMessage> {
    let text = text_of(payload);
    let attachments: Vec<_> = payload["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "attachment")
        .filter_map(|part| {
            Some(MediaAttachment {
                name: part["attachment_name"].as_str()?.to_owned(),
                format: AttachmentFormat::from_mime(part["mime"].as_str()?)?,
                reference: part["$attachment"].as_str()?.to_owned(),
                bytes: None,
            })
        })
        .collect();
    if !attachments.is_empty() {
        Some(ChatMessage::UserWithAttachments { text, attachments })
    } else {
        (!text.is_empty()).then_some(ChatMessage::User(text))
    }
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
mod tests;
