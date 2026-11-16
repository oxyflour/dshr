//! M4 W5：用 `wasmtime::component::bindgen!` 从 `docs/spec/cpp-v1.wit` 生成宿主绑定。
//!
//! 当前只验证 WIT 能生成可编译的 world/type；真正的 host imports 实现与
//! ComponentPlugin 加载器仍待接入（见 docs/m4-component.md）。

#![cfg(test)]

use dshr_plugin_api::{Plugin as PluginTrait, PluginContext};
use dshr_plugin_wasm::CppComponentPlugin;
use serde_json::json;
use wasmtime::component::{bindgen, Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

bindgen!({
    path: "../../docs/spec/cpp-v1.wit",
    world: "plugin",
});

struct HostState {
    table: ResourceTable,
    ctx: WasiCtx,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            table: ResourceTable::new(),
            ctx: WasiCtxBuilder::new().build(),
        }
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl dshr::cpp_v1::types::Host for HostState {}

impl dshr::cpp_v1::types::HostToolCall for HostState {
    fn arguments(
        &mut self,
        _self_: wasmtime::component::Resource<dshr::cpp_v1::types::ToolCall>,
    ) -> String {
        "{}".to_owned()
    }

    fn request_id(
        &mut self,
        _self_: wasmtime::component::Resource<dshr::cpp_v1::types::ToolCall>,
    ) -> String {
        "req".to_owned()
    }

    fn is_cancelled(
        &mut self,
        _self_: wasmtime::component::Resource<dshr::cpp_v1::types::ToolCall>,
    ) -> bool {
        false
    }

    fn drop(
        &mut self,
        _rep: wasmtime::component::Resource<dshr::cpp_v1::types::ToolCall>,
    ) -> wasmtime::Result<()> {
        Ok(())
    }
}

impl dshr::cpp_v1::host::Host for HostState {
    fn log(&mut self, _level: String, _message: String) {}

    fn get_quota(&mut self) -> dshr::cpp_v1::types::Quota {
        dshr::cpp_v1::types::Quota {
            fuel: 1_000_000,
            memory_mb: 64,
            timeout_ms: 1_000,
            max_output_bytes: 1 << 20,
            max_concurrency: 1,
        }
    }

    fn config(&mut self) -> String {
        "{}".to_owned()
    }

    fn wait_cancelled(&mut self, _deadline_ms: u64) -> bool {
        false
    }
}

const GUEST: &[u8] = include_bytes!("fixtures/cpp-component-guest.wasm");

/// M4-COMPONENT-014：真实 guest component（wasm32-wasip2）实现 cpp-v1 world，
/// 宿主 bindgen 实例化并调用 `list-tools` 得到空列表。
#[test]
fn m4_component_014_guest_tool_provider_round_trip() {
    let engine = Engine::default();
    let component = Component::new(&engine, GUEST).expect("guest component parses");
    let mut linker: Linker<HostState> = Linker::new(&engine);
    Plugin::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
        .expect("host imports link");
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi imports link");
    let mut store = Store::new(&engine, HostState::default());
    let instance = Plugin::instantiate(&mut store, &component, &linker)
        .expect("guest component instantiates");
    let tools = instance
        .dshr_cpp_v1_tool_provider()
        .call_list_tools(&mut store)
        .expect("list-tools call");
    assert_eq!(tools.len(), 1, "guest should declare one tool");
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].timeout_ms, Some(1_000));
}

/// M4-COMPONENT-015：真实 cpp-v1 guest 的 `invoke("echo", args)` 走 canonical ABI
/// 返回 `content[0].text == args`。
#[test]
fn m4_component_015_guest_invoke_round_trip() {
    let engine = Engine::default();
    let component = Component::new(&engine, GUEST).expect("guest component parses");
    let mut linker: Linker<HostState> = Linker::new(&engine);
    Plugin::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
        .expect("host imports link");
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi imports link");
    let mut store = Store::new(&engine, HostState::default());
    let instance = Plugin::instantiate(&mut store, &component, &linker)
        .expect("guest component instantiates");
    let result = instance
        .dshr_cpp_v1_tool_provider()
        .call_invoke(&mut store, "echo", "hello")
        .expect("invoke call")
        .expect("guest invoke ok");
    match &result.content[0] {
        dshr::cpp_v1::types::ContentPart::Text(text) => assert_eq!(text, "hello"),
        other => panic!("unexpected content part: {other:?}"),
    }
    assert!(!result.is_error);
}

/// M4-COMPONENT-016：CppComponentPlugin 从真实 guest 注册 echo 并调用。
#[test]
fn m4_component_016_cpp_component_plugin_round_trip() {
    let mut plugin = CppComponentPlugin::new("guest.component", "0.1.0", GUEST)
        .expect("cpp component plugin constructs");
    let mut ctx = PluginContext::new("guest.component");
    plugin.apply(&mut ctx).expect("apply registers guest tools");
    let (tools, _) = ctx.take_registrations();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let result = plugin
        .call("echo", &json!({ "text": "hello" }))
        .expect("guest invoke");
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["content"][0]["text"], "{\"text\":\"hello\"}");
    assert_eq!(result["is_error"], false);
}

/// M4-COMPONENT-020：两个独立 CppComponentPlugin 实例可并行调用 guest，
/// 对应宿主对不同插件并行 call 的模型。
#[test]
fn m4_component_020_parallel_component_instances() {
    let handles: Vec<_> = (0..2)
        .map(|index| {
            std::thread::spawn(move || {
                let mut plugin =
                    CppComponentPlugin::new(format!("guest.{index}"), "0.1.0", GUEST)
                        .expect("cpp component plugin constructs");
                plugin
                    .call("echo", &json!({ "index": index }))
                    .expect("guest invoke")
            })
        })
        .collect();
    for (index, handle) in handles.into_iter().enumerate() {
        let result = handle.join().expect("thread joins");
        assert_eq!(
            result["content"][0]["text"],
            format!("{{\"index\":{index}}}")
        );
    }
}

/// M4-COMPONENT-003：cpp-v1.wit 能生成 world `plugin` 的宿主绑定，
/// 并把 `host` imports 接到 Linker。
#[test]
fn m4_component_003_host_bindings_compile() {
    let engine = Engine::default();
    let mut linker: Linker<HostState> = Linker::new(&engine);
    Plugin::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
        .expect("host imports link");
    let mut store = Store::new(&engine, HostState::default());
    let _ = &mut store;
}
