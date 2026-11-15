# WASM 插件主机（M3，wasmtime core-module 口径）

> 更新：2026-11-15 · 实现：[`crates/plugin-wasm`](../crates/plugin-wasm/src/lib.rs) ·
> 配额模型：[ADR-0010](decisions/0010-isolation-runtime-quota.md) ·
> 协议草案：[`cpp-v1.wit`](spec/cpp-v1.wit) · 计划：PLAN §3.3/§3.5/§8 M3 G3

## 1. 结论与口径

- 采用 **wasmtime `=50.0.1`**（精确 pin）实现 **core WASM module** 主机，四类配额可执行：
  fuel（计算量）、epoch 墙钟超时、线性内存上限、输出字节上限；并发上限返回 `E_BACKPRESSURE`。
- 每次 `invoke` 新建 Store（fuel/内存/epoch 重置），trap 只影响本次调用；**宿主与其它插件存活**，
  同一 runtime 可继续调用/重建。
- **偏离**：本轮**没有**用 Component Model / WIT canonical ABI，也没有从 `cpp-v1.wit` 生成绑定；
  插件工具声明由 manifest `[[tools]]` 提供。`cpp-v1.wit` 仍是草案，Component 主机作为后续项
  （不影响"隔离"这一 G3 判定）。
- 现有 `external-process` 运行时（[ADR-0010](decisions/0010-isolation-runtime-quota.md) 退路）保留，
  用于需要 OS 进程隔离/高成本场景；WASM 主方案不替代它。

## 2. ABI：`dshr-wasm-core/v1`

core module（`wasm32-unknown-unknown` 或等价）必须导出：

| 导出 | 签名 | 语义 |
|---|---|---|
| `memory` | memory | guest 线性内存 |
| `dshr_alloc` | `(i32 len) -> i32 ptr` | 在 guest 内分配 `len` 字节，返回指针；`0`/负值视为失败 |
| `dshr_invoke` | `(i32 ptr, i32 len) -> i64` | 输入是 UTF-8 JSON 参数；返回打包值 `(ret_ptr << 32) | ret_len`（`ret_ptr` 指向 UTF-8 JSON 结果），`-1` 表示 guest 业务错误 |

宿主要求：

1. 调用前把 JSON 参数写入 guest 内存（`dshr_alloc`）；调用后按返回值读取 guest 内存，**先校验
   `ret_ptr+ret_len` 在 memory 内**、`ret_len ≤ max_output_bytes`，再解析 JSON。
2. 任何 trap（fuel/epoch/内存/unreachable）或 ABI 违规都映射为结构化 `WasmError`，不 panic。
3. 工具名由 manifest `[[tools]]` 声明（与 external-process 相同）；module 只实现通用
   `dshr_invoke`（工具名在 JSON 参数里，`{ "tool": "...", "arguments": {...} }`）。

## 3. 配额映射（ADR-0010 §1）

| 维度 | wasmtime 机制 | 默认/上限 | 超限错误 |
|---|---|---|---|
| 计算量 | `Store::set_fuel`（每次 invoke 重置） | manifest `runtime.fuel`（上限 1e10） | `E_QUOTA`（OutOfFuel trap） |
| 墙钟超时 | epoch deadline + 后台线程 increment | `runtime.timeout_ms`（1s–600s） | `E_TIMEOUT` |
| 线性内存 | `StoreLimits.memory_size` + `trap_on_grow_failure` | `runtime.max_memory_mb`（1–512） | `E_QUOTA`（MemoryLimit trap） |
| 输出 | 读取结果前检查 `max_output_bytes` | 默认 1 MiB，上限 8 MiB | `E_TOO_LARGE` |
| 并发 | 宿主并发计数（per-runtime） | 4（全局 16） | `E_BACKPRESSURE` |

`WasmEngineConfig::from_manifest(&Manifest)` 做映射；`runtime.kind == "wasm-component"` 时由装载方
构造（plugin-host 接入点：`load_with_manifest` 的 wasm 分支，尚未接线）。

## 4. 证据（`CPP-WASM-001..020`）

命令：

```powershell
tools\build-env.cmd cargo test --offline -p dshr-plugin-wasm
```

覆盖（全部离线、WAT fixture）：
fuel 死循环 → `E_QUOTA(OutOfFuel)` 且同 runtime 继续可用；epoch 100ms → `E_TIMEOUT`（实测 ~100ms）；
`StoreLimits` 1MiB + `trap_on_grow_failure` → `E_QUOTA(MemoryLimit)`；1MiB 结果 > 4KiB 上限 →
`E_TOO_LARGE`（不读越界内存）；ABI 缺导出/负 alloc、非法模块、guest `-1`、`unreachable` trap、
trap 后同 runtime 复用/`relaunch`、跨 runtime 隔离、并发 → `E_BACKPRESSURE`、`from_manifest`/`from_file`。

## 5. 接入与后续

1. `plugin-host::load_with_manifest`：`runtime.kind == "wasm-component"` 分支 → 构造
   `WasmPluginRuntime` → `impl Plugin`（工具来自 manifest；`call` → `invoke`）。
2. 打包/信任：WASM 包 = `package.json` + `plugin.toml`（`runtime.entry` 指向 `.wasm`）+
   `dshr-trust.json`；信任链复用（摘要覆盖 `.wasm`）。
3. Component/WIT：用 `wit-bindgen`/`wasmtime::component` 生成 canonical ABI，替换
   `dshr-wasm-core/v1`；tool/panel 声明从组件导出读取。在此之前 `cpp-v1.wit` 只有文档价值。
4. 性能：wasmtime 179 个依赖包，首次编译分钟级；运行时每调用新 Store，冷启动成本待实测
   （M4 可做实例池/预编译缓存）。
