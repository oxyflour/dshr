//! 集成测试用的外部进程插件 fixture（spec/cpp-v1.md §12）。
//!
//! 用 `dshr-plugin-sdk` 写的一个真实子进程：声明 5 个工具 + 2 个 panel，
//! 覆盖正常调用、超时、超大输出、内存超限、崩溃五类 G3 故障注入。
//! stdout 只走协议；诊断写 stderr。

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dshr_plugin_sdk::{
    CancelFlag, ChatNodeContext, ChildSession, FileViewContext, PanelDecl, ReadyDecls, ToolCall,
    ToolDecl, ToolError, ToolResult, ToolViewContext, ViewHandler,
};
use serde_json::{json, Value};

/// fixture 的工具声明（timeout 故意设小：`slow` 300 ms 让宿主看门狗可测）。
fn declarations() -> ReadyDecls {
    let tools = vec![
        ToolDecl::new("echo", "echo back the input text").with_input_schema(json!({
            "type": "object",
            "required": ["text"],
            "properties": { "text": { "type": "string" } }
        })),
        ToolDecl::new("slow", "sleep for N ms without returning early")
            .with_input_schema(json!({
                "type": "object",
                "properties": { "ms": { "type": "integer" } }
            }))
            .with_timeout_ms(300),
        ToolDecl::new("big", "return a result larger than the 1 MiB frame limit")
            .with_timeout_ms(5_000),
        ToolDecl::new(
            "hog",
            "allocate 512 MiB to trip the Job Object memory limit",
        )
        .with_timeout_ms(5_000),
        ToolDecl::new("crash", "terminate the whole child process").with_timeout_ms(5_000),
        ToolDecl::new(
            "env",
            "report whether credential env vars leaked into the child",
        )
        .with_timeout_ms(5_000),
    ];
    let panels = vec![
        PanelDecl::new(
            "workspace.panel",
            "fixture.one",
            "Fixture One",
            "fixture.adc",
        )
        .with_location("right")
        .with_order(100.0),
        PanelDecl::new(
            "workspace.panel",
            "fixture.two",
            "Fixture Two",
            "fixture.unknown",
        )
        .with_location("bottom")
        .with_order(200.0),
    ];
    let declarations = ReadyDecls::new(tools, panels);
    match std::env::var("DSHR_PLUGIN_ID") {
        Ok(plugin_id) => declarations.with_slots(
            [
                "conversation.input.left",
                "conversation.input.right",
                "settings.general.item",
                "settings.section",
                "workspace.sidebar.tab",
                "workspace.page",
                "status.bar.item",
            ]
            .into_iter()
            .map(|slot| {
                json!({
                    "slot_id": slot, "plugin_id": plugin_id, "id": "fixture.quick",
                    "label": "Fixture Quick Action", "view": "fixture.quick"
                })
            })
            .chain([
                json!({
                    "slot_id": "conversation.chat.node", "plugin_id": plugin_id,
                    "key": "fixture.note", "view": "fixture.node"
                }),
                json!({
                    "slot_id": "viewer.tab", "plugin_id": plugin_id, "key": "txt", "view": "fixture.file"
                }),
                json!({
                    "slot_id": "command", "plugin_id": plugin_id, "id": "fixture.refresh",
                    "label": "Refresh fixture", "run": "fixture.refresh", "keys": ["Ctrl+Shift+G"]
                }),
                json!({
                    "slot_id": "menu.item", "plugin_id": plugin_id, "key": "composer.add",
                    "id": "fixture.add", "label": "Refresh fixture from menu", "run": "fixture.refresh"
                }),
                json!({
                    "slot_id": "tool.call.toolview", "plugin_id": plugin_id,
                    "key": "echo", "view": "fixture.tool"
                }),
                json!({
                    "slot_id": "tool.result.extra", "plugin_id": plugin_id, "id": "fixture.extra",
                    "label": "Fixture Result", "view": "fixture.extra"
                }),
            ])
            .collect(),
        ),
        Err(_) => declarations,
    }
}

fn handler(call: &ToolCall, cancel: &CancelFlag) -> Result<ToolResult, ToolError> {
    match call.tool.as_str() {
        "echo" => {
            let text = call
                .argument("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = ToolResult::json(json!({ "echo": text }));
            Ok(
                if call.argument("publish_node").and_then(Value::as_bool) == Some(true) {
                    result.with_meta(json!({"adc_node": {
                        "key": "fixture.note", "fallback": "saved note", "data": text
                    }}))
                } else {
                    result
                },
            )
        }
        "slow" => {
            let millis = call
                .argument("ms")
                .and_then(Value::as_u64)
                .unwrap_or(5_000)
                .min(30_000);
            let mut waited = 0;
            while waited < millis {
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                thread::sleep(Duration::from_millis(20));
                waited += 20;
            }
            Ok(ToolResult::text(format!("slept {millis} ms")))
        }
        "big" => Ok(ToolResult::text("x".repeat(1_200 * 1024))),
        "hog" => {
            // 触碰全部页：Job Object 的 64 MB 内存上限会在这里终止进程。
            let data = vec![0xABu8; 512 * 1024 * 1024];
            std::hint::black_box(&data);
            thread::sleep(Duration::from_secs(5));
            Ok(ToolResult::text("allocated"))
        }
        "crash" => {
            // 直接退出整个子进程：宿主应看到 generation 失效，在途调用报错。
            std::process::exit(3);
        }
        "env" => Ok(ToolResult::json(json!({
            "has_deepseek_key": std::env::var_os("DEEPSEEK_API_KEY").is_some(),
            "plugin_id": std::env::var("DSHR_PLUGIN_ID").ok(),
            "generation": std::env::var("DSHR_PLUGIN_GENERATION").ok(),
        }))),
        other => Err(ToolError::invalid_arguments(format!(
            "unknown fixture tool `{other}`"
        ))),
    }
}

fn render_view(view: &str, state_seq: u64, action_count: u64) -> Result<Value, ToolError> {
    match view {
        "fixture.quick" => Ok(json!({
            "type": "Button",
            "props": {
                "label": format!("actions={action_count}"), "a11yLabel": "Refresh",
                "onPress": { "call": "refresh" }
            }
        })),
        "fixture.one" => Ok(json!({
            "type": "Text",
            "props": { "text": format!("fixture one seq={state_seq}") },
            "a11yLabel": "fixture one"
        })),
        "fixture.two" => Ok(json!({
            "type": "Card",
            "props": { "title": "Fixture Two" },
            "children": [],
            "a11yLabel": "fixture two"
        })),
        // 合法 ADC-v1 树（`--selftest-adc-panel` 的 "合法树 0 诊断" 用例）。
        "fixture.adc" => Ok(json!({
            "type": "Card",
            "props": { "title": "Fixture ADC" },
            "children": [
                { "type": "Text", "props": { "value": format!("external fixture seq={state_seq} actions={action_count}") } },
                { "type": "Row", "props": { "gap": 8, "align": "center" }, "children": [
                    { "type": "Badge", "props": { "label": "external-process", "tone": "success" } },
                    { "type": "Text", "props": { "value": "ADC panel 已接通", "style": "caption" } }
                ]},
                { "type": "Button", "props": {
                    "label": "刷新",
                    "a11yLabel": "刷新 fixture 计数",
                    "onPress": { "call": "refresh" }
                }},
                { "type": "Table", "props": {
                    "columns": [
                        { "key": "k", "title": "键" },
                        { "key": "v", "title": "值" }
                    ],
                    "rows": [
                        { "k": "来源", "v": "external-process" },
                        { "k": "view", "v": "fixture.adc" }
                    ],
                    "a11yLabel": "fixture adc table"
                }}
            ]
        })),
        // 未知元素：运行时 MUST 降级为 Placeholder + 诊断（P4），不 panic。
        "fixture.unknown" => Ok(json!({
            "type": "Sparkline",
            "props": { "unexpected": true }
        })),
        other => Err(ToolError::invalid_arguments(format!(
            "unknown fixture view `{other}`"
        ))),
    }
}

struct FixtureViews(Arc<Mutex<u64>>);

impl ViewHandler for FixtureViews {
    fn render(&mut self, view: &str, state_seq: u64) -> Result<Value, ToolError> {
        let count = *self.0.lock().unwrap();
        render_view(view, state_seq, count)
    }

    fn render_tool(
        &mut self,
        view: &str,
        _state_seq: u64,
        context: &ToolViewContext,
    ) -> Result<Value, ToolError> {
        Ok(json!({ "type": "Text", "props": {
            "value": format!("{view}:{}:{}:{}:{}", context.call_seq, context.tool, context.arguments["text"], context.value["echo"])
        } }))
    }

    fn render_file(
        &mut self,
        view: &str,
        _: u64,
        context: &FileViewContext,
    ) -> Result<Value, ToolError> {
        if view != "fixture.file" {
            return Err(ToolError::invalid_arguments("unknown file view"));
        }
        Ok(
            json!({"type": "Text", "props": {"value": format!("{}:{}:{}", context.name, context.file_type, context.text.as_deref().unwrap_or("[binary]"))}}),
        )
    }

    fn render_node(
        &mut self,
        view: &str,
        _: u64,
        context: &ChatNodeContext,
    ) -> Result<Value, ToolError> {
        if view != "fixture.node" {
            return Err(ToolError::invalid_arguments(
                "unknown conversation node view",
            ));
        }
        let data = context.data.as_str().unwrap_or_default();
        Ok(
            json!({"type": "Text", "props": {"value": format!("{}:{}:{data}", context.key, context.call_seq)}}),
        )
    }
}

fn main() {
    let actions_in = Arc::new(Mutex::new(0u64));
    let views = FixtureViews(Arc::clone(&actions_in));
    let action_handler = move |view: &str, action: &str, payload: &Value| {
        if action == "call"
            || (action == "command"
                && view == "fixture.refresh"
                && matches!(
                    payload["id"].as_str(),
                    Some("fixture.refresh" | "fixture.add")
                ))
        {
            *actions_in.lock().unwrap() += 1;
            Ok(())
        } else {
            Err(ToolError::internal(format!(
                "fixture does not handle action `{action}`"
            )))
        }
    };
    if let Err(error) =
        ChildSession::serve_with_views_and_actions(declarations(), views, action_handler, handler)
    {
        eprintln!("plugin-fixture: session error: {error}");
        std::process::exit(1);
    }
}
