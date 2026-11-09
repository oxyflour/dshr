# M1 实验记录

> M1 的定义见 [`PLAN.md`](PLAN.md) §8：三个判死实验 + 一条端到端闭环。
> 本文件只记录**可复现的实验事实**，不做进度汇报。

## 实验 A — GPUI 承载能力

### A1 虚拟化基线（2026-11-09，已完成）

**问题**：10,000 条事件下，GPUI 的列表是否真虚拟化，稳态帧时间是否可接受？

**载体**：`apps/desktop-gpui`（单个 `uniform_list`，10,000 行，行高 22px，窗口 900×600）。
测量方式：在 `render()` 内记录距上一帧的时间与行构造耗时，每帧打印；`batch` 为当帧实际构造的行数。

**结果**（本机 Windows，MSVC，`cargo build` 未优化 debug）：

| 指标 | 值 | 判读 |
|---|---:|---|
| 逻辑行数 | 10,000 | — |
| 每帧构造行数 `batch` | **24** | 虚拟化成立：24 ≪ 10,000 |
| 第 2 帧 | 22.7 ms | 正常 |
| 第 3 帧 | 1122 ms | **启动尖峰**：字体/管线初始化 |
| 稳态第 30 帧 | **17.0 ms**（≈58.7 fps） | 达到 60fps 基线附近 |
| 全程最大帧 | 2090 ms | 冷启动尖峰，需单独设预算 |
| 行构造耗时 `render_ms` | 0.05–0.08 ms | 行构造不是瓶颈 |

**注意**：`render_ms` 只包含构造行元素的时间，不包含 GPUI 内部的 layout/paint，因此**不能**当作完整帧预算使用。

**结论**：虚拟化成立，稳态帧时间可用于 M1 基线；冷启动存在 1–2 秒尖峰，需要在 M1 结束前拆解（字体加载？管线编译？）并给出可接受阈值。

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
# 构建并运行探针（会打开一个窗口，Ctrl-C 或关闭窗口退出）
tools\build-env.cmd cargo build -p dshr-desktop-gpui
.\target\debug\dshr-desktop-gpui.exe

# 只看编译
tools\build-env.cmd cargo check -p dshr-desktop-gpui
```
