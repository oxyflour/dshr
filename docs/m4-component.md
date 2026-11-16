# M4 W5：WASM Component / WIT

## 1. 已落地

- `wasmtime = 50.0.1` 启用 `component-model`。
- `M4-COMPONENT-001`（[component_model.rs](../crates/plugin-wasm/tests/component_model.rs)）：
  用内联 component WAT 走 `Component::new` → `Linker::instantiate` →
  `get_typed_func::<(), (u32,)>` → canonical ABI 返回 `42`。
- `M4-COMPONENT-002`：用 `wit-parser` 解析 [cpp-v1.wit](spec/cpp-v1.wit)，校验 world
  `plugin` 导入 `host`、导出 `tool-provider`。解析时发现并修复了 `host` 里类型
  `quota` 与函数 `quota` 重名 → 函数改为 `get-quota`。

命令：

```text
cargo test -p dshr-plugin-wasm --test component_model
→ 2 passed
```

## 2. 还没做

- 没有 `ComponentPlugin` 加载器实现 `Plugin` trait；component 还不能被
  `dshr-plugin-host` 安装/调用。
- 没有用 `wasmtime::component::bindgen!` 生成 `cpp-v1.wit` 的宿主/guest 绑定，
  也没有把 host imports（log/get-quota/config/wait-cancelled）接上。
- component 的 fuel/epoch/内存/输出/并发配额未验证；当前 core-module 的配额路径
  不能直接假设适用。
- 没有 component 模板、签名/安装/卸载链路、作者文档。

W5 仍是 **部分完成**，不能因为 2 个探针测试标完成。
