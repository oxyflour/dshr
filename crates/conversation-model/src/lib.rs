//! Session Log → 可在 GPUI 中渲染的会话模型。
//!
//! 这一层是 M1 的"回放"断言：**同一份 JSONL 必须得到同一份模型**，
//! 且不触发任何插件回调或 LLM 请求（规范 §6）。
//!
//! 它只做三件事：
//! 1. 把 `SessionLog` 的 durable 事件折叠成有序的展示项；
//! 2. 把 `tool.result` / `tool.aborted` 回填到对应的工具卡片；
//! 3. 给出可由测试断言的统计（不依赖 UI）。

#![forbid(unsafe_code)]

use std::collections::HashMap;

use dshr_session_store::{Durability, LogError, SessionLog, ToolState};
use dshr_tool_runtime::Presentation;
use serde_json::Value;

mod card;

pub use card::{card_for, content_text, error_card, CardKind, ToolCard};

/// 演示用会话：包含 native 工具、sidecar 工具、崩溃中止、压缩区间与一个未来事件。
pub const DEMO_SESSION: &str = include_str!("demo_session.jsonl");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolItem {
    pub call_id: String,
    pub tool: String,
    /// `native` / `sidecar` / `mcp`
    pub origin: String,
    /// 插件声明的卡片类型（未知值保留原文，解析失败按 `generic` 渲染）。
    pub presentation: Option<String>,
    pub state: ToolState,
    /// 结果卡片：`tool.call` 时还是 `None`，终态事件把它填上。
    pub card: Option<ToolCard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User { text: String },
    Assistant { text: String },
    Tool(ToolItem),
    Compaction { from_seq: u64, to_seq: u64 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub user: usize,
    pub assistant: usize,
    pub tools: usize,
    pub settled: usize,
    pub aborted: usize,
    pub compaction: usize,
}

pub fn build(jsonl: &str, session_id: &str) -> Result<Vec<Item>, LogError> {
    let log = SessionLog::from_jsonl(session_id, jsonl)?;
    Ok(build_from_log(&log))
}

pub fn build_from_log(log: &SessionLog) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut tool_index: HashMap<String, usize> = HashMap::new();

    for event in log.events() {
        if event.durability == Durability::Ephemeral {
            continue;
        }
        match event.event_type.as_str() {
            "message.user" => items.push(Item::User {
                text: text_of(&event.payload),
            }),
            "message.assistant.settled" => items.push(Item::Assistant {
                text: text_of(&event.payload),
            }),
            "tool.call" => {
                let call_id = str_of(&event.payload, "call_id");
                tool_index.insert(call_id.clone(), items.len());
                items.push(Item::Tool(ToolItem {
                    call_id,
                    tool: str_of(&event.payload, "tool"),
                    origin: str_of(&event.payload, "origin"),
                    presentation: event
                        .payload
                        .get("presentation")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    state: ToolState::Pending,
                    card: None,
                }));
            }
            "tool.result" => {
                let call_id = str_of(&event.payload, "call_id");
                let Some(ix) = tool_index.get(&call_id).copied() else {
                    continue;
                };
                let Some(Item::Tool(tool)) = items.get_mut(ix) else {
                    continue;
                };
                let is_error = event
                    .payload
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                tool.state = ToolState::Settled { is_error };

                let content = event.payload.get("content").cloned().unwrap_or(Value::Null);
                tool.card = Some(if is_error {
                    error_card("失败", &content_text(&content))
                } else {
                    let value = event
                        .payload
                        .get("value")
                        .cloned()
                        .unwrap_or_else(|| fallback_value(&content));
                    card_for(&tool.tool, card_presentation(tool), &value, &content)
                });
            }
            "tool.aborted" => {
                let call_id = str_of(&event.payload, "call_id");
                let Some(ix) = tool_index.get(&call_id).copied() else {
                    continue;
                };
                let Some(Item::Tool(tool)) = items.get_mut(ix) else {
                    continue;
                };
                let reason = str_of(&event.payload, "reason");
                tool.state = ToolState::Aborted {
                    reason: reason.clone(),
                };
                tool.card = Some(error_card("已中止", &reason));
            }
            "context.compacted" => items.push(Item::Compaction {
                from_seq: num_of(&event.payload, "from_seq"),
                to_seq: num_of(&event.payload, "to_seq"),
            }),
            _ => {}
        }
    }

    items
}

pub fn stats(items: &[Item]) -> Stats {
    let mut stats = Stats {
        total: items.len(),
        ..Stats::default()
    };
    for item in items {
        match item {
            Item::User { .. } => stats.user += 1,
            Item::Assistant { .. } => stats.assistant += 1,
            Item::Tool(tool) => {
                stats.tools += 1;
                match tool.state {
                    ToolState::Settled { .. } => stats.settled += 1,
                    ToolState::Aborted { .. } => stats.aborted += 1,
                    ToolState::Pending => {}
                }
            }
            Item::Compaction { .. } => stats.compaction += 1,
        }
    }
    stats
}

fn card_presentation(tool: &ToolItem) -> Presentation {
    tool.presentation
        .as_deref()
        .and_then(Presentation::parse)
        .unwrap_or(Presentation::Generic)
}

/// 旧插件把值只放在 `content[0].value`：缺少顶层 `value` 时回退，避免卡片为空。
fn fallback_value(content: &Value) -> Value {
    content
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("value"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn str_of(payload: &Value, field: &str) -> String {
    payload
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn num_of(payload: &Value, field: &str) -> u64 {
    payload.get(field).and_then(Value::as_u64).unwrap_or(0)
}

fn text_of(payload: &Value) -> String {
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    payload
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_session_store::EventSpec;

    fn demo() -> Vec<Item> {
        build(DEMO_SESSION, "s-demo").expect("demo session is valid")
    }

    #[test]
    fn demo_has_expected_items_and_order() {
        let items = demo();
        assert_eq!(items.len(), 6, "10 条事件中 4 条不产出展示项");
        assert!(matches!(items[0], Item::User { .. }));
        assert!(matches!(items[1], Item::Assistant { .. }));
        assert!(matches!(items[2], Item::Tool(_)));
        assert!(matches!(items[5], Item::Assistant { .. }));
    }

    #[test]
    fn tool_cards_are_backfilled_with_terminal_state() {
        let items = demo();
        let tools: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Item::Tool(tool) => Some(tool.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tools.len(), 2);

        assert_eq!(tools[0].tool, "bash");
        assert_eq!(tools[0].origin, "native");
        assert_eq!(tools[0].state, ToolState::Settled { is_error: false });

        assert_eq!(tools[1].tool, "cad_validate");
        assert_eq!(tools[1].origin, "sidecar");
        assert_eq!(
            tools[1].state,
            ToolState::Aborted {
                reason: "crash".to_owned()
            }
        );
    }

    #[test]
    fn stats_match_demo() {
        let stats = stats(&demo());
        assert_eq!(stats.total, 6);
        assert_eq!(stats.user, 1);
        assert_eq!(stats.assistant, 2);
        assert_eq!(stats.tools, 2);
        assert_eq!(stats.settled, 1);
        assert_eq!(stats.aborted, 1);
        assert_eq!(stats.compaction, 1);
    }

    #[test]
    fn compaction_keeps_its_position() {
        let items = demo();
        let ix = items
            .iter()
            .position(|item| matches!(item, Item::Compaction { .. }))
            .expect("compaction item exists");
        assert_eq!(ix, 4, "压缩事件位于两条工具卡片之后、最后一条消息之前");
    }

    #[test]
    fn replay_is_deterministic_and_jsonl_round_trips() {
        let first = demo();
        let log = SessionLog::from_jsonl("s-demo", DEMO_SESSION).unwrap();
        let restored = SessionLog::from_jsonl("s-demo", &log.to_jsonl()).unwrap();
        let second = build_from_log(&restored);
        assert_eq!(first, second, "JSONL 往返后模型必须逐项相等");
    }

    #[test]
    fn unknown_event_is_skipped_without_panic() {
        // 第 10 条事件类型未知：不应进入模型，也不应报错。
        let items = demo();
        assert_eq!(items.len(), 6);
    }

    #[test]
    fn broken_jsonl_is_rejected() {
        let err = build("{not json}", "s-demo").unwrap_err();
        assert!(matches!(err, LogError::Invalid(_)));
    }

    fn tool_items(items: &[Item]) -> Vec<ToolItem> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Tool(tool) => Some(tool.clone()),
                _ => None,
            })
            .collect()
    }

    fn log_with(events: &[(&str, Value)]) -> SessionLog {
        let mut log = SessionLog::new("s-t");
        for (kind, payload) in events {
            log.append(EventSpec::durable(*kind, "t0", payload.clone()))
                .expect("append test event");
        }
        log
    }

    #[test]
    fn demo_tool_cards_carry_the_result() {
        let tools = tool_items(&demo());
        assert_eq!(tools[0].presentation.as_deref(), Some("terminal"));
        let card = tools[0].card.as_ref().expect("settled 工具必须有卡片");
        assert_eq!(card.kind, CardKind::Terminal);
        assert_eq!(card.title, "exit 0");
        assert_eq!(card.preview, vec!["main.rs"]);

        let aborted = tools[1].card.as_ref().expect("aborted 工具必须有卡片");
        assert_eq!(aborted.kind, CardKind::Error);
        assert_eq!(aborted.title, "已中止");
        assert_eq!(aborted.preview, vec!["crash"]);
    }

    #[test]
    fn legacy_result_without_value_uses_content_value() {
        let log = log_with(&[
            (
                "tool.call",
                serde_json::json!({
                    "call_id": "c1", "tool": "probe_echo",
                    "origin": "sidecar", "presentation": "generic"
                }),
            ),
            (
                "tool.result",
                serde_json::json!({
                    "call_id": "c1", "is_error": false,
                    "content": [{ "type": "json", "value": { "echo": { "text": "hi" } } }]
                }),
            ),
        ]);
        let tools = tool_items(&build_from_log(&log));
        let card = tools[0].card.as_ref().expect("card exists");
        assert_eq!(card.kind, CardKind::Generic);
        assert!(card.detail.contains("hi"), "detail={}", card.detail);
    }

    #[test]
    fn unknown_presentation_string_still_renders() {
        let log = log_with(&[
            (
                "tool.call",
                serde_json::json!({
                    "call_id": "c1", "tool": "probe_echo",
                    "origin": "native", "presentation": "markdown"
                }),
            ),
            (
                "tool.result",
                serde_json::json!({
                    "call_id": "c1", "is_error": false,
                    "content": [], "value": { "echo": 1 }
                }),
            ),
        ]);
        let tools = tool_items(&build_from_log(&log));
        let card = tools[0].card.as_ref().expect("card exists");
        assert_eq!(card.kind, CardKind::Generic);
        assert_eq!(tools[0].presentation.as_deref(), Some("markdown"));
    }

    #[test]
    fn error_result_uses_failure_card() {
        let log = log_with(&[
            (
                "tool.call",
                serde_json::json!({
                    "call_id": "c1", "tool": "bash", "origin": "native", "presentation": "terminal"
                }),
            ),
            (
                "tool.result",
                serde_json::json!({
                    "call_id": "c1", "is_error": true,
                    "content": [{ "type": "text", "text": "permission denied" }]
                }),
            ),
        ]);
        let tools = tool_items(&build_from_log(&log));
        assert_eq!(
            tools[0].state,
            ToolState::Settled { is_error: true },
            "is_error 必须进入终态"
        );
        let card = tools[0].card.as_ref().expect("card exists");
        assert_eq!(card.kind, CardKind::Error);
        assert_eq!(card.title, "失败");
        assert!(card.preview[0].contains("permission denied"));
    }

    #[test]
    fn result_for_unknown_call_is_ignored() {
        // 结算事件先于 call 到达（或 call 丢失）：不能让模型错位，也不能 panic。
        let log = log_with(&[(
            "tool.result",
            serde_json::json!({ "call_id": "ghost", "is_error": false, "content": [] }),
        )]);
        assert!(build_from_log(&log).is_empty());
    }
}
