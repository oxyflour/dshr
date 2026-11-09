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
use serde_json::Value;

/// 演示用会话：包含 native 工具、sidecar 工具、崩溃中止、压缩区间与一个未来事件。
pub const DEMO_SESSION: &str = include_str!("demo_session.jsonl");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolItem {
    pub call_id: String,
    pub tool: String,
    /// `native` / `sidecar` / `mcp`
    pub origin: String,
    pub state: ToolState,
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
                    state: ToolState::Pending,
                }));
            }
            "tool.result" => {
                let call_id = str_of(&event.payload, "call_id");
                if let Some(ix) = tool_index.get(&call_id) {
                    if let Item::Tool(tool) = &mut items[*ix] {
                        tool.state = ToolState::Settled {
                            is_error: event
                                .payload
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        };
                    }
                }
            }
            "tool.aborted" => {
                let call_id = str_of(&event.payload, "call_id");
                if let Some(ix) = tool_index.get(&call_id) {
                    if let Item::Tool(tool) = &mut items[*ix] {
                        tool.state = ToolState::Aborted {
                            reason: str_of(&event.payload, "reason"),
                        };
                    }
                }
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
}
