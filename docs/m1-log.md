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

### A1c 帧预算（2026-11-09，已完成）

**问题**：A1b 的 31 ms 只是出帧节拍，真实 `draw` 成本是多少？加压后会不会超预算？

**方法**：开启 gpui `profiler` feature，`set_trace_enabled(true)` 后每帧记录 `Window::draw` 时长，
取 `frame_duration_snapshot()` 的直方图分位数。三档加压：

| 档位 | 行内容 | 是否每帧滚动 |
|---|---|---|
| `light` | 单行单文本 | 否 |
| `heavy` | 徽章 + 主文本 + meta + 时间（4 个子元素、3 段文本） | 否 |
| `heavy+scroll` | 同上 | 是，每帧跳 37 行 |

**结果**（122 帧，单位 ms）：

| 档位 | draw p50 | draw p95 | draw max | present p50 | 行构造 | 占 60Hz 预算（16.7ms） |
|---|---:|---:|---:|---:|---:|---:|
| light | **6.28** | 9.17 | 25.67 | 31.28 | 0.06 | 38% |
| heavy | **15.15** | 21.33 | 26.95 | 31.21 | 0.36 | 91% |
| heavy+scroll | **22.74** | 29.66 | 38.14 | 31.03 | 0.65 | **136%** |

**判读**：

1. 行结构是主要成本：单行从 1 段文本加到 4 个元素后，`draw` 从 6.3 → 15.2 ms（2.4×）。
2. 每帧滚动再加 7.6 ms（重排版 + 重建可视行），达到 22.7 ms。
3. **行构造只占 0.36–0.65 ms**，成本几乎全在 layout/prepaint/paint，不在构造元素
   —— 所以优化方向是"减少每行元素/文本段数"，而不是"优化 Rust 侧构建代码"。
4. 按 60 Hz 目标算，重行 + 每帧滚动超预算 36%；按本机 32 Hz 节拍算则仍在预算内。

**环境警告（重要）**：本机窗口落在 `Microsoft Remote Display Adapter`（2256×1504 @ **32 Hz**），
所以 `present_p50 ≈ 31.2 ms` 正好等于 32 Hz 的帧间隔，**不是**被节流。这意味着：
本机所有绝对帧时间都受 32 Hz 限制，60/144 Hz 桌面上的表现需要另测；百分比列（按 16.7 ms 折算）
才是可跨设备比较的口径。

**结论**：虚拟化没问题，**每行元素数量是真正的预算消耗者**。ADC-v1 的元素集和会话渲染层
应按"单行 ≤ 3 个文本段"设计；工具卡片等复杂内容要避免在滚动列表里逐帧重建。

### A2 中文 IME —— 可运行，待人工验证

GPUI（此 rev）没有内置 `TextInput` 元素，IME 必须通过 `EntityInputHandler` 自绘输入框实现。
探针已就位：`tools/ime-probe`（vendor 自上游 `examples/input.rs`，Apache-2.0，改动仅去掉 wasm 字体加载）。

| 项 | 状态 |
|---|---|
| 编译 | 通过（edition 2024；上游用了 let-chains） |
| 启动 | 通过（窗口存活 6 s 无崩溃） |
| 中文组合输入 | **有人工实测**：首次候选窗出现在**桌面右下角**，第二次起才出现在光标旁 |
| 候选窗定位 | **已知缺陷**，已加缓解（见下），待复测 |
| 删除/选词/UTF-16 选区 | 未验证 |

**根因分析**（读 gpui 源码得出，未打补丁验证）：

- `gpui_windows` 只在 `WM_IME_STARTCOMPOSITION` 时调 `retrieve_caret_position()`，
  再用 `ImmSetCandidateWindow(CFS_CANDIDATEPOS)` 定位候选窗（`events.rs` 152 / 658–690）。
- `retrieve_caret_position` 依赖 input handler 的 `selected_text_range` + `bounds_for_range`；
  后者在 `last_layout` 缺失或与 `content` 不一致时返回 `None`（`input.rs` 的 `bounds_for_range`）。
- 首次组合输入时若拿不到 bounds，就**不会调用** `ImmSetCandidateWindow`，IME 回落到默认位置
  （屏幕右下）；第二次输入时布局已就绪，所以正常。
- 上游示例从头到尾**没有调用** `Window::invalidate_character_coordinates()`
  ——这个 API 正是"主动告知 IME 光标位置"的入口（`window.rs` 6944）。

**缓解措施**（已改探针，尚未复测）：在获得焦点时与鼠标按下时各调用一次
`window.invalidate_character_coordinates()`，它在下一帧用 `selected_bounds` 调
`update_ime_position`，把候选窗位置提前写好。

**如果缓解无效**，下一层修复是给 `gpui_windows` 打补丁：在 `WM_IME_SETCONTEXT` 或焦点切换时
也执行一次 `ImmSetCandidateWindow`（需要 fork 或 `[patch]`，成本较高，先验证缓解是否够）。

验证步骤与要观察的点写在 [`../tools/ime-probe/README.md`](../tools/ime-probe/README.md)。
结论必须写回本节，不能用"能启动"替代"IME 可用"。

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
