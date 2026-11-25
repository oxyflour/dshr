use std::sync::OnceLock;

use serde_json::{json, Value};

pub(super) const RENDER: &str =
    "fn render(&mut self, view: &str, state_seq: u64) -> Result<serde_json::Value, ToolError>";
pub(super) const RENDER_TOOL: &str = "fn render_tool(&mut self, view: &str, state_seq: u64, context: &ToolViewContext) -> Result<serde_json::Value, ToolError>";
pub(super) const RENDER_FILE: &str = "fn render_file(&mut self, view: &str, state_seq: u64, context: &FileViewContext) -> Result<serde_json::Value, ToolError>";
pub(super) const RENDER_NODE: &str = "fn render_node(&mut self, view: &str, state_seq: u64, context: &ChatNodeContext) -> Result<serde_json::Value, ToolError>";
pub(super) const HANDLE_ACTION: &str = "fn action(&mut self, view: &str, action: &str, payload: &serde_json::Value) -> Result<(), ToolError>";

const SDK: &[(&str, &[&str], &str)] = &[
    ("ActionHandler", &[HANDLE_ACTION], "A FnMut(&str, &str, &serde_json::Value) -> Result<(), ToolError> closure also implements ActionHandler. Register it as the third argument of ChildSession::serve_with_views_and_actions. Errors go to stderr. Verify the side effect or refreshed tree."),
    ("CancelFlag", &["fn is_cancelled(&self) -> bool"], "Long-running ToolHandler code should check this flag and return Err(ToolError::Cancelled) when set."),
    ("ChatNodeContext", &["pub key: String", "pub call_seq: std::num::NonZeroU64", "pub data: serde_json::Value"], "One successful native tool publication. Match key to the declared node, and use call_seq to distinguish instances."),
    ("ChildSession", &[
        "fn serve<H: ToolHandler>(decls: ReadyDecls, handler: H) -> Result<(), SessionError>",
        "fn serve_with_views<H: ToolHandler, V: ViewHandler>(decls: ReadyDecls, views: V, handler: H) -> Result<(), SessionError>",
        "fn serve_with_views_and_actions<H: ToolHandler, V: ViewHandler, A: ActionHandler>(decls: ReadyDecls, views: V, actions: A, handler: H) -> Result<(), SessionError>",
    ], "Call one of these associated functions from main. stdout carries protocol frames. Write diagnostics to stderr. For an actions-only package, supply a ViewHandler that returns an error for unknown views."),
    ("FileViewContext", &["pub name: String", "pub file_type: String", "pub size_bytes: u64", "pub text: Option<String>", "pub truncated: bool"], "The filename has no directory. The type is a lowercase extension without its dot. text holds at most a 16 KiB UTF-8 prefix and can be None. The host sends this context after the user selects a delivered file for preview."),
    ("PanelDecl", &[
        "fn new(slot: impl Into<String>, id: impl Into<String>, label: impl Into<String>, view: impl Into<String>) -> Self",
        "fn with_location(self, location: impl Into<String>) -> Self", "fn with_order(self, order: f64) -> Self",
    ], "Use workspace.panel for the legacy panel path and pass the declaration in ReadyDecls::new's second argument. The fourth argument is the view ID."),
    ("ReadyDecls", &[
        "fn new(tools: Vec<ToolDecl>, panels: Vec<PanelDecl>) -> Self",
        "fn with_slots(self, slots: Vec<serde_json::Value>) -> Self",
    ], "Use panels for workspace.panel. Use with_slots for other supported desktop slots. Supply the complete slot vector in one call."),
    ("SessionError", &["Io(std::io::Error)", "Protocol(dshr_plugin_sdk::ProtocolError)"], "Use Result<(), SessionError> as the return type of main when returning the ChildSession call."),
    ("ToolCall", &[
        "pub call_id: String", "pub tool: String", "pub arguments: serde_json::Value",
        "pub timeout_ms: u64", "pub cancelable: bool", "pub generation: Option<u64>",
        "fn argument(&self, name: &str) -> Option<&serde_json::Value>",
    ], "Dispatch on call.tool. Read validated JSON arguments with call.argument(name) or call.arguments."),
    ("ToolDecl", &[
        "fn new(name: impl Into<String>, description: impl Into<String>) -> Self",
        "fn with_input_schema(self, schema: serde_json::Value) -> Self",
        "fn with_output_schema(self, schema: serde_json::Value) -> Self",
        "fn with_timeout_ms(self, timeout_ms: u64) -> Self",
        "fn with_cancelable(self, cancelable: bool) -> Self",
        "fn with_presentation(self, presentation: impl Into<String>) -> Self",
    ], "Tool names match ^[a-z][a-z0-9_]{0,63}$. Use JSON Schema for input and output. Add each tool to ReadyDecls and handle its exact name in ToolHandler."),
    ("ToolError", &[
        "fn failed(message: impl Into<String>) -> Self", "fn invalid_arguments(message: impl Into<String>) -> Self",
        "fn internal(message: impl Into<String>) -> Self", "Cancelled",
    ], "Use Err(ToolError::invalid_arguments(...)) for invalid input. Tool handlers and all view and action callbacks use ToolError."),
    ("ToolHandler", &["fn call(&mut self, call: &ToolCall, cancel: &CancelFlag) -> Result<ToolResult, ToolError>"], "A Send + 'static FnMut(&ToolCall, &CancelFlag) -> Result<ToolResult, ToolError> closure also implements ToolHandler."),
    ("ToolResult", &[
        "fn text(text: impl Into<String>) -> Self", "fn json(value: serde_json::Value) -> Self",
        "fn error(text: impl Into<String>) -> Self", "fn with_value(self, value: serde_json::Value) -> Self",
        "fn with_meta(self, meta: serde_json::Value) -> Self",
    ], "Use text or json for success. To publish a conversation node, add meta.adc_node as defined by the slot query."),
    ("ToolViewContext", &["pub call_seq: std::num::NonZeroU64", "pub tool: String", "pub arguments: serde_json::Value", "pub value: serde_json::Value"], "Identifies a settled successful tool call. arguments and value belong to that call. The plugin receives this context only for its own tool."),
    ("ViewHandler", &[RENDER, RENDER_TOOL, RENDER_FILE, RENDER_NODE], "Implement render for ordinary view IDs. Override a contextual method to consume its data; its default calls render. A FnMut(&str, u64) -> Result<serde_json::Value, ToolError> closure implements only the ordinary render method. state_seq is a host revision hint; it can be 0."),
];

pub(super) fn names() -> impl Iterator<Item = &'static str> {
    SDK.iter().map(|entry| entry.0)
}

pub(super) fn render_context(field: &str) -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    let schema = SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schema/cpp-v1/render-request.schema.json"
        )))
        .expect("CPP render request schema")
    });
    &schema["properties"][field]
}

pub(super) fn contract(name: &str) -> Option<Value> {
    let (_, signatures, notes) = SDK.iter().find(|entry| entry.0 == name)?;
    let mut result = json!({"crate": "dshr_plugin_sdk", "signatures": signatures, "notes": notes});
    let context = match name {
        "ChatNodeContext" => Some("node_context"),
        "FileViewContext" => Some("file_context"),
        "ToolViewContext" => Some("tool_context"),
        _ => None,
    };
    if let Some(context) = context {
        result["wire_field"] = json!(context);
        result["schema"] = render_context(context).clone();
    }
    if name == "ActionHandler" {
        result["examples"] = json!({
            "call": {"view": "hello.view", "action": "call", "payload": {"method": "set_value", "args": {"value": true}}},
            "command": {"view": "refresh", "action": "command", "payload": {"id": "hello.refresh"}}
        });
        result["payload_notes"] = json!("For call, view is the render view ID and method is the value of the ADC call property. For a registered command or menu item, view is the declaration's run string and payload.id is the registered id. An Input or Textarea editor can instead dispatch a command to its current view with payload.id only. Copy and openExternal are handled by the host.");
    }
    Some(result)
}
