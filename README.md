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

**M1 进行中（核心闭环已打通）。** 已完成：A1 虚拟化、A1b 启动拆解、A1c 帧预算、B Cordis 选型、A3 WebView2 引擎自检、A5 端到端回放、A5b 真实调用闭环、C Sidecar 协议往返（纯协议 p50 148 µs）、A6 基本对话、A7 Native Plugin、A7b 统一 Registry、A8 真实 Cordis 插件、A9 流式消息、**A10 Agent 接口**、**A11 真实 LLM**（`DeepSeekProvider`，从 `~/.dsh/.env` 读 key）、A12/A13 协议对齐与结果卡片、A14 真 SSE 流式（首字 ~0.7–0.85 s）、**A15 多轮上下文 + 严格工具协议**（`role: tool` + `tool_call_id`，真实多轮记忆与多工具已验证）、**A16 真实输入组件**（`ui-gpui::TextInput`，实现 `EntityInputHandler`，中文 IME 可组合输入）。共 98 个测试 + 四个 `chat --selftest*` + `conversation --check` + `chat --live/--live-chat`。A2 IME 有已知缺陷（[issues/0001](docs/issues/0001-ime-candidate-first-composition.md)，产品界面待人工复验）。未做：中文 IME 的人工复验、并行工具调用、多行输入。详见 [`docs/m1-log.md`](docs/m1-log.md)。

**M2 首个里程碑：官方 DSH 插件可装载（2026-11-11）。** 未修改的官方包
`@deepseek-ai/dsh-tool-workspace-dependencies` 与只读 `@deepseek-ai/dsh-tool-fs`
经 Cordis 4.1.0 装载、注册进 Rust Registry，真实 DeepSeek 经官方 `read` 工具读文件成功。
依赖由 `tools/dsh-vendor.mjs` 从本机安装的 `app.asar` 抽取（不提交仓库）。
边界（写入/沙箱/会话等 service 未提供、Loader 未接）与复现步骤见
[`docs/m2-dsh-official.md`](docs/m2-dsh-official.md)。

**界面：按 DSH Desktop 的设计 token 对齐（2026-11-11）。** 侧栏 280、内容列 748、
输入卡片圆角 28 / max-width 780、链接色/气泡/边框/字体全部取自 DSH 安装包解析出的
`--dsw-*` 值（[`crates/ui-gpui/src/theme.rs`](crates/ui-gpui/src/theme.rs)），
默认浅色并可切换深色。助手正文支持 Markdown 子集（标题/列表/表格/代码块/引用/
粗斜体/行内代码/链接，[`crates/ui-gpui/src/markdown.rs`](crates/ui-gpui/src/markdown.rs)），
无语法高亮与图片。侧栏已是真实会话列表：新建、切换、行内重命名、二次确认删除，
重启恢复（[`docs/m2-sessions.md`](docs/m2-sessions.md)）。
上下文在请求前计量：超长工具结果头尾裁剪、超预算丢最旧整轮，丢弃的旧轮次由模型
生成摘要并写 `context.compacted`（[`docs/m2-context.md`](docs/m2-context.md)）。
差异清单见 [`docs/ui-dsh-alignment.md`](docs/ui-dsh-alignment.md)。
