# 性能基线（F，第二阶段，2026-11-11）

M2 gate 2 要求：10k 事件冷启动 < 1.5s，并有滚动/渲染的可重复基线。
这一轮把**数据层**和**滚动帧**都测出来了，并把 transcript 改成**变高行虚拟化**。

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
`FrameTiming` 记录 draw 各阶段耗时与呈现间隔 → 达到目标帧数后写报告并退出。
滚动由探针自己推进（`ScrollHandle::set_offset` 在 0..1 之间往返）。

```powershell
# 1. 造 10k 事件会话（只需一次）
.\target\release\chat.exe --seed-perf-session .tmp\perf-sessions

# 2. 自动化测量（脚本读 docs/m2-performance-baseline.json 判定回归）
tools\frame-probe.ps1 -Frames 240

# 或手动
$env:DSHR_SESSIONS_DIR = (Resolve-Path .tmp\perf-sessions).Path
$env:DSHR_FRAME_PROBE = "240"
$env:DSHR_FRAME_LOG = (Join-Path (Get-Location) ".tmp\frame-probe.txt")
.\target\release\chat.exe
```

### 测量结果（本机，2026-11-11）

release build，10k 事件（7500 行全部参与虚拟化），240 帧滚动，3 次重复：

| 指标 | 结果（3 次） |
|---|---|
| draw_work p50 | **1.9 / 2.2 / 2.0 ms** |
| draw_work p95 | **3.1 / 3.4 / 3.2 ms** |
| prepaint p50 | 1.0 / 1.1 / 1.0 ms |
| missed_refresh | **0 / 0 / 0**（budget 31.25 ms） |
| avg_fps | 24.0 / 23.9 / 24.1（受远端显示 ~24 Hz 上限约束） |

debug build 同条件对照：draw_work p50 14–30 ms、p95 67–74 ms、missed 20–48%，
说明**release 是产品口径基线**，debug 数字只用于相对回归。

基线落盘：`docs/m2-performance-baseline.json`（`frame-probe.ps1` 用它做
"avg_fps ≥ 基线 95%、draw p95 ≤ 基线 120%"的判定）。

## 内存基线（10k 会话，release，本机）

打开 10k 事件会话（7500 行全部参与虚拟化）并稳定 14s 后：

| 进程 | RSS | 说明 |
|---|---:|---|
| `chat.exe` | **79.8 MB**（private 105.3 MB） | GPUI 窗口 + 索引 + 虚拟化 transcript |
| `node.exe`（sidecar） | 36.7 MB | 探针/官方插件侧车 |
| conhost.exe | 17.6 MB | 控制台宿主（开发版） |

结论：10k 会话的内存占用在 100 MB 量级，与历史长度关系不大（虚拟化只保留可见行元素）。

## 未做 / 已知限制

- `avg_fps` 被本机远端显示刷新率限制（refresh_ns≈31.25ms），不能直接和 60/144 Hz 机器比较；
  判定应以 draw_work 是否超 budget、missed_refresh 是否为 0 为准。
- 元素树/text shaping 缓存的**细粒度**内存（单行/单元素）未测；上面是进程级基线。
- 虚拟化行高是估算，滚动条比例与"跳到底部"的像素位置有少量误差；
  真正的等高校验/锚点恢复留给后续。
