# M2 状态对账（2026-11-11，收尾轮）

对照 `PLAN.md` §8 的 M2 四条 exit gate 与清单项，逐条列证据与缺口。
**结论**：四条 gate 均有**可复现命令 + 数字**；M2 清单项（除 `PLAN.md` §8 明确允许后移的
MCP/ACP，以及 M3/M4 的签名与更新通道）已全部落地。**会话树产品面**（spawn 执行 +
树形侧栏 + 中断传播）、**导出/保留 CLI**、**崩溃诊断**、**插件错误提示**、**内存基线**
均已在本轮接线并自检。

验证状态（本轮）：**17 个 `chat` 自检全部 PASS**，`cargo test --workspace` 全绿，
`clippy -D warnings` 干净，`fmt --check` 干净，docs 117 链接 0 broken，sidecar schema 20/20。

**复核补丁（2026-11-11 晚）**：真机双击复测发现 Windows 启动面与 DSH 默认配置两处遗留，
已修复并补文档：[`m2-windows-launch.md`](m2-windows-launch.md)、
[`m2-dsh-defaults.md`](m2-dsh-defaults.md)。详见本文末尾「复核补丁」一节。

## Gate 1：20 步 Turn 工具调用成功率 ≥99%

**证据**

```text
# 真实网络（DeepSeek deepseek-chat，SSE 流式）
[live-20] provider=auto steps=20 ok=20 failed=0 rate=1.0000
         tool_calls=20 tool_results=20 tool_errors=0 p50_ms=2697 p95_ms=4847
[selftest-live-20] PASS

# 本地故障注入（每第 3 次请求 503，max_attempts=4）
[retry] turns=20 succeeded=20 failed=0 tool_executions=20 provider_calls=59
```

- 命令：`chat --selftest-live-20`（需 `DEEPSEEK_API_KEY`；无 key 返回 SKIP）、
  `chat --selftest-retry`；
- 重试/退避与流式安全见 [`m2-stability.md`](m2-stability.md)；
- sidecar 瞬时错误分类与重试建议见 [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §9.2。

## Gate 2：10k 事件冷启动 < 1.5s，滚动帧率有基线

**证据**

```text
[perf] events=10000 jsonl_bytes=2619734 load_ms=119 build_ms=28 cold_ms=147

# 10k 会话（7500 行全部参与虚拟化）滚动 240 帧，release build
[frame-probe] draws=240 avg_fps=24.0 rows=7500
[frame-probe] draw_work p50_us=2037 p95_us=3067
[frame-probe] missed_refresh=0 missed_ratio=0.0000
```

- 冷启动 **147 ms**（目标 <1500 ms）；
- 滚动帧：draw p50 **~2 ms**、p95 **~3 ms**，240 帧 **0 次超预算**（budget 31.25 ms）；
  `avg_fps=24` 是本机远端显示刷新上限，判定用 draw_work 与 missed_refresh；
- 实现：变高行虚拟化（只排版视口附近几十行 + 占位块），不再每帧克隆整份 rows；
- 基线文件 `docs/m2-performance-baseline.json`，`tools/frame-probe.ps1` 自动判定回归；
- 详情 [`m2-performance.md`](m2-performance.md)。

## Gate 3：压缩后上下文 ≤ 窗口 80%，回放与 UI 一致

**证据**

```text
# 单次压缩 + 长会话 8 次叠加压缩（mock provider，窗口 8000）
[long-context] round=7 compactions=8 ratio=0.611 tokens=4915/8000 dropped=0
               rows_equal=true history_equal=true summary=true
[selftest-long-context] PASS
```

- 每轮压缩后 ratio **0.611 ≤ 0.8**、`dropped_messages=0`、配对合法；
- JSONL 重放后的 UI 行与模型历史与在线状态**逐行/逐条一致**；
- 叠加压缩 8 次仍收敛，UI 显示"最近一次压缩"标记（更早区间被后续压缩覆盖，属预期）；
- 外部化（§6.4 第五项）：`chat --selftest-spill` 验证超阈值工具结果落盘为内容寻址对象、
  模型只见"引用 + 预览"、可无损取回、重复计划不重复外部化；
- 详情 [`m2-context.md`](m2-context.md)、[`m2-spill.md`](m2-spill.md)。

## Gate 4：安装/启用/禁用普通 DSH Tool 插件且不改源码

**证据**

```text
[dsh-write] registered=["apply_patch","edit","load_workspace_dependencies",
            "probe_native","probe_write","read","write"]
            write=true edit=true patch=true escape_rejected=true deny_kept=true snapshots=1
[selftest-dsh-write] PASS
```

- 官方 `dsh-tool-fs` 的 `read` 在只读模式装载（`--selftest-dsh`）；
- 能力协商 `fs.write` 后，官方 `write`/`edit`/`apply_patch` **不改一行源码**可用：
  写盘请求回到 Rust → `dshr-sandbox` 边界校验 → 审批 → 写前快照 → 原子写；
- 越界（`../`）被拒绝且不落盘；审批 Deny 时工具报错且文件不变；
- 安装：`chat --install-plugin <目录|.tgz>`（离线解包，校验 package.json，拒绝路径逃逸）；
- 启停：`--selftest-profile` 端到端（禁用 fs → `read` 消失，启用 → 回来）；
- Loader YAML bundle + patch 派生只读插件列表（`--selftest-loader`）；
- 详情 [`m2-official-write.md`](m2-official-write.md)、[`m2-plugins.md`](m2-plugins.md)。

## M2 清单对账

| 清单项 | 状态 | 证据/说明 |
|---|---|---|
| Agent Loop（≥2 provider / 流式 / 重试 / 取消） | ✅ | `--selftest-live-20`、`--selftest-retry` |
| Session Store（持久化/索引/重启恢复） | ✅ | 多会话索引、回放自检；新会话首条写 `session.created` |
| 未完整结果修复 | ✅ | `tool.aborted` + 回放不变量 |
| 保留与导出 | ✅ | `chat --export-session <id> [--export-dir]`、`--retention-plan`（只出计划不删除）、`--selftest-export`（含外部化附件 + manifest sha256 自校验） |
| 上下文：计量/压缩/裁剪/外部化/一致性 | ✅ | `--selftest-context` / `-long-context` / `-spill` |
| 会话树（spawn/中断传播/drain/树形 Dock） | ✅ | `--selftest-tree`：真实链路派发 + 父子事件 + 单层限制 + 中断传播 + `from_logs` 重建；侧栏按树缩进（截图核对） |
| GPUI：多会话/输入/Markdown/代码块/工具卡 | ✅ | 截图与自检；transcript 变高行虚拟化 |
| GPUI：Diff / Terminal | ✅ 组件 + 工具卡接线 | `docs/m2-ui-views.md` |
| 工具审批/拒绝/审计 | ✅ | `--selftest-approval`、`--selftest-dsh-write` |
| DSH L1（注册/调用/取消/流式/卸载/重启） | ✅ | `--selftest-dsh`、`--selftest-cordis` |
| Node 运行时分发（捆绑/离线/安装确认） | ✅ | `runtime/node` vendor + 解析链 + Job Object；`--install-plugin` 显式确认并写安装审计日志 |
| Sidecar OS 级隔离（Job Object/独立 profile/env 清洗） | ✅ | `crates/sidecar-guard`；chat 启动已接 |
| 崩溃诊断 / 插件错误提示 | ✅ | panic 写 `DSHR_CRASH_LOG`（默认 `.tmp/crash.log`）；sidecar `log.emit(error)` 显示在插件面板顶部 |
| 内存基线 | ✅ | 10k 会话：chat RSS 79.8 MB（private 105.3）+ sidecar 36.7 MB（见 `m2-performance.md`） |
| MCP / ACP Adapter | ⏭ 后移 M3 | `PLAN.md` §8 转向允许 |
| Windows 打包（dev） | ✅ dev zip；签名/更新通道 M3/M4 | `tools/package-dev.ps1`、`docs/m2-runtime.md` |

## 复核补丁：Windows 启动面与 DSH 默认配置（2026-11-11 晚）

用户真机双击 `target\release\chat.exe` 后报告两个问题；本轮均已定位到可复现根因并修复。

### 1. Windows 权限被拦 + 命令行窗口

**现象**：双击/快捷方式启动时出现命令行窗口；某些启动方式下进程直接退出，像是"被 Windows
权限拦住"。

**根因（全部实测）**：

| 症状 | 根因 | 证据 |
|---|---|---|
| 双击弹控制台 | release 二进制 PE `Subsystem=3 (Console)` | 读 PE 头；修复后 `Subsystem=2` |
| "权限拦住"、一闪而过 | 会话/状态目录是 CWD 相对 `.tmp/sessions`；快捷方式"起始位置"为空时 CWD=`C:\Windows\System32`，`create_dir_all` 报 `os error 5` 后 `exit(1)` | `Start-Process target\release\chat.exe -WorkingDirectory C:\Windows\System32` → `[chat] 无法创建会话目录: 拒绝访问。(os error 5)` |
| 打包目录换机器 sidecar 起不来 | `spawn_sidecar()` 用编译期 `CARGO_MANIFEST_DIR` 拼仓库根 | 旧代码；dist 拷走后仍指向 `C:\Projects\dshr` |
| 慢工具约 5s 必失败 | sidecar 调用写死 `Duration::from_secs(5)` | 旧 `ChatExecutor`；`ToolDefinition.timeout_ms` 默认 120s |

**修复**：release 改为 GUI 子系统，CLI 用法（`--selftest*` 等）用 `AttachConsole` 接回父控制台并
设 UTF-8 代码页；数据根与 CWD 解耦（`DSHR_HOME` → 包内可写目录 → `%LOCALAPPDATA%\dshr`）；
资源根改为 exe 相对（`DSHR_APP_ROOT` 可覆盖）；sidecar 工具超时改用工具声明
（`DSHR_TOOL_TIMEOUT_MS` 可覆盖，clamp 1–600s）。详见
[`m2-windows-launch.md`](m2-windows-launch.md)。

### 2. 默认配置与官方 DSH 的偏差（工具调用只有 8 次）

**现象**：在 `chat.exe` 输入"从 1 数到 10，间隔 2 s"，模型一步发起 9 个 `probe_slow` 调用，
第 9 个触发 `tool call budget exhausted (8)`，整轮返回"12345678处理失败：…"。

**根因**：`crates/agent-core` 的 `pub const MAX_TOOL_CALLS: usize = 8`，超出即
`AgentEvent::TurnFailed`。官方 DSH `0.2.2-rc.1` 的 `@deepseek-ai/dsh-agent-loop` **没有每轮
工具调用上限**（只有 `maxParallelToolCalls=10`）。

**修复**：新增 `ToolBudget{max_tool_calls,max_steps}`（默认 `None`=不限制，env
`DSHR_MAX_TOOL_CALLS`/`DSHR_MAX_STEPS` 可收紧）；命中上限改为**降级**：未执行的调用仍发
`ToolCall`+`is_error` 的 `ToolResult`，随后 `TurnEnded`，不再整轮失败。同时对齐
retry（6 次/500ms/10s/±10% jitter）、temperature（默认不发送）、context window（按模型分档：
flash/v4-pro → 1M、deepseek-chat → 128K、OpenAI → 128K、其余 64K）。完整差异表与证据索引见
[`m2-dsh-defaults.md`](m2-dsh-defaults.md)。

### 复核验收命令

```powershell
# 构建 release 与 debug
tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin chat
tools\build-env.cmd cargo build -p dshr-desktop-gpui --bin chat

# 1) PE 子系统应为 2（GUI）；双击无控制台
#    用 EnumWindows 枚举该 PID 顶层窗口：只有 Zed::Window，无 ConsoleWindowClass
# 2) release CLI 仍能打印（AttachConsole）
.\target\release\chat.exe --selftest        # 期望 [selftest] PASS
# 3) 不可写 CWD 不再退出（无 DSHR_* 时数据落 %LOCALAPPDATA%\dshr）
Start-Process .\target\release\chat.exe -WorkingDirectory C:\Windows\System32 `
  -RedirectStandardOutput .tmp\launch-out.txt -RedirectStandardError .tmp\launch-err.txt
# 4) 工具预算：默认下 20 次循环调用 / 一步 10 个调用全部执行（agent-core 单测）
tools\build-env.cmd cargo test -p dshr-agent-core -p dshr-agent-context -p dshr-llm-retry
```

**本轮实测结果（2026-11-11 晚，复核补丁后）**：

```text
cargo test --workspace                      -> 49 suites, 329 passed, 0 failed
cargo clippy --workspace --all-targets -D warnings -> clean
cargo fmt --all -- --check                  -> clean
node schema/check.mjs                       -> 20/20 cases passed
node tools/check-links.mjs                  -> 130 links, 0 broken
chat --selftest（release，GUI 子系统）       -> [selftest] PASS, exit 0
17 个 chat 自检（debug）                     -> 17/17 PASS, exit 0
release GUI 启动（DSHR_HOME 临时）           -> 顶层窗口只有 Zed::Window / PlatformWindow；无 ConsoleWindowClass
release 从 C:\Windows\System32 启动           -> 进程存活、有窗口；数据落 %LOCALAPPDATA%\dshr\{sessions,profile,workspace}
dist/dshr-dev.zip                            -> 42.3 MiB；chat.exe Subsystem=2；含 RUNNING.txt；包内 --selftest PASS
```

## 剩余缺口（M3 / 需人工）

1. **Loader 完整语义**：`include` 展开、group 展开、`!!js` 求值（当前只做 bundle+patch 派生）；
2. **安装依赖树/生命周期脚本**：不联网解析依赖、不执行 npm install scripts；安装确认目前是
   CLI 显式调用 + 审计日志，没有交互式 UI 确认；
3. **打包分发**：签名、更新通道、干净机器验证；
4. **子会话异步/并行调度**与 sidecar 工具继承（M3）；
5. **虚拟化锚点/等高校验**、元素级内存细分（优化项，非 gate）；
6. **中文 IME 人工复验**：[`issues/0001`](issues/0001-ime-candidate-first-composition.md)。
7. **工具并行执行**：dshr 仍串行；DSH 每步 `maxParallelToolCalls=10`（M3）。
8. **默认差异（不阻塞 M2）**：reasoning effort 档位、LLM 摘要压缩、重复工具调用软提醒
   （DSH `thresholds [3,5,8]`）、`EMPTY_RESPONSE` 重试码、approval 的
   `read-only`/`danger-full-access` 模式切换；见 [`m2-dsh-defaults.md`](m2-dsh-defaults.md)。
9. **Windows 分发**：代码签名/安装器（消除 SmartScreen/Defender 提示）、
   sidecar 协议透传插件声明的 `timeoutMs`、受限 token/AppContainer（M3/M4）；
   sidecar 注册协议当前不带 timeout，Rust 侧用 `ToolDefinition.timeout_ms` 默认 120s。

## 复现命令（全量）

```powershell
tools\build-env.cmd cargo test --workspace
tools\build-env.cmd cargo clippy --workspace --all-targets -- -D warnings
tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin chat
tools\build-env.cmd cargo build -p dshr-desktop-gpui --bin chat

# 启动面（复核补丁）：PE 子系统=2、release --selftest 仍有输出、不可写 CWD 可启动
# 详见「复核验收命令」一节
$env:DSHR_SESSIONS_DIR = ".tmp\perf-sessions"
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --seed-perf-session .tmp\perf-sessions
powershell -ExecutionPolicy Bypass -File tools\frame-probe.ps1

# 功能自检（17 个）
foreach ($f in "--selftest","--selftest-stream","--selftest-context","--selftest-long-context",
  "--selftest-spill","--selftest-approval","--selftest-sandbox","--selftest-profile",
  "--selftest-loader","--selftest-install","--selftest-retry","--selftest-perf",
  "--selftest-dsh","--selftest-dsh-write","--selftest-cordis","--selftest-tree",
  "--selftest-export") {
  .\target\debug\chat.exe $f
}

# 导出 / 保留计划（不需要窗口）
.\target\debug\chat.exe --export-session <会话 id> --export-dir .tmp\exports
.\target\debug\chat.exe --retention-plan
```
