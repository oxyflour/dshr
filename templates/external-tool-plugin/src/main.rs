//! __PLUGIN_NAME__ — CPP-v1 `external-process` 插件模板。
//!
//! 形态：一个 `echo` 工具 + 一个挂在 `workspace.panel` 的只读面板。
//! 宿主协议与配额见 `docs/spec/cpp-v1.md` §12；作者文档见 `docs/plugin-authoring.md`。
//!
//! 运行约束（SDK 已处理，作者只需知道）：
//! - stdout 只写协议（一行一条 JSON），日志一律 `eprintln!` 走 stderr；
//! - 宿主每 15 s 发 `process.ping`，SDK 自动回 `process.pong`；
//! - `process.shutdown` 后 SDK 回 `process.bye` 并退出；
//! - 工具 handler 返回 `ToolResult`（内容对模型可见）；面板渲染走 `ui.render`。

use dshr_plugin_sdk::{
    CancelFlag, ChildSession, PanelDecl, ReadyDecls, ToolCall, ToolDecl, ToolError, ToolResult,
};

fn main() -> Result<(), dshr_plugin_sdk::SessionError> {
    // stderr 是日志通道（stdout 只走协议帧）。
    eprintln!("__PLUGIN_ID__: starting (cpp-process/v1)");

    // 1) 工具声明：名字/描述/内联 JSON Schema（spec §12.3，schema ≤ 8 KiB）。
    //    工具名全局唯一；重名会在装载阶段被宿主拒绝。
    let echo = ToolDecl::new(
        "__TOOL_NAME__",
        "Echo the input text back. Template demo tool for external-process plugins.",
    )
    .with_input_schema(serde_json::json!({
        "type": "object",
        "required": ["text"],
        "additionalProperties": false,
        "properties": {
            "text": { "type": "string", "maxLength": 4096 }
        }
    }))
    .with_output_schema(serde_json::json!({ "type": "object" }))
    .with_timeout_ms(30_000)
    .with_cancelable(true)
    .with_presentation("generic");

    // 2) 面板声明：slot 必须是 ADC-v1 slot 目录里的 id（这里用 workspace.panel）。
    //    location ∈ left/right/bottom/center；order 越小越靠前。
    let panel = PanelDecl::new(
        "workspace.panel",
        "__PLUGIN_ID__.panel",
        "__PLUGIN_NAME__",
        "__PLUGIN_ID__.panel",
    )
    .with_location("right")
    .with_order(100.0);

    let decls = ReadyDecls::new(vec![echo], vec![panel]);

    // 3) 挂两个回调：
    //    - views: 收到 ui.render(view, state_seq) 时返回 ADC-v1 元素树（JSON）；
    //    - handler: 收到 tool.invoke 时执行工具，返回 ToolResult。
    ChildSession::serve_with_views(
        decls,
        |_view: &str, _state_seq: u64| -> Result<serde_json::Value, ToolError> { Ok(panel_tree()) },
        |call: &ToolCall, cancel: &CancelFlag| -> Result<ToolResult, ToolError> {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            match call.tool.as_str() {
                "__TOOL_NAME__" => {
                    // call.argument(...) 返回 Option<&serde_json::Value>；缺参数就是调用方的错。
                    let text = call
                        .argument("text")
                        .and_then(|value| value.as_str())
                        .ok_or_else(|| ToolError::InvalidArguments("missing string argument `text`".to_owned()))?;
                    // value 供展示/回放；content 对模型可见（spec §7.3）。
                    Ok(ToolResult::text(text))
                }
                other => Err(ToolError::invalid_arguments(format!("unknown tool `{other}`"))),
            }
        },
    )
}

/// `ui.render` 返回的 ADC-v1 元素树。
///
/// 规则：元素集与字段约束见 `docs/spec/adc-v1.md` §6 与
/// `schema/adc-v1/element.schema.json`；未知元素在运行时降级为 Placeholder。
fn panel_tree() -> serde_json::Value {
    serde_json::json!({
        "type": "Stack",
        "key": "__PLUGIN_ID__.panel.root",
        "props": { "gap": 8, "padding": 12 },
        "children": [
            {
                "type": "Text",
                "key": "title",
                "props": { "value": "__PLUGIN_NAME__", "style": "title" }
            },
            {
                "type": "Text",
                "key": "body",
                "props": { "value": "本面板由 external-process 插件通过 ui.render 提供。" }
            },
            {
                "type": "Badge",
                "key": "kind",
                "props": { "label": "external-process", "tone": "info", "a11yLabel": "运行形态：external-process" }
            },
            {
                "type": "Button",
                "key": "copy-id",
                "props": {
                    "label": "复制插件 ID",
                    "a11yLabel": "复制插件 ID __PLUGIN_ID__",
                    "onPress": { "copy": "__PLUGIN_ID__" }
                }
            }
        ]
    })
}
