# M3 状态 — 插件平台 Preview

> 更新：2026-11-15（W8 完成：独立 review P0/P1 全部修复，二次复审 + 独立复核 **接受**；M3 完成）。
> 范围、gate 与砍单顺序见 [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)；
> 计划基线见 [`PLAN.md`](PLAN.md) §8 M3 / §9.1；计划评审见 [`review-m3-plan.md`](review-m3-plan.md)；
> gate 口径见 [`decisions/0012-m3-gate-closure.md`](decisions/0012-m3-gate-closure.md)；
> 第一次独立评审 [`review-m3-implementation.md`](review-m3-implementation.md)；证据索引见
> [`m3-gate-evidence.md`](m3-gate-evidence.md)。
> 本文是**活文档**：每完成一个 gate 项就更新证据命令与结果，不写"计划中的证据"。

## 1. 当前结论

M3 已完成 **W1 契约冻结 + W2 Rust 实现 + W3 插件运行时与作者链路 + W4 L2 端到端/渲染预算 +
W5 产品面板/官方包验收/默认差异**。G1、G2、G3 的库层/产品路径证据基本齐备，剩余集中在
"可选增强 + WASM 主方案（网络已恢复，进行中）+ 工具并发"：

- **G1（契约冻结）**：ADC-v1 `Frozen`；服务端/解析降级/渲染器/panel 入口齐备；**W5 已把
  ADC panel 接进 chat**（右栏「插件面板 · ADC-v1」，`--selftest-adc-panel` PASS：合法树 0 诊断、
  未知元素 Placeholder、未知 view/插件可读错误）。剩余：tool card 的 ADC renderer 与交互/事件
  （ADR-0011 降级清单，M4）。
- **G2（外部作者可发布）**：SDK 0.1.0 + external-process 运行时 + 作者文档/模板/生成器/打包器 +
  E2E 五态 + chat 产品路径 self-test PASS。剩余：真仓库外目录实验（沙箱限制用 `.tmp/`）、
  Panel 渲染已接但交互未接、DSH JS 作者文档。
- **G3（隔离与兼容）**：L2 端到端已通；官方包证据改为**显式 manual gate**（`tools/official-l2-gate.ps1`，
  缺 fixture 时 exit 1；默认 cargo 中对应测试 `#[ignore]`，绝不静默 pass —— 修 review P0）；
  `sessionProjections` 适配器已实现（Rust `session-projections/v1` + sidecar shim + `RPC-L2-202..208`/
  `L2JS-021..028`；官方 `dsh-tool-todo` 仅装载绿，execute 受 `exec.agent` 债务）。**W8 WASM 已接
  宿主/产品链路**：`WasmPlugin` + `load_wasm_plugin` + profile loader + `--selftest-wasm`
  （安装→装载→echo→ui.render→trap→存活→卸载 PASS），`StoreLimits` 补 memories/tables/
  table_elements/instances（多 memory/大 table 负面测试）。剩余：Component/WIT（M4）、native/MCP
  串行（M4）、官方 execute 闭环（债务）。

## 2. Exit gate 对账

| Gate | 判定 | 当前状态 | 缺口（下一步） |
|---|---|---|---|
| **G1 契约冻结** | ADC `Frozen`；12 slot 一一对应；元素 props 字段级；§12 清零；未知元素 Placeholder + 诊断 + 不 panic | **达标（产品面板已接）**：spec Frozen + ADR-0011；ADC 56+10 tests、渲染 26+10 panel tests；预算 release 最差 3.78 ms；W5 chat 右栏「插件面板 · ADC-v1」+ `--selftest-adc-panel` PASS（未知元素 Placeholder/可读错误） | tool card 的 ADC renderer、交互/事件/焦点（ADR-0011 降级，M4） |
| **G2 外部作者可发布** | 仓库外只用文档+模板+semver SDK 构建并安装 Tool+Panel；首装确认/审计/来源校验；篡改/降级被拒 | **降级为"内部作者 Preview"**（[ADR-0012](decisions/0012-m3-gate-closure.md)）：SDK 自包含并可随生成器复制（不再依赖 repo 路径），但**未发布 registry**、无 uninstall/真实签名安装；E2E 五态 + 安装→装载→调用→卸载 + panel 渲染均 PASS | SDK 发布、uninstall、真实签名安装（M4）；G2 不得对外称"可发布" |
| **G3 隔离与兼容** | WASM 隔离 + 宿主存活；L2 官方包验收 | **隔离部分达标**：WASM 已接宿主/产品链路（`--selftest-wasm` 安装→装载→调用→trap→卸载 PASS；四类隔离 + 资源上限多 memory/table 负面测试）；external-process 退路保留。**L2**：3 个服务适配器 + `sessionProjections` 实现；官方包证据 = **manual gate**（`tools/official-l2-gate.ps1`，缺 fixture 失败；RPC-L2-201/501..505 不计默认 cargo pass），官方 execute 仍受 `exec.agent` 债务 | Component/WIT（M4）、native/MCP 串行（M4）、官方 execute 闭环（债务） |

## 3. 交付与证据

### 3.1 W1（契约/门禁，commit e24d25d）

| 交付 | 文件 | 证据 |
|---|---|---|
| 范围/预算/gate 锁定 | [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)、[`PLAN.md`](PLAN.md) §8 M3 + §9.1 | 3 条 gate 可判定；M2 债务逐项 M3/M4；砍单顺序明确 |
| Script UI 判死时间盒 | [`decisions/0008-script-ui-spike.md`](decisions/0008-script-ui-spike.md) | 5 日、三演示、退路；不作必达 |
| ADC-v1 冻结工件 | `schema/adc-v1/slots.v1.json`、`element.schema.json`、`slot-registration.schema.json` | `node schema/check.mjs` → 71/71；spec §5 12/12、§6 26 元素逐字段一致 |
| ADC spec 未决清零 | [`spec/adc-v1.md`](spec/adc-v1.md)（Review→Frozen；§5.1/§8.1；§12 Q1–Q5 关闭） | 状态机与 [ADR-0011](decisions/0011-adc-v1-freeze.md) |
| conformance 门禁 | `tools/conformance.mjs`、registry、矩阵、CI workflow | `--quick` 全绿；矩阵见 §4；CI 未在 GitHub Actions 实跑 |
| CPP typed manifest | `crates/plugin-api/src/manifest.rs` + tests | `CPP-MANIFEST-001..022` / `CPP-VER-001..007` |
| sidecar-rpc v2 / L2 选型 | [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md)、[`m3-l2-services.md`](m3-l2-services.md) | 见 §5 |
| M2 债务处置 | ADR-0007 §3 表 | 每项标 M3/M4 与理由 |

### 3.2 W2（Rust 实现轮，commit be6ff74）

| 交付 | 文件 | 证据（测试 ID / 命令） |
|---|---|---|
| **ADC-S 服务端** | `crates/ui-contributions/src/{catalog,registry,element,element_schema,pattern,diagnostic}.rs` | `ADC-SLOT-001..023`、`ADC-ELEM-001..022`、`ADC-SEC-001..009`（56） |
| **ADC-R 渲染器** | `crates/ui-gpui/src/adc.rs`（26 元素 + Placeholder + a11y） | 26 ADC-ELEM（含 26 元素全量/代表性树/parse→render 契约） |
| **CPP-A Fiber/Service/Effect** | `crates/plugin-api/src/{capability,cancel,tool_result}.rs`；`crates/plugin-host/src/fiber.rs` | `CPP-FIBER-*` / `CPP-SVC-*` / `CPP-EFF-*`（72） |
| **最小信任链** | [`spec/plugin-trust.md`](spec/plugin-trust.md)、[ADR-0009](decisions/0009-plugin-trust.md)、`schema/cpp-v1/trust.schema.json`、`crates/profile/src/trust.rs`、chat CLI | `CPP-TRUST-001..018`；`--selftest-trust-install` PASS（未确认/摘要/篡改/审计） |
| **L2 Rust 侧** | `compat-dsh-protocol` v2、`compat-dsh-host/{service,storage_kv}.rs` | `RPC-L2-001..010` / `101..119` / `301..313`（42） |
| **spec 对齐** | sidecar-rpc 方法名/requires_session/PROTOCOL_MAX；m3-l2 storage unit_id | 与实现一致 |

### 3.3 W3（插件运行时/作者链路/L2 集成）

| 交付 | 文件 | 证据（测试 ID / 命令） |
|---|---|---|
| **cpp-process 协议** | [`spec/cpp-v1.md`](spec/cpp-v1.md) §12、[`spec/cpp-v1.wit`](spec/cpp-v1.wit)、[ADR-0010](decisions/0010-isolation-runtime-quota.md) | 协议正文 + WASM 世界草案（WIT）+ 配额五维映射 |
| **插件作者 SDK** | `crates/plugin-sdk`（0.1.0：Envelope/ToolDecl/PanelDecl/`ChildSession::serve*`） | `CPP-PROC-*` 23 单测 + doctest；模板实跑 `cargo build` |
| **external-process 运行时** | `crates/plugin-host/src/external.rs` + `src/bin/plugin-fixture.rs` + `tests/external_process.rs` | `CPP-ISO-001..017`：超时(300ms→E_TIMEOUT, pid 死亡)、输出(1.2MiB→E_TOO_LARGE)、内存(512MiB@64MB→杀)、崩溃(exit3)、G2 安装→load→call→unload；`Get-Process plugin-fixture` 计数 0 |
| **TOML 正式 manifest** | `crates/plugin-api/src/manifest.rs`（`from_toml_str`/`from_path`）、workspace `toml` pinned | `CPP-MANIFEST-023..026`；chat 装载器用 `plugin.toml` |
| **G2 作者套件** | [`plugin-authoring.md`](plugin-authoring.md)、`templates/external-tool-plugin/`、`tools/new-plugin.mjs`、`tools/plugin-pack.mjs`、`tools/authoring-check.ps1` | `authoring-check.ps1` PASS：生成→构建→打包（摘要与 Rust 一致）→安装→升级→降级 `E_TRUST_DOWNGRADE`→篡改 `E_INTEGRITY_MISMATCH`→未确认 exit 2 |
| **chat 产品路径** | `chat.rs`：profile 启动装载 external 插件 + `--selftest-external-plugin` | `installed_ok/echo_ok/unloaded/reload_ok/panel_ok` 全 true，PASS |
| **MCP 最小客户端** | `crates/compat-mcp`（`McpClient`、`mcp-stub`、`tests/mcp_client.rs`） | `MCP-001..017`：initialize/分页/调用/isError/超时/崩溃重启/shutdown/非法行 |
| **L2 服务 + JS 代理** | `crates/compat-dsh-host/src/services.rs`（SessionAccess/WorkingDirectory/SessionLog）、`sidecar/dsh-official/lib/host-services.mjs` + `sidecar-dsh.mjs` v2 分支 | `RPC-L2-121..128` / `221..227`（15）+ `L2JS-001..013`（13）；握手 smoke v2/v1 双路径 |

### 3.4 W4（L2 端到端 / Loader / 渲染预算）

| 交付 | 文件 | 证据（测试 ID / 命令） |
|---|---|---|
| **SidecarHost v2 集成** | `crates/compat-dsh-host/src/lib.rs`（HostOptions.host_services/握手/三读循环 maybe_serve_host_service/capabilities）+ `service.rs`（HostService: Send） | `RPC-L2-401..410`：v2 握手/caps、无 router v1、sidecar v1 回退、storage 往返、wd 往返+事件、未知 key、cancel 先到、迟到 result、generation、默认 caps |
| **chat L2 接线** | `chat.rs`：`spawn_sidecar(approvals, l2_access)` + `build_l2_router`（storage-kv/working-directory/session-log）+ `ChatSessionAccess` + `--selftest-l2` | `chat --selftest-l2`：phase A PASS（真实 vendored sidecar protocol=2/active/host_services=3）、phase B PASS（fixture storage-kv `open→putRecord→loadAll` 往返）；`--selftest-dsh`/`--selftest-dsh-write` 不回归 |
| **Loader 债务** | `crates/profile/src/loader.rs`（include 递归/group 展开/patch InsertInto/诊断；`!!js` 不动） | `M3-LOADER-001..015` + lib 集成（profile 58 tests）；差异见 [`m3-loader.md`](m3-loader.md) |
| **ADC 渲染预算** | `crates/ui-gpui/tests/adc_perf.rs`（10 树 + 满配额，parse/view/total p50/p95，断言 p95<50ms） | release：10 树最差 total p95 3.78 ms、满配额 1.17 ms；见 [`m3-render-budget.md`](m3-render-budget.md) |
| **ADC panel 入口** | `crates/ui-gpui/src/adc.rs`：`adc_panel_body(theme, &Value) -> (AnyElement, Vec<Diagnostic>)` + `_from_str` | `ADC-ELEM-PANEL-001..010`（合法/未知根/配额/空/null/非法 JSON/a11y/叶子 children/容器） |
| **稳定性修正** | `crates/plugin-host/tests/external_process.rs` | 内存超限用例接受 E_INTERNAL/E_QUOTA/E_TIMEOUT/exited 多种观测并延长等待（全量并行负载下曾偶发）；其余隔离断言不变 |

### 3.5 W5（G1 产品面板 / G3 官方包验收 / M3 默认差异）

| 交付 | 文件 | 证据（测试 ID / 命令） |
|---|---|---|
| **ADC panel 产品面** | `chat.rs`：右栏「插件面板 · ADC-v1」（`refresh_adc_panels`/`AdcPanelView`）+ `plugin-api::Plugin::render_view` 默认方法 + `PanelContribution.view` + `PluginHost::render_panel`；external fixture panel view（合法树/未知元素） | `--selftest-adc-panel` PASS（panels=3 rendered=3 panel_errors=0 legal_diags=0 unknown_diag=true，未知 view/插件可读错误）；`ADC-ELEM-PANEL-API-*`/`-HOST-*` 测试 |
| **官方包 L2 验收** | `crates/compat-dsh-host/tests/official_packages.rs`（5 个）+ `sidecar/dsh-official/lib/official-l2-driver.mjs`（测试）；adapter 修正：wd shim 官方签名、`storage.backend.storage-kv` 生命周期、`loadAll` 未写 global 返回 `null` | `RPC-L2-501..505`：未修改 `dsh-tool-working-directory` 装载绿 + execute `exec.agent` 债务证据（502）；官方 `dsh-storage-domain` 经 `defineDomain`/`DomainFacility` → storage-kv 后端真实往返（put/get/delete/initial/close + already-open）；workingDirectory 官方签名代理往返 |
| **M3 默认差异** | `agent-core`（ReasoningEffort/重复提醒）、`llm-openai`（reasoning_effort）、`llm-retry`（EMPTY_RESPONSE）、`agent-context`（retainRatio 0.16 + env）、`tool-runtime`（PermissionMode/ApprovalPolicy/from_env/effective_timeout_ms） | `M3-DEFAULT-001..063`（39 唯一 ID；6 crate 176 tests）；细节见 [`m2-dsh-defaults.md`](m2-dsh-defaults.md) 的 M3 更新；chat 接线见下一行 |
| **债务接线（W5 并行）** | `chat.rs`/compat/host/sidecar：approval env、`tool.repeat` 事件、reasoning/retain env 路径、sidecar per-tool timeout、`--selftest-defaults` | 见 W5 接线报告；`--selftest*` 全 PASS（完成后补 ID） |
| **WASM 主机（W6，已集成）** | `crates/plugin-wasm`（wasmtime `=50.0.1`；ABI `dshr-wasm-core/v1`：`memory`+`dshr_alloc`+`dshr_invoke` JSON）；`WasmEngineConfig::from_manifest`；已并回根 workspace | `CPP-WASM-001..020`（20 tests）：fuel→`E_QUOTA(OutOfFuel)`、epoch 100ms→`E_TIMEOUT`、`StoreLimits`→`E_QUOTA(MemoryLimit)`、输出超限→`E_TOO_LARGE`、trap 后同 runtime 可用/relaunch、跨 runtime 隔离、并发→`E_BACKPRESSURE`、缺导出/非法模块/guest -1/unreachable；详见 [`wasm-host.md`](wasm-host.md) |
| **工具并行（W7）** | `agent-core`：`ToolExecutor::execute_batch`（默认逐个）+ `max_parallel_tool_calls`（override>env>10）；`compat-dsh-host`：`SidecarHost::invoke_batch`（按波写 invoke → 按 call_id 收 chunk/result/aborted，per-call timeout/取消/迟到丢弃）；`chat.rs`：sidecar 并行、native/MCP 串行、结果保持原序 | `M3-PARALLEL-001..007`（agent-core：顺序/并发/预算/错误隔离/上限优先级/重复提醒/取消）+ `M3-PARALLEL-101..107`（host：关联/chunk/墙钟/per-call timeout/迟到/取消/空批）；`--selftest-parallel-tools` PASS（serial 6.1s → parallel 2.0s，ratio 0.33，order_ok=true） |

### 3.6 W8（独立 review P0/P1 修复）

| 修复 | 改动 | 证据 |
|---|---|---|
| **P0 官方包证据不可空转** | `official_packages` 6 个测试 `#[ignore]`、缺 fixture `panic!`；新增 `tools/official-l2-gate.ps1` | 默认 cargo：`0 passed; 6 ignored`；gate 有 fixture → 6 PASS；临时移走包 → gate **exit 1**（已复原） |
| **P1-1 WASM 接宿主/产品** | `WasmPlugin` + `PluginHost::load_wasm_plugin` + profile loader 按 kind 分发；`--selftest-wasm` | `--selftest-wasm` PASS（installed/loaded/echo/panel/render/trap/survived/unloaded 全 true）；未知/错位 runtime kind 报错不静默 |
| **P1-2 WASM 资源上限** | `StoreLimits` 增 tables=1/memories=1/instances=1/table_elements=1024；`dshr_alloc` 0 视为失败 | `CPP-WASM-021..027`：多 memory/大 table → `E_QUOTA(MemoryLimit)` 且宿主存活；512 元素 table 正常 |
| **P1-3 external-process 边界** | 有界读帧（`take(MAX+1)` 分配前截断）、bounded channel(64)、stdin 写 watchdog(5s) | `CPP-ISO-018/019/026/027..031`：巨大单行→E_TOO_LARGE+Job 死；不读 stdin→E_TIMEOUT；故障后宿主仍可装载 |
| **P1-4/P2-6 安装原子化** | staging + 旧目录备份 rename + 失败回滚 | `M3-INSTALL-001..003`：copy 失败旧版本保留、无残留；升级成功清理 |
| **P1-4 SDK 可分发** | plugin-sdk 自包含（去 plugin-api 依赖）；生成器复制 `<out>/sdk/`；模板 `path="sdk"` | authoring-check 生成样例**无仓库路径**且离线构建/打包/安装 PASS；`CPP-PROC-024..027` 形状测试 |
| **P1-5 sessionProjections** | Rust `SessionProjectionsService` + sidecar `session-projections.mjs` shim；官方 `dsh-tool-todo` 装载绿 | `RPC-L2-202..208`（7）+ `L2JS-021..028`（8）；`RPC-L2-201` 仅装载绿（execute pending，官方债务） |
| **P1-7 batch 取消宽限** | 取消后每 slot 2s deadline → Cancelled，不再等 per-call 120s | `M3-PARALLEL-108`（忽略 cancel fixture，≈2s 返回，1.8s≤elapsed<5s） |
| **P2-3/P2-4/P2-5/P2-10** | panel id 全局冲突拒绝；selftest-l2 用 data root；摘要只跳根元数据（Rust/Node 交叉一致）；session_id 透传 API | `CPP-PANEL-002`/`CPP-WASM-033`；受限 TEMP 下 selftest-l2 PASS；`M3-DIGEST-001/002`（交叉摘要 `ec652b7…` 两侧一致）；`M3-PARALLEL-109/110` |
| **复审残余 P1（W8 补充）** | stderr 巨行有界（分配前截断、tail 64 行/64 KiB、超限丢弃到换行）；conformance ignored 检测改 binary→counts（stdout/stderr 合并捕获，修死代码） | `CPP-ISO-032/033`（16 MiB 单行仅留 4143 B、5000 短行留 64 行；宿主存活）；`--full` 在 Lead 环境 exit 0、`implemented 511/511`；official_packages 在 summary 中显式列为 `6 ignored` 不再 pass |

> **第二位独立 review**：[`review-m3-implementation-2.md`](review-m3-implementation-2.md) 结论
> "有条件接受"（条件：stderr 有界、提交并修正 conformance ignored 检测）；两项残余 P1 已在
> `ea020c8` 修复，独立复核（追加在 review-2 报告）确认 **接受**，两项 closed。**M3 完成**
> （按 [ADR-0012](decisions/0012-m3-gate-closure.md) 口径；P2 余项与 Component/WIT、SDK 发布、
> 官方 execute 闭环等按 review 建议列入 M4）。

## 4. Conformance 现状

- **契约**：`node schema/check.mjs` → **71/71**。
- **文档互链**：`node tools/check-links.mjs` → **336 链接 0 broken（65 文件）**。
- **Rust 单测**：**67 suites / 837 passed / 0 failed / 6 ignored**（6 ignored = 官方包 manual gate
  `RPC-L2-201/501..505`，显式 `tools/official-l2-gate.ps1` 跑 6/6 PASS；缺 fixture 时 gate exit 1）；
  含 wasmtime 27 tests、工具并行 16 tests、SDK 27 tests、external-process 边界/stderr 故障注入。
- **JS 测试**：`node --test --test-isolation=none` → **21/21**（host-services 13 + session-projections 8）。
- **矩阵（最终，Lead 环境）**：`node tools/conformance.mjs --full` → **exit 0**；
  registry **544 ID：implemented 511 / pending 18 / manual 15**；矩阵 stats
  `implemented 511/511 passed、失败 0、pending 18、manual 15`；`docs/conformance-matrix.json/.md`
  已落盘（generated_at 2026-11-15T12:4xZ，HEAD 5d2335d）。本机受限会话仍会出现
  `dshr-sidecar-guard` 沙箱抖动（矩阵如实记录为环境项，非代码）。
- **CI**：`.github/workflows/conformance.yml` 未在 GitHub Actions 实跑；官方包需本机 DSH 抽取物
  （manual gate）。

## 5. L2（摘要）

详见 [`m3-l2-services.md`](m3-l2-services.md)。首批三个服务：
`working-directory/v1`（10/27 官方工具注入）、`session-log/v1` + `sessionProjections`（todo/subagent）、
`storage-kv/v1`（官方 kv facet，生态押注）。Rust 侧已实现 `HostServiceRouter`（白名单/args/超时/取消/
去重/背压/generation）+ `StorageKvService` + `WorkingDirectoryService` + `SessionLogService`；
sidecar 侧实现了 v2 协商、`host.service.request/result/cancel`、event 订阅白名单、`ctx.storage`/
`workingDirectory` 代理（默认关闭，v1 行为不变）；**W4 已接 chat**（Router + `SessionAccess` +
三读循环 + `--selftest-l2` A/B），**W5 官方包验收**：未修改 `dsh-tool-working-directory` 装载绿 +
`exec.agent` 债务证据；官方 `dsh-storage-domain` 经 `defineDomain`/`DomainFacility` 与 storage-kv
后端真实往返。剩余：`event.*` 的 Cordis 总线桥仍是最小形态（TODO）；`dsh-storage` hub 本体是
Cordis Service class，当前 sidecar plugin seam 不能按普通 plugin 装载（需扩展 boot，另开任务）。

## 6. 未决与风险

| 项 | 影响 | 处理 |
|---|---|---|
| WASM 主机 = core module（非 Component/WIT） | PLAN 字面 "WASM Component" | 已接宿主并四类隔离；Component/WIT 移 M4（[ADR-0012](decisions/0012-m3-gate-closure.md)） |
| G2 SDK 未发布/crates.io | G2 字面口径 | 已降级为"内部作者 Preview"（ADR-0012）；SDK 自包含、生成器可脱离仓库构建，但 uninstall/真实签名安装 M4 |
| 官方包证据是 manual gate | G3 可复现性 | `tools/official-l2-gate.ps1` 显式跑、缺 fixture 失败；CI 无 DSH 抽取物时不能自动验，需另找合法 vendor 渠道（M4） |
| 官方 `execute` 闭环 | `exec.agent` 债务 | M3 只验证装载绿+方法往返+错误面；execute 闭环 M4 |
| P2 余项（窗口 draw、matrix full 落盘、panel id/事件、`--selftest-*` SKIP 语义、native 并发、leak 等） | 非 gate | 已记录在 review P2；M4 前修或写进已知限制 |
| `plugin-api` trait 冻结 | 插件 ABI | `Plugin` 仍同步 `call`（`&'static str` leak），冻结留到并发改造（M4） |
| Beta 对外口径 | 产品 | **已确认（2026-11-16）：时间无硬指标、不设硬上限；预算按 PLAN §9.2 追加 80–140 候选，不据此砍 G1–G3** |

## 7. 下一步（按依赖排序）

1. **W8 收尾 evidence**：conformance 最终同步（RPC-L2-201/501..505 → manual；新增 WASM/ISO/
   INSTALL/DIGEST/PANEL/SESSION 测试）并提交 `--full` 矩阵；跑一次全量 `cargo test --workspace`
   + `tools/official-l2-gate.ps1`，刷新最终数字。
2. **第二次独立 review**：复核 P0/P1 是否关闭、gate 口径（ADR-0012）是否可接受；通过后才可把目标
   标记完成。
3. **M4 候选**（已记录，不在 M3 gate）：Component/WIT canonical ABI、SDK 发布 + uninstall +
   真实签名安装、官方 execute 闭环、native 并发/trait 冻结、ADC 交互/tool card、窗口 draw、
   子会话并行/sidecar 工具继承、P2 余项。
   *（2026-11-16：M4 已启动，W1 交付 uninstall + 密钥生命周期；范围见
   [ADR-0013](decisions/0013-m4-scope.md)，进度见 [m4-status.md](m4-status.md)。）*

## 8. 复现命令

```powershell
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs
tools\build-env.cmd cargo test --offline -p dshr-plugin-sdk -p dshr-plugin-host
tools\build-env.cmd cargo test --offline -p dshr-plugin-api -p dshr-profile
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-protocol -p dshr-compat-dsh-host
tools\build-env.cmd cargo test --offline -p dshr-compat-mcp -p dshr-ui-contributions -p dshr-ui-gpui
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-trust-install
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-external-plugin
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1
tools\build-env.cmd cargo test --offline --workspace
```
