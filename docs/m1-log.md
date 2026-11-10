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

### A2 中文 IME —— 可运行，缺陷已归档

> 候选窗首次错位的问题已转入 [`issues/0001`](issues/0001-ime-candidate-first-composition.md)（Open）。
> 本节只保留结论：编译/启动通过；首次组合输入候选窗位置错误，第二次起正常；缓解无效。

GPUI（此 rev）没有内置 `TextInput` 元素，IME 必须通过 `EntityInputHandler` 自绘输入框实现。
探针已就位：`tools/ime-probe`（vendor 自上游 `examples/input.rs`，Apache-2.0，改动仅去掉 wasm 字体加载）。

| 项 | 状态 |
|---|---|
| 编译 | 通过（edition 2024；上游用了 let-chains） |
| 启动 | 通过（窗口存活 6 s 无崩溃） |
| 中文组合输入 | **有人工实测**：首次候选窗出现在**桌面右下角**，第二次起才出现在光标旁 |
| 候选窗定位 | **已知缺陷**，已加缓解（见下），待复测 |
| 删除/选词/UTF-16 选区 | 未验证 |

根因分析、已尝试的缓解、下一步修复路径全部记在
[`issues/0001`](issues/0001-ime-candidate-first-composition.md)，此处不重复。

### A3 WebView2 —— 引擎已自证，叠层行为待视觉确认

**载体**：`tools/webview-probe`（vendor 自 `gpui-kit` `examples/webview`，另加一条故意压在页面上的
GPUI 横幅，用于观察浮层遮挡）。

**自动可验证部分（已通过）**：

| 项 | 结果 |
|---|---|
| WebView2 运行时 | 已安装 `151.0.4129.107` |
| 构建 | 通过；冷构建 6 m 27 s；必须设 `GPUI_DISABLE_DIRECT_COMPOSITION=true`（上游要求） |
| 启动 | 窗口存活 10 s 无崩溃 |
| **引擎自检（IPC）** | **通过**：页面 JS 经 `window.ipc.postMessage` 回传 `ua=Edge/151`、`dpr=1.75`、视口 `982×638` |
| 页面加载 | `started` 1392 ms → `finished` 1416 ms（`about:blank`，自带 HTML） |

引擎自检的做法：用 `with_html` 注入一段立即执行的脚本，把 UA / devicePixelRatio / 视口尺寸
通过 IPC 发回 Rust 并打印。**这证明 WebView2 进程、JS 执行与 IPC 通道都通了**，不依赖人眼。

**尚未自证的部分（视觉）**：

| 观察点 | 预期（按上游文档） |
|---|---|
| 页面像素是否真的可见 | 可见；不可见说明 DirectComposition 没关掉 |
| 压在页面上的 GPUI 横幅 | **不可见**（原生子窗口在 GPUI 内容之上，同矩形遮挡） |
| 窗口缩放 / 裁剪 / 焦点切换 | Windows 上原生视图**不裁剪**，会溢出容器；焦点需单独验证 |

> 为什么没有自动截图：本机没有装 computer-use 插件，我写了一个 `tools/screen.ps1`
> （截屏 + 置顶 + 点击 + 按键）。截屏对普通窗口有效，但 GPUI 的 `GpuiFast::Window` 在
> 置顶/前台化后仍无法稳定出现在全屏截图里（拿到的常是前台浏览器），所以这三项没有用截图断言。

**对 M2 的含义**：WebView 作为"独立 Panel / 独立窗口"的承载方式可行；但**不能**把 GPUI 浮层
压在页面上（菜单、dialog、tooltip 都要避开 WebView 矩形），这与 PLAN §4.2 C 的结论一致。
| 125%/150% 缩放 | 记录模糊与尺寸偏差 |

观察步骤见 [`../tools/webview-probe/README.md`](../tools/webview-probe/README.md)。

### A4 流式 Markdown —— 未开始

依赖会话渲染层，排在 A2/A3 之后。

## A5 端到端回放（Session Log → 模型 → UI，2026-11-09 已完成）

**问题**：同一份 Session Log 能不能稳定重建出会话界面？这是 M1 退出条件的核心断言。

**实现**：`crates/conversation-model`（`Log → Vec<Item>`）+ `apps/desktop-gpui --bin conversation`。
模型把 durable 事件折叠为：用户消息、助手消息、工具卡片（pending / ok / error / aborted）、
压缩占位；`tool.result` / `tool.aborted` 回填到对应卡片；未知事件跳过。

**自证方式（不依赖人眼）**：

1. `cargo test -p dshr-conversation-model` → **7 passed**，覆盖：
   顺序、工具卡片终态、统计、压缩位置、**JSONL 往返后模型逐项相等**、未知事件跳过、坏 JSON 拒绝。
2. 二进制启动时先跑同一组断言（不一致直接 panic，不开窗），然后打印模型统计。
3. 实际运行输出：

```text
[model] total=6 user=1 assistant=2 tools=2 settled=1 aborted=1 compaction=1
[ui] frame=1 items=6 frame_ms=0.4
[ui] frame=2 items=6 frame_ms=46.6
```

**演示会话覆盖**（`demo_session.jsonl`，10 条事件）：

| # | 事件 | 模型产物 |
|---|---|---|
| 1 | `turn.started` | 不产出（仅计数） |
| 2 | `message.user` | USER |
| 3 | `message.assistant.settled` | AGENT |
| 4–5 | `tool.call` + `tool.result`（native） | TOOL bash [native] ok |
| 6–7 | `tool.call` + `tool.aborted`（sidecar） | TOOL cad_validate [sidecar] aborted(crash) |
| 8 | `context.compacted` | CONTEXT 已压缩 seq 1–3 |
| 9 | `message.assistant.settled` | AGENT |
| 10 | `some.future.event` | 跳过（前向兼容） |

**结论**：M1 退出条件中"同一份 Log 可回放 Native/JS 两类工具调用"已由**Native + sidecar 两个来源**
的工具卡片覆盖；回放的确定性有测试保证，不依赖运行时状态。

### A5b 真实调用闭环（2026-11-09）

在 A5 的基础上补上"记录侧"：`crates/compat-dsh-host/tests/m1_closure.rs` 走完整链路。

```text
sidecar 进程 → invoke probe_echo（3 个 chunk + 结果）
            → 写入 SessionLog（1 turn + 1 user + 1 tool.call + 3 ephemeral chunk + 1 result + 1 assistant）
            → 落盘 JSONL → drop(sidecar)（进程销毁）
            → SessionLog::from_jsonl 重新加载
            → conversation-model 回放 → 与落盘前的模型逐项相等
```

实测输出：

```text
[closure] events=8 items=3 state=Tool(ToolItem { call_id: "call-m1-1", tool: "probe_echo",
          origin: "sidecar", state: Settled { is_error: false } })
test result: ok. 1 passed
```

关键断言：

1. `call_id` 在 IPC 与 Session Log 中是**同一个**（用 `invoke_as` 显式传入，不让两边各自生成）。
2. 流式 chunk 以 `ephemeral` 写入，不进入回放但保留在日志里。
3. **销毁 sidecar 之后**才重新加载日志——证明回放不依赖插件进程存活。
4. 重放模型与关闭前逐项相等（`assert_eq!(before, after)`）。

至此 M1 的"统一 Session Log + 崩溃隔离 + 回放"三件事都有了可跑的端到端证据。

## A6 基本对话（M1 收口，2026-11-09）

**目标**：不接 LLM，也要有一条**能打字、能发送、能看工具卡片、关掉再开还在**的最小产品路径。

**实现**：`apps/desktop-gpui/src/bin/chat.rs`

```text
输入框（Enter 发送）→ 写入 message.user
  → 若文本含「工具」/tool：tool.call → 真实 sidecar invoke → chunk(ephemeral) + tool.result
  → message.assistant.settled（mock 回复）
  → 每次提交后写 .tmp/chat-session.jsonl
启动时若日志存在 → from_jsonl → 直接恢复整段会话（含工具卡片终态）
```

**自检**（无窗口，`chat --selftest`）：

```text
[selftest] items=5
  你    · 你好
  助手  · 收到：你好。当前是 mock agent，尚未接入 LLM。
  你    · 用工具试一下
  工具  · probe_echo [sidecar] · 成功
  助手  · 工具返回 {"echo":{"text":"用工具试一下"}}。当前是 mock agent，尚未接入 LLM。
[selftest] persisted=.tmp/chat-selftest.jsonl replay_equal=true
[selftest] PASS
```

覆盖：两轮对话、真实 sidecar 工具调用、落盘、**从磁盘重新加载后模型逐项相等**。

**已知限制**（都不阻塞"基本对话"）：

| 限制 | 说明 |
|---|---|
| 模型是 mock | 没有接 LLM（M2 的 provider 归一化层） |
| 输入走 `keystroke.key_char` | 没有 `EntityInputHandler`，中文可能要靠 IME 提交后的字符；组合输入候选窗见 [issues/0001](issues/0001-ime-candidate-first-composition.md) |
| 回复不流式 | assistant 文本一次结算，没有逐字渲染 |
| 无多会话 | 固定 `s-chat` 一个会话、一个固定文件 |

**UI 路径的自动化验证**（不靠人眼）：启动 `chat.exe` 后，用
`tools/screen.ps1 -Action keys -Text "hello`n"` 注入真实按键，然后检查落盘文件：

```text
{"type":"message.user", "payload":{"text":"hello"}}
{"type":"message.assistant.settled", "payload":{"text":"收到：hello。当前是 mock agent，尚未接入 LLM。"}}
```

说明 `keystroke.key_char` → 输入框 → Enter 提交 → 写日志这条链路是通的，
而不只是 `--selftest` 里直接调用 `submit()`。未覆盖：中文输入（见下表）。

**运行**：

```powershell
cargo run -p dshr-desktop-gpui --bin chat             # 交互
cargo run -p dshr-desktop-gpui --bin chat -- --selftest   # 无窗口自检
```

## A7 Native Plugin（2026-11-09）

**目标**：补齐 M1 清单里"M1 退出条件 2"缺的那一半——**Native Tool 存在且 Sidecar 崩溃后仍可用**。

**实现**：

| crate | 内容 |
|---|---|
| `plugin-api` | `Plugin` trait（id/apply/call/unload）、`PluginContext`（register_tool / register_panel，纯声明） |
| `plugin-host` | `PluginHost`：load 时**原子提交**声明（失败回滚）、unload 时撤销该插件全部工具与面板、`call` 先过 Registry 的 Policy + schema |
| `tool-runtime` | 上一轮已建：`ToolOrigin::Native/Sidecar{generation}/Mcp`，generation 失效整批撤销 |
| `chat` | 内置 `ProbeNativePlugin`：注册 `probe_native`（文本转大写）+ 一个 Panel |

**自检证据**（`chat --selftest`，其中显式把 sidecar 置空模拟崩溃）：

```text
[selftest] items=8
  你    · 你好
  助手  · 收到：你好。...
  你    · 用工具试一下
  工具  · probe_echo [sidecar] · 成功
  助手  · 工具返回 {"echo":{"text":"用工具试一下"}}。...
  你    · 用原生工具
  工具  · probe_native [native] · 成功
  助手  · 原生工具返回 {"native_echo":"用原生工具"}。...
[selftest] persisted=.tmp/chat-selftest.jsonl replay_equal=true
[selftest] PASS
```

关键断言：`state.host = None`（销毁 sidecar）之后，`probe_native` 仍然成功并写入
`origin: native` 的工具卡片——**这就是 M1 退出条件 2 的直接证明**。

### A7b 统一 Registry（2026-11-09）

**目标**：让 sidecar 工具与原生工具进入**同一个** `ToolRegistry`，由同一个 `authorize` 闸门放行；
并用 generation 处理崩溃与重启。

**改动**：

| 位置 | 改动 |
|---|---|
| `compat-dsh-host` | `tool_definitions()` 把注册映射成 `ToolDefinition{origin: Sidecar{generation}}`；generation 改由**宿主分配**（`node-0001`…），sidecar 只回声 —— 之前 sidecar 硬编码，重启会复用同一代，`invalidate_generation` 就失去意义 |
| `plugin-host` | 暴露 `registry_mut()` 与 `invalidate_generation()`，让外部登记 sidecar 工具 |
| `chat` | 启动时把 sidecar 工具登记进 Registry；每次 sidecar 调用先 `authorize`；`kill_sidecar()` 销毁进程并整代撤销；`restart_sidecar()` 新 generation + 重新登记 |

**自检证据**（`chat --selftest`，4 轮 11 项）：

```text
用工具试一下        → 工具 probe_echo [sidecar] · 成功
kill_sidecar()      → registry 里 probe_echo 消失、probe_native 仍在
用原生工具          → 工具 probe_native [native] · 成功
restart_sidecar()   → 新 generation 重新登记，probe_echo 恢复
再次用工具          → 工具 probe_echo [sidecar] · 成功
[selftest] persisted=.tmp/chat-selftest.jsonl replay_equal=true
[selftest] PASS
```

**意义**：M1 退出条件 1 与 2 现在同时成立——**同一份 Log 里两种来源的工具卡片**，
以及**杀 Sidecar 后原生工具仍可用**；且两条路径共用 `authorize`（Policy + schema）。

**下一代 generation 的细节**：`SidecarHost` 用进程内 `AtomicU64` 生成 `node-0001`…
并经 `runtime.hello` 传给 sidecar；如果 sidecar 自己生成，重启后可能撞号，
导致 `invalidate_generation` 误伤新注册——这是接入 Registry 时才暴露的问题。

## A9 流式消息（2026-11-09）

**目标**：补齐 M1 清单里的"单 Session 流式消息"——助手回复逐字出现，但日志里只有一条 settled 消息。

**实现**：

- `ChatState::begin_turn()`：用户消息 + 工具调用，返回待流式展示的回复文本，先落盘用户/工具事件。
- `ChatApp::send()`：把回复切成小块，用 `cx.spawn` + `cx.background_executor().timer()` 每轮追加一块
  并 `cx.notify()`，**不写日志**。
- 流式结束后调用 `ChatState::finish_turn()`：写一条 `message.assistant.settled` 并保存。
- 渲染：`streaming: Option<String>` 作为虚拟行追加在会话末尾（带 `▌` 光标），不参与回放。

**实测**：

```text
# 合成按键发送后，轮询会话文件
assistant_settled_after_ms=2532   # 3 字符/块，38 字回复
assistant_settled_after_ms=2031   # 同上，复测
```

**调参发现**：GPUI 的 `timer + notify + 重绘` 一轮实际约 **150 ms**（本机窗口在 32 Hz 远程显示适配器上）。
所以"每 12ms 推一个字符"实际是 2.5 秒的慢动作；改成每轮 **8 个字符**后降到约 0.8–1 秒。
结论：流式粒度应由**每次推送的字符数**控制，而不是 timer 精度。

**验证方式**：合成按键输入 → 轮询 `.tmp/chat-session.jsonl`，确认 `message.user` 先出现、
`message.assistant.settled` 在流式结束后才出现。`chat --selftest` 仍走同步 `submit()`，不受影响。

## A10 Agent 接口（2026-11-10）

**目标**：把 chat 里那段 if-else 的 mock 逻辑抽成真正的 Agent Loop：模型产意图、宿主执行、事件可测。

**`crates/agent-core`**（6 个单元测试）：

| 类型 | 作用 |
|---|---|
| `ModelProvider::complete(&CompletionRequest) -> ModelReply` | 模型接口；当前实现 `MockProvider`（关键词决定回文本还是调工具） |
| `ToolExecutor::{specs, execute}` | 工具接口；由应用实现（chat 里接 PluginHost + Sidecar） |
| `Agent::run_turn(user_text, executor)` | 编排：`TurnStarted → (StepStarted → TextDelta* → ToolCall → ToolResult)+ → TurnEnded` |
| `MAX_TOOL_CALLS = 2` | 防模型自激循环；超出产出 `TurnFailed` |

事件流设计要点：`Agent` **不碰 Session Log、不碰 UI**，只产出事件；
落盘（message.user / tool.call / tool.result / assistant.settled）由 chat 映射事件完成。

**接线后的行为**（`chat --selftest` 11 项全部 PASS，三张工具卡片都是"成功"）：
模型从 `tool_specs` 里挑工具（`probe_native` 之外的第一个 sidecar 工具），
执行器先 `authorize` 再按 `ToolOrigin` 分发到 PluginHost 或 Sidecar。

**发现并修掉的 bug**：最初每轮 `Agent::new(...)`，turn id 永远是 1，
于是 `call_id` 都是 `call-1-1`；Session Log 拒收第二条同 id 的结算，
表现为第一张工具卡片成功、后面两张永远"运行中"。改为把 `Agent` 存在 `ChatState` 里跨轮复用。
——这正是规范 §4 I3（一次调用至多一次结算）在真实数据流上的体现。

**仍未做**：`ModelProvider` 没有真实 LLM（无 API key）；流式 delta 目前由 `Agent` 按 16 字切分，
真实 provider 接入后应改由模型流驱动。

## A11 真实 LLM（2026-11-10）

**目标**：把 `MockProvider` 换成真模型，验证 Agent Loop 在真实模型下能跑通工具调用。

**实现**：`crates/llm-deepseek`

| 项 | 内容 |
|---|---|
| 传输 | `ureq` 3.5.1（同步 HTTP），`stream: false` |
| 凭据 | 环境变量 `DEEPSEEK_API_KEY`；缺失时读 `~/.dsh/.env`（只读，不打印、不写日志） |
| 模型 | `deepseek-chat`（可用 `DEEPSEEK_MODEL` / `DEEPSEEK_BASE_URL` 覆盖） |
| 工具 | 把 Registry 里的 `ToolSpec.parameters`（JSON Schema）原样传给模型，`tool_choice: auto` |
| 解析 | `parse_reply` 独立成函数，4 个无网络单测（纯文本 / tool_calls / API 错误 / 坏 JSON） |

**真实调用证据**：

```text
[chat] provider=deepseek model=deepseek-chat
[live] reply=我是一个桌面 Agent，可以调用工具帮你完成查询、处理文本等任务。
elapsed_ms=3275          # 含 sidecar 启动；单次 HTTP 约 3 秒

# 带工具的提示
$ chat --live "请用 probe_native 工具把 hello 转成大写"
[live] reply=HELLO
```

落盘事件（真实模型驱动）：

```text
1 message.user
2 tool.call   call-1-1 probe_native native
3 tool.result call-1-1
4 message.assistant.settled
```

即：**模型选择工具 → 宿主 authorize → PluginHost 执行 → 结果回填 → 模型给出最终答复**。

**确定性问题**：自检模式强制 `DSHR_FORCE_MOCK=1`，否则一旦机器上有 key，
`--selftest` 会真的调模型，断言随模型输出漂移。

**尚未做（M2 待办）**：

| 项 | 现状 |
|---|---|
| 流式 | 未做 SSE；UI 的逐字效果仍是本地切分 |
| 工具协议严格性 | 第二轮用 `role: user` 传工具结果，不是 OpenAI 的 `assistant.tool_calls` + `role: tool` + `tool_call_id` |
| 重试 / 限流 | 无；只有 60s 全局超时 |
| 用量计量 | 未记录 token（PLAN §6.4 的计量还没接） |

## A12 与 DSH 的协议对照（2026-11-10）

问题：我们的 sidecar 协议和 DSH 一致吗？

**答：不一致，而且不应一致。** 结论与逐字段对照见
[`spec/dsh-compat-mapping.md`](spec/dsh-compat-mapping.md)。要点：

- DSH 没有 sidecar：工具在同进程用 `ctx.tools.register(ToolDefinition)` 注册；
  Host↔浏览器走 `hostProtocolVersion: 4` 的 Remote RPC。我们的 NDJSON 是 ADR-004/005 的产物。
- 判断标准不是"字节一致"，而是"DSH 工具的定义与结果能否无损穿过我们的层"。目前**不能无损**：
  缺 `output.schema` / `presentCall` / `presentResult` / scope / `isConcurrencySafe` /
  `additionalContexts` / `concludesTurn`，终端与 diff 卡片会退化为文本。
- 已做的对齐（本轮）：`ToolDefinition` 增加 `output_schema` 与 `presentation`，
  `tool.register` 携带它们并在测试中断言（`l1_roundtrip`）。

## A8 真实 Cordis 插件（2026-11-09）

**目标**：验证 "compatibility, not captivity" 的第一步——插件写的是 **Cordis API**，
而不是本项目自定义的 probe 形状；装载它的是 Cordis 运行时（与 DSH 内嵌同一版本）。

**做法**：

| 项 | 内容 |
|---|---|
| 依赖 | `sidecar/cordis/package.json` 固定 `@deepseek-ai/cordis@4.0.5`（= DSH 0.2.1-rc.3 内嵌版本） |
| 插件 | `bridge-plugin.mjs`：`export const inject = ['bridge']`，`apply(ctx)` 内用 `ctx.effect(() => ctx.bridge.tool({...}))` 注册工具 |
| 侧车 | `sidecar-cordis.mjs`：`new Context()` → `ctx.provide('bridge', …)` → `ctx.plugin(plugin)`；协议与 probe 版完全相同 |
| 测试 | `crates/compat-dsh-host/tests/cordis_plugin.rs` |

**结果**：

```text
[cordis] ready=60.1ms generation=node-0001
[cordis] registrations=["cordis_echo"]
[cordis] invoke elapsed=561.2µs
test result: ok. 1 passed
```

**产品路径验证**（`chat --selftest-cordis`，`DSHR_SIDECAR=cordis`）：

```text
[selftest-cordis] items=3
  你    · 用工具试一下
  工具  · cordis_echo [sidecar] · 成功
  助手  · 工具返回 {"cordis":{"text":"用工具试一下"}}。...
[selftest-cordis] replay_equal=true
[selftest-cordis] PASS
```

**踩到的坑**：Cordis 插件装载最初放在进程启动时，`tool.register` 会在 `runtime.ready`
之前发出，携带的是握手前的占位 generation，被宿主按旧代丢弃。改为**收到 hello 并回 ready 之后
再 `ctx.plugin()`** 才正确——这再次印证规范 §5 的 generation 全链路约束。

**边界（必须说清）**：这里用的是 **Cordis 核心**（Context / Service / effect / inject），
不是 DSH 的 `ctx.tools` 服务与 Loader。真正的"装载未修改的 DSH 官方插件"需要 DSH 的整套
包（`@deepseek-ai/dsh-tools`、Loader、Schemastery 在 npm 上版本与 0.2.1-rc.3 不对齐），
仍是 M2 的任务。本次达成的是：**Cordis 语义可装载、注册可桥接、调用可回放**。

## 实验 B — Rust Cordis 选型（2026-11-09，已完成）

结论与证据见 [`decisions/0002-cordis-rs-selection.md`](decisions/0002-cordis-rs-selection.md)。
要点：两候选测试套件均全绿（dshbox 167 / jincdream 111），选 `jincdream/cordis-rs`。

## 实验 C — Sidecar 单位成本（2026-11-09，协议层完成）

**问题**：L1 的真实成本在协议本身，还是在别处？

**载体**：

- `sidecar/probe-sidecar.mjs` + `sidecar/probe-plugin.mjs`：NDJSON over stdio 的最小 Sidecar，
  插件以 `register(ctx)` 形式注册工具（模拟 DSH 插件形状，不依赖 Cordis）。
- `crates/compat-dsh-protocol`：信封与错误码（5 个单元测试）。
- `crates/compat-dsh-host`：spawn / 握手 / 注册 / invoke / cancel / kill。
- `crates/compat-dsh-host/tests/l1_roundtrip.rs`：真实 Node 进程的端到端测试（5 个）。

**测量结果**（本机 Windows，Node 24.14，50 次调用）：

| 阶段 | 测量 | 说明 |
|---|---|---|
| spawn → `runtime.ready` | **53.1 ms** | 含 Node 启动与握手 |
| 工具注册 | 3 个 | `probe_echo` / `probe_immediate` / `probe_slow` |
| **纯协议 invoke（无工具逻辑）** | **p50 148 µs，p95 917 µs** | 一次请求-响应往返 |
| 流式 invoke（3 chunk + 3×5ms sleep） | p50 47.1 ms | 时间几乎全在工具自己的 sleep |
| 取消 → `tool.aborted` | **111 ms** | 工具按 200 ms 粒度轮询取消标志 |
| 中途 kill | `Closed`（管道断开） | 在途调用不产生结果，符合规范 §10 |

**结论**：

1. **协议不是瓶颈**：纯往返不到 1 ms。L1 的 10–15 日估算应当理解为
   "schema 校验 + 流式背压 + generation 失效 + 重注册 + 附件传递 + 崩溃恢复"的成本，
   而不是 IPC 的速度成本。
2. **成本在纪律上**。探针第一次运行时 `invoke` 全部超时，原因是 Sidecar 忘了在
   `tool.chunk` / `tool.aborted` 的信封上带 `call_id`，Host 侧的关联过滤直接丢弃——
   这正是规范 §5「身份全链路携带」要防的那类 bug。
3. 另一个实现陷阱：async generator 的 `return` 值不会出现在 `for await` 里，
   首版插件把结果放在 `return` 中导致结算丢失；改为"最后 yield 一个终态对象"。

**M1 退出条件的覆盖度**：

| 条件 | 状态 |
|---|---|
| JS 工具经 IPC 注册到 Rust | **达成**（host 维护 registration 列表并回填 registration_id） |
| Rust 按名字调用 JS 工具 | **达成**（`probe_echo` / `probe_immediate` 端到端） |
| 取消传播 | **达成**（111 ms 内由工具侧观察到） |
| 崩溃隔离 | **达成**（kill 后调用报 Closed，不影响宿主进程） |
| 经 Rust Tool Registry（schema + policy）调度 | **未做**（当前是 host 直调，Registry 集成留到 M2） |

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
