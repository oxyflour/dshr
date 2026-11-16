//! __NAME__ — cpp-v1 WASM Component 插件模板。
//!
//! 构建：rustup target add wasm32-wasip2
//!      cargo build --release --target wasm32-wasip2

wit_bindgen::generate!({
    path: "wit/cpp-v1.wit",
    world: "plugin",
});

struct Component;

impl exports::dshr::cpp_v1::tool_provider::Guest for Component {
    fn list_tools() -> Vec<dshr::cpp_v1::types::ToolDecl> {
        vec![dshr::cpp_v1::types::ToolDecl {
            name: "echo".to_owned(),
            description: "Echo the canonical JSON arguments.".to_owned(),
            input_schema: r#"{"type":"object"}"#.to_owned(),
            output_schema: None,
            timeout_ms: Some(30_000),
            cancelable: Some(false),
            presentation: Some("generic".to_owned()),
        }]
    }

    fn list_panels() -> Vec<dshr::cpp_v1::types::PanelDecl> {
        Vec::new()
    }

    fn invoke(tool: String, args: String) -> Result<dshr::cpp_v1::types::ToolResult, String> {
        if tool != "echo" {
            return Err(format!("unknown tool: {tool}"));
        }
        Ok(dshr::cpp_v1::types::ToolResult {
            content: vec![dshr::cpp_v1::types::ContentPart::Text(args)],
            value: None,
            meta: None,
            is_error: false,
        })
    }
}

export!(Component);
