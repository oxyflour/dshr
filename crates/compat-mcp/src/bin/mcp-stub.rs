//! 离线 MCP fixture server（stdio NDJSON，JSON-RPC 2.0）。
//!
//! 只用于 `dshr-compat-mcp` 的集成测试；零外部 crate（除 serde_json）。
//! 提供的工具（两页 `tools/list`）：
//! - 第 1 页：`echo`（structuredContent 回显）、`fail`（`isError:true`）
//! - 第 2 页（`cursor:"page2"`）：`slow`（睡 2s）、`big`（>1 MiB text）
//!
//! 额外行为（由环境变量控制，便于测试边界）：
//! - `MCP_STUB_PROTOCOL_VERSION`：`initialize` 回显的协议版本（默认 `2024-11-05`）
//! - `MCP_STUB_EMIT_GARBAGE=1`：启动时写一行非 JSON（测试非法行诊断）
//! - `MCP_STUB_NOTIFY=1`：`initialize` 后发一条 `notifications/message`
//! - `MCP_STUB_IGNORE_CANCEL=1`：`slow` 忽略 `notifications/cancelled`（测试迟到答复丢弃）
//!
//! 另有一个未列入 `tools/list` 的 `crash` 工具：直接 `exit(3)`，用于崩溃恢复测试。

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

type SharedWriter = Arc<Mutex<io::Stdout>>;

fn send(writer: &SharedWriter, message: Value) {
    let Ok(mut stdout) = writer.lock() else {
        return;
    };
    if serde_json::to_writer(&mut *stdout, &message).is_ok() {
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

fn tool(name: &str, description: String, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn empty_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
    })
}

fn echo_tool() -> Value {
    // 描述故意超过 1024 字符：验证 ToolDefinition 的截断策略。
    tool(
        "echo",
        format!("echo tool; {}", "x".repeat(1500)),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["echo"],
            "properties": { "echo": { "type": "string" } },
        }),
    )
}

fn fail_tool() -> Value {
    tool(
        "fail",
        "always returns isError".to_owned(),
        empty_input_schema(),
    )
}

fn slow_tool() -> Value {
    tool(
        "slow",
        "sleeps two seconds before answering".to_owned(),
        empty_input_schema(),
    )
}

fn big_tool() -> Value {
    tool(
        "big",
        "returns more than one MiB of text".to_owned(),
        empty_input_schema(),
    )
}

/// `tools/list` 的分页视图：返回值 + nextCursor。
fn tools_page(cursor: Option<&str>) -> Option<(Vec<Value>, Option<&'static str>)> {
    match cursor {
        None | Some("") => Some((vec![echo_tool(), fail_tool()], Some("page2"))),
        Some("page2") => Some((vec![slow_tool(), big_tool()], None)),
        _ => None,
    }
}

fn main() {
    let writer: SharedWriter = Arc::new(Mutex::new(io::stdout()));
    let protocol_version =
        std::env::var("MCP_STUB_PROTOCOL_VERSION").unwrap_or_else(|_| "2024-11-05".to_owned());
    let notify_after_initialize = std::env::var("MCP_STUB_NOTIFY").ok().as_deref() == Some("1");
    let ignore_cancel = std::env::var("MCP_STUB_IGNORE_CANCEL").ok().as_deref() == Some("1");
    let cancelled: Arc<Mutex<BTreeSet<u64>>> = Arc::new(Mutex::new(BTreeSet::new()));

    if std::env::var("MCP_STUB_EMIT_GARBAGE").ok().as_deref() == Some("1") {
        if let Ok(mut stdout) = writer.lock() {
            let _ = stdout.write_all(b"this line is not json\n");
            let _ = stdout.flush();
        }
    }

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(_) => {
                send(
                    &writer,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": "parse error" },
                    }),
                );
                continue;
            }
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

        match method.as_str() {
            "initialize" => {
                if let Some(id) = id {
                    send(
                        &writer,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": protocol_version,
                                "capabilities": { "tools": { "listChanged": false } },
                                "serverInfo": {
                                    "name": "mcp-stub",
                                    "version": env!("CARGO_PKG_VERSION"),
                                },
                            },
                        }),
                    );
                }
                if notify_after_initialize {
                    send(
                        &writer,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": { "level": "info", "data": "stub initialized" },
                        }),
                    );
                }
            }
            "notifications/initialized" => {}
            "notifications/cancelled" => {
                if let Some(request_id) = params.get("requestId").and_then(Value::as_u64) {
                    if let Ok(mut cancelled) = cancelled.lock() {
                        cancelled.insert(request_id);
                    }
                }
            }
            "shutdown" => {
                if let Some(id) = id {
                    send(&writer, json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
                }
            }
            "exit" => std::process::exit(0),
            "tools/list" => {
                let cursor = params.get("cursor").and_then(Value::as_str);
                match tools_page(cursor) {
                    Some((tools, next_cursor)) => {
                        if let Some(id) = id {
                            let mut result = json!({ "tools": tools });
                            if let Some(next_cursor) = next_cursor {
                                result["nextCursor"] = json!(next_cursor);
                            }
                            send(
                                &writer,
                                json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                            );
                        }
                    }
                    None => {
                        if let Some(id) = id {
                            send(
                                &writer,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": { "code": -32602, "message": "unknown cursor" },
                                }),
                            );
                        }
                    }
                }
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match name {
                    "echo" => {
                        if let Some(id) = id {
                            send(
                                &writer,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": [{
                                            "type": "text",
                                            "text": arguments.to_string(),
                                        }],
                                        // 结构化结果优先于 content 文本；回显 arguments.echo。
                                        "structuredContent": {
                                            "echo": arguments.get("echo").cloned().unwrap_or(Value::Null),
                                        },
                                        "isError": false,
                                    },
                                }),
                            );
                        }
                    }
                    "fail" => {
                        if let Some(id) = id {
                            send(
                                &writer,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": [{ "type": "text", "text": "boom" }],
                                        "isError": true,
                                    },
                                }),
                            );
                        }
                    }
                    "slow" => {
                        // 关键：慢调用放到后台线程，主循环继续读 stdin——
                        // 否则超时后的新请求会排在 2s 睡眠后面，测不出"客户端仍可用"。
                        if let (Some(id), Some(request_id)) =
                            (id.clone(), id.as_ref().and_then(Value::as_u64))
                        {
                            let writer = Arc::clone(&writer);
                            let cancelled = Arc::clone(&cancelled);
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_secs(2));
                                if !ignore_cancel {
                                    let already_cancelled = cancelled
                                        .lock()
                                        .map(|cancelled| cancelled.contains(&request_id))
                                        .unwrap_or(false);
                                    if already_cancelled {
                                        return;
                                    }
                                }
                                send(
                                    &writer,
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {
                                            "content": [{ "type": "text", "text": "slow-done" }],
                                            "isError": false,
                                        },
                                    }),
                                );
                            });
                        }
                    }
                    "big" => {
                        if let Some(id) = id {
                            send(
                                &writer,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": [{
                                            "type": "text",
                                            "text": "x".repeat(1_500_000),
                                        }],
                                    },
                                }),
                            );
                        }
                    }
                    "crash" => std::process::exit(3),
                    other => {
                        if let Some(id) = id {
                            send(
                                &writer,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {
                                        "code": -32602,
                                        "message": format!("unknown tool `{other}`"),
                                    },
                                }),
                            );
                        }
                    }
                }
            }
            other => {
                if let Some(id) = id {
                    send(
                        &writer,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("method `{other}` not found"),
                            },
                        }),
                    );
                }
            }
        }
    }
}
