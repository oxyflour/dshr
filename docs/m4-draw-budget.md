# M4 窗口 draw 预算（W3）

G4 的 `tools/perf-gate.mjs` 只测 ADC 元素树**构造**；真正窗口内 layout/prepaint/paint
由 `tools/frame-probe.ps1` 采样。本文件记录 M4 的一次实测，避免把构造耗时当成 draw。

## 1. 命令与环境

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools\frame-probe.ps1 -Frames 120
```

| 项 | 值 |
|---|---|
| 主机 / 会话 | OXYFLOUR-PC11 / `RDP-Tcp#0`（**远程会话**） |
| GPU | Microsoft Remote Display Adapter，2256×1504 |
| refresh | 31.25 ms（32 Hz）→ budget 31 250 µs |
| 二进制 | `target/release/chat.exe`（commit `b1e8f71`，`tree_dirty=False`） |
| 场景 | 7 500 行、每帧滚动、120 draws |

## 2. 结果（2026-11-16）

```text
[frame-probe] draws=120 elapsed_ms=6240 avg_fps=19.2 rows=7500
[frame-probe] draw_work p50_us=2786.70 p95_us=9396.50 max_us=12390.70
[frame-probe] request_layout p50_us=791.80 p95_us=2361.50
[frame-probe] prepaint p50_us=1504.20 p95_us=5010.10
[frame-probe] paint p50_us=323.40 p95_us=1261.40
[frame-probe] present_interval p50_us=31215.10 p95_us=35245.20
[frame-probe] missed_refresh=0 missed_ratio=0.0000
[frame-probe] HARD PASS: missed_ratio=0, draw_p95_us=9396.5 <= budget_us=31250
```

**硬性判定通过**：没有错过刷新，draw_work p95 9.40 ms < 31.25 ms。成本主要在
prepaint（p95 5.01 ms）与 layout（2.36 ms），paint 只 1.26 ms。

## 3. 相对基线未通过（环境原因，未改判为回归）

同一主机/会话的旧基线（commit `f4226dc`）为 avg_fps=23.6、draw_p95=8405.7 µs；
本次 avg_fps=19.2，低于 95% 下限 22.42，脚本 exit 1。当前是 RDP 会话，present
p50 正好 31.2 ms（32 Hz 节拍），远端合成器/负载会压低 avg_fps；这不能单独判为
产品回归，也不拿它当"通过"证据。要下结论需在本地控制台前台复测。

## 4. 与 G4 的分工

| 门禁 | 测什么 | 是否进 CI |
|---|---|---|
| `tools/perf-gate.mjs` | ADC 构造 p95 / 满配额 / a11y | 是（conformance.yml step + artifact） |
| `tools/frame-probe.ps1` | 窗口 draw/layout/prepaint/paint、missed refresh | 否（需要真实窗口/GPU，手动） |

W3 的 draw 预算因此记为"**硬性预算本机 PASS；相对基线受 RDP 影响未通过**"，
不能标成"窗口 draw 已稳定达标"。
