//! WASM 插件主机（wasmtime）：fuel / epoch / 内存 / 输出四类配额 + core-module ABI。
//!
//! 对应 spec/cpp-v1.md §3.3（`runtime.kind = "wasm-component"`）、§3.5（配额）、
//! §10（版本/capability）、§12（进程/隔离语义）与 ADR-0010 §1 的五维配额模型。
//!
//! # 与 spec 的关系（诚实标注）
//!
//! 本 crate 实现的是 **core WASM module**（导出 `memory` + 两个自定义函数）而不是
//! Component Model / WIT 世界（`docs/spec/cpp-v1.wit` 仍是草案）。原因：M3 的验收
//! 口径是"四类配额隔离 + 宿主存活"，core module 已能完整验证；Component 适配
//! （canonical ABI / WIT 绑定）作为后续项。ABI 见 [`ABI_VERSION`]。
//!
//! # ABI（`dshr-wasm-core/v1`）
//!
//! 模块必须导出：
//!
//! | 导出 | 签名 | 说明 |
//! |---|---|---|
//! | `memory` | memory | 线性内存，宿主读写 JSON |
//! | `dshr_alloc(len: i32) -> i32` | 分配 `len` 字节，返回偏移；负数 = 失败 |
//! | `dshr_invoke(ptr: i32, len: i32) -> i64` | 入参 JSON 在 `memory[ptr..ptr+len]`；返回打包结果 |
//!
//! 返回值打包：`(out_ptr as u64) << 32 | (out_len as u64)`；返回 `-1` 表示 guest 错误。
//! 入参/返回都是 UTF-8 JSON：入参 `{"tool": "<name>", "arguments": <json>}`，返回值为
//! 工具结果 JSON。非法导出/越界指针/非法 JSON 都返回结构化 [`WasmError`]，不 panic。
//!
//! # 隔离
//!
//! 每个插件一个 `Engine`（含后台 epoch 线程）与 `Module`；每次调用新建 `Store`
//! （带 `StoreLimits` + fuel + epoch deadline），所以单次 trap 不会污染后续调用。
//! 四类配额：
//!
//! - **fuel**：`Config::consume_fuel(true)` + `Store::set_fuel`；耗尽 → [`WasmError::OutOfFuel`]；
//! - **墙钟超时**：`Config::epoch_interruption(true)` + 后台线程 `Engine::increment_epoch`；
//!   超过 `timeout_ms` → [`WasmError::Timeout`]；
//! - **内存**：`StoreLimits::memory_size(max_memory_bytes).trap_on_grow_failure(true)`；
//!   超限 → [`WasmError::MemoryLimit`]；
//! - **输出**：返回长度 > `max_output_bytes` → [`WasmError::OutputTooLarge`]（不读取越界内存）。
//!
//! # 接入 `plugin-host`（本轮不接）
//!
//! 建议接入点：`PluginHost::load_with_manifest` 在 `runtime.kind == "wasm-component"`
//! 分支构造 `WasmEngineConfig::from_manifest(manifest)` + `WasmPluginRuntime::from_file`
//! （entry 相对包目录解析，复用 `external.rs` 的 `resolve_entry` 安全检查），再实现一个
//! `impl Plugin for WasmPlugin`：`apply` 从 `process.ready` 等价声明注册工具（本 crate
//! 暂未实现工具声明 ABI，先按 manifest `[[tools]]` 或后续 WIT 定义），`call` 走
//! [`WasmPluginRuntime::invoke`]。

#![forbid(unsafe_code)]

pub mod component_plugin;
pub mod cpp_component;
pub mod plugin;

pub use component_plugin::ComponentPlugin;
pub use cpp_component::CppComponentPlugin;
pub use plugin::WasmPlugin;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dshr_plugin_api::manifest::{Manifest, RuntimeKind, RuntimeSpec};
use dshr_plugin_api::PluginError;
use serde_json::Value;
use thiserror::Error;
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};

/// 单个 Store 允许的 table 个数上限（P1-2：防止用多 table 绕过内存预算）。
pub const MAX_TABLES: usize = 1;
/// 单个 Store 允许的 table 元素总数上限（P1-2）。
pub const MAX_TABLE_ELEMENTS: usize = 1024;
/// 单个 Store 允许的 linear memory 个数上限（P1-2：`memory_size` 是按 memory 计的）。
pub const MAX_MEMORIES: usize = 1;
/// 单个 Store 允许的实例数上限（P1-2）。
pub const MAX_INSTANCES: usize = 1;

/// 本 crate 的 ABI 版本（进入 manifest/capability 协商时使用）。
pub const ABI_VERSION: &str = "dshr-wasm-core/v1";

/// 固定的 wasmtime 版本（`Cargo.toml` 的 `=50.0.1` 必须与此一致）。
pub const WASMTIME_VERSION: &str = "50.0.1";

/// 默认 fuel：2e9（spec/cpp-v1.md §3.5）。
pub const DEFAULT_FUEL: u64 = 2_000_000_000;
/// fuel 上限：1e10（spec/cpp-v1.md §3.5）。
pub const MAX_FUEL: u64 = 10_000_000_000;
/// 默认线性内存：128 MB（spec/cpp-v1.md §3.5）。
pub const DEFAULT_MAX_MEMORY_MB: u64 = 128;
/// 线性内存上限：512 MB（spec/cpp-v1.md §3.5）。
pub const MAX_MEMORY_MB: u64 = 512;
/// 默认墙钟超时：30 s（spec/cpp-v1.md §3.2 示例）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// 单次 Tool 输出上限：1 MB（spec/cpp-v1.md §3.5）。
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 1_048_576;
/// 每插件并发上限：4（spec/cpp-v1.md §3.5）。
pub const DEFAULT_MAX_CONCURRENCY: u32 = 4;
/// epoch 线程的 tick 粒度：默认 10 ms（ADR-0010 反悔条件允许 100ms 粒度）。
pub const DEFAULT_EPOCH_TICK_MS: u64 = 10;

/// 一个 WASM 插件的配额与运行参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmEngineConfig {
    /// wasmtime fuel（指令预算）。
    pub fuel: u64,
    /// 线性内存上限（字节）。
    pub max_memory_bytes: u64,
    /// 墙钟超时（毫秒）。
    pub timeout_ms: u64,
    /// 单次调用输出上限（字节）。
    pub max_output_bytes: u64,
    /// 每插件并发调用上限。
    pub max_concurrency: u32,
    /// epoch 线程 tick（毫秒）；不是 manifest 字段，供测试调小。
    pub epoch_tick_ms: u64,
}

impl Default for WasmEngineConfig {
    fn default() -> Self {
        Self {
            fuel: DEFAULT_FUEL,
            max_memory_bytes: DEFAULT_MAX_MEMORY_MB * 1024 * 1024,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            epoch_tick_ms: DEFAULT_EPOCH_TICK_MS,
        }
    }
}

impl WasmEngineConfig {
    /// 显式构造（测试/宿主自定义配额用）。
    pub fn new(
        fuel: u64,
        max_memory_bytes: u64,
        timeout_ms: u64,
        max_output_bytes: u64,
        max_concurrency: u32,
    ) -> Self {
        Self {
            fuel,
            max_memory_bytes,
            timeout_ms,
            max_output_bytes,
            max_concurrency,
            epoch_tick_ms: DEFAULT_EPOCH_TICK_MS,
        }
    }

    /// 调整 epoch tick（测试用；越小超时越精确、开销越大）。
    pub fn with_epoch_tick_ms(mut self, epoch_tick_ms: u64) -> Self {
        self.epoch_tick_ms = epoch_tick_ms.max(1);
        self
    }

    /// 从 manifest 映射（spec/cpp-v1.md §3.3/§3.5）。
    ///
    /// - `runtime.fuel`（缺省 2e9，上限 1e10）→ wasmtime fuel；
    /// - `runtime.max_memory_mb`（缺省 128，1–512）→ `StoreLimits`；
    /// - `runtime.timeout_ms`（缺省 30 s，1–600 s）→ epoch deadline；
    /// - 输出上限 / 并发上限取 spec 默认值（manifest 目前没有这两个字段）。
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, WasmError> {
        if manifest.runtime.kind != RuntimeKind::WasmComponent {
            return Err(WasmError::Manifest(PluginError::InvalidManifest(format!(
                "runtime.kind is `{:?}`, not `wasm-component`",
                manifest.runtime.kind
            ))));
        }
        Self::from_runtime(&manifest.runtime)
    }

    /// 从 `[runtime]` 段映射（`from_manifest` 的实现体，便于单测）。
    pub fn from_runtime(runtime: &RuntimeSpec) -> Result<Self, WasmError> {
        let fuel = runtime.fuel.unwrap_or(DEFAULT_FUEL);
        if fuel > MAX_FUEL {
            return Err(WasmError::Manifest(PluginError::InvalidManifest(format!(
                "runtime.fuel {fuel} exceeds maximum {MAX_FUEL}"
            ))));
        }
        let memory_mb = runtime.max_memory_mb.unwrap_or(DEFAULT_MAX_MEMORY_MB);
        if memory_mb == 0 || memory_mb > MAX_MEMORY_MB {
            return Err(WasmError::Manifest(PluginError::InvalidManifest(format!(
                "runtime.max_memory_mb {memory_mb} must be within 1..={MAX_MEMORY_MB}"
            ))));
        }
        let timeout_ms = runtime.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        if !(1_000..=600_000).contains(&timeout_ms) {
            return Err(WasmError::Manifest(PluginError::InvalidManifest(format!(
                "runtime.timeout_ms {timeout_ms} must be within 1000..=600000"
            ))));
        }
        Ok(Self {
            fuel,
            max_memory_bytes: memory_mb * 1024 * 1024,
            timeout_ms,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            epoch_tick_ms: DEFAULT_EPOCH_TICK_MS,
        })
    }

    /// epoch deadline：把毫秒超时折算成 tick 数（至少 1 个 tick）。
    fn epoch_deadline(&self) -> u64 {
        let tick = self.epoch_tick_ms.max(1);
        self.timeout_ms.div_ceil(tick).max(1)
    }
}

/// WASM 主机错误；`code()` 映射到 spec/cpp-v1.md §9/§12.2 的 `E_*`。
#[derive(Debug, Error)]
pub enum WasmError {
    #[error("manifest/runtime config: {0}")]
    Manifest(#[from] PluginError),
    #[error("invalid wasm module: {0}")]
    InvalidModule(String),
    #[error("ABI error: {0}")]
    Abi(String),
    #[error("E_QUOTA: wasm fuel exhausted")]
    OutOfFuel,
    #[error("E_TIMEOUT: wall-clock timeout after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("E_QUOTA: memory limit exceeded: {0}")]
    MemoryLimit(String),
    #[error("E_TOO_LARGE: output {size} bytes exceeds limit {limit}")]
    OutputTooLarge { size: usize, limit: u64 },
    #[error("E_BACKPRESSURE: max_concurrency {limit} reached")]
    Backpressure { limit: u32 },
    #[error("guest error: {0}")]
    Guest(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("wasm trap: {0}")]
    Trap(String),
    #[error("wasm engine error: {0}")]
    Engine(String),
}

impl WasmError {
    /// 稳定错误码（与 spec 的 `E_*` 闭集对齐；`E_MEMORY` 归入配额）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Manifest(_) => "E_MANIFEST",
            Self::InvalidModule(_) | Self::Abi(_) => "E_PROTOCOL",
            Self::OutOfFuel | Self::MemoryLimit(_) => "E_QUOTA",
            Self::Timeout { .. } => "E_TIMEOUT",
            Self::OutputTooLarge { .. } => "E_TOO_LARGE",
            Self::Backpressure { .. } => "E_BACKPRESSURE",
            Self::Guest(_) | Self::Trap(_) => "E_INTERNAL",
            Self::Protocol(_) => "E_PROTOCOL",
            Self::Engine(_) => "E_INTERNAL",
        }
    }

    /// 是否属于配额类（fuel/内存/输出/超时），供宿主标记 `E_QUOTA` 诊断。
    pub fn is_quota(&self) -> bool {
        matches!(
            self,
            Self::OutOfFuel
                | Self::MemoryLimit(_)
                | Self::OutputTooLarge { .. }
                | Self::Timeout { .. }
        )
    }
}

/// 运行时信息（诊断/证据用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmRuntimeInfo {
    pub abi_version: &'static str,
    pub wasmtime_version: &'static str,
    pub fuel: u64,
    pub max_memory_bytes: u64,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub max_concurrency: u32,
}

/// 一个 WASM 插件实例：`Engine` + `Module` + 并发/epoch 状态。
///
/// `invoke` 只取 `&self`，可在多线程里共享（`Arc<WasmPluginRuntime>`），并发数受
/// [`WasmEngineConfig::max_concurrency`] 限制（超出返回 `E_BACKPRESSURE`，不排队，
/// 对齐 spec §5.2/§12.4 的"不排队"语义）。
pub struct WasmPluginRuntime {
    engine: Engine,
    module: Module,
    /// 原始模块字节（WAT 文本或 wasm 二进制），`relaunch` 用。
    module_source: Vec<u8>,
    config: WasmEngineConfig,
    plugin_id: String,
    active: Arc<AtomicU32>,
    traps: Arc<AtomicU32>,
    ticker_stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for WasmPluginRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmPluginRuntime")
            .field("plugin_id", &self.plugin_id)
            .field("config", &self.config)
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("traps", &self.traps.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for WasmPluginRuntime {
    fn drop(&mut self) {
        self.stop_ticker();
    }
}

impl WasmPluginRuntime {
    /// 从 WAT 文本创建（fixture/测试；不需要 wasm32 target）。
    pub fn from_wat(wat: &str, config: WasmEngineConfig) -> Result<Self, WasmError> {
        Self::from_bytes(wat.as_bytes().to_vec(), config)
    }

    /// 从 wasm 二进制或 WAT 文本创建。
    pub fn from_bytes(source: Vec<u8>, config: WasmEngineConfig) -> Result<Self, WasmError> {
        let engine = build_engine(&config)?;
        let module = compile_module(&engine, &source)?;
        let (ticker_stop, ticker) = start_epoch_ticker(&engine, &config);
        Ok(Self {
            engine,
            module,
            module_source: source,
            config,
            plugin_id: String::new(),
            active: Arc::new(AtomicU32::new(0)),
            traps: Arc::new(AtomicU32::new(0)),
            ticker_stop,
            ticker: Some(ticker),
        })
    }

    /// 从文件创建（`.wasm` 二进制或 `.wat` 文本）。
    pub fn from_file(path: impl AsRef<Path>, config: WasmEngineConfig) -> Result<Self, WasmError> {
        let path = path.as_ref();
        let source = std::fs::read(path).map_err(|error| {
            WasmError::InvalidModule(format!("cannot read `{}`: {error}", path.display()))
        })?;
        Self::from_bytes(source, config)
    }

    /// 设置插件 id（诊断/`plugin.failed` 用；可选）。
    pub fn with_plugin_id(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = plugin_id.into();
        self
    }

    /// 静态校验：能被编译即可（不实例化、不执行）。
    ///
    /// 接受 wasm 二进制与 WAT 文本；拒绝非 wasm 内容与非法导出留给实例化期
    /// （[`WasmPluginRuntime::invoke`] 会返回结构化 `E_PROTOCOL`）。
    pub fn validate_module(source: &[u8]) -> Result<(), WasmError> {
        let engine = build_engine(&WasmEngineConfig::default())?;
        compile_module(&engine, source).map(|_| ())
    }

    pub fn config(&self) -> &WasmEngineConfig {
        &self.config
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// 累计 trap 次数（宿主存活证据）。
    pub fn trap_count(&self) -> u32 {
        self.traps.load(Ordering::Relaxed)
    }

    /// 当前在途调用数。
    pub fn active_calls(&self) -> u32 {
        self.active.load(Ordering::Relaxed)
    }

    pub fn info(&self) -> WasmRuntimeInfo {
        WasmRuntimeInfo {
            abi_version: ABI_VERSION,
            wasmtime_version: WASMTIME_VERSION,
            fuel: self.config.fuel,
            max_memory_bytes: self.config.max_memory_bytes,
            timeout_ms: self.config.timeout_ms,
            max_output_bytes: self.config.max_output_bytes,
            max_concurrency: self.config.max_concurrency,
        }
    }

    /// 重建实例（trap / engine 状态异常后恢复用）：重建 Engine + Module + epoch 线程。
    ///
    /// 语义上等价于"同一插件的新 generation"：旧的 `Store` 已随 trap 丢弃。
    pub fn relaunch(&mut self) -> Result<(), WasmError> {
        self.stop_ticker();
        let engine = build_engine(&self.config)?;
        let module = compile_module(&engine, &self.module_source)?;
        let (ticker_stop, ticker) = start_epoch_ticker(&engine, &self.config);
        self.engine = engine;
        self.module = module;
        self.ticker_stop = ticker_stop;
        self.ticker = Some(ticker);
        Ok(())
    }

    /// 调用一个工具：JSON 入参 → JSON 返回。
    ///
    /// 每次调用使用**新的 `Store`/`Instance`**（fuel/内存/epoch 都按本次配额重置），
    /// 因此一次 trap 不影响后续调用；需要跨调用状态的插件 ABI 留给后续版本。
    pub fn invoke(&self, tool: &str, args: &Value) -> Result<Value, WasmError> {
        let _slot = self.acquire_slot()?;
        self.invoke_inner(tool, args)
    }

    fn acquire_slot(&self) -> Result<ConcurrencySlot, WasmError> {
        let limit = self.config.max_concurrency.max(1);
        loop {
            let current = self.active.load(Ordering::SeqCst);
            if current >= limit {
                return Err(WasmError::Backpressure { limit });
            }
            if self
                .active
                .compare_exchange_weak(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(ConcurrencySlot {
                    active: Arc::clone(&self.active),
                });
            }
        }
    }

    fn invoke_inner(&self, tool: &str, args: &Value) -> Result<Value, WasmError> {
        // ABI 请求形状固定为 `{"tool":"<name>","arguments":<json>}`：**tool 在前**，
        // 这样 guest 可以在固定偏移（9）读到工具名首字符做最简分发（WAT fixture 依赖）。
        // 不直接用 `json!` 是因为 serde_json 的 Map 默认按 key 排序（arguments 会在前）。
        let tool_json = serde_json::to_string(tool)
            .map_err(|error| WasmError::Protocol(format!("cannot serialize tool name: {error}")))?;
        let args_json = serde_json::to_string(args)
            .map_err(|error| WasmError::Protocol(format!("cannot serialize arguments: {error}")))?;
        let request_bytes =
            format!("{{\"tool\":{tool_json},\"arguments\":{args_json}}}").into_bytes();
        // 输入与输出用同一上限：避免把超大参数灌进 guest（spec §3.5 单值 ≤1MB）。
        if request_bytes.len() as u64 > self.config.max_output_bytes {
            return Err(WasmError::OutputTooLarge {
                size: request_bytes.len(),
                limit: self.config.max_output_bytes,
            });
        }

        let mut store = self.new_store()?;
        store
            .set_fuel(self.config.fuel)
            .map_err(|error| WasmError::Engine(format!("cannot set fuel on store: {error}")))?;
        store.set_epoch_deadline(self.config.epoch_deadline());

        let instance =
            Instance::new(&mut store, &self.module, &[]).map_err(|error| self.classify(error))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| WasmError::Abi("module must export `memory`".to_owned()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "dshr_alloc")
            .map_err(|_| {
                WasmError::Abi("module must export `dshr_alloc(i32) -> i32`".to_owned())
            })?;
        let invoke = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "dshr_invoke")
            .map_err(|_| {
                WasmError::Abi("module must export `dshr_invoke(i32, i32) -> i64`".to_owned())
            })?;

        let request_len = i32::try_from(request_bytes.len())
            .map_err(|_| WasmError::Abi("request JSON is too large for the i32 ABI".to_owned()))?;
        let ptr = alloc
            .call(&mut store, request_len)
            .map_err(|error| self.classify(error))?;
        // P3：ABI 约定 0 也是无效地址（wasm 线性内存 0 页/0 偏移不可写），统一按失败处理。
        if ptr <= 0 {
            return Err(WasmError::Abi(format!(
                "`dshr_alloc` returned non-positive offset {ptr}"
            )));
        }
        let start = ptr as usize;
        let memory_size = memory.data_size(&store);
        if start.saturating_add(request_bytes.len()) > memory_size {
            return Err(WasmError::Abi(format!(
                "`dshr_alloc({request_len})` returned out-of-bounds offset {start} (memory {memory_size})"
            )));
        }
        memory
            .write(&mut store, start, &request_bytes)
            .map_err(|error| {
                WasmError::Abi(format!("cannot write request into guest memory: {error}"))
            })?;

        let packed = invoke
            .call(&mut store, (ptr, request_len))
            .map_err(|error| self.classify(error))?;
        if packed < 0 {
            return Err(WasmError::Guest(
                "`dshr_invoke` returned -1 (guest error)".to_owned(),
            ));
        }
        let out_ptr = ((packed as u64) >> 32) as u32 as usize;
        let out_len = ((packed as u64) & 0xFFFF_FFFF) as u32 as usize;
        if out_len as u64 > self.config.max_output_bytes {
            return Err(WasmError::OutputTooLarge {
                size: out_len,
                limit: self.config.max_output_bytes,
            });
        }
        let memory_size = memory.data_size(&store);
        if out_ptr.saturating_add(out_len) > memory_size {
            return Err(WasmError::Abi(format!(
                "`dshr_invoke` returned out-of-bounds result (ptr {out_ptr}, len {out_len}, memory {memory_size})"
            )));
        }
        let mut output = vec![0u8; out_len];
        memory.read(&store, out_ptr, &mut output).map_err(|error| {
            WasmError::Abi(format!("cannot read result from guest memory: {error}"))
        })?;
        serde_json::from_slice(&output).map_err(|error| {
            WasmError::Protocol(format!("guest returned invalid JSON result: {error}"))
        })
    }

    fn new_store(&self) -> Result<Store<StoreLimits>, WasmError> {
        // P1-2：只限 `memory_size` 会被"多 memory / 多 table / 多实例"绕过，
        // 所以 table/memories/instances 都设置保守上限（与 manifest 内存预算联动）。
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.config.max_memory_bytes as usize)
            .table_elements(MAX_TABLE_ELEMENTS)
            .tables(MAX_TABLES)
            .memories(MAX_MEMORIES)
            .instances(MAX_INSTANCES)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        // 让 store 内部的 fuel 初始化就位（每次 invoke 前再显式设置）。
        store
            .set_fuel(self.config.fuel)
            .map_err(|error| WasmError::Engine(format!("cannot enable fuel: {error}")))?;
        Ok(store)
    }

    /// 把 wasmtime 错误分类成稳定的 [`WasmError`]（并累计 trap 次数）。
    fn classify(&self, error: wasmtime::Error) -> WasmError {
        self.traps.fetch_add(1, Ordering::Relaxed);
        classify_wasmtime_error(error, self.config.timeout_ms)
    }

    fn stop_ticker(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

/// 并发槽位：Drop 时归还（caller 不持有 `&mut self`，用原子计数）。
struct ConcurrencySlot {
    active: Arc<AtomicU32>,
}

impl Drop for ConcurrencySlot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn build_engine(_config: &WasmEngineConfig) -> Result<Engine, WasmError> {
    let mut wasm_config = Config::new();
    wasm_config.consume_fuel(true);
    wasm_config.epoch_interruption(true);
    // 只接受 core module（Component Model 留给后续 WIT 适配）。
    wasm_config.wasm_component_model(false);
    Engine::new(&wasm_config).map_err(|error| WasmError::Engine(error.to_string()))
}

fn compile_module(engine: &Engine, source: &[u8]) -> Result<Module, WasmError> {
    Module::new(engine, source).map_err(|error| WasmError::InvalidModule(error.to_string()))
}

fn start_epoch_ticker(
    engine: &Engine,
    config: &WasmEngineConfig,
) -> (Arc<AtomicBool>, JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let engine = engine.clone();
    let tick_ms = config.epoch_tick_ms.max(1);
    let handle = thread::Builder::new()
        .name("dshr-wasm-epoch".to_owned())
        .spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(tick_ms));
                engine.increment_epoch();
            }
        })
        .expect("spawn dshr-wasm epoch ticker");
    (stop, handle)
}

/// wasmtime 错误 → 稳定错误码。
///
/// 注意：`Error::to_string()` 只给最外层（wasm backtrace），配额上下文在 error chain
/// 里（如 `forcing trap when growing memory to ... bytes`），所以要拼上 `chain()`。
fn classify_wasmtime_error(error: wasmtime::Error, timeout_ms: u64) -> WasmError {
    let mut text = error.to_string();
    for cause in error.chain() {
        text.push('\n');
        text.push_str(&cause.to_string());
    }
    let lower = text.to_ascii_lowercase();
    if let Some(trap) = error.downcast_ref::<Trap>() {
        match *trap {
            Trap::OutOfFuel => return WasmError::OutOfFuel,
            Trap::Interrupt => return WasmError::Timeout { timeout_ms },
            _ => {}
        }
    }
    if lower.contains("out of fuel") || lower.contains("fuel") {
        return WasmError::OutOfFuel;
    }
    if lower.contains("interrupt") || lower.contains("epoch") || lower.contains("deadline") {
        return WasmError::Timeout { timeout_ms };
    }
    if lower.contains("memory") || lower.contains("grow") {
        return WasmError::MemoryLimit(text);
    }
    if lower.contains("table") || lower.contains("instance") {
        return WasmError::MemoryLimit(text);
    }
    WasmError::Trap(text)
}
