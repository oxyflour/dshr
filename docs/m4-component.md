# M4 W5：WASM Component / WIT

## 1. 已落地

- `wasmtime = 50.0.1` 启用 `component-model`。
- `M4-COMPONENT-001`（[component_model.rs](../crates/plugin-wasm/tests/component_model.rs)）：
  用内联 component WAT 走 `Component::new` → `Linker::instantiate` →
  `get_typed_func::<(), (u32,)>` → canonical ABI 返回 `42`。
- `M4-COMPONENT-002`：用 `wit-parser` 解析 [cpp-v1.wit](spec/cpp-v1.wit)，校验 world
  `plugin` 导入 `host`、导出 `tool-provider`。解析时发现并修复了 `host` 里类型
  `quota` 与函数 `quota` 重名 → 函数改为 `get-quota`。
- `M4-COMPONENT-003`（[component_bindings.rs](../crates/plugin-wasm/tests/component_bindings.rs)）：
  `bindgen!` 从 WIT 生成 world `plugin` 的宿主 trait，实现 `host`/`types` 接口
  （含 `tool-call` resource），用 `HasSelf` 把 imports 接到 `Linker`。
- `M4-COMPONENT-004`：component 导出死循环在 `consume_fuel(true)` + `set_fuel(10_000)`
  下 call 返回 Err（fuel 配额 trap）。
- `M4-COMPONENT-005`：`epoch_interruption(true)` + `set_epoch_deadline(1)`，后台
  `increment_epoch()` 后死循环 call 返回 Err（epoch 中断）。
- `M4-COMPONENT-006`：`StoreLimitsBuilder::memory_size(64 KiB)`，component 尝试
  `memory.grow` 到超限后 `unreachable`，call 返回 Err（内存上限）。

命令：

```text
cargo test -p dshr-plugin-wasm --test component_model
cargo test -p dshr-plugin-wasm --test component_bindings
→ 6 passed
```

## 2. 还没做

- 没有 `ComponentPlugin` 加载器实现 `Plugin` trait；component 还不能被
  `dshr-plugin-host` 安装/调用。
- host imports 已能生成/接入 `Linker`，但没有真实 component 实例调用它们；
  fuel/epoch/内存配额已有测试，输出/并发仍未验证；当前 core-module 的配额路径
  不能直接假设适用。
- 没有 component 模板、签名/安装/卸载链路、作者文档。

W5 仍是 **部分完成**，不能因为 2 个探针测试标完成。
