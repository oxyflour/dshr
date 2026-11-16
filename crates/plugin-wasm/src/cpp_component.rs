//! M4 W5：真实 `cpp-v1.wit` Component 插件。
//!
//! 用 `bindgen!` 生成宿主绑定，实例化 wasm32-wasip2 guest；
//! `apply` 从 guest `list-tools` 注册工具，`call` 走 guest `invoke` 并把
//! `tool-result` 映射回 JSON。WASI p2 由 `wasmtime-wasi` 提供。

use dshr_plugin_api::{Plugin as PluginTrait, PluginContext, PluginError};
use dshr_tool_runtime::{ToolDefinition, ToolOrigin};
use serde_json::{json, Value};
use wasmtime::component::{bindgen, Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

bindgen!({
    path: "../../docs/spec/cpp-v1.wit",
    world: "plugin",
});

/// Host state for WASI + cpp-v1 imports.
pub struct CppHostState {
    table: ResourceTable,
    ctx: WasiCtx,
}

impl Default for CppHostState {
    fn default() -> Self {
        Self {
            table: ResourceTable::new(),
            ctx: WasiCtxBuilder::new().build(),
        }
    }
}

impl WasiView for CppHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl dshr::cpp_v1::types::Host for CppHostState {}

impl dshr::cpp_v1::types::HostToolCall for CppHostState {
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

impl dshr::cpp_v1::host::Host for CppHostState {
    fn log(&mut self, _level: String, _message: String) {}

    fn get_quota(&mut self) -> dshr::cpp_v1::types::Quota {
        dshr::cpp_v1::types::Quota {
            fuel: 1_000_000,
            memory_mb: 64,
            timeout_ms: 30_000,
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

/// Real cpp-v1 component plugin.
pub struct CppComponentPlugin {
    id: String,
    version: String,
    engine: Engine,
    component: Component,
}

impl CppComponentPlugin {
    pub fn new(id: impl Into<String>, version: impl Into<String>, source: &[u8]) -> Result<Self, PluginError> {
        let engine = Engine::new(&Config::new()).map_err(|error| {
            PluginError::Invalid(format!("component engine: {error}"))
        })?;
        let component = Component::new(&engine, source).map_err(|error| {
            PluginError::Invalid(format!("component does not parse: {error}"))
        })?;
        Ok(Self {
            id: id.into(),
            version: version.into(),
            engine,
            component,
        })
    }

    fn instantiate(
        &self,
    ) -> Result<(Store<CppHostState>, Plugin), PluginError> {
        let mut linker: Linker<CppHostState> = Linker::new(&self.engine);
        Plugin::add_to_linker::<CppHostState, HasSelf<CppHostState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| PluginError::Invalid(format!("host imports: {error}")))?;
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|error| {
            PluginError::Invalid(format!("wasi imports: {error}"))
        })?;
        let mut store = Store::new(&self.engine, CppHostState::default());
        let instance = Plugin::instantiate(&mut store, &self.component, &linker)
            .map_err(|error| PluginError::Invalid(format!("component instantiate: {error}")))?;
        Ok((store, instance))
    }
}

fn content_json(part: &dshr::cpp_v1::types::ContentPart) -> Value {
    match part {
        dshr::cpp_v1::types::ContentPart::Text(text) => json!({ "type": "text", "text": text }),
        dshr::cpp_v1::types::ContentPart::Json(value) => json!({ "type": "json", "json": value }),
        dshr::cpp_v1::types::ContentPart::Image(image) => {
            json!({ "type": "image", "image": image })
        }
    }
}

impl PluginTrait for CppComponentPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let (mut store, instance) = self.instantiate()?;
        let tools = instance
            .dshr_cpp_v1_tool_provider()
            .call_list_tools(&mut store)
            .map_err(|error| PluginError::Invalid(format!("list-tools trap: {error}")))?;
        for decl in tools {
            let input_schema = serde_json::from_str(&decl.input_schema)
                .unwrap_or_else(|_| json!({ "type": "object" }));
            let mut definition = ToolDefinition::new(
                decl.name,
                decl.description,
                ToolOrigin::Native {
                    plugin_id: self.id.clone(),
                },
            )
            .with_schema(input_schema)
            .with_timeout_ms(decl.timeout_ms.unwrap_or(30_000));
            if let Some(presentation) = decl.presentation {
                definition = definition.with_presentation(presentation);
            }
            ctx.register_tool(definition)?;
        }
        Ok(())
    }

    fn call(&mut self, tool: &str, arguments: &Value) -> Result<Value, PluginError> {
        let (mut store, instance) = self.instantiate()?;
        let result = instance
            .dshr_cpp_v1_tool_provider()
            .call_invoke(&mut store, tool, &arguments.to_string())
            .map_err(|error| PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason: format!("component invoke trap: {error}"),
            })?
            .map_err(|reason| PluginError::ToolFailed {
                tool: tool.to_owned(),
                reason,
            })?;
        Ok(json!({
            "content": result.content.iter().map(content_json).collect::<Vec<_>>(),
            "value": result.value,
            "meta": result.meta,
            "is_error": result.is_error,
        }))
    }
}
