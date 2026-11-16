//! M4 W5 示例 guest：用 `wit-bindgen` 实现 `cpp-v1.wit` 的 `plugin` world。
//!
//! 目标 `wasm32-wasip2` 会直接产出 component；当前只是一个空工具集，
//! 用于验证 guest 侧 WIT 生成与宿主 bindgen 能对上。

wit_bindgen::generate!({
    path: "../../docs/spec/cpp-v1.wit",
    world: "plugin",
});

struct Component;

impl exports::dshr::cpp_v1::tool_provider::Guest for Component {
    fn list_tools() -> Vec<dshr::cpp_v1::types::ToolDecl> {
        vec![
            dshr::cpp_v1::types::ToolDecl {
                name: "echo".to_owned(),
                description: "echo the canonical JSON arguments".to_owned(),
                input_schema: r#"{"type":"object"}"#.to_owned(),
                output_schema: None,
                timeout_ms: Some(1_000),
                cancelable: Some(false),
                presentation: Some("generic".to_owned()),
            },
            dshr::cpp_v1::types::ToolDecl {
                name: "spin".to_owned(),
                description: "burn fuel until the host quota trips".to_owned(),
                input_schema: r#"{"type":"object"}"#.to_owned(),
                output_schema: None,
                timeout_ms: Some(1_000),
                cancelable: Some(false),
                presentation: Some("generic".to_owned()),
            },
        ]
    }

    fn list_panels() -> Vec<dshr::cpp_v1::types::PanelDecl> {
        Vec::new()
    }

    fn invoke(tool: String, args: String) -> Result<dshr::cpp_v1::types::ToolResult, String> {
        if tool == "spin" {
            loop {
                std::hint::spin_loop();
            }
        }
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
