# M2 实现独立 review（2026-11-11）

reviewer: review-m2（只读评审；本文件是唯一写入产物）。
范围：HEAD `1ac851b`（master，工作区干净）、`docs/PLAN.md` §8「M2 — Windows Alpha」四条 exit gate
与 M2 清单、M2 全部代码路径，重点是 `30a8b69`（Windows 启动面）与 `1ac851b`（ToolBudget /
DSH 默认值）两笔复核补丁的回归面。**未照抄 `docs/m2-status.md` 的结论**：下面的每个数字都是
本轮自己跑出来的，证据不足处明确标注。

## 结论（verdict）

**有条件接受（conditional accept）**。

- 四条 exit gate 里，**Gate 3、Gate 4 有可复现实证**；**Gate 1 证据弱**（真实网络 20/20 只有单次
  样本，统计上不足以支撑「≥99%」，且本轮无 key 无法复跑）；**Gate 2 冷启动有证据、滚动帧基线
  两次复跑均 FAIL**（docs 的 p95≈3ms 在本机不可复现，但基线文件 host 与当前主机名不一致）。
- 代码审计发现 **1 个数据丢失级缺陷**（损坏的会话日志被静默清空并覆盖）、**取消语义不完整**
  （停止在工具执行/步之间会丢失，sidecar 的 `tool.cancel` 从未接线）、**工具错误事件契约偏差**
  （普通工具错误被写成 `tool.aborted`）。这三点直接影响 M2 清单里「取消」「未完整结果修复 /
  重启恢复」「工具审批/审计」的 ✅ 口径。
- 因此「M2 已完成」可以接受为**有条件的完成**：先修 P1-1/P1-2/P1-3（或在 `m2-status.md`
  把对应 ✅ 降级为「已知缺陷/后移」并写清条件），再按完成对外；不建议按「完全完成、无已知
  缺陷」口径进入 M3。

> **2026-11-12 修复状态**：P1-1（会话日志原子写/损坏隔离/`.bak` 恢复/读失败禁写）、
> P1-2（Agent 取消事件 + provider token 不再清零 + 审批可取消 + sidecar `tool.cancel`）、
> P1-3（普通工具错误改回 `tool.result(is_error)`）、P1-4（frame-probe 元数据/判定加固与
> 环境标注）已修复并回归：`cargo test --workspace` 49 suites / 352 passed，
> 19 个 `chat` 自检全 PASS（新增 `--selftest-corrupt-session`、`--selftest-cancel`）。
> 详见 [`m2-status.md`](m2-status.md)「P1 修复轮」。剩余 P2 项按该节列表后移 M3/后续。
>
> P1-4 的两次 FAIL 已定位为**测量环境**（当前是 RDP `RDP-Tcp#0` + 评审时并行跑构建/自检的
> 后台负载），不是代码回归：frame-probe 现在记录 host/session/gpu/refresh budget 并按
> 「missed_refresh=0 + p95 ≤ budget」判定，当前环境 13 次复测全部通过（p95 2.52–9.14ms，
> budget 31.25ms）；跨 host 不再做相对回归。见 [`m2-performance.md`](m2-performance.md)。

## 本轮验证清单（命令 + 实测结果）

| # | 命令 | 结果 |
|---|---|---|
| 1 | `tools\build-env.cmd cargo test --workspace` | **49 suites / 329 passed / 0 failed / 0 ignored**，TEST_EXIT=0 |
| 2 | `tools\build-env.cmd cargo clippy --workspace --all-targets -- -D warnings` | exit 0，无 warning 输出（缓存命中，1.55s） |
| 3 | `tools\build-env.cmd cargo fmt --all -- --check` | exit 0，无 diff |
| 4 | `node schema/check.mjs` | 20/20 cases passed |
| 5 | `node tools/check-links.mjs` | 130 links, 0 broken, 43 files |
| 6 | 17 个 `target\debug\chat.exe --selftest*`（逐个跑，输出重定向到 `.tmp/rev-selftest-*.log`） | **17/17 exit 0**：selftest / stream / context / long-context / spill / approval / sandbox / profile / loader / install / retry / perf / dsh / dsh-write / cordis / tree / export |
| 7 | ToolBudget 单测（test log 抽取） | `tool_budget_is_enforced`、`default_budget_does_not_cap_tool_calls`、`many_tool_calls_in_one_turn_are_not_capped`、`over_budget_calls_in_one_step_are_marked_failed`、`max_steps_ends_turn_without_failure`、`zero_or_unset_env_means_unlimited` 全 ok |
| 8 | release PE 头 | `target\release\chat.exe` Subsystem=**2**（GUI）；debug=3（Console）——与 `m2-windows-launch.md` 一致 |
| 9 | `Start-Process release\chat.exe --selftest -RedirectStandardOutput` | exit 0，输出文件是**合法 UTF-8**，末行 `[selftest] PASS`（GUI 子系统下 CLI 输出可用） |
| 10 | release GUI 从 `C:\Windows\System32` 启动（`DSHR_HOME` 指向临时目录，重定向 stdout/stderr） | 进程存活；EnumWindows 只有 `Zed::Window|visible=True`、`Zed::PlatformWindow`、`MSCTFIME UI`、`IME`，**无 ConsoleWindowClass**；创建 `sessions/profile/workspace`；stderr 无 `os error 5`；Stop-Process 后无本进程派生的 vendored node 残留（Job Object 收尾符合预期） |
| 11 | 打包目录 `dist/dshr-dev` 复制到 `.tmp` 后从 System32 启动 `--selftest` | **FAIL，exit 1**：`[selftest] 保存会话失败: 系统找不到指定的路径 (os error 3)`，`replay_equal=false`（原因见 P2-2） |
| 12 | 同上，CWD=包目录再跑 | exit 0，`persisted=.tmp/chat-selftest.jsonl replay_equal=true` + PASS（资源根 exe 相对生效） |
| 13 | release `--retention-plan` / `--export-session s-nope` | retention exit 0 并打印计划；export 对不存在会话打印错误并 exit 1（CLI 入口在 GUI 子系统下可用） |
| 14 | 损坏会话日志复现（见 P1-1） | 对照组 5830B 日志原样保留；截断 120B 后 GUI 启动 4s，文件被改写成 **222B 的 session.created**，历史全丢，stderr 仅一行「无法加载历史会话（忽略并从空会话开始）」 |
| 15 | `tools\frame-probe.ps1 -Frames 240`（release，10k 事件 7500 行） | **两次均 exit 1（FAIL）**：draw_work p95 **13505 / 12767.6 us**（baseline 3178.9，阈值 3814.68）；draw p50 8222/7953us；avg_fps 23.2/23.5（floor 22.8 通过）；missed_refresh=0 |
| 16 | debug 帧探针 20 帧冒烟 | 收敛：draw p50 11711.9us / p95 19881.2us；240 帧 debug 首跑 180s 内未写报告（原因未确认，后续被中止） |

本轮未跑真实网络：`DEEPSEEK_API_KEY` / `OPENAI_API_KEY` 均为空，`--selftest-live-20` 只会打印
SKIP（exit 2），按任务要求不做网络调用。`--selftest-live-20` 的代码证据见下方 Gate 1。

## Exit gate 逐条核对

| Gate | 判定 | 证据 / 差距 |
|---|---|---|
| 1. 20 步 Turn 工具调用成功率 ≥99% | **证据弱** | 唯一 live 证据是 `docs/m2-stability.md:47-62` 的一次 `20/20`（本机无 key，无法复跑）；本轮可复现的只有本地故障注入 `--selftest-retry`：`turns=20 succeeded=20 failed=0 provider_calls=59`（`.tmp/rev-selftest--selftest-retry.log`）。统计问题：单次 20/20 的 95% 置信下界约 83%，**不足以证明 ≥99%**；且 `selftest_live_20` 打印的 `tool_errors` 只统计 `tool.result.is_error`（chat 从不写这种事件，见 P1-3），该列恒为 0，指标失真。 |
| 2. 10k 事件冷启动 <1.5s；滚动帧不低于基线 | **部分有证据（滚动帧不可复现）** | 冷启动可复现：`--selftest-perf` 本轮 `load_ms=139 build_ms=29 snapshot_ms=34 history_ms=28 cold_ms=168`（docs 147ms；同量级，远低于 1500ms）。滚动帧：两次 `frame-probe.ps1 -Frames 240` 均 FAIL，p95 是基线的 4 倍（见验证清单 #15）；`missed_refresh=0`，说明「不丢帧」仍成立，但 docs 的「draw p95≈3ms / 0 超预算」结论在本环境不可复现。基线文件 host=`DESKTOP-5J5JM4L` ≠ 本机 `OXYFLOUR-PC11`，且含 `binary`/`note` 两个 `frame-probe.ps1` 不会写出的字段（手工维护、出处不明）。`git diff 4fe0d2a..HEAD` 显示渲染/探针热路径未变，4× 差异更可能是环境（RDP/合成器/机器状态）而非代码回归，但这意味着 **Gate 2 的滚动帧证据不成立**。 |
| 3. 压缩后上下文 ≤ 窗口 80%，回放与 UI 一致 | **有证据（边界未覆盖）** | `--selftest-long-context` 复现：`round=7 compactions=8 ratio=0.611 tokens=4915/8000 dropped=0 rows_equal=true history_equal=true summary=true`；`--selftest-context`：`ratio=0.610 window=8000 dropped=0 compacted=true summary_chars=Some(1200)`。压缩区间在 `conversation-model` 与 `agent-context` 同规则跳过（`crates/conversation-model/src/lib.rs:67-89,159-162`、`crates/agent-context/src/lib.rs:432-596`）。边界：`trim_to_budget` 会把「最后一条本身超预算」的消息无条件留下（`agent-context/src/lib.rs:373-402`），若单条 user 消息 > 窗口 80%，ratio 可 >0.8 且会直接发给 provider，无二次保护（P2-9）。 |
| 4. 可安装/启用/禁用普通 DSH Tool 插件且不改源码 | **有证据（安装环节偏弱）** | `--selftest-dsh-write` 复现：`registered=["apply_patch","edit","load_workspace_dependencies","probe_native","probe_write","read","write"] write=true edit=true patch=true escape_rejected=true deny_kept=true snapshots=1`；`--selftest-profile`：`read_absent=true / read_present=true / persisted=true`（禁用 fs→read 消失，启用→回来）；`--selftest-install`：目录与 `.tgz` 解包、路径逃逸拒绝。弱项：安装只验证了合成包，**没有真正第三方 npm tarball 的端到端证据**；「官方插件源码未改」没有与 DSH 安装包做哈希比对（未验证）。 |

## Findings（P0 无；以下为 P1/P2）

### P1（应在 M2 内修，或把对应 ✅ 降级）

| # | 文件:行 | 问题与证据 | 建议 |
|---|---|---|---|
| P1-1 | `apps/desktop-gpui/src/bin/chat.rs:1066-1074,1129-1131,1186-1193`；`crates/session-store/src/session_index.rs:195-198`；spec `docs/spec/session-log.md:129` | **损坏会话日志被静默清空并覆盖 = 数据丢失**。加载失败时 `unwrap_or_else` 直接换成空 `SessionLog`，随后因 `log.is_empty()` 写入 `session.created` 并 `state.save()`，用全新 1 行文件覆盖原文件；`save()` 用 `std::fs::write` 全量重写（非临时文件+rename、无 fsync），崩溃/断电发生在写入中即可产生这种截断文件。实测：5830B 有效日志对照组原样保留；截断 120B 后 GUI 启动 4s，文件被改写成 222B，历史不可恢复，stderr 仅一行提示。spec 明确要求「违反 I1–I5 视为数据损坏：停止加载该会话并提示导出，不猜测修复」。`docs/m2-status.md:99` 把「未完整结果修复」标 ✅ 无实现支撑（全仓 grep 无 repair/incomplete 逻辑；`tool.aborted` 只覆盖运行时错误）。 | ① 写会话改为「临时文件 + rename」并保留 `.bak`；② 加载失败时**隔离**原文件（重命名 `.corrupt`）而不是覆盖，GUI 显式报错并提供导出；③ 恢复/导出/损坏自检加用例。 |
| P1-2 | `chat.rs:1731-1738`；`crates/agent-core/src/lib.rs:414-451`；`crates/llm-deepseek/src/lib.rs:251-254`；`crates/llm-openai/src/lib.rs:133-134,164-166`；`crates/compat-dsh-host/src/lib.rs:453-463`；`chat.rs:858-880` | **取消语义不完整**：① `stop()` 只置 `AtomicBool`，Agent 在工具循环/步之间从不检查它；② 每次 provider 请求开始都会把 cancel token **清零**（deepseek:252-253、openai:133-134），因此「工具执行期间按停止」会被下一次模型请求悄悄丢弃；③ sidecar 的 `tool.cancel` RPC 已实现但 **apps 里没有任何调用点**（`grep '\.cancel\('` 在 chat.rs 无命中），慢工具（默认 timeout 120s）期间停止无效；④ 审批等待最长 300s（chat.rs:865）也不响应 cancel，UI 停止后审批条仍在。 | Agent 每步前检查 cancel；工具执行可中断（sidecar 走 `cancel`，native 传取消标志）；审批 `request` 接受取消/超时清理；openai/deepseek 对 cancel 的返回值统一为「部分成功」语义。 |
| P1-3 | `chat.rs:1359-1384`；spec `docs/spec/session-log.md:72-77`；`crates/conversation-model/src/lib.rs:145-157` | **普通工具错误被写成 `tool.aborted`**：只要 `outcome.is_error` 就写 `tool.aborted`，`reason` 是 `outcome.content.to_string()`（JSON 数组字符串），违反 spec 的 reason 枚举 `crash/cancel/timeout/unload`；`tool.result(is_error:true)` 这条（UI「失败」卡片、session-store `Settled{is_error}`、审计 value/meta）在 chat 路径上永远走不到。后果：用户拒绝/工具报错/预算耗尽在 UI 上都显示「已中止 + 原始 JSON」，审计无法区分；`selftest_live_20` 的 `tool_errors` 指标恒 0（这也是 Gate 1 证据失真的根因之一）。 | 工具**执行失败**写 `tool.result(is_error:true, content, value)`；只有真正中止写 `tool.aborted` 且 reason 用枚举值；预算命中单独 reason（如 `budget`/`cancelled` 需先扩 spec）。 |
| P1-4 | `tools/frame-probe.ps1:102-121`；`docs/m2-performance-baseline.json:9`；`docs/m2-performance.md:59-75` | **Gate 2 滚动帧基线不可复现**：两次 240 帧 release 探针均 exit 1（p95 13.5ms/12.8ms vs 上限 3.81ms），而文档声称 p95≈3.2ms、0 超预算；基线 host 与当前主机名不符且字段手工添加，无法确认「同机同环境」。这是**证据不足**而非已证实的代码回归（热路径 diff 未变），但 Gate 2 的滚动帧结论不能按 docs 采信。 | 在非 RDP/目标桌面环境复测并把 host/二进制/commit 写进 baseline；或把 Gate 2 判定改为「missed_refresh=0 + 冷启动」并在文档说明环境限制。 |

### P2（M3 或文档）

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| P2-1 | `chat.rs:1186-1193` | 每次事件后全量重写会话 JSONL（10k 会话 2.6MB/次，一轮内多次），既是 P1-1 的触发条件，也是长会话 I/O 退化点 | 改为 append 追加 + 定期 truncate/compact，写入加 fsync/rename |
| P2-2 | `chat.rs:3445,5392,4571-4576,5613-5616` | `--selftest*`、`--selftest-live-20`、会话目录兜底仍用 **CWD 相对 `.tmp/...`**。实测打包 exe 从 `C:\Windows\System32` 跑 `--selftest` 直接 FAIL（`保存会话失败: os error 3`）；从包目录跑才 PASS。与「数据根与 CWD 解耦」的产品叙事不一致 | selftest 一律用 `DSHR_SESSIONS_DIR`/数据根下的临时子目录 |
| P2-3 | `agent-core/src/lib.rs:424-450`；`chat.rs:1362-1384,1435-1441` | ToolBudget 命中后只发 `tool.aborted` 工具事件 + 普通 `TurnEnded`/assistant settled，**无 budget 命中的显式事件或 UI 标记**；且这些 aborted 结果下一轮会被 `history_from_log` 还原成 `ERROR: 工具已中止（[{"text":"tool call budget exhausted..."}]）` 进模型上下文（原始 JSON） | 增加 `turn.degraded{reason:"tool_budget"}` 类事件/UI 提示；预算结果不要以原始 JSON 进历史 |
| P2-4 | `m2-dsh-defaults.md:42,181`；`crates/agent-context/src/lib.rs:16-17`；`m2-context.md:12`；`m2-closeout.md:72`；`m2-status.md:10` | 文档与代码/现状不一致：① 两处说「压缩仍是有损裁剪，没有 LLM 摘要」，但 `chat.rs:1221-1258,421-444` 已实现 LLM 摘要 + `context.compacted`；② `m2-context.md` 说窗口默认 64000，实际 `agent-context/src/lib.rs:46` 是 131072（1M 模型另算）；③ `m2-closeout.md` 列了不存在的 `--selftest-frame`；④ `m2-status.md` 说 117 链接，实测 130 | 统一以代码为准修文档 |
| P2-5 | `chat.rs:994-1001,1006-1011,1403-1409` | 子会话 `outcome` 只看 `reply.is_some()`，provider 失败（reply 为「处理失败：…」）仍记 `done`；父被停止时子日志同时写 `session.interrupted`（by_parent=false）与 `session.interrupted_by_parent`，与 spec「发起方写 interrupted、被传播方写 interrupted_by_parent」不符 | 用 reply 是否以「处理失败」开头或显式结局枚举判定；子会话只在 `interrupted_by_parent` 一个事件上落「被父中断」 |
| P2-6 | `crates/tool-runtime/src/approval.rs:112-143` | `resolve()` 无条件把决定写进 `decisions`，无 pending 时也不清理（泄漏）；若 call_id 跨轮重复可能误放行后续调用；停止生成时 pending 审批不清理，UI 最长继续显示 300s | resolve 只在 pending 存在且 call_id 匹配时接受；turn 结束/取消时清理 pending |
| P2-7 | `crates/sidecar-guard/src/process.rs:225-241` | 先 `command.spawn()` 再 `AssignProcessToJobObject`：子进程在这窗口内派生的后代不进 job，`KILL_ON_JOB_CLOSE` 的「杀干净」保证有竞态窗口（实测正常路径无残留 node，未观察到实际逃逸） | 用 `CREATE_SUSPENDED` spawn → assign job → resume |
| P2-8 | `chat.rs:163-216,1066-1074,1186-1193` | 数据根/`DSHR_HOME` 不可写、会话保存失败等只 `eprintln`（GUI 双击无控制台，用户看不到）；本沙盒实测 `%LOCALAPPDATA%\dshr` 对 chat 子进程返回 `os error 5`，程序静默回退到 exe 目录 | GUI 里显示「数据目录不可用」横幅/对话框；失败写入重试或提示 |
| P2-9 | `crates/agent-context/src/lib.rs:373-402` | Gate 3 边界：单条超预算消息（尤其 user prompt）永远保留，ratio 可 >0.8 甚至 >window，无 secondary guard | 对 user 消息做硬截断或拒绝并提示 |
| P2-10 | `crates/llm-openai/src/lib.rs:164-166` | OpenAI provider 收到 cancel 返回 `Err("cancelled")` → chat 落 `TurnFailed`、assistant 文本被写成「处理失败：cancelled」；DeepSeek 则返回部分文本。两个 provider 的「停止」语义不一致 | 统一 cancel 为部分成功（或显式 `session.interrupted` 而非失败） |
| P2-11 | `crates/tool-runtime/src/lib.rs:211-240` | 审批策略按**工具名**命中写/执行名单，插件只要换个名字（如 `fs_patch`）即可绕过 Ask 直接执行；`Presentation` 是唯一补充判据 | 策略应基于能力/来源（插件声明 capability）而不是名字（M3） |

## 与 PLAN / 文档的表述差异（需要收敛的「已完成」口径）

1. `docs/PLAN.md:665-691` 与 `m2-status.md` 对「取消」「未完整结果修复」「工具审批/审计」的
   ✅ 与实现不符：取消不覆盖工具执行与审批（P1-2）；损坏日志被清空覆盖而非「修复/提示导出」
   （P1-1）；工具错误事件用 `tool.aborted` 顶替 `tool.result(is_error)`（P1-3）。
2. `m2-status.md:99`「未完整结果修复 ✅ `tool.aborted` + 回放不变量」：`tool.aborted` 只覆盖
   运行时错误；没有任何「重启后修复未完整 tool.call」的代码或测试。
3. Gate 1 的 `20/20=100%` 与 Gate 2 的「p95≈3ms」属于**单次/环境相关证据**，本轮均无法独立
   复现到同一结论（Gate 1 无 key、Gate 2 两次 FAIL）；文档应标注环境与样本量，或把
   判定指标改为可复现口径。
4. 文档之间互相矛盾（LLM 摘要、默认窗口、117 vs 130 链接、不存在的 `--selftest-frame`），
   建议以代码为准做一次 docs 对账。

## M2 → M3 交接风险

- **P1-1 必须在 M3 之前修**：M3 插件平台会引入更多写会话的路径（多会话/树/导出），非原子写 +
  损坏即清空的组合会把数据丢失面放大。
- **P1-2 是并发工具的前置**：M3 要对齐 DSH `maxParallelToolCalls=10`，如果单工具取消还没接线，
  并发下「停止」会变得更不可控。
- **P1-3 会牵动 UI/replay/审计**：修 event 契约应放在 M3 早期，避免插件都按「aborted」语义写。
- Gate 1/Gate 2 需要 CI 化（真实 provider 有 key 的 nightly 样本；非 RDP 目标环境基线），否则
  M3 的性能/稳定性回归没有可信基线。
- ToolBudget 语义（默认不限、显式上限降级）本身已正确（单测覆盖），M3 加并发时要保持
  `TurnEnded` 降级路径与事件配对不变。

## 未验证 / 假设项（避免过度结论）

- `--selftest-live-20` 真实网络证据：无 key，本轮未复跑，不对其结果真伪下结论（只指出统计不足）。
- 普通桌面（非 RDP/DSH 沙盒）下的 `%LOCALAPPDATA%\dshr` 默认数据根、滚动帧基线、中文 IME、内存
  RSS（docs 79.8MB）未复测：本会话里 chat 子进程写 `%LOCALAPPDATA%` 被拒（`os error 5`，与
  `m2-windows-launch.md:167-174` 描述的受限父进程一致），只能验证回退路径。
- `dist/dshr-dev.zip` 的 `--selftest PASS` 只在可写 CWD 复现（P2-2）；包内 GUI 双击行为未逐项
  复测（只验证了 release 本体从 System32 启动）。
- 官方 DSH 插件「源码未修改」未做哈希比对（需要 asar 抽取，属只读外部目录）；Gate 4 依据
  `--selftest-dsh-write` 的注册/写盘/越界/拒绝结果。
- clippy 为缓存命中（exit 0、无 warning 输出），未强制 `touch` 重建；如需完全干净证据可
  `cargo clean -p ...` 后重跑。
- debug 240 帧探针首跑 180s 内未写报告（原因未确认）；debug 20 帧冒烟正常，release 240 帧正常退出。

## 复现命令（本轮用到的关键几条）

```powershell
tools\build-env.cmd cargo test --workspace                                  # 49 suites, 329 passed
tools\build-env.cmd cargo clippy --workspace --all-targets -- -D warnings  # exit 0
tools\build-env.cmd cargo fmt --all -- --check                             # exit 0
node schema/check.mjs; node tools/check-links.mjs                          # 20/20；130 links 0 broken
.\target\debug\chat.exe --selftest-long-context                            # ratio=0.611, compactions=8
.\target\debug\chat.exe --selftest-dsh-write                               # registered 7 tools, write/edit/patch ok
powershell -ExecutionPolicy Bypass -File tools\frame-probe.ps1 -Frames 240 # 本轮 FAIL（p95≈13ms）
# P1-1 复现：有效会话复制到临时 sessions 目录 → 原样保留；截断 120B → 启动 GUI 后文件变 222B
# P2-2 复现：dist 包复制到 .tmp 后 Start-Process -WorkingDirectory C:\Windows\System32 --selftest → FAIL(os error 3)
```

结论一句话：**M2 的骨架与四条 gate 的意图基本落地，但「已完成」应降为有条件接受**——先修
会话日志数据安全（P1-1）、取消语义（P1-2）与工具事件契约（P1-3），并在文档里如实标注
Gate 1/Gate 2 证据强度；其余 P2 可作为 M3 首批。
