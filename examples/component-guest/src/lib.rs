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
        Vec::new()
    }

    fn list_panels() -> Vec<dshr::cpp_v1::types::PanelDecl> {
        Vec::new()
    }

    fn invoke(_tool: String, _args: String) -> Result<dshr::cpp_v1::types::ToolResult, String> {
        Err("component guest has no tools".to_owned())
    }
}

export!(Component);
