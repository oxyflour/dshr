# M3 Gate 证据与偏离说明（review-ready）

> 用途：给最终独立 review 的**证据索引**。每个 gate 列出：判定口径 → 可复现命令 → 当前证据 →
> 已知偏离/限制 → 未完成项。活文档状态见 [`m3-status.md`](m3-status.md)。
> 更新：2026-11-14（W3 后）。

## 0. 判定口径（来自 [ADR-0007](decisions/0007-m3-scope.md) §4）

| Gate | 判定 |
|---|---|
| **G1** | ADC spec `Frozen`；12 slot 与目录一一对应；元素 props 字段级；§12 未决清零；未知元素 → `Placeholder` + 诊断 + 不 panic、同树其余元素仍渲染 |
| **G2** | 仓库外目录只用文档 + 模板 + semver SDK + `schema/` 构建并安装 Tool + Panel 插件；首装确认/审计/来源校验生效；篡改/降级被拒 |
| **G3** | 隔离运行时在超时/内存/输出/崩溃四类下隔离且宿主存活、Session 无脏写（WASM 主方案 = wasmtime core module；external-process 退路保留）；3 个 L2 service 用未修改官方包跑通并回填耗时 |

## 1. G1 — ADC-v1 契约冻结

**状态：库层达标；产品面待接（不算 gate 失败，但影响"可见"）。**

证据：

| 项 | 证据 |
|---|---|
| spec Frozen + 冻结 ADR | [`spec/adc-v1.md`](spec/adc-v1.md) 头 `Frozen`；[ADR-0011](decisions/0011-adc-v1-freeze.md) |
| 12 slot 与 spec 一一对应 | [`schema/adc-v1/slots.v1.json`](../schema/adc-v1/slots.v1.json)；`node schema/check.mjs` → 71/71（含目录自检与 spec §5 逐项比对测试） |
| 元素 props 字段级 | [`schema/adc-v1/element.schema.json`](../schema/adc-v1/element.schema.json)；26 元素；check.mjs 正反例 |
| 未知元素降级 + 不 panic + 同树其余渲染 | `crates/ui-contributions` 的 `parse_tree`（`ADC-ELEM-*`，56 tests）→ `Placeholder` + 诊断；`crates/ui-gpui::adc_view` Placeholder 渲染（26 tests） |
| §12 未决清零 | spec §12 Q1–Q5 关闭（Q5 WebView 明确排除） |
| panel 渲染入口 | `dshr_ui_gpui::adc_panel_body(theme, &Value) -> (AnyElement, Vec<Diagnostic>)`（`ADC-ELEM-PANEL-001..010`） |
| **panel 产品面（W5）** | `plugin-api::Plugin::render_view` 默认方法 + `PanelContribution.view` + `PluginHost::render_panel`；`chat.rs` 右栏「插件面板 · ADC-v1」；`chat --selftest-adc-panel` PASS（panels=3 rendered=3 legal_diags=0 unknown_diag=true） |
| 渲染预算 | [`m3-render-budget.md`](m3-render-budget.md)：release 10 棵典型树最差 total p95 **3.78 ms**（16.7ms 预算 23%）；满配额 200 节点树 1.17 ms；窗口内 draw 留 M4/手动 |

未完成/偏离：
- tool card 的 ADC renderer、交互/事件/焦点顺序未接（ADR-0011 降级清单，M4）。
- `Chart` line/donut、`Image` 内容、交互事件/焦点顺序是**实现降级**（ADR-0011 §后果），不是规范变更。
- `ADC-BIND-001`（绑定/事件接线）仍 pending；窗口内 layout/paint 预算待 M4/手动。

## 2. G2 — 外部作者可发布

**状态：链路已通，E2E 五态 PASS；沙箱转为 danger-full-access 后"仓库外目录"可复跑；panel 已接产品渲染（无交互）。**

证据：

| 项 | 证据 |
|---|---|
| semver SDK | `dshr-plugin-api` 0.1.0（`HOST_VERSION`）；`dshr-plugin-sdk` 0.1.0 |
| 文档/模板/生成器/打包器 | [`plugin-authoring.md`](plugin-authoring.md)、`templates/external-tool-plugin/`、`tools/new-plugin.mjs`、`tools/plugin-pack.mjs` |
| 外部作者 E2E | `tools/authoring-check.ps1` → PASS：生成→构建→打包（摘要与 Rust `package_digest` 逐字节一致）→安装（审计 accepted）→升级→降级 `E_TRUST_DOWNGRADE`→篡改 `E_INTEGRITY_MISMATCH`→未确认 exit 2 |
| 安装→装载→调用→卸载产品路径 | `chat --selftest-external-plugin` → PASS（installed/echo/unload/reload/panel 全 true） |
| 首装确认/审计/来源校验 | [`spec/plugin-trust.md`](spec/plugin-trust.md) + `crates/profile/src/trust.rs`（18 tests）+ chat CLI |
| 篡改/降级被拒 | trust 测试 + authoring E2E 反例 |

未完成/偏离：
- "仓库外目录"：本机文件沙箱只允许 workspace 写入，样例生成在 `.tmp/`（gitignored），**没有读 `crates/` 源码**（checklist 在作者文档）。严格口径的"真仓库外"需在普通终端复跑。
- **Panel 产品渲染**未接（`ui.render` → GPUI）；Tool 链路已通。
- DSH JS 插件作者路径只在文档指向 sidecar，无独立章节/模板。
- 签名工具只有规范（ed25519 校验实现），无 `plugin-sign.mjs` 之类发布工具（M3 不要求）。

## 3. G3 — 隔离与兼容

**状态：WASM 已接宿主/产品链路（core-module 口径）并通过四类隔离 + 资源上限；L2 三适配器 + sessionProjections 已实现，官方包证据=显式 manual gate（缺 fixture 失败）；external-process 退路保留。**

证据：

| 项 | 证据 |
|---|---|
| 隔离运行时 | `CPP-ISO-001..017`（`crates/plugin-host/tests/external_process.rs`）：超时(300ms→E_TIMEOUT/Job 杀)、输出(1.2MiB→E_TOO_LARGE)、内存(512MiB@64MB→杀)、崩溃(exit3)→宿主存活、`Get-Process plugin-fixture` 计数 0 |
| 配额模型 | [ADR-0010](decisions/0010-isolation-runtime-quota.md)；`crates/plugin-host/src/external.rs`（Job Object：内存/CPU/进程数、env 清洗） |
| 三个 L2 服务（Rust） | `crates/compat-dsh-host/src/{service,services,storage_kv}.rs`；`RPC-L2-001..010`（协议）、`101..119`（路由）、`121..128`（working-directory）、`221..227`（session-log）、`301..313`（storage） |
| sidecar v2 代理（JS） | `sidecar/dsh-official/sidecar-dsh.mjs` v2 分支 + `lib/host-services.mjs`；`L2JS-001..013`（node:test 13/13） |
| **宿主 v2 集成 + chat 端到端** | `crates/compat-dsh-host`（HostOptions.host_services、hello protocol_max=2、三读循环 `maybe_serve_host_service`、capabilities、generation）；`chat --selftest-l2` phase A（真实 vendored sidecar：protocol=2 / active=true / host_services=3）+ phase B（fixture storage-kv 真实往返）；`RPC-L2-401..410`（10 个集成测试，fixture Node sidecar） |
| **官方包验收（W5/W8，manual gate）** | `tools/official-l2-gate.ps1`（缺 `.tmp/dsh-tools-all` → exit 1；有 fixture → 6/6 PASS）。`crates/compat-dsh-host/tests/official_packages.rs` 6 个测试 `#[ignore]`（默认 cargo **0 passed / 6 ignored**，不会静默 pass）：`RPC-L2-201`（官方 `dsh-tool-todo` 装载绿，execute pending `exec.agent`）+ `501..505`（working-directory 装载绿+execute 债务证据、官方 dsh-storage-domain 经 `defineDomain`/`DomainFacility` → storage-kv 真实往返、官方签名 wd 代理往返、storage hub Service class 证据） |
| WASM 主方案（W6/W8，已接宿主） | `crates/plugin-wasm`（wasmtime `=50.0.1`，ABI `dshr-wasm-core/v1`）：`CPP-WASM-001..020` 四类隔离 + `CPP-WASM-021..027`（多 memory/大 table/alloc=0/panel 冲突）；`WasmPlugin` + `PluginHost::load_wasm_plugin` + profile loader + `--selftest-wasm`（安装→装载→echo→ui.render→trap→存活→卸载 PASS） |
| L2 sessionProjections（W8） | Rust `SessionProjectionsService`（`session-projections/v1`）+ sidecar `session-projections.mjs` shim；`RPC-L2-202..208`（7）+ `L2JS-021..028`（8） |
| 工具并行（W7/W8） | `agent-core::ToolExecutor::execute_batch` + `max_parallel_tool_calls`（override>env>10）；`SidecarHost::invoke_batch`（按波写 `tool.invoke` → 按 `call_id` 收 chunk/result/aborted，per-call timeout/取消 2s 宽限/迟到丢弃）；chat sidecar 并行、native/MCP 串行、结果保持原序；`M3-PARALLEL-001..007` + `101..110`（16 tests，含取消宽限/session_id 透传）；`--selftest-parallel-tools` PASS：3×2s 串行 6.1s → 并行 2.0s（ratio 0.33），order_ok=true；事件序 `ToolCall×N → 终态×N`（session-log 已注） |
| external-process 退路 | `CPP-ISO-001..026`（Job Object 内存/CPU/进程配额、四类隔离；W8 增有界读帧/bounded channel/stdin watchdog 故障注入 `CPP-ISO-018/019/026..031`） | 见 [`cpp-v1.md`](spec/cpp-v1.md) §12、[`wasm-host.md`](wasm-host.md) |
| 协议 v2 向后兼容 | 无 router / sidecar max=1 → negotiated=1、不发 v2 kind、capabilities 回 `E_VERSION`；M2 `--selftest-dsh` / `--selftest-dsh-write` 不回归 | `RPC-L2-402/403`；`--selftest-l2` phase A/B |

未完成/偏离（**review 重点**）：
1. **WASM 主方案是 core module，不是 Component/WIT**：host 已接入（`WasmPlugin`/loader/`--selftest-wasm`），
   Component Model/WIT canonical ABI 仍移 M4；已由 [ADR-0012](decisions/0012-m3-gate-closure.md) 记录，
   不再声称"WASM Component 已交付"。
2. **官方包证据=manual gate**：`tools/official-l2-gate.ps1` 显式跑、缺 fixture exit 1；默认 cargo 中
   `RPC-L2-201/501..505` 为 `#[ignore]`（P0 已修，不再静默 pass）。官方 execute 仍受 `exec.agent` 债务，
   storage hub 本体是 Cordis Service class；`sessionProjections` 已实现（Rust+JS），官方 todo 仅装载绿。
3. **工具并行的剩余差异**：native/MCP/`spawn_child` 仍串行；并行仅约束 sidecar 批；`tool.chunk` 尚未被
   agent 消费；同 step 事件顺序变化要求 UI/回放按 `call_id` 配对（已在 session-log 规范化）。
4. `event.*` 的 Cordis 总线桥是最小形态（TODO）；storage 无游标/backupRecord/fsync（记录在
   [`m3-l2-services.md`](m3-l2-services.md) §6.3）；G2 已降级为内部作者 Preview（ADR-0012）。

## 4. 环境限制（不是代码失败，需在 review 中区分）

| 项 | 现象 | 处置 |
|---|---|---|
| 网络/沙箱策略 | 早期 crates.io 不可达、workspace-write；2026-11-15 起 danger-full-access + 网络可用 | WASM/cordis 可通过新依赖实现；`wasmtime` 已缓存；文档保留"离线退路"（ADR-0010） |
| 进程限制 | `dshr-sidecar-guard` 的 `drop_terminates_the_guarded_process` 在部分受限会话偶发失败 | 记为环境项；Lead 普通会话 67 suites/831 passed/0 failed；CI 用普通 runner 复验 |
| `node --test` 子进程隔离 | 受限会话报 EPERM | 用 `--test-isolation=none` 或直接 `node <file>`（conformance node-test suite 已处理） |
| GitHub Actions | 仓库无 CI 历史，workflow 未实跑；官方包 fixture 无法自动 vendor | 首次推送后 `workflow_dispatch` 验证；官方包证据走 manual gate（`tools/official-l2-gate.ps1`） |
| 数据目录 ACL | `%LOCALAPPDATA%\dshr` 不可写 → 回退 `target/debug` | 不影响自检；真机 GUI 需普通用户上下文 |

## 5. 复现命令（review 建议顺序）

```powershell
# 1) 契约与门禁（秒级）
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick

# 2) 各 crate 关键测试
tools\build-env.cmd cargo test --offline -p dshr-plugin-sdk -p dshr-plugin-host
tools\build-env.cmd cargo test --offline -p dshr-plugin-api -p dshr-profile
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-protocol -p dshr-compat-dsh-host
tools\build-env.cmd cargo test --offline -p dshr-compat-mcp
tools\build-env.cmd cargo test --offline -p dshr-ui-contributions -p dshr-ui-gpui

# 3) 产品路径自检
tools\build-env.cmd cargo build --offline -p dshr-plugin-host --bins
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-trust-install
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-external-plugin
powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1

# 4) 全量（plain cargo 会在第一个失败处停；harness 用 --no-fail-fast）
tools\build-env.cmd cargo test --offline --workspace
node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs
```

## 6. Review 建议关注点

1. WASM 主方案以 core module 实现是否被接受（PLAN 写 Component/WIT）；是否需要在 M4 承诺中单列 Component 主机。
2. G2 的"仓库外"实验被 `.tmp/` 替代是否可接受；作者文档是否真的足以让第三方不读源码。
3. `Plugin` trait 仍同步、`&'static str` leak 一次、无 `tool.chunk` 消费/并发=1——对 trait 冻结与 M4 兼容的影响。
4. chat.rs 的 L2 接线已通（`--selftest-l2` phase A+B），端到端证据是否足够；官方包验收（未修改装载/方法往返）尚未做，是否接受"装载绿+方法往返"口径；`event.*` 最小桥的 TODO 是否可接受。
5. storage-vs-systemPrompt 首批服务选择（0/27 官方工具调用方 vs 14/27）；证据是否支持当前决定。
6. 环境项（sidecar-guard/CI/ACL）是否被误计为代码缺陷。
