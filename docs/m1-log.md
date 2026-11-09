# M1 实验记录

> M1 的定义见 [`PLAN.md`](PLAN.md) §8：三个判死实验 + 一条端到端闭环。
> 本文件只记录**可复现的实验事实**，不做进度汇报。被推翻的结论保留并标注。

## 实验 A — GPUI 承载能力

### A1 虚拟化（2026-11-09，已完成）

**问题**：10,000 条事件下，GPUI 的列表是否真虚拟化？

**载体**：`apps/desktop-gpui`（单个 `uniform_list`，10,000 行，行高 22px，窗口 900×600）。
`batch` = 当帧实际构造的行数。

**结果**：`batch = 24`（600px / 22px ≈ 27 可视行）。**虚拟化成立**，24 ≪ 10,000。

> 初测里「稳态 17 ms（≈58.7fps）、最大 2090 ms」的说法**已被 A1b 推翻**：那组数字测的是
> "两次 render 之间的间隔"，而这个界面按需重绘，间隔里大部分是空闲。保留此条以免后来者重复踩坑。

### A1b 启动尖峰拆解（2026-11-09，已完成）

**问题**：首帧后出现 1000–2000 ms 空档，是渲染卡顿，还是启动开销？

**方法**：`PROBE_MODE` 切四档隔离变量，`PROBE_ANIM=1` 用 `request_animation_frame`
强制连续出帧（122 帧后打印分位数并退出），从而把"空闲间隔"从"帧成本"里剔除。

| mode | 行数 | 行内文本 | 用途 |
|---|---|---:|---|
| `empty` | 0 | — | 只剩窗口 + GPU 初始化 |
| `no_text` | 10,000 | 无 | 列表布局，不含文字 shaping |
| `ascii` | 10,000 | ASCII | 排除 CJK 字体回退 |
| `full` | 10,000 | 中文 | 基线 |

**结果**（单次运行，单位 ms）：

| mode | 应用初始化 `t` | 窗口创建 | 首帧 `t` | 连续帧 p50 | p95 | 行构造 |
|---|---:|---:|---:|---:|---:|---:|
| empty | 621.7 | 326.0 | 634 | 31.15 | 32.26 | 0.00 |
| no_text | 725.7 | 337.5 | 739 | 31.24 | 32.87 | 0.05 |
| ascii | 737.2 | 407.5 | 760 | 31.23 | 32.70 | 0.10 |
| full | 656.9 | 346.6 | 676 | 31.24 | 32.19 | 0.14 |

`full` 连跑 3 次：应用初始化 735.6 / 642.0 / 759.4，窗口创建 332.3 / 355.8 / 385.0，
p50 稳定在 31.19–31.26。

**结论**：

1. **所谓"启动尖峰"就是启动总耗时**，不是渲染停滞：应用初始化 0.62–0.76 s + 窗口创建
   0.33–0.41 s ≈ **1.0–1.15 s 到首帧**。
2. `empty` 与 `full` 的启动耗时同量级 → 尖峰**与行数、文本、CJK 无关**，开销在
   GPU/平台/字体系统的初始化里。
3. 连续出帧下四档 p50 全部 ≈ **31.2 ms（≈32 fps）**，且 `render_ms` ≤ 0.14 ms →
   当前工作负载**低于出帧节拍的下限**，测不出真实渲染成本。这同时说明 A1 的 17 ms 是空闲间隔。
4. 没有明显预热效应（3 次重复方差 ±60 ms）。
5. **Caveat**：窗口在后台未聚焦，31 ms 可能是 Windows 的节流节奏，而不是显示器刷新率。
   需要在前台聚焦状态下复测才能下结论。

**尚未拆开的部分**：应用初始化那 0.7 s 内，GPU 设备创建 vs 字体枚举各占多少。外部只能测到总时长，
要拆开需要 gpui 的 `profiler` feature（`FrameTiming` 有 `draw_start`/`draw_end`/`phases`），
或在初始化路径内部埋点。这是 A1 的下一个增量。

**对 M2 的含义**：PLAN §8 M2 的"冷启动 < 1.5 s"目前是**勉强达标**——但这是一个 900×600、
零业务逻辑的空壳。真实应用还要加字体、Markdown、会话加载，1.5 s 需要重新论证或调整。

### A2 中文 IME —— 未开始

GPUI（此 rev）没有内置 `TextInput` 元素，IME 必须通过 `EntityInputHandler` 自绘输入框实现；
`gpui/examples/input.rs` 提供了约 800 行的参考实现。下一步应基于它做最小 IME 探针，而不是先做完整输入框。

### A3 WebView2 叠层 —— 未开始

需要 `gpui-kit` 的 WebView 组件；已知上游限制：WebView 与 GPUI 内容同矩形，popover/dialog 无法可靠覆盖（PLAN §4.2 C）。实验要产出的是"能/不能 + 具体失效场景"。

### A4 流式 Markdown —— 未开始

依赖会话渲染层，排在 A2/A3 之后。

## 实验 B — Rust Cordis 选型（2026-11-09，已完成）

结论与证据见 [`decisions/0002-cordis-rs-selection.md`](decisions/0002-cordis-rs-selection.md)。
要点：两候选测试套件均全绿（dshbox 167 / jincdream 111），选 `jincdream/cordis-rs`。

## 实验 C — Sidecar 真实单位成本 —— 未开始

需要一个真实 DSH 插件（带持久化 + 审批）作为样本，并在实现 L1 协议后进行。

## 附带基线 — Session Log

`crates/session-store` 已实现规范 §4 的 I2/I3/I4 与回放/JSONL 往返，8 个单元测试全绿：

```powershell
tools\build-env.cmd cargo test -p dshr-session-store
# test result: ok. 8 passed; 0 failed
```

覆盖：`seq` 连续、同一次工具调用只能结算一次（result XOR aborted）、assistant 消息只结算一次、
缺字段拒绝写入、回放重建消息/工具/轮次并跳过未知类型、JSONL 往返与损坏输入拒绝。

## 复现方式

```powershell
# 四档启动拆解
tools\build-env.cmd cargo build -p dshr-desktop-gpui
$env:PROBE_MODE='full'; $env:PROBE_ANIM='1'; .\target\debug\dshr-desktop-gpui.exe

# 连续帧分位数（122 帧后自动退出）：替换 PROBE_MODE 为 empty / no_text / ascii / full
```
