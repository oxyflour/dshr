use std::sync::{Arc, Mutex};

use dshr_plugin_sdk::{
    CancelFlag, ChildSession, PanelDecl, ReadyDecls, SessionError, ToolCall, ToolDecl, ToolError,
    ToolResult,
};
use serde_json::{json, Value};

#[derive(Default)]
struct State {
    name: String,
    greeting: String,
}

fn main() -> Result<(), SessionError> {
    let state = Arc::new(Mutex::new(State {
        greeting: "Hello, world!".to_owned(),
        ..State::default()
    }));
    let panel = PanelDecl::new("workspace.panel", "hello.panel", "Hello", "hello.view");
    let tool = ToolDecl::new("hello_status", "Return the current greeting from the panel");
    let display = Arc::clone(&state);
    let edit = Arc::clone(&state);

    ChildSession::serve_with_views_and_actions(
        ReadyDecls::new(vec![tool], vec![panel]),
        move |view: &str, _revision: u64| -> Result<Value, ToolError> {
            if view != "hello.view" {
                return Err(ToolError::invalid_arguments("unknown view"));
            }
            let state = display.lock().map_err(|_| ToolError::internal("state unavailable"))?;
            Ok(json!({
                "type": "Stack", "props": { "gap": 8 },
                "children": [
                    { "type": "Text", "props": { "value": state.greeting } },
                    { "type": "Input", "props": {
                        "a11yLabel": "Your name", "placeholder": "Your name", "value": state.name,
                        "onChange": { "call": "set_name", "args": {} }
                    }},
                    { "type": "Button", "props": {
                        "a11yLabel": "Say hello", "label": "Say hello",
                        "onPress": { "call": "greet", "args": {} }
                    }}
                ]
            }))
        },
        move |_view: &str, action: &str, payload: &Value| -> Result<(), ToolError> {
            let mut state = edit.lock().map_err(|_| ToolError::internal("state unavailable"))?;
            match (action, payload["method"].as_str()) {
                ("call", Some("set_name")) => {
                    let name = payload["args"]["value"].as_str()
                        .ok_or_else(|| ToolError::invalid_arguments("args.value must be a string"))?;
                    state.name = name.to_owned();
                }
                ("call", Some("greet")) => {
                    let name = state.name.trim();
                    state.greeting = format!("Hello, {}!", if name.is_empty() { "world" } else { name });
                }
                _ => return Err(ToolError::invalid_arguments("unknown panel action")),
            }
            Ok(())
        },
        move |_call: &ToolCall, _cancel: &CancelFlag| -> Result<ToolResult, ToolError> {
            let state = state.lock().map_err(|_| ToolError::internal("state unavailable"))?;
            Ok(ToolResult::text(state.greeting.clone()))
        },
    )
}
