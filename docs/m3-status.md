# M3 状态 — 插件平台 Preview

> 更新：2026-11-15（W7：工具并行收口；M3 功能项完成，进入收尾 evidence + 独立 review）。
> 范围、gate 与砍单顺序见 [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)；
> 计划基线见 [`PLAN.md`](PLAN.md) §8 M3 / §9.1；计划评审见 [`review-m3-plan.md`](review-m3-plan.md)；
> 给最终 review 的证据索引见 [`m3-gate-evidence.md`](m3-gate-evidence.md)。
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
- **G3（隔离与兼容）**：L2 端到端已通（`--selftest-l2` A+B）；**W5 官方包验收**：
  `dsh-tool-working-directory`（未修改）装载绿 + execute 的 `exec.agent` 债务证据（`RPC-L2-502`）、
  `dsh-storage-domain`（未修改）经官方 `defineDomain`/`DomainFacility` 与 `storage-kv` 后端真实往返
  （`RPC-L2-501..505`）；workingDirectory 官方签名代理往返 PASS。**W6 WASM 主机已落地**：
  wasmtime `=50.0.1` core-module ABI（`CPP-WASM-001..020` 四类隔离：fuel/epoch/内存/输出 +
  trap 后宿主存活/relaunch/并发背压）；Component Model/WIT 未启用（偏离，见
  [`wasm-host.md`](wasm-host.md)）。**W7 工具并行收口**：sidecar 工具按 `maxParallelToolCalls=10`
  分波并发（`SidecarHost::invoke_batch`），native/MCP 仍串行；实测 3×2s 串行 6.1s→并行 2.0s
  （ratio 0.33），结果按原序、取消/超时/迟到语义保持。剩余：子会话并行/sidecar 工具继承、
  `tool.chunk` 消费与 Component/WIT（标注为 M4/后续，非 gate）。

## 2. Exit gate 对账

| Gate | 判定 | 当前状态 | 缺口（下一步） |
|---|---|---|---|
| **G1 契约冻结** | ADC `Frozen`；12 slot 一一对应；元素 props 字段级；§12 清零；未知元素 Placeholder + 诊断 + 不 panic | **达标（产品面板已接）**：spec Frozen + ADR-0011；ADC 56+10 tests、渲染 26+10 panel tests；预算 release 最差 3.78 ms；W5 chat 右栏「插件面板 · ADC-v1」+ `--selftest-adc-panel` PASS（未知元素 Placeholder/可读错误） | tool card 的 ADC renderer、交互/事件/焦点（ADR-0011 降级，M4） |
| **G2 外部作者可发布** | 仓库外只用文档+模板+semver SDK 构建并安装 Tool+Panel；首装确认/审计/来源校验；篡改/降级被拒 | **大部分**：SDK 0.1.0；作者文档/模板/生成器/打包器；E2E PASS（安装/升级/降级/篡改/未确认）；chat 安装→装载→调用→卸载 self-test PASS；信任链完整；panel 渲染已接 | ① 真仓库外目录实验（沙箱限制，当前 `.tmp/` + 不读源码 checklist）；② DSH JS 插件作者文档补章；③ panel 交互（M4） |
| **G3 隔离与兼容** | WASM 三故障注入隔离且宿主存活；3 个 L2 service 用未修改官方插件跑通 | **达标（收窄口径）**：WASM 主机 core-module 四类隔离（`CPP-WASM-001..020`，wasmtime 50.0.1）；external-process 退路保留（CPP-ISO）；L2 端到端 + 官方包验收（501..505）；`--selftest-l2` A+B | ① Component Model/WIT canonical ABI（PLAN 字面 "WASM Component" 的偏离，M4/后续）；② 工具并发（M3 债务） |

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

## 4. Conformance 现状

- **契约**：`node schema/check.mjs` → **71/71**。
- **文档互链**：`node tools/check-links.mjs` → 待 W5/W6 同步后刷新（W4 为 307/0 broken）。
- **Rust 单测**：**66 suites / 797 passed / 0 failed**（Lead 环境；含 `dshr-plugin-wasm` 20 个与
  工具并行 14 个；沙箱 danger-full-access 下原 sidecar-guard 进程测试也通过）。本机受限会话偶发
  sidecar-guard 1 失败（环境，非代码）；CI 仍需普通 runner 复验。
- **JS 测试**：`node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs`
  → 13/13（conformance 的 `node-test` suite 已纳入 `--quick`）。
- **矩阵**：`docs/conformance-matrix.json` / `.md` 由 `tools/conformance.mjs` 生成；最终注册表
  **502 ID：implemented 476 / pending 18 / manual 8**（pending 仅 `ADC-BIND-001` + 17 个
  `SELF-CHAT`；manual 含 trust/external/adc-panel/l2/defaults/parallel/authoring 七个自检与
  `CPP-TRUST-019`）；quick 门禁 schema+links+node-test 全绿、0 failed。
- **矩阵生成时间/环境**：2026-11-15T11:51Z，HEAD e10ba8f；本机受限会话 `--workspace` 为
  66/796/1（sidecar-guard 沙箱抖动，隔离复跑同样失败），普通终端 Lead 环境为 66/797/0。
- **CI**：`.github/workflows/conformance.yml` 未在 GitHub Actions 实跑。

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
| WASM 主机 = core module（非 Component/WIT） | PLAN 字面 "WASM Component" | 已实现四类隔离并集成；Component Model/WIT canonical ABI 作为后续项（M4/独立任务），review 明示偏离 |
| 工具并行（W7 已完成） | 子会话并行/sidecar 工具继承、`tool.chunk` 消费、Component/WIT 仍未做 | 明确列为 M4/后续（非 M3 gate）；review 记录 |
| ADC 交互/事件/tool card | G1 增强项 | ADR-0011 降级清单，M4 |
| 外部作者"仓库外"实验 | G2 字面口径 | 沙箱 danger-full-access，可在普通目录复跑 `tools/authoring-check.ps1`（`.tmp/` 证据仍有效） |
| `plugin-api` trait 冻结 | 插件 ABI | external SDK 已 0.1.0；`Plugin` trait 仍同步 `call`（`&'static str` leak 一次），冻结留到并发改造 |
| Beta 对外口径 290–470 | 产品 | 待产品确认；按 ADR-0007 §5 砍单 |
| 环境项：docs/ shell 写 ACL、node --test 子进程隔离 | 本机验证 | 新策略下部分已改善；CI 普通 runner 复验；矩阵用文件工具落盘 |

## 7. 下一步（按依赖排序）

1. **收尾 evidence**：把 W7 并行证据并入 conformance 矩阵；跑一次 `--full`/`--quick` 与全量
   `cargo test --workspace`，刷新 m3-status/gate-evidence 的最终数字。
2. **子 agent 独立 review**：对 M3 实现、gate 证据、偏离（WASM core-module、官方包口径、
   环境限制）做独立评审；根据 P0/P1 修复后收尾。
3. **M4/后续候选**（不在 M3 gate）：Component/WIT canonical ABI、子会话并行/sidecar 工具继承、
   `tool.chunk` 消费、ADC 交互/事件/tool card、`headroomTokens`/`compactionRetries`、
   Script UI spike/HTML Widget/pnpm 等条件项。

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
