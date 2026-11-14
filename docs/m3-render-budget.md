# M3 ADC 渲染预算（G1 验收记录）

> 更新：2026-11-14 · 探针：[`crates/ui-gpui/tests/adc_perf.rs`](../crates/ui-gpui/tests/adc_perf.rs) ·
> 规范：[`docs/spec/adc-v1.md`](spec/adc-v1.md) §10 · 冻结：[ADR-0011](decisions/0011-adc-v1-freeze.md)
> 结论：**几何构造（parse + AnyElement 构建）在 release 下 10 棵典型树最差 total p95 = 3.78 ms**，
> 占 60 fps（16.7 ms）预算 23%；窗口内 layout/paint 预算仍待 M4/手动（本机离线无法启用
> gpui `test-support`）。

## 1. 测量范围

测的是**无窗口可测**的部分：`parse_tree`（ADC 解析/降级/校验）+ `adc_view`（构造 GPUI
`AnyElement` 树）。不含 layout / prepaint / paint（需要 `Window`；gpui 的 `test-support`
feature 会拉入 proptest/reqwest 等 dev 依赖，本机离线不可用）。

命令：

```powershell
tools\build-env.cmd cargo test --offline --release -p dshr-ui-gpui --test adc_perf -- --nocapture
# debug 保守参考：
tools\build-env.cmd cargo test --offline -p dshr-ui-gpui --test adc_perf -- --nocapture
```

参数：每棵树 60 轮，丢弃前 5 轮预热，报 p50/p95；断言上界 `P95_LIMIT_MS = 50`（本机余量 13×）。
环境：Windows 10.0.26200 x64，RDP 32 Hz 会话（±10% release 波动），release profile。

## 2. Release 基线（ms）

| 树 | 节点 | 数据行/单元格 | 文本字节 | parse p50/p95 | view p50/p95 | **total p50/p95** |
|---|---:|---:|---:|---:|---:|---:|
| chat-card | 33 | 5 | 22 | 0.059 / 0.115 | 0.069 / 0.103 | 0.068 / 0.107 |
| tool-card | 30 | 47 | 500 | 0.121 / 0.151 | 0.234 / 0.280 | 0.228 / 0.308 |
| settings-panel | 44 | 8 | 21 | 0.082 / 0.183 | 0.075 / 0.161 | 0.083 / 0.148 |
| table-panel | 32 | 2400 | 8 | 0.578 / 1.363 | 2.085 / 3.499 | 2.074 / **3.778** |
| list-panel | 30 | 200 | 8 | 0.456 / 1.076 | 0.434 / 0.787 | 0.430 / 0.737 |
| tree-panel | 30 | 200 | 8 | 0.366 / 0.868 | 0.426 / 0.650 | 0.415 / 0.784 |
| nested-layout | 36 | 32 | 12 | 0.045 / 0.112 | 0.039 / 0.072 | 0.038 / 0.071 |
| charts-dashboard | 37 | 32 | 9 | 0.090 / 0.177 | 0.091 / 0.167 | 0.091 / 0.156 |
| markdown-8kb | 30 | 136 | 8148 | 0.046 / 0.081 | 0.731 / 0.992 | 0.724 / 1.201 |
| code-400 | 31 | 400 | 5780 | 0.074 / 0.148 | 1.150 / 1.879 | 1.146 / 1.893 |
| max-quota（200 节点/深度 8） | 200 | — | — | — | 0.414 / 0.535 | 0.809 / 1.172 |

结论：除 table/markdown/code 外均 < 0.55 ms；最差 3.778 ms（2400 单元格的表格）仍在 60 fps 的 23%。
debug 下 table-panel total p95 ≈ 15.3 ms（接近 16.7 ms 帧预算、波动大），**以 release 为准**，
但 app 内实测一次再对外承诺（见 §4）。

## 3. 已知限制与降级建议

1. **draw 未测**：按 M1 A1c 结论，draw 主要由单行内元素数决定（单行 1→4 元素使 draw 6.3→15.2 ms）。
   table-panel 这类 2400 单元格的树不应在滚动列表中每帧整棵重建——建议 chat 侧对 >50 行表格
   分段/折叠，列数 >8 折叠（M4 表格虚拟化）。
2. **panel 外壳不含滚动/虚拟化**：宿主负责把 `adc_panel_body` 放进可滚动容器时限制高度。
3. **降级仍在**：Chart line/donut、Image 内容、交互事件/焦点顺序是 ADR-0011 记录的实现降级。
4. 本机为 RDP 32 Hz，p95 有 ±10%（release）波动；换普通桌面会话需重跑并更新本表。

## 4. 复现与后续

- 复现：上 §1 命令；输出含 `ADC-PERF-JSON {...}` 与 `ADC-PERF-MAX-QUOTA {...}` 两行可解析数据。
- 后续（M4）：启用 gpui `test-support`/`profiler` 做窗口内 layout/paint p95 与 missed-refresh；
  大表格虚拟化；把本表纳入 CI 基线（需要普通桌面 runner）。
- 关联：panel 渲染入口 `dshr_ui_gpui::adc_panel_body(theme, &Value) -> (AnyElement, Vec<Diagnostic>)`
  （`adc_panel_body_from_str` 为 JSON 文本版），chat 接线在下一轮。
