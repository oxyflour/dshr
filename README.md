# Native Agent Desktop — workspace

Rust 是唯一权威运行时；GPUI 是原生渲染底座；DSH 通过 Node Sidecar 兼容；UI 扩展走 ADC-v1。

## 文档地图

| 文件 | 内容 |
|---|---|
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划：目标、架构决策、里程碑、估算 |
| [`docs/spec/`](docs/spec/README.md) | 设计规范：ADC-v1、CPP-v1、Sidecar RPC、Session Log |
| [`docs/decisions/`](docs/decisions/README.md) | ADR：版本 pin、Cordis 选型、不走 Web 壳 |
| [`docs/product-difference.md`](docs/product-difference.md) | 与上游 DSH 桌面端的差异（待核实假设） |
| [`docs/m0-runbook.md`](docs/m0-runbook.md) | M0 状态、环境事实、复验命令 |
| [`schema/`](schema/README.md) | 契约的 JSON Schema 与校验用例 |

## 目录结构

```text
apps/desktop-gpui        桌面壳入口（占位，UI 原型属 M1 实验 A）
crates/agent-core        Agent Loop / Turn / Step / 取消
crates/agent-context     计量、压缩、裁剪、外部化
crates/session-store     事件日志、回放、迁移
crates/tool-runtime      Tool 注册、执行、Policy
crates/plugin-api        CPP-v1 公开契约
crates/plugin-host       Cordis 适配层（唯一允许依赖 cordis-rs 的 crate）
crates/ui-contributions  ADC-v1 服务端（slot 注册表）
crates/ui-gpui           GPUI 渲染实现
crates/compat-dsh-*      Sidecar 协议与宿主
crates/compat-acp|mcp    外部 Agent / Tool 接入
schema/                  契约 Schema 与 fixture 校验
tools/                   构建环境脚本与 GPUI 编译探针
```

## 快速开始

```powershell
# 契约校验（只需 Node）
node schema/check.mjs
node tools/check-links.mjs

# 构建。Windows 上必须走 build-env：本机 %TEMP% 不能让链接器写入
tools\build-env.cmd cargo build --workspace

# 基本对话（输入框 + 会话流 + 真实 sidecar 工具 + 落盘恢复 + SSE 流式）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest          # 无窗口自检
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest-stream   # 流式自检
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --live "一句话"      # 真实 LLM（需 key）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --live-chat "第一句" "第二句"  # 多轮记忆验证

# 官方 DSH 插件（需要本机安装 DeepSeek Harness 桌面版）
node tools/dsh-vendor.mjs                     # 从本机 app.asar 抽取官方插件依赖（gitignored）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest-dsh
$env:DSHR_SIDECAR = 'dsh'                       # 用官方插件运行时（tools + 只读 fs + 官方 read 工具）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat
```

## 已固定的版本

| 组件 | 版本 | 记录 |
|---|---|---|
| Rust | `1.99.1` (stable) | [`ADR-0001`](docs/decisions/0001-gpui-pin.md) |
| GPUI | zed `d2a4fb6` | 同上 |
| GPUI Kit | `dbb0b9a`（0.9.0） | 同上 |
| Rust Cordis | `jincdream/cordis-rs` | [`ADR-0002`](docs/decisions/0002-cordis-rs-selection.md) |
| DSH | 本机安装副本（采样 `0.2.2-rc.1` / Cordis `4.1.0`） | [`ADR-0003`](docs/decisions/0003-dsh-pin.md)（含 2026-11-11 版本漂移复核；license 待补） |

## 当前状态

**M0：4 项完成、1 项部分、1 项转入 M1。** 工具链已就绪，`cargo build --workspace` 通过，契约校验 20/20、文档互链 0 broken。剩余项见 [`docs/m0-runbook.md`](docs/m0-runbook.md) §5。

**M4 进行中（跨平台 Beta）。** 范围、8 个工作包与四条 gate 口径见
[`docs/decisions/0013-m4-scope.md`](docs/decisions/0013-m4-scope.md)，证据与未验证项见
[`docs/m4-status.md`](docs/m4-status.md)。W1 第一部分已交付：`--uninstall-plugin`（目录/profile/
审计原子化）、信任密钥吊销/时间窗/轮换与 `trust-key-audit.jsonl`、`schema/cpp-v1/trust-keys.schema.json`
（`M4-UNINSTALL-001..008` / `M4-KEY-001..010`；`--selftest-plugin-lifecycle` PASS）。
三平台 CI、键盘全流程、性能进 CI 仍未完成，**不得**对外称 Beta 达标。

**M3 实现完成（插件平台 Preview）。** 范围与 exit gate 见
[`docs/decisions/0007-m3-scope.md`](docs/decisions/0007-m3-scope.md) 与收尾口径
[`docs/decisions/0012-m3-gate-closure.md`](docs/decisions/0012-m3-gate-closure.md)、计划基线见
[`docs/PLAN.md`](docs/PLAN.md) §8 M3；证据与偏离索引见
[`docs/m3-gate-evidence.md`](docs/m3-gate-evidence.md)，过程对账见
[`docs/m3-status.md`](docs/m3-status.md)。已交付：ADC-v1 冻结 + 服务端/渲染器/chat 面板；
CPP-v1（manifest/Fiber/Service/Effect/external-process 运行时 + SDK 0.1.0 + 作者模板/打包/信任链）；
WASM 主机（wasmtime，core-module 四类隔离 + 接入宿主）；DSH L2（协议 v2 + 三个服务 +
`sessionProjections` + 官方包 manual gate）；M3 默认差异与工具并行（sidecar ≤10 批并发）；
conformance 544 ID / `--full` 在 Lead 环境 exit 0（67 suites/837 passed/0 failed/6 ignored）。
独立评审见 [`review-m3-implementation.md`](docs/review-m3-implementation.md) 与
[`review-m3-implementation-2.md`](docs/review-m3-implementation-2.md)：P0/P1 全部修复，二次复审
**接受**（按 [ADR-0012](docs/decisions/0012-m3-gate-closure.md) 修正后的口径）；未确认的 P2/后续项
列入 M4。

**M1 完成（核心闭环已打通）。** 已完成：A1 虚拟化、A1b 启动拆解、A1c 帧预算、B Cordis 选型、A3 WebView2 引擎自检、A5 端到端回放、A5b 真实调用闭环、C Sidecar 协议往返（纯协议 p50 148 µs）、A6 基本对话、A7 Native Plugin、A7b 统一 Registry、A8 真实 Cordis 插件、A9 流式消息、**A10 Agent 接口**、**A11 真实 LLM**（`DeepSeekProvider`，从 `~/.dsh/.env` 读 key）、A12/A13 协议对齐与结果卡片、A14 真 SSE 流式（首字 ~0.7–0.85 s）、**A15 多轮上下文 + 严格工具协议**（`role: tool` + `tool_call_id`，真实多轮记忆与多工具已验证）、**A16 真实输入组件**（`ui-gpui::TextInput`，实现 `EntityInputHandler`，中文 IME 可组合输入）。共 98 个测试 + 四个 `chat --selftest*` + `conversation --check` + `chat --live/--live-chat`。A2 IME 有已知缺陷（[issues/0001](docs/issues/0001-ime-candidate-first-composition.md)，产品界面待人工复验）。未做：中文 IME 的人工复验、并行工具调用、多行输入。详见 [`docs/m1-log.md`](docs/m1-log.md)。

**M2 收尾：四条 exit gate 已达标（2026-11-11）。** 官方 DSH 插件不改一行源码装载：
`@deepseek-ai/dsh-tool-workspace-dependencies` 与 `@deepseek-ai/dsh-tool-fs`（`read`）
经 Cordis 4.1.0 注册进 Rust Registry；能力协商 `fs.write` 后，官方
`write`/`edit`/`apply_patch` 的写盘请求回到 Rust → 沙箱边界 → 审批 → 写前快照 →
原子写（`--selftest-dsh-write`；[`docs/m2-official-write.md`](docs/m2-official-write.md)）。
依赖由 `tools/dsh-vendor.mjs` 从本机安装的 `app.asar` 抽取（不提交仓库），
Node 运行时可由 `tools/vendor-node.mjs` 捆绑到 `runtime/node`，sidecar 以
Windows Job Object 包裹（进程/内存上限、随宿主退出、环境变量清洗）。

**界面：按 DSH Desktop 的设计 token 对齐。** 侧栏 280、内容列 748、输入卡片圆角 28 /
max-width 780、链接色/气泡/边框/字体全部取自 DSH 安装包解析出的 `--dsw-*` 值
（[`crates/ui-gpui/src/theme.rs`](crates/ui-gpui/src/theme.rs)），默认浅色并可切换深色。
助手正文支持 Markdown 子集；工具卡有 Diff / Terminal 原生视图
（[`docs/m2-ui-views.md`](docs/m2-ui-views.md)）。侧栏是真实会话列表：新建、切换、
行内重命名、二次确认删除，重启恢复（[`docs/m2-sessions.md`](docs/m2-sessions.md)）；
**会话树**按父子缩进展示（子会话带 `↳` 标记），模型可经 `spawn_child` 派发子会话，
中断会向下传播（[`docs/m2-session-tree.md`](docs/m2-session-tree.md)）。
transcript 采用**变高行虚拟化**：只排版视口附近几十行，10k 会话（7500 行）滚动
draw p95 ≈ 3 ms、0 次超预算；进程 RSS ≈ 80 MB + sidecar 37 MB
（release；[`docs/m2-performance.md`](docs/m2-performance.md)）。

**上下文工程。** 请求前计量：超长工具结果头尾裁剪、超预算丢最旧整轮、模型生成摘要并写
`context.compacted`；超过 16 KiB 的工具结果**外部化**为内容寻址对象，模型只见引用与预览
（`spill.created` / `tool.result.pruned`，[`docs/m2-spill.md`](docs/m2-spill.md)）。
`--selftest-long-context` 连续 8 次叠加压缩：ratio 0.611 ≤ 0.8，回放与 UI 逐行一致。

**审批与沙箱。** 写文件/执行命令类工具默认需要用户审批（审批卡片在输入框上方，
痕迹落 `approval.asked` / `approval.answered`）；沙箱提供工作区相对路径根、1 MiB 限制、
写前快照、原子写（[`docs/m2-approval.md`](docs/m2-approval.md)、
[`docs/m2-sandbox.md`](docs/m2-sandbox.md)）。官方 `bash` 仍关闭（需要 Node
`subprocess` 服务与更完整的执行隔离）。

**插件与稳定性。** profile（`plugins.json`）可持久化启停，侧栏插件面板切换后重启 sidecar，
装载失败的错误会显示在面板顶部；支持 `cordis.yml` + `cordis.patch.yml` 只读派生
（[`docs/m2-loader.md`](docs/m2-loader.md)）；`chat --install-plugin <目录|.tgz>` 离线安装
并写安装审计日志（[`docs/m2-plugins.md`](docs/m2-plugins.md)）。
provider 已有 DeepSeek 与 OpenAI 兼容两家，统一重试/退避；真实网络 20 步工具调用
成功率 20/20（[`docs/m2-stability.md`](docs/m2-stability.md)）。
会话可导出（JSONL + 外部化附件 + manifest sha256 自校验）并查看保留计划：
`chat --export-session <id>` / `chat --retention-plan`；panic 会写入
`DSHR_CRASH_LOG`（默认 `.tmp/crash.log`）——[`docs/m2-sessions.md`](docs/m2-sessions.md)、
[`docs/m2-spill.md`](docs/m2-spill.md)。
M2 四条 gate 的对账与 17 个自检命令见 [`docs/m2-status.md`](docs/m2-status.md)；
MCP/ACP 与签名/更新通道按 `PLAN.md` 后移 M3/M4。
差异清单见 [`docs/ui-dsh-alignment.md`](docs/ui-dsh-alignment.md)。
