# M3 状态 — 插件平台 Preview

> 更新：2026-11-14（W2：ADC/CPP/信任链/L2 实现轮）。范围、gate 与砍单顺序见
> [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)；计划基线见 [`PLAN.md`](PLAN.md) §8 M3 / §9.1；
> 计划评审见 [`review-m3-plan.md`](review-m3-plan.md)。
> 本文是**活文档**：每完成一个 gate 项就更新证据命令与结果，不写"计划中的证据"。

## 1. 当前结论

M3 已完成 **W1 契约冻结批次 + W2 实现批次的 Rust 侧**，G1/G2/G3 均有实质推进但**尚未全部达标**：

- **G1（契约冻结）**：ADC-v1 冻结工件 + 服务端 slot 注册表 + 26 元素解析/降级/校验 + GPUI 渲染器
  均已实现（56 + 26 个测试），剩余是 spec `Review → Frozen`、把渲染器/注册表接进 `chat.rs` 产品面。
- **G2（外部作者可发布）**：最小信任链已完整落地（摘要/可选 ed25519/首装确认/审计/降级拒绝 +
  CLI 端到端自检）；剩余是 SDK semver、两套模板、`plugin-authoring.md` 与仓库外样例。
- **G3（隔离与兼容）**：L2 的 Rust 侧已落地（协议 v2 类型/校验 + HostServiceRouter + storage-kv
  adapter，42 个 `RPC-L2-*`）；剩余是 JS sidecar 代理、chat.rs 路由接线、三个服务的真实往返，
  以及 WASM 主机（本机离线拉不到 wasmtime / cordis-rs，需要退路决策）。

## 2. Exit gate 对账

| Gate | 判定 | 当前状态 | 缺口（下一步） |
|---|---|---|---|
| **G1 契约冻结** | ADC `Frozen`；12 slot 一一对应；元素 props 字段级；§12 清零；未知元素 Placeholder + 诊断 + 不 panic | **大部分**：冻结工件齐；`ui-contributions` 实现目录/注册表/`parse_tree`（未知元素→Placeholder + 诊断 + 不 panic，56 测试）；`ui-gpui` 渲染器 26 元素 + Placeholder（26 测试）。spec 仍 `Review` | ① spec 冻结 ADR 与状态改 `Frozen`；② 渲染器/注册表接进 `chat.rs` 产品面；③ 渲染预算实测 |
| **G2 外部作者可发布** | 仓库外只用文档+模板+semver SDK 构建并安装 Tool+Panel；首装确认/审计/来源校验；篡改/降级被拒 | **部分**：信任链完整（`profile::trust` 18 测试 + chat CLI `--selftest-trust-install` PASS：未确认拒绝、摘要匹配、篡改拒绝、审计接受/拒绝）；`dshr-plugin-api` 仍 0.0.0，模板/作者文档/仓库外样例未做 | ① SDK semver；② rust-static/wasm-component 模板；③ `plugin-authoring.md`；④ 仓库外样例 + "未读源码" checklist |
| **G3 隔离与兼容** | WASM 三故障注入隔离且宿主存活；3 个 L2 service 用未修改官方插件跑通 | **部分**：L2 Rust 侧完成（protocol 20 + host 35 + `RPC-L2-*` 42）；无 WASM/WIT，无 JS 代理，未接 chat.rs | ① sidecar JS 侧 `ctx.storage`/`host.service.*` 代理；② chat.rs 路由接线 + 三服务往返；③ WASM（或 `external-process` 退路）三故障注入 |

## 3. 交付与证据

### 3.1 W1（契约/门禁，2026-11-12，commit e24d25d）

| 交付 | 文件 | 证据 |
|---|---|---|
| 范围/预算/gate 锁定 | [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)、[`PLAN.md`](PLAN.md) §8 M3 + §9.1 | 3 条 gate 可判定；M2 债务逐项 M3/M4；砍单顺序明确 |
| Script UI 判死时间盒 | [`decisions/0008-script-ui-spike.md`](decisions/0008-script-ui-spike.md) | 5 日、三演示、退路；不作必达 |
| ADC-v1 冻结工件 | `schema/adc-v1/slots.v1.json`（12 slot）、`element.schema.json`（26 元素字段级 props）、`slot-registration.schema.json`（4 种载荷形状） | `node schema/check.mjs` → **71/71**（含 trust fixture）；spec §5 12/12、§6 26 元素逐字段一致 |
| ADC spec 未决清零 | [`spec/adc-v1.md`](spec/adc-v1.md)（`Review`；§5.1 目录唯一事实源；§8.1 插件持久化状态；§12 Q1–Q5 关闭） | 状态机 `Draft → Review`；Q5 WebView 明确排除（v1/v2） |
| conformance 门禁 | `tools/conformance.mjs`、`docs/conformance.d/registry.json`、[`conformance.md`](conformance.md)、矩阵、`.github/workflows/conformance.yml` | `--quick` 全绿（schema+links+注册表）；矩阵见 §4；CI **未在 GitHub Actions 实跑** |
| CPP typed manifest | `crates/plugin-api/src/manifest.rs` + tests；`schema/cpp-v1/manifest.schema.json` | `CPP-MANIFEST-001..022` / `CPP-VER-001..007`（29 tests） |
| sidecar-rpc v2 / L2 选型 | [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md)、[`m3-l2-services.md`](m3-l2-services.md) | 见 §5 |
| M2 债务处置 | ADR-0007 §3 表（9 项 + ACP） | 每项标 M3/M4 与理由 |

### 3.2 W2（实现轮，2026-11-14）

| 交付 | 文件 | 证据（测试 ID / 命令） |
|---|---|---|
| **ADC-S 服务端** | `crates/ui-contributions/src/{catalog,registry,element,element_schema,pattern,diagnostic}.rs` | 56 tests：`ADC-SLOT-001..023`、`ADC-ELEM-001..022`、`ADC-SEC-001..009`；`cargo test -p dshr-ui-contributions` 全绿 |
| **ADC-R 渲染器** | `crates/ui-gpui/src/adc.rs`（26 元素 + Placeholder + a11y AccessKit） | 26 ADC-ELEM 单测（含 3 个无窗口 smoke：26 元素全量/代表性树/parse→render 契约）；`cargo test -p dshr-ui-gpui` 69 passed |
| **CPP-A Fiber/Service/Effect** | `crates/plugin-api/src/{capability,cancel,tool_result}.rs`；`crates/plugin-host/src/fiber.rs` | 72 唯一 ID：`CPP-FIBER-001..048`、`CPP-SVC-001..014`、`CPP-EFF-001..010`；两 crate 107 tests 全绿；`PluginHost::load_with_manifest` 校验版本/配额 |
| **最小信任链** | [`spec/plugin-trust.md`](spec/plugin-trust.md)、[`decisions/0009-plugin-trust.md`](decisions/0009-plugin-trust.md)、`schema/cpp-v1/trust.schema.json`、`crates/profile/src/trust.rs`、`chat.rs` 可信安装 CLI | `CPP-TRUST-001..018`（profile 42 tests 全绿）；`chat --selftest-trust-install` PASS（未确认退出码 2、摘要匹配、篡改拒绝、审计 accepted+rejected）；`--yes-install` / `--allow-downgrade` |
| **L2 Rust 侧** | `crates/compat-dsh-protocol`（v2 常量/类型/校验）、`crates/compat-dsh-host/{service,storage_kv}.rs` | `RPC-L2-001..010`（协议）、`RPC-L2-101..119`（路由）、`RPC-L2-301..313`（storage）= 42；protocol 20 + host 35 全绿 |
| **spec 对齐** | `sidecar-rpc.md` 方法名 regex/`requires_session`/`PROTOCOL_MAX` 注记；`m3-l2-services.md` storage `unit_id` | 与实现一致（方法名允许点分段、storage 每个 unit.* 带 unit_id） |

## 4. Conformance 现状

- **契约**：`node schema/check.mjs` → **71/71**（原 20 条不回归；新增 slots/元素 props/注册 kind/v2 envelope/
  CPP manifest 新字段/dshr-trust fixture）。
- **文档互链**：`node tools/check-links.mjs` → **240 链接 0 broken**。
- **Rust 单测**：`tools\build-env.cmd cargo test --offline --workspace` → **26 suites / 448 passed / 1 failed**。
  唯一失败：`dshr-sidecar-guard process::tests::drop_terminates_the_guarded_process`
  （本会话沙箱的进程创建/进程枚举限制导致；隔离复跑同样失败，非本轮改动引入，需在正常桌面会话复验）。
- **矩阵**：`docs/conformance-matrix.json` / `.md` 由 `tools/conformance.mjs` 生成；本轮新增 200+ 已实现
  测试 ID 正在并入 `docs/conformance.d/registry.json`（上轮为 57 ID：25 implemented / 31 pending / 1 manual），
  完成后以矩阵统计为准。
- **CI**：`.github/workflows/conformance.yml` 已定义（Windows：schema + links + cargo + 矩阵 artifact），
  **未在 GitHub Actions 实跑**，本机证据以上面命令为准。

## 5. L2（摘要）

详见 [`m3-l2-services.md`](m3-l2-services.md)。证据：27 个官方工具包的 `inject` 频次
（`workingDirectory` 10、`sessionProjections` 2、`storage` 0、`settings` 0）与 DSH 0.2.2-rc.1
的 `SERVICE_API`（**107 项**，v0.2 写 98）。首批三个：`working-directory/v1`（1.5–2.5 日）、
`session-log/v1` + `sessionProjections`（2.5–3.5 日）、`storage-kv/v1`（2–3 日），合计 9.5–14 日。

协议 v2 是**向后兼容增补**：`PROTOCOL_MAX=2` + `service_key` 可选字段 + `host.service.*` /
`service.cancel` 四个 kind；`service.proxy` 能力协商。Rust 侧已实现 `HostServiceRouter`（方法表白名单、
args 最小校验、超时/取消/去重/背压、generation 失效）与 `StorageKvService`（7 个 unit 方法、版本戳、
per-unit 串行化、768 KiB 上限）。`event.*` 通道仍是 L2 的顺序前置（v1 未实现）。
**关键风险**：24/27 官方工具的 `execute` 读 `exec.agent`——服务到位 ≠ 工具可执行，G3 的 L2
判定已按"装载绿 + 方法往返 + 超时/取消/参数错误"收口，工具 `execute` 级闭环不进 M3。

## 6. 未决与风险

| 项 | 影响 | 处理 |
|---|---|---|
| WASM 主机：本机离线，`wasmtime`/WIT 拉不到 | G3 的 WASM 分支无法按原方案 | 先写 `cpp-v1.wit` + 配额 ADR-0010；运行时分发走 **`external-process` 退路**（ADR-0007 反悔条件）或 vendored runtime 决策；不在 M3 静默跳过 |
| 渲染器/注册表尚未接进 `chat.rs` | G1 只在库层可判定，产品面不可见 | 下一轮：`ui-contributions` 注册表面板 + `adc_view` 接入 tool/panel 渲染；补渲染预算实测 |
| `FiberHost` 未接 chat.rs | CPP 运行时未进入真实装载路径 | 下一轮：`PluginHost::load_with_manifest` + FiberHost 事件/Config/取消布线 |
| L2 的 JS 代理与 chat.rs 路由未接 | G3 往返无真实证据 | 下一轮：sidecar JS `ctx.storage` 代理 + chat.rs 读循环 `host.service.request`；先补 `event.*` |
| Beta 对外口径 290–470（原 240–390） | 产品/商业承诺 | **待产品确认**；若维持原上限按 ADR-0007 §5 砍单（不得砍 G1–G3） |
| `plugin-api` 版本仍 0.0.0 | `min_host_version` 基准不可用；G2 不成立 | 下一轮给 SDK semver，`Manifest::validate` 用宿主真实版本 |
| `menu.item` 闭合 key 域的具体菜单名 | ADC 注册校验 | 目录标 `source: "host"`，待 Host 菜单目录回填 |
| `dshr-sidecar-guard` 进程测试在受限会话失败 | workspace 测试 1/449 | 环境项；正常桌面会话复验；不阻塞 quick gate |
| GitHub Actions 未跑 | CI 证据缺口 | 首次推送后补跑；矩阵可由 `--quick` 本地复现 |
| storage 语义限制 | v2 不做游标/backupRecord/fsync/跨进程锁 | 记录在 `m3-l2-services.md` §6.3；真实大域出现再升 major |
| Script UI / HTML Widget 未验证 | 条件项 | spike 5 日（ADR-0008）；HTML Widget 依赖 WebView 决策 |

## 7. 下一步（按依赖排序）

1. **G1 收口**：冻结 ADR + `adc-v1.md` 改 `Frozen`；渲染器/注册表接进 `chat.rs`；渲染预算测试。
2. **G2 外部作者**：`dshr-plugin-api` semver、`rust-static`/`wasm-component` 模板、`plugin-authoring.md`、
   仓库外样例构建 + 安装 + 卸载 + 反例（篡改/降级）——G2 的可复现实验。
3. **G3 L2 集成**：先补 `event.*`（v1 未实现）；sidecar JS `ctx.storage` 代理 + `host.service.request`；
   chat.rs 路由接线；三个服务真实往返与耗时回填 PLAN §5.1。
4. **G3 WASM 退路**：`cpp-v1.wit` + ADR-0010 配额；`wasmtime` 不可用则实现 `external-process` 隔离
   （Job Object + 三故障注入），在 ADR 中明确偏离与原因。
5. **M3 债务**：工具并行执行、子会话并行/sidecar 工具继承、默认差异（reasoning effort / 摘要压缩 /
   重复调用提醒 / `EMPTY_RESPONSE` / approval 模式）、`timeoutMs` 透传、Loader `include`/group 子集、MCP 最小闭环。
6. **条件项**：Script UI 5 日 spike；HTML Widget（WebView 决策）；pnpm 捆绑评估。
7. 全部完成后：conformance 矩阵全绿 + 开子 agent 独立 review。

## 8. 复现命令

```powershell
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
tools\build-env.cmd cargo test --offline -p dshr-ui-contributions -p dshr-ui-gpui
tools\build-env.cmd cargo test --offline -p dshr-plugin-api -p dshr-plugin-host
tools\build-env.cmd cargo test --offline -p dshr-profile
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-protocol -p dshr-compat-dsh-host
tools\build-env.cmd cargo run --offline -p dshr-desktop-gpui --bin chat -- --selftest-trust-install
tools\build-env.cmd cargo test --offline --workspace
```
