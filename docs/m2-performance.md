# 性能基线（F，第二阶段，2026-11-11；滚动帧基线 2026-11-12 修订）

M2 gate 2 要求：10k 事件冷启动 < 1.5s，并有滚动/渲染的可重复基线。
这一轮把**数据层**和**滚动帧**都测出来了，并把 transcript 改成**变高行虚拟化**。

2026-11-12 修订：滚动帧探针补齐环境元数据、判定改为"硬性条件 + 同 host 相对基线"；
旧基线（另一台机器 `DESKTOP-5J5JM4L`，含手工写入字段）在本机不可复现，已按当前环境
用 `-UpdateBaseline` 重新生成。

## 数据层数字（本机，debug build）

`chat --selftest-perf` 构造 10,000 条事件（2,500 轮 × user/assistant/tool.call/tool.result）：

```text
[perf] events=10000 jsonl_bytes=2619734 load_ms=119 build_ms=28
       snapshot_ms=30 history_ms=26 cold_ms=147
       items=7500 rows=7500 history_messages=60
```

| 项 | 结果 |
|---|---|
| JSONL 大小 | 2.62 MB |
| `SessionLog::from_jsonl`（含全部不变量重放） | 119 ms |
| `build_from_log`（对话模型） | 28 ms |
| `rows_snapshot`（UI 快照） | 30 ms |
| 冷启动合计（load + build） | **147 ms**，目标 < 1500 ms |
| `history_for_model`（上下文重建 + 60 条截断） | 26 ms |

结论：数据层不是瓶颈，10k 事件有约 10 倍余量。

release build 复测（2026-11-12，`chat --selftest-perf`，同一构造方式）：

```text
[perf] events=10000 jsonl_bytes=2619734 load_ms=16 build_ms=6
       snapshot_ms=7 history_ms=6 cold_ms=22
       items=7500 rows=7500 history_messages=60
```

release 下冷启动合计 **22 ms**，远低于 1500 ms 目标（debug 数字只用于相对回归）。

## UI：变高行虚拟化（本轮新增）

之前 transcript 每帧为可见 rows 全部构造元素；10k 会话下 400 行就要
prepaint 125 ms（debug），滚动只有 ~6 fps。现在：

- 只构造/排版视口附近的几十行（overscan 240px），用上下占位块维持滚动高度；
- 行高按内容估算（CJK 保守估计每行字符数），估算偏差只影响滚动条比例；
- 不再每帧克隆整份 `rows`（7500 个 String）；
- 滚动事件只触发一次重绘（`on_scroll` → `cx.notify()`），不重建历史；
- `DSHR_VISIBLE_LIMIT` 保留旧的"只看尾部 N 行"开关（默认不限制）。

## 滚动帧探针（真实帧时间，不是 sleep 估算）

`chat` 在 `DSHR_FRAME_PROBE=<帧数>` 下启用 GPUI profiler：
`set_trace_enabled(true)` → 每帧 `window.on_next_frame` 请求下一帧 →
`FrameTiming` 记录 draw 各阶段耗时与呈现间隔 → 达到目标帧数后把报告写到
`DSHR_FRAME_LOG`（同时 print 到 stdout）并退出。滚动由探针自己推进
（`ScrollHandle::set_offset` 在 0..1 之间往返）。

```powershell
# 0. 构建 release（若 target\release\dshr.exe 不存在）
tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin dshr

# 1. 造 10k 事件会话（只需一次；frame-probe 脚本也会自动做）
.\target\release\dshr.exe --seed-perf-session .tmp\perf-sessions

# 2. 自动化测量（采集环境元数据 + 按下面的口径判定）
tools\frame-probe.ps1 -Frames 240
```

### 判定口径（2026-11-12 修订：环境诚实、可判定）

`tools/frame-probe.ps1` 每次运行都会采集并输出完整元数据：`host`、`sessionName`
（`$env:SESSIONNAME`）、`gpu`（当前显示适配器 Name/DriverVersion + 全部适配器）、
`binary`（相对路径）、`binaryMtime`、`gitCommit`、`treeDirty`、`frames`、`rows`、
`avg_fps`、`draw_p50_us`、`draw_p95_us`、`missed_refresh`、`missed_ratio`、
`refresh_ns`、`budget_us`、`at`。`-UpdateBaseline` 会把这些字段一次性写进
`docs/m2-performance-baseline.json`，不再有手工维护的字段。

判定分两层：

1. **硬性条件（与基线无关，任何环境都必须满足）**
   - `missed_ratio == 0`（没有帧超过 refresh budget）；
   - `draw_work p95 <= refresh budget`，`budget_us = refresh_ns / 1000`（取自探针报告）；
     报告缺 `refresh_ns` 时回退 **31250 us**，即 32 fps 预算。
   任一不满足 → `FAIL`，exit 1。
2. **相对基线（只在同一 host 下有意义）**
   - 仅当 `baseline.host` 与当前 `host` 相同，或显式 `-CompareAcrossHosts` 时，
     检查 `avg_fps >= 基线 × 95%` 且 `draw_work p95 <= 基线 × 120%`；
   - host 不一致时打印 `WARN` 并**跳过**相对判定（数字仍完整记录）。不同机器的
     GPU/合成器/刷新率不同，跨主机比较会得出假回归；`-CompareAcrossHosts` 仅作参考。

`-UpdateBaseline` 只在硬性条件通过时写基线，不会用 FAIL 轮次覆盖基线。

### 当前环境测量（2026-11-12，RDP 会话）

| 项 | 值 |
|---|---|
| host | `OXYFLOUR-PC11` |
| 会话 | `RDP-Tcp#0`（远程桌面，`remoteSession=true`） |
| 当前显示适配器 | Microsoft Remote Display Adapter 10.0.26100.8972，2256×1504 |
| 机器 GPU | NVIDIA GeForce RTX 3090 Ti（驱动 32.0.15.9649） |
| 二进制 | `target/release/chat.exe`（构建于 2026-11-11T22:27:53） |
| git | `f4226dc`（测量时工作树含 M2 review 修复，`treeDirty=true`；渲染热路径未改，见下） |
| 探针刷新率 | `refresh_ns=31250000`（32 Hz），budget **31250 us** |

release build，10k 事件（7500 行全部参与虚拟化），240 帧滚动，2026-11-12 共重复
13 次（含基线写入、同机复验、跨主机对照）：

| 指标 | 结果（13 次） |
|---|---|
| `draw_work` p50 | **1.72–3.45 ms**（多数在 2.1 ms 内） |
| `draw_work` p95 | **2.52–9.14 ms** |
| `missed_refresh` | **0**（13 次全部 0；budget 31.25 ms，`missed_ratio=0`） |
| `avg_fps` | 23.6–24.0（受远端显示 32 Hz / 合成器上限约束，见下） |
| 硬性条件 | **13/13 PASS**：p95 最大 9.14 ms，仍低于 31.25 ms budget（3.4 倍余量） |

debug build 同条件对照（2026-11-11）：draw_work p50 14–30 ms、p95 67–74 ms、
missed 20–48%，说明**release 是产品口径基线**，debug 数字只用于相对回归。

关键输出（`tools\frame-probe.ps1 -Frames 240`，2026-11-12）：

```text
[frame-probe] meta host=OXYFLOUR-PC11 session=RDP-Tcp#0 remote=True gpu=Microsoft Remote Display Adapter (driver 10.0.26100.8972, 2256x1504) binary=target/release/chat.exe built=2026-11-11T22:27:53+08:00 commit=f4226dc tree_dirty=True frames=240 rows=7500 refresh_ns=31250000 budget_us=31250
[frame-probe] measured avg_fps=23.6 draw_p50_us=2321.3 draw_p95_us=9135.8 missed_refresh=0 missed_ratio=0
[frame-probe] HARD PASS: missed_ratio=0, draw_p95_us=9135.8 <= budget_us=31250
[frame-probe] baseline host=OXYFLOUR-PC11 session=RDP-Tcp#0 at=2026-11-12T08:24:21+08:00 commit=f4226dc avg_fps=23.7 draw_p95_us=8122.2
[frame-probe] baseline_fps=23.7 fps_floor=22.51 baseline_p95_us=8122.2 p95_ceil=9746.64
[frame-probe] PASS
```

- 基线写入轮（`-UpdateBaseline`）：`avg_fps=23.7 draw_p50_us=2052.6 draw_p95_us=8122.2`，
  硬性条件与相对基线均通过（exit 0）。
- 同机复验轮：`avg_fps=23.6 draw_p50_us=2321.3 draw_p95_us=9135.8`，
  `p95_ceil=9746.64`，PASS。
- 旧基线 `-CompareAcrossHosts` 对照：`draw_p95_us=3009.8` vs 旧 `p95_ceil=3814.68`、PASS
  （仅作跨主机参考，见下）。

**环境说明**：当前会话是 RDP（Microsoft Remote Display Adapter 为当前显示适配器），
draw/prepaint 会受远端合成器与后台负载影响，本机 p95 在 2.5–9.1 ms 之间抖动。
**该数字不代表本地控制台基线**；Gate 2 的滚动帧结论以**硬性条件**
（`missed_refresh=0` + `draw p95 <= refresh budget`）为准，相对基线判定只在同 host 下生效。

### 基线文件（当前环境）

`docs/m2-performance-baseline.json` 已由 `tools\frame-probe.ps1 -UpdateBaseline`
按当前环境重新生成（`at=2026-11-12T08:24:21+08:00`），带完整元数据：
host=`OXYFLOUR-PC11`、sessionName=`RDP-Tcp#0`、gpu、binary/binaryMtime、
gitCommit=`f4226dc`、frames=240、rows=7500、avg_fps=23.7、draw_p95_us=8122.2、
missed_ratio=0、refresh_ns=31250000、budget_us=31250。

该数字**只用于同 host 回归检测**，不能直接和 60/144 Hz 本地控制台机器比较。

## 旧基线为何不可复现（2026-11-11 基线，host=DESKTOP-5J5JM4L）

旧 `docs/m2-performance-baseline.json`（`at=2026-11-11T21:10:00`，host=`DESKTOP-5J5JM4L`，
avg_fps=24.0、draw_p95_us=3178.9）在本机（`OXYFLOUR-PC11`，RDP 会话）用
`tools\frame-probe.ps1 -Frames 240` 复测两次均为 FAIL：draw_work p95
**13505 / 12767.6 us**（旧基线 3178.9 us，阈值 3814.68 us）、p50 8222 / 7953 us、
avg_fps 23.2 / 23.5、**missed_refresh=0**。旧文档当时声称 3 次 p95 3.1 / 3.4 / 3.2 ms、
avg_fps 24.0 / 23.9 / 24.1，但那组数字来自另一台机器且无环境元数据。

两类原因：

1. **host 不同**：旧基线记录的是另一台机器（DESKTOP-5J5JM4L），旧脚本没有校验 host
   就直接做 120% 比较；本机是 RDP 会话，当前显示适配器是 Microsoft Remote Display
   Adapter。不同机器的 GPU/合成器/刷新率不同，draw 数字不可直接比。旧基线还有
   `binary`/`note` 是手工写入、无 commit/会话/GPU 记录，无法证明"同机同环境"。
2. **测量时后台负载/环境抖动**：两次 FAIL 的 p50≈8 ms 是本轮 p50≈1.72–3.45 ms 的
   约 3–4 倍，而**渲染热路径自基线 commit 未变**：
   - `git diff --name-only 4fe0d2a..HEAD -- crates/ui-gpui`：0 个文件；
   - `git diff 4fe0d2a..HEAD -- apps/desktop-gpui/src/bin/chat.rs` 的新增/删除行中，
     `FrameProbe`/`fn transcript`/`on_scroll`/`draw_work`/`refresh_ns`/`set_offset`/`virtual`
     关键词命中 **0** 次（改动集中在 CLI/数据根/spawn/导出/自检）；
   - 对 `fn transcript`（132 行，含虚拟化渲染与 `on_scroll`）和
     `FrameProbe`/`fn tick`/`fn report`（190 行）逐行比较 4fe0d2a 与 HEAD，均 **0 差异**。

   因此更可能是测量时后台负载（评审轮同时跑构建/自检）与远端合成器抖动，而非代码回归。

结论：旧基线的数字不能作为 Gate 2 的采信依据；已按当前环境重新生成带元数据的基线，
并修正判定口径（见上）。Gate 2 滚动帧仍需在**目标本地控制台**复测一次才能给出产品口径
结论。复测命令：

```powershell
tools\frame-probe.ps1 -Frames 240                       # 硬性条件 + 同 host 相对基线
tools\frame-probe.ps1 -Frames 240 -UpdateBaseline       # 在目标桌面重建基线
tools\frame-probe.ps1 -Frames 240 -CompareAcrossHosts   # 仅作参考的跨主机比较
```

## 内存基线（10k 会话，release，本机）

打开 10k 事件会话（7500 行全部参与虚拟化）并稳定 14s 后：

| 进程 | RSS | 说明 |
|---|---:|---|
| `chat.exe` | **79.8 MB**（private 105.3 MB） | GPUI 窗口 + 索引 + 虚拟化 transcript |
| `node.exe`（sidecar） | 36.7 MB | 探针/官方插件侧车 |
| conhost.exe | 17.6 MB | 控制台宿主（开发版） |

结论：10k 会话的内存占用在 100 MB 量级，与历史长度关系不大（虚拟化只保留可见行元素）。

## 未做 / 已知限制

- 当前测量环境是 RDP 会话（`RDP-Tcp#0`，当前显示适配器 Microsoft Remote Display
  Adapter），draw/prepaint 受远端合成器与后台负载影响，本机 draw p95 在 2.5–9.1 ms
  抖动；**该数字不代表本地控制台基线**，Gate 2 滚动帧需在目标桌面复测（命令见上）。
  判定口径：硬性条件 `missed_refresh=0` + `draw p95 <= refresh budget`（当前 31.25 ms）；
  相对基线只在同 host 下比较。
- `avg_fps` 被远端显示刷新率限制（`refresh_ns≈31.25ms`，即 32 Hz 上限；实测约
  23.6–24.0），不能直接和 60/144 Hz 机器比较；判定应以 draw_work 是否超 budget、
  missed_refresh 是否为 0 为准。
- 基线是单一 run 的样本（p95 受环境抖动影响），复测建议连跑 3 次看中位数；
  `-UpdateBaseline` 会同时写入 host/GPU/commit 等元数据，换机器后不要再手工改基线。
- 元素树/text shaping 缓存的**细粒度**内存（单行/单元素）未测；上面是进程级基线。
- 虚拟化行高是估算，滚动条比例与"跳到底部"的像素位置有少量误差；
  真正的等高校验/锚点恢复留给后续。
