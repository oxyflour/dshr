# M3 状态 — 插件平台 Preview

> 更新：2026-11-14（W3：external-process 运行时 / G2 作者套件 / G3 L2 集成）。范围、gate 与砍单顺序见
> [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)；计划基线见 [`PLAN.md`](PLAN.md) §8 M3 / §9.1；
> 计划评审见 [`review-m3-plan.md`](review-m3-plan.md)。
> 本文是**活文档**：每完成一个 gate 项就更新证据命令与结果，不写"计划中的证据"。

## 1. 当前结论

M3 已完成 **W1 契约冻结 + W2 Rust 实现 + W3 插件运行时与作者链路**。三条 gate 现在都**接近达标**，
剩余项集中在"产品面接线"与"外部作者形式化实验"：

- **G1（契约冻结）**：ADC-v1 已 `Frozen`（[ADR-0011](decisions/0011-adc-v1-freeze.md)），冻结工件/
  服务端/解析降级/渲染器齐备（82 个 ADC 测试）。剩余：渲染器与 slot 注册表接进 `chat.rs` 产品面、
  渲染预算实测。
- **G2（外部作者可发布）**：`dshr-plugin-api` 0.1.0 + `dshr-plugin-sdk` 0.1.0（semver）；external-process
  运行时（Job Object 配额/超时/崩溃隔离，`CPP-ISO-*`）；作者文档 + 模板 + 生成器 + 打包器 + E2E
  （安装/升级/降级/篡改/未确认五种结果）；chat 产品路径 `--selftest-external-plugin` PASS
  （安装→装载→调用→卸载→重载→panel）。剩余：真正仓库外目录实验受本机沙箱限制（用 `.tmp/` 代替）、
  Panel 产品渲染、DSH JS 插件作者路径的单列文档。
- **G3（隔离与兼容）**：L2 Rust 侧 + sidecar JS 代理 + 两个新服务（working-directory/session-log，
  `RPC-L2-121..128`/`221..227`）与 JS 单测 13 个完成；隔离由 external-process 承担
  （`CPP-ISO-*` 17 个：超时/输出/内存/崩溃四类，宿主存活、无残留进程）。剩余：chat.rs 把 Router 接进
  sidecar 读循环（真实往返）、三个服务用官方包验收、WASM 主方案（本机离线，ADR-0010 退路已启用）。

## 2. Exit gate 对账

| Gate | 判定 | 当前状态 | 缺口（下一步） |
|---|---|---|---|
| **G1 契约冻结** | ADC `Frozen`；12 slot 一一对应；元素 props 字段级；§12 清零；未知元素 Placeholder + 诊断 + 不 panic | **达标（库层）/产品面待接**：spec Frozen + ADR-0011；`ui-contributions` 56 tests、`ui-gpui` 26 tests；未知元素 P4 有测试 | ① 渲染器/注册表接进 chat.rs；② 渲染预算 p95 实测 |
| **G2 外部作者可发布** | 仓库外只用文档+模板+semver SDK 构建并安装 Tool+Panel；首装确认/审计/来源校验；篡改/降级被拒 | **大部分**：SDK 0.1.0；作者文档/模板/生成器/打包器；E2E PASS（安装/升级/降级/篡改/未确认）；chat 安装→装载→调用→卸载 self-test PASS；信任链完整 | ① 真仓库外目录实验（沙箱限制，当前 `.tmp/` + 不读源码 checklist）；② Panel 产品渲染；③ DSH JS 插件作者文档补章 |
| **G3 隔离与兼容** | WASM 三故障注入隔离且宿主存活；3 个 L2 service 用未修改官方插件跑通 | **大部分（退路口径）**：external-process 隔离四类故障（CPP-ISO）；L2 Rust+JS 服务完成；chat 路由接线/官方包往返待做；WASM 由 ADR-0010 退路替代 | ① chat.rs 接 Router + SessionAccess（真实往返）；② 三服务官方包验收；③ WASM 主机（离线阻塞）在 review 中明示偏离 |

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

## 4. Conformance 现状

- **契约**：`node schema/check.mjs` → **71/71**。
- **文档互链**：`node tools/check-links.mjs` → **285 链接 0 broken**。
- **Rust 单测**：`cargo test --offline --workspace` plain 运行在第一个失败处中止
  （31 suites / 524 passed / 1 failed）；harness `--no-fail-fast` 完整结果：
  **59 suites / 671 passed / 1 failed**，唯一失败仍是 `dshr-sidecar-guard` 的沙箱进程限制
  （非本轮代码；普通桌面会话/CI 复验）。
- **JS 测试**：`node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs`
  → 13/13（conformance 的 `node-test` suite 已纳入 `--quick`）。
- **矩阵**：`docs/conformance-matrix.json` / `.md`（generated_at 2026-11-14T15:01Z）由
  `tools/conformance.mjs` 生成；注册表 **378 ID：implemented 355 / pending 19 / manual 4**
  （manual 含 `chat --selftest-external-plugin`、`tools/authoring-check.ps1`）；quick 门禁
  schema+links+node-test 全绿、0 failed。
- **CI**：`.github/workflows/conformance.yml` 未在 GitHub Actions 实跑。

## 5. L2（摘要）

详见 [`m3-l2-services.md`](m3-l2-services.md)。首批三个服务：
`working-directory/v1`（10/27 官方工具注入）、`session-log/v1` + `sessionProjections`（todo/subagent）、
`storage-kv/v1`（官方 kv facet，生态押注）。Rust 侧已实现 `HostServiceRouter`（白名单/args/超时/取消/
去重/背压/generation）+ `StorageKvService` + `WorkingDirectoryService` + `SessionLogService`；
sidecar 侧实现了 v2 协商、`host.service.request/result/cancel`、event 订阅白名单、`ctx.storage`/
`workingDirectory` 代理（默认关闭，v1 行为不变）。**剩余**：chat.rs 把 Router/SessionAccess 接进
sidecar 读循环，之后用官方包跑 `RPC-L2-*` 真实往返并回填耗时；`event.*` 的 Cordis 总线桥仍是最小形态。

## 6. 未决与风险

| 项 | 影响 | 处理 |
|---|---|---|
| WASM 主机（离线不可获取 wasmtime） | G3 原口径 | [ADR-0010](decisions/0010-isolation-runtime-quota.md)：external-process 退路承担隔离；WIT/配额已备；review 必须明示偏离 |
| 渲染器/slot 注册表未接 chat 产品面 | G1 的"产品可见" | 下一轮：`adc_view` + `ContributionRegistry` 接入 tool/panel 渲染；渲染预算实测 |
| L2 未接 chat.rs | G3 真实往返 | 下一轮：spawn_sidecar 协商 v2 + Router + SessionAccess + 三读循环 |
| 外部作者"仓库外"实验 | G2 字面口径 | 沙箱只允许 workspace 写入；当前 `.tmp/` 生成样例 + "未读 crates 源码" checklist；review 中说明 |
| 工具并行/子会话并行/默认差异 | M3 债务 | 下一轮（tool concurrency、reasoning effort、摘要压缩、重复提醒、EMPTY_RESPONSE、approval 模式） |
| `plugin-api` trait 冻结 | 插件 ABI | external SDK 已 0.1.0；`Plugin` trait 仍同步 `call`（`&'static str` leak 一次），冻结留到并发改造 |
| Beta 对外口径 290–470 | 产品 | 待产品确认；按 ADR-0007 §5 砍单 |
| 环境项：sidecar-guard 进程测试、docs/ shell 写 ACL、node --test 子进程隔离 | 本机验证 | 记录；正常桌面/CI 复验；矩阵用文件工具落盘 |

## 7. 下一步（按依赖排序）

1. **G3 收口**：chat.rs 接 L2 Router + SessionAccess（真实 sidecar 往返）；三服务官方包验收。
2. **G1 收口**：ADC 渲染器/注册表接 chat 面板与 tool card；渲染预算测试。
3. **M3 债务**：工具并行（DSH maxParallelToolCalls=10）、子会话并行/sidecar 工具继承、默认差异对齐、
   `timeoutMs` 透传、Loader `include`/group 子集。
4. **WASM 决策收口**：如仍离线，提交 ADR-0010 偏离说明 + `external-process` 隔离证据；
   如可获取运行时，接 Wasmtime 并跑 `CPP-WASM-*`。
5. **最终 gate 证据 + 子 agent 独立 review**。
6. 条件项：Script UI 5 日 spike、HTML Widget（WebView 决策）、pnpm 捆绑。

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
