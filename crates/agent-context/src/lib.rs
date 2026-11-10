//! Session Log → 模型消息历史（以及最小上下文工程）。
//!
//! 两条硬约束：
//! 1. **回放一致**：历史完全从 durable 事件重建，不读内存状态、不读插件；
//!    同一份 JSONL 必须得到同一串消息。
//! 2. **协议合法**：`tool.call` / `tool.result` 折叠成
//!    `assistant{tool_calls}` + `tool{call_id, content}`，配对关系逐条可断言——
//!    这正是当前工具轮次"对不上号"的根因。
//!
//! 上下文工程的第一步只做截断：保留最近 N 条且不拆散配对。计量/压缩/裁剪（PLAN §6.4
//! 其余四项）在 M2 补。

#![forbid(unsafe_code)]

use std::collections::HashSet;

use dshr_agent_core::{ChatMessage, ModelToolCall};
use dshr_session_store::{Durability, SessionLog};
use serde_json::Value;

/// 默认保留的历史消息条数。60 条足够覆盖常见多轮对话，又不至于一次塞满窗口。
pub const DEFAULT_MAX_MESSAGES: usize = 60;

/// 从日志重建全部历史消息（不截断）。
pub fn history_from_log(log: &SessionLog) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending: HashSet<String> = HashSet::new();
    let mut settled: HashSet<String> = HashSet::new();

    for event in log.events() {
        if event.durability == Durability::Ephemeral {
            continue;
        }
        match event.event_type.as_str() {
            "message.user" => {
                let text = text_of(&event.payload);
                if !text.is_empty() {
                    messages.push(ChatMessage::user(text));
                }
            }
            "message.assistant.settled" => {
                // 空文本（例如纯工具轮次）不产生消息：空 assistant 只会干扰模型。
                let text = text_of(&event.payload);
                if !text.is_empty() {
                    messages.push(ChatMessage::Assistant {
                        text,
                        tool_calls: Vec::new(),
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
                    Some(ChatMessage::Assistant { tool_calls, .. }) => tool_calls.push(call),
                    _ => messages.push(ChatMessage::Assistant {
                        text: String::new(),
                        tool_calls: vec![call],
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
                    let text =
                        content_text(&event.payload.get("content").cloned().unwrap_or(Value::Null));
                    if event
                        .payload
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        format!("ERROR: {text}")
                    } else {
                        text
                    }
                };
                messages.push(ChatMessage::Tool { call_id, content });
            }
            _ => {}
        }
    }

    messages
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
        assert!(messages.len() >= 5, "messages={messages:?}");
        assert!(matches!(&messages[0], ChatMessage::User(text) if text.contains("两个工具")));
        assert!(
            matches!(&messages[1], ChatMessage::Assistant { text, .. } if text.contains("先调用"))
        );
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
}
