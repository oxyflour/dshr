//! M4 W5：用 `wasmtime::component::bindgen!` 从 `docs/spec/cpp-v1.wit` 生成宿主绑定。
//!
//! 当前只验证 WIT 能生成可编译的 world/type；真正的 host imports 实现与
//! ComponentPlugin 加载器仍待接入（见 docs/m4-component.md）。

#![cfg(test)]

use wasmtime::component::{bindgen, HasSelf, Linker};
use wasmtime::{Engine, Store};

bindgen!({
    path: "../../docs/spec/cpp-v1.wit",
    world: "plugin",
});

struct HostState;

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

/// M4-COMPONENT-003：cpp-v1.wit 能生成 world `plugin` 的宿主绑定，
/// 并把 `host` imports 接到 Linker。
#[test]
fn m4_component_003_host_bindings_compile() {
    let engine = Engine::default();
    let mut linker: Linker<HostState> = Linker::new(&engine);
    Plugin::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
        .expect("host imports link");
    let mut store = Store::new(&engine, HostState);
    let _ = &mut store;
}
