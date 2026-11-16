//! M4 W5 实验性 Component 插件加载器。
//!
//! 当前 ABI 是 `cpp-component-probe/v1`：组件只需导出一个 `run() -> u32`。
//! 它用于验证 Component 的生命周期（加载/实例化/调用/trap）能套进 [`Plugin`] 接口；
//! 正式的 `cpp-v1.wit`（工具声明/调用/资源）仍待接（见 docs/m4-component.md）。
//!
//! 这是 additive/spike 代码：不会被 `plugin-host` 自动识别；宿主需要显式构造它。

use dshr_plugin_api::{Plugin, PluginContext, PluginError};
use dshr_tool_runtime::{ToolDefinition, ToolOrigin};
use serde_json::{json, Value};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

/// 实验性 component 插件：只支持 `component.run` 探针工具。
pub struct ComponentPlugin {
    id: String,
    version: String,
    engine: Engine,
    component: Component,
    /// 单次 call 的 fuel 上限（默认 u64::MAX，即不主动耗尽）。
    fuel: u64,
    /// 序列化结果字节上限（probe ABI 的输出配额；默认 1 MiB）。
    max_output_bytes: usize,
}

impl ComponentPlugin {
    /// 从 component WAT/二进制构造。`id`/`version` 作为插件元数据返回给宿主。
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        source: impl AsRef<[u8]>,
    ) -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|error| {
            PluginError::Invalid(format!("component engine: {error}"))
        })?;
        let component = Component::new(&engine, source.as_ref()).map_err(|error| {
            PluginError::Invalid(format!("component does not parse: {error}"))
        })?;
        Ok(Self {
            id: id.into(),
            version: version.into(),
            engine,
            component,
            fuel: u64::MAX,
            max_output_bytes: 1024 * 1024,
        })
    }

    /// 设置单次 call 的 fuel 上限（用于 W5 fuel 配额）。
    pub fn with_fuel_limit(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// 设置 probe 输出的字节上限（用于 W5 输出配额测试）。
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    fn invoke_run(&mut self) -> Result<u32, PluginError> {
        let linker = Linker::new(&self.engine);
        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(self.fuel)
            .map_err(|error| PluginError::Invalid(format!("component fuel: {error}")))?;
        let instance = linker
            .instantiate(&mut store, &self.component)
            .map_err(|error| PluginError::Invalid(format!("component instantiate: {error}")))?;
        let run = instance
            .get_typed_func::<(), (u32,)>(&mut store, "run")
            .map_err(|error| PluginError::Invalid(format!("component run export: {error}")))?;
        let (value,) = run
            .call(&mut store, ())
            .map_err(|error| PluginError::ToolFailed {
                tool: "component.run".to_owned(),
                reason: format!("component trap: {error}"),
            })?;
        Ok(value)
    }
}

impl Plugin for ComponentPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "component.run",
                "Run the experimental component probe (returns u32).",
                ToolOrigin::Native {
                    plugin_id: self.id.clone(),
                },
            )
            .with_schema(json!({ "type": "object", "additionalProperties": false })),
        )
    }

    fn call(&mut self, tool: &str, _arguments: &Value) -> Result<Value, PluginError> {
        if tool != "component.run" {
            return Err(PluginError::UnknownTool {
                plugin: self.id.clone(),
                tool: tool.to_owned(),
            });
        }
        let value = self.invoke_run()?;
        let result = json!({ "value": value });
        let encoded = serde_json::to_vec(&result).map_err(|error| PluginError::ToolFailed {
            tool: "component.run".to_owned(),
            reason: format!("serialize result: {error}"),
        })?;
        if encoded.len() > self.max_output_bytes {
            return Err(PluginError::ToolFailed {
                tool: "component.run".to_owned(),
                reason: format!(
                    "output {} bytes exceeds limit {}",
                    encoded.len(),
                    self.max_output_bytes
                ),
            });
        }
        Ok(result)
    }
}
