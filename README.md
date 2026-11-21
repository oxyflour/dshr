[![cross-platform](https://github.com/oxyflour/dshr/actions/workflows/cross-platform.yml/badge.svg)](https://github.com/oxyflour/dshr/actions/workflows/cross-platform.yml)
[![conformance](https://github.com/oxyflour/dshr/actions/workflows/conformance.yml/badge.svg)](https://github.com/oxyflour/dshr/actions/workflows/conformance.yml)

# Native Agent Desktop — workspace

Rust 管理 Agent、会话和工具执行，GPUI 渲染桌面界面。dshr 自有插件使用 CPP-v1，界面扩展使用 ADC-v1。
已有 DSH 工具包可通过 Node sidecar 复用，具体取决于包版本、所需的 `ctx` 服务和实际调用路径。
已验证的操作、启用条件和已知限制见 [DSH 插件兼容矩阵](docs/spec/dsh-compat-mapping.md#兼容矩阵)。

## 文档地图

| 文件 | 内容 |
|---|---|
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划：目标、架构决策、里程碑、估算 |
| [插件作者指南](docs/plugin-authoring.md) | 选择插件入口，开发、打包和安装插件 |
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

解压桌面发行包后直接启动 `dshr.exe` 或 `dshr`。桌面默认加载随包提供的官方 DSH sidecar 和文件工具。

```powershell
# 契约校验（只需 Node）
node schema/check.mjs
node tools/check-links.mjs

# 构建。Windows 上必须走 build-env：本机 %TEMP% 不能让链接器写入
tools\build-env.cmd cargo build --workspace

# Windows 主程序为 target\debug\dshr.exe，release 构建输出到 target\release\dshr.exe
# 启动桌面，使用默认的官方 DSH sidecar
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr -- --selftest          # 探针自检
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr -- --selftest-stream   # 流式自检
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr -- --live "一句话"      # 真实 LLM（需 key）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr -- --live-chat "第一句" "第二句"  # 多轮记忆验证

# 只读自检，验证 load_workspace_dependencies 和 read
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr -- --selftest-dsh
```

默认 profile 包含 `load_workspace_dependencies` 和 `read`。桌面宿主协商 `fs.write` 后可注册
`write`、`edit` 和 `apply_patch`，写入由宿主按当前权限和工作区边界裁决。`probe_*` 只供仓库自检使用。
如果源码运行时提示官方依赖缺失，可用 `node tools/dsh-vendor.mjs` 从本机 DeepSeek Harness 提取依赖。
[启用条件与验证记录](docs/spec/dsh-compat-mapping.md#兼容矩阵)

在桌面新会话界面，点工作区旁的模式菜单并选择「创造模式」。用 `creator_inspect` 查看清单样例，用 `creator_write_file` 从空工作区写入插件文件。可预览 ADC-v1 面板草稿。Windows x64 可在「设置 → 运行组件」安装创造模式构建组件，随后审批构建和试用 Rust 外部进程插件。[创造模式规范](docs/spec/creator-mode.md)
每个创造会话保有自己的试用实例。切换会话后可以回来继续试用，删除会话或退出应用会释放实例。工具注册后从下一次模型请求起可用。
官方 Sidecar 的创造会话使用 `cordis` 预设。桌面后台发布 Loader 预设配置变更，新会话使用当前修订，已绑定会话保留原修订。
[插件创作和试用指南](docs/plugin-authoring.md#在创造会话中预览和试用) · [Agent 预设](docs/spec/agent-presets.md)

## 在桌面配置真实模型

打开左下角的「设置」，进入「模型」。可以编辑 DeepSeek，或点击「添加 OpenAI 兼容模型」来配置一个或多个模型。每条连接填写 API 根地址、模型 ID 和 API Key，再点「保存并使用」。在已有 OpenAI 兼容连接上点击「添加模型」，会预填 API 根地址，API Key 留空即可沿用该服务的密钥。输入卡右下角的模型菜单显示各模型的连接地址，便于切换同名模型。

每条连接独立保存在系统凭据库，并随 DSHR 数据根隔离。编辑同一服务时，API Key 留空可继续使用已保存的密钥。更换协议、主机或端口时，填写新服务的 API Key。可在编辑页用「移除配置」删除单条连接。重启会恢复设置中选定的模型。通过环境变量提供的模型仍可在模型列表中选择。显式设置 `DSHR_PROVIDER` 时，启动时优先选择指定类型下可用的环境模型。终端任务沿用下面的配置方式。

没有可用模型时，输入区的「配置模型」直达模型设置，草稿可在模型配置好后发送。测试时可设置 `DSHR_FORCE_MOCK=1` 或 `DSHR_PROVIDER=mock` 显示本地模拟。

打开输入区的模型菜单，进入「推理等级」，可为当前连接保存强度。DeepSeek 提供关闭、低、高和最高。OpenAI 兼容模型可选的强度由服务端支持情况决定。选择「默认」可沿用环境变量和服务端设置。

消息上方的「思考过程」可展开查看模型返回的推理内容和连续工具调用。点击停止时会先显示「正在停止…」，结束后显示「已停止」并保留已生成的内容。悬停消息后点击复制，按钮会短暂显示勾选。

生成或等待审批时，可在输入区切换工具权限。选择「工具免审批」会放行当前等待的普通工具，并直接处理后续工作区内写入。插件构建和试用仍需单独确认。[桌面对话控制](docs/spec/desktop-conversation.md)

要只放行当前会话中的某个工具，可在它的审批卡片上选择「始终允许」。重启后仍可在同一会话使用这项授权，其他工具继续按当前模式审批。

## 从终端运行一次性任务

Windows 发行包使用 `bin\dshr.exe`，也可以把包内的 `bin` 目录加入 PATH。Linux 和 macOS 使用发行包的 `dshr`。
先配置 `DEEPSEEK_API_KEY` 或 `OPENAI_API_KEY`，运行时可从参数或 stdin 接收任务。

```powershell
.\bin\dshr.exe --profile headless "总结这个工作区"
Get-Content -Raw .\prompt.txt | .\bin\dshr.exe --profile headless --json
.\bin\dshr.exe --profile headless --session-id s-123 "继续上一轮"
```

用 JSON 输出首行的 `sessionId` 续跑，并保持原工作区。`0` 表示本轮已完成并保存，`1` 表示运行失败或被取消。
具名 profile 从数据根的 `profiles/<name>/package.json` 读取，可使用 DSH 的 headless 清单和受支持的静态补丁。
可用 `DSHR_HOME` 指定数据根，并用 `--patch <path>` 叠加本次运行的配置。
[输入、退出码与 profile 示例](docs/spec/headless-profile.md)

## 已固定的版本

| 组件 | 版本 | 记录 |
|---|---|---|
| Rust | `1.99.1` (stable) | [`ADR-0001`](docs/decisions/0001-gpui-pin.md) |
| GPUI | zed `d2a4fb6` | 同上 |
| GPUI Kit | `dbb0b9a`（0.9.0） | 同上 |
| Rust Cordis | `jincdream/cordis-rs` | [`ADR-0002`](docs/decisions/0002-cordis-rs-selection.md) |
| DSH 插件测试基线 | `0.2.2-rc.1`，Cordis `4.1.0` | 抽取版本取决于本机安装副本。各版本的测试范围见 [兼容矩阵](docs/spec/dsh-compat-mapping.md#兼容矩阵) |

## 当前状态

**M0：4 项完成、1 项部分、1 项转入 M1。** 工具链已就绪，`cargo build --workspace` 通过，契约校验 20/20、文档互链 0 broken。剩余项见 [`docs/m0-runbook.md`](docs/m0-runbook.md) §5。

**M4 四条 gate 已完成（跨平台 Beta；W2/W3/W4/W6/W8 仍在收尾）。** 范围、8 个工作包与四条 gate 口径见
[`docs/decisions/0013-m4-scope.md`](docs/decisions/0013-m4-scope.md)，证据与未验证项见
[`docs/m4-status.md`](docs/m4-status.md)。W1（分发与信任生命周期）按离线渠道口径完成：
`--uninstall-plugin`（目录/profile/审计原子化）、信任密钥吊销/时间窗/轮换与
`trust-key-audit.jsonl`、作者 ed25519 签名打包/安装、`tools/sdk-pack.mjs` 离线 SDK
（manifest + SHA256SUMS）；`M4-UNINSTALL-001..008` / `M4-KEY-001..010` /
`M4-SDK-001` / `M4-SIGN-001`。W2 部分完成：全局快捷键（新建/聚焦/侧栏/插件/审批/关闭）、
按钮 role/aria_label 与 TextInput a11y 元数据已实现（[m4-keyboard.md](docs/m4-keyboard.md)），
G3 已由 `DSHR_KEYBOARD_FLOW=1` 在真实窗口自动验证；IME、macOS 键位与完整 Tab 焦点环仍未覆盖。
W3 已接 ADC 交互：插件面板 Button `onPress` 回到宿主，`copy` 写剪贴板，`call`/`command` 经
external-process `ui.action` 到插件后再 render；`openExternal` 走确认条（取消/Esc 不打开）；
工具结果 `meta.ui` 已接到 tool card 附加区；Checkbox/Switch/Select/Input/Textarea/Slider 的
`onChange` 与 Slider 拖拽已接（[m4-adc.md](docs/m4-adc.md)）；真实窗口 copy/openExternal/IME 待手测。
W8 修掉两处"node 缺失时 SKIP 记 pass"的集成测试。G2 已由三项 headless 自检 +
`M4-RECOVERY-004`（`--selftest-window-recovery`：真实崩溃留下 `.bak` → 启动恢复 → 窗口内继续发送）覆盖。
W5 已启用 wasmtime `component-model`，并用 `M4-COMPONENT-001..023` 验证 canonical ABI、
`cpp-v1.wit`、bindgen、配额、probe、真实 guest、`CppComponentPlugin`/manifest loader、
`--selftest-cpp-component`/`--selftest-cpp-install`/`--selftest-cpp-cli-install`、并行/共享调用与真实 fuel trap；
真实窗口/剪贴板 smoke 也已在 Windows 通过。
W6 已补最小 `exec.agent` facade，官方 todo/working_directory/fs read+write 的 execute 在 official gate 8/8 通过。
G4 已有 `tools/perf-gate.mjs`（阈值 + CI step），GitHub Actions 已实跑成功（run 44122517731）。
G2 除损坏会话/插件回滚外，`--selftest-sidecar-recovery` 覆盖 kill sidecar，`--selftest-crash-recovery`
覆盖真实子进程 abort 后从 `.bak` 继续追加。
W4 已去掉动态插件 id 的 `Box::leak`（`Plugin::id/version` 借用），并实现 native 跨插件批并行
与调用前协作式取消；运行中抢占/MCP 与异步 trait 冻结仍未做。
G1 的 `cross-platform.yml`（三平台契约 + 核心 crate + desktop-build + 包 5 项自检）已由 GitHub Actions 实跑全绿：
[run 44122517682](https://github.com/oxyflour/dshr/actions/runs/44122517682)，9/9 jobs success；签名/公证仍为可选、未配证书。
WSL Linux 上核心 6 crate、`dshr-desktop-gpui` 构建与 ADC/recovery/crash 自检均已通过；
macOS target 上 `dshr-ui-gpui` 在 x86_64/aarch64 均类型检查 PASS，桌面二进制因本机无 macOS SDK 在 C 依赖处未过。
Windows Authenticode 管线已用一次性自签证书验证（`status=UnknownError`，见 [m4-signing.md](docs/m4-signing.md)），
CI 已有可选 PFX/codesign 步骤；真实证书/时间戳/notarization 仍未接。
`package-desktop.ps1/sh` 生成自带 `runtime/node` 与 `dsh-official`（含 node_modules）的
Windows zip（59.7MB）与 Linux tar.gz（197.1MB）；`verify-package.ps1/sh` 校验 sha256、解压并跑
sidecar/official 自检，双平台 PASS，仍不含签名/安装器。
官方包 gate 之前先用 `official-provenance.mjs` 校验版本/SHA-256（lock 已提交）。
W3 窗口 draw 在 RDP 下硬预算 PASS（p95 9.40ms / 31.25ms），相对基线受远程环境干扰未通过。
**M4 四条 gate 已全部 PASS（G1/G2/G3/G4）**；W4/W6/W8 仍有部分收尾项，registry/CA/自动更新、
生产签名/公证/安装器未交付，因此不能把整个 M4 范围说成“全部完成”。

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

**M1 完成（核心闭环已打通）。** 已完成：A1 虚拟化、A1b 启动拆解、A1c 帧预算、B Cordis 选型、A3 WebView2 引擎自检、A5 端到端回放、A5b 真实调用闭环、C Sidecar 协议往返（纯协议 p50 148 µs）、A6 基本对话、A7 Native Plugin、A7b 统一 Registry、A8 真实 Cordis 插件、A9 流式消息、**A10 Agent 接口**、**A11 真实 LLM**（`DeepSeekProvider`，从 `~/.dsh/.env` 读 key）、A12/A13 协议对齐与结果卡片、A14 真 SSE 流式（首字 ~0.7–0.85 s）、**A15 多轮上下文 + 严格工具协议**（`role: tool` + `tool_call_id`，真实多轮记忆与多工具已验证）、**A16 真实输入组件**（`ui-gpui::TextInput`，实现 `EntityInputHandler`，中文 IME 可组合输入）。共 98 个测试 + 四个 `dshr --selftest*` + `conversation --check` + `dshr --live/--live-chat`。A2 IME 有已知缺陷（[issues/0001](docs/issues/0001-ime-candidate-first-composition.md)，产品界面待人工复验）。未做：中文 IME 的人工复验、并行工具调用、多行输入。详见 [`docs/m1-log.md`](docs/m1-log.md)。

**M2 收尾：四条 exit gate 已达标（2026-11-11）。** 官方 DSH 插件不改一行源码装载：
`@deepseek-ai/dsh-tool-workspace-dependencies` 与 `@deepseek-ai/dsh-tool-fs`（`read`）
经 Cordis 4.1.0 注册进 Rust Registry；能力协商 `fs.write` 后，官方
`write`/`edit`/`apply_patch` 的写盘请求回到 Rust → 沙箱边界 → 审批 → 写前快照 →
原子写（`--selftest-dsh-write`；[`docs/m2-official-write.md`](docs/m2-official-write.md)）。
插件依赖随桌面包提供，可用 `tools/dsh-vendor.mjs` 从本机安装的 `app.asar` 更新。
基础发行包在 `runtime/node` 内置 Node。自建包可用 `tools/vendor-node.mjs` 准备它。Windows x64 可在「设置 → 运行组件」另外安装 Python 解释器和标准库。[运行组件规范](docs/spec/runtime-components.md)

Sidecar 以
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
（[Loader 支持的配置](docs/m3-loader.md)）；`dshr --install-plugin <目录|.tgz>` 离线安装
并写安装审计日志（[`docs/m2-plugins.md`](docs/m2-plugins.md)）。
provider 已有 DeepSeek 与 OpenAI 兼容两家，统一重试/退避；真实网络 20 步工具调用
成功率 20/20（[`docs/m2-stability.md`](docs/m2-stability.md)）。
会话可导出（JSONL + 外部化附件 + manifest sha256 自校验）并查看保留计划：
`dshr --export-session <id>` / `dshr --retention-plan`；panic 会写入
`DSHR_CRASH_LOG`（默认 `.tmp/crash.log`）——[`docs/m2-sessions.md`](docs/m2-sessions.md)、
[`docs/m2-spill.md`](docs/m2-spill.md)。
M2 四条 gate 的对账与 17 个自检命令见 [`docs/m2-status.md`](docs/m2-status.md)；
MCP/ACP 与签名/更新通道按 `PLAN.md` 后移 M3/M4。
差异清单见 [`docs/ui-dsh-alignment.md`](docs/ui-dsh-alignment.md)。
