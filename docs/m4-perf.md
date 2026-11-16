# M4 性能基线（G4）

G4 要求"基线 + 阈值由 CI 执行"，不是只在本机跑一次。本文件定义 ADC 构造阶段
（`parse_tree` + `adc_view`，无窗口）的门禁，窗口 draw 仍由 `tools/frame-probe.ps1`
在真机手动采样，二者不混用。

## 1. 阈值

| 指标 | 阈值 | 来源 |
|---|---|---|
| 10 棵典型 ADC 树的 `worst_total_p95_ms` | **≤ 50 ms** | `crates/ui-gpui/tests/adc_perf.rs` 的 `P95_LIMIT_MS` |
| 满配额树（200 节点 / depth 8）`total_p95_ms` | **≤ 16.7 ms** | 单帧 60 Hz 预算，调试构建留余量 |
| a11y 缺失 | **0** | 每棵树 `a11y_missing` 必须为 0 |

50 ms 是"构造阶段远小于 16.7 ms 帧预算"的保守上界；实际上限由行 2 的 16.7 ms
单独约束满配额树。阈值改动必须同时改本文与 `tools/perf-gate.mjs` 默认值。

## 2. 命令

```text
node tools/perf-gate.mjs
node tools/perf-gate.mjs --json docs/perf-baseline.json
node tools/perf-gate.mjs --limit 50 --max-quota-limit 16.7
```

脚本跑 `cargo test -p dshr-ui-gpui --test adc_perf -- --nocapture`，解析
`ADC-PERF-JSON` / `ADC-PERF-MAX-QUOTA`，任一超阈值/缺 JSON/测试失败都 exit 1。
`--json` 写稳定 schema（`schema_version: 1`），供 CI artifact 与趋势对比。

## 3. 本机基线（2026-11-16，debug）

```text
[perf-gate] worst_total_p95=10.799ms (limit 50ms), trees=10
[perf-gate] max_quota_total_p95=4.033ms (limit 16.7ms), nodes=200
[perf-gate] PASS
```

最慢的典型树是 `table-panel`（2400 行）与 `code-400`；两者都在 debug 构建下低于
阈值。完整数据见 [`perf-baseline.json`](perf-baseline.json)。

## 4. CI 接入

[`.github/workflows/conformance.yml`](../.github/workflows/conformance.yml) 在
`cargo test --workspace` 之后执行：

```yaml
- name: ADC performance baseline (G4)
  if: always()
  run: node tools/perf-gate.mjs --json artifacts/perf-baseline.json
```

并上传 `perf-baseline` artifact（30 天）。**该 workflow 尚未在 GitHub Actions 实跑**，
所以 G4 目前是"门禁已落地、本机通过"，不是"CI 已绿"。

## 5. 与 W3 窗口 draw 预算的关系

`adc_perf` 只测元素树构造；窗口内 layout/prepaint/paint 需要 `gpui::Window` 与
test-support feature，当前仓库未启用。窗口 draw 的采样口径仍是
[`m1-log.md`](m1-log.md) A1c（profiler + `FrameTiming`），M4 W3 继续用手动
`frame-probe` 记录，不能拿本文件的构造耗时冒充 draw 耗时。
