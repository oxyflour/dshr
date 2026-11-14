# M3 Gate 证据与偏离说明（review-ready）

> 用途：给最终独立 review 的**证据索引**。每个 gate 列出：判定口径 → 可复现命令 → 当前证据 →
> 已知偏离/限制 → 未完成项。活文档状态见 [`m3-status.md`](m3-status.md)。
> 更新：2026-11-14（W3 后）。

## 0. 判定口径（来自 [ADR-0007](decisions/0007-m3-scope.md) §4）

| Gate | 判定 |
|---|---|
| **G1** | ADC spec `Frozen`；12 slot 与目录一一对应；元素 props 字段级；§12 未决清零；未知元素 → `Placeholder` + 诊断 + 不 panic、同树其余元素仍渲染 |
| **G2** | 仓库外目录只用文档 + 模板 + semver SDK + `schema/` 构建并安装 Tool + Panel 插件；首装确认/审计/来源校验生效；篡改/降级被拒 |
| **G3** | 隔离运行时在超时/内存/输出/崩溃四类下隔离且宿主存活、Session 无脏写（WASM 主方案离线不可用，ADR-0010 退路为 external-process）；3 个 L2 service 用未修改官方包跑通并回填耗时 |

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
| 渲染预算 | [`m3-render-budget.md`](m3-render-budget.md)：release 10 棵典型树最差 total p95 **3.78 ms**（16.7ms 预算 23%）；满配额 200 节点树 1.17 ms；窗口内 draw 留 M4/手动 |

未完成/偏离：
- 渲染器与 slot 注册表**尚未接进 chat 产品面**（`adc_panel_body` + `ContributionRegistry` 到 panel/tool card 的布线）。
- `Chart` line/donut、`Image` 内容、交互事件/焦点顺序是**实现降级**（ADR-0011 §后果），不是规范变更。
- `ADC-BIND-001`（绑定/事件接线）仍 pending；窗口内 layout/paint 预算待 M4/手动。

## 2. G2 — 外部作者可发布

**状态：链路已通，E2E 五态 PASS；字面"仓库外"受本机沙箱限制（用 `.tmp/` 近似）。**

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

**状态：隔离（external-process 退路）达标；L2 宿主↔sidecar↔chat 端到端已通；官方包验收待做。**

证据：

| 项 | 证据 |
|---|---|
| 隔离运行时 | `CPP-ISO-001..017`（`crates/plugin-host/tests/external_process.rs`）：超时(300ms→E_TIMEOUT/Job 杀)、输出(1.2MiB→E_TOO_LARGE)、内存(512MiB@64MB→杀)、崩溃(exit3)→宿主存活、`Get-Process plugin-fixture` 计数 0 |
| 配额模型 | [ADR-0010](decisions/0010-isolation-runtime-quota.md)；`crates/plugin-host/src/external.rs`（Job Object：内存/CPU/进程数、env 清洗） |
| 三个 L2 服务（Rust） | `crates/compat-dsh-host/src/{service,services,storage_kv}.rs`；`RPC-L2-001..010`（协议）、`101..119`（路由）、`121..128`（working-directory）、`221..227`（session-log）、`301..313`（storage） |
| sidecar v2 代理（JS） | `sidecar/dsh-official/sidecar-dsh.mjs` v2 分支 + `lib/host-services.mjs`；`L2JS-001..013`（node:test 13/13） |
| **宿主 v2 集成 + chat 端到端** | `crates/compat-dsh-host`（HostOptions.host_services、hello protocol_max=2、三读循环 `maybe_serve_host_service`、capabilities、generation）；`chat --selftest-l2` phase A（真实 vendored sidecar：protocol=2 / active=true / host_services=3）+ phase B（fixture storage-kv 真实往返）；`RPC-L2-401..410`（10 个集成测试，fixture Node sidecar） |
| 协议 v2 向后兼容 | 无 router / sidecar max=1 → negotiated=1、不发 v2 kind、capabilities 回 `E_VERSION`；M2 `--selftest-dsh` / `--selftest-dsh-write` 不回归 |
| WASM 主方案 | `spec/cpp-v1.wit` + ADR-0010 配额五维已落盘；**Wasmtime 主机未实现**（离线无法获取 crate） |

未完成/偏离（**review 重点**）：
1. **WASM 主方案未交付**：G3 原口径"WASM 在 fuel/内存/超时下隔离"由 `external-process`（Job Object）
   承担；`CPP-WASM-*` 保持 pending。这是 ADR-0007 反悔条件允许的退路 + ADR-0010 显式记录，
   **不是静默跳过**。
2. 三服务**官方包**验收仍待做（`dsh-tool-working-directory`/`dsh-tool-todo`/`dsh-storage*` 未修改装载、
   方法往返、错误面）；`exec.agent` 债务使"工具 execute 级闭环"明确不在 M3。
3. `event.*` 的 Cordis 总线桥是最小形态（TODO）；storage 无游标/backupRecord/fsync（记录在
   [`m3-l2-services.md`](m3-l2-services.md) §6.3）。

## 4. 环境限制（不是代码失败，需在 review 中区分）

| 项 | 现象 | 处置 |
|---|---|---|
| crates.io 不可达 | 无法新增 `wasmtime`/`cordis-rs` 等外部 crate | ADR-0010 退路；自研最小 Fiber（CPP-A）；WASM 待运行时 |
| 文件沙箱 | shell 子进程写 `docs/` 报 `EPERM`；写 workspace 外目录被拒 | 矩阵/文档用文件工具落盘；"仓库外"实验用 `.tmp/` 近似 |
| 进程限制 | `dshr-sidecar-guard` 的 `drop_terminates_the_guarded_process` 在受限会话恒失败；`node --test` 默认子进程隔离 EPERM | 记为环境项，普通桌面/CI 复验；`node --test --test-isolation=none` |
| GitHub Actions | 仓库无 CI 历史，workflow 未实跑 | 首次推送后 `workflow_dispatch` 验证；本地 `--quick`/`--workspace` 可复现 |
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

1. WASM 主方案缺失是否被 ADR-0010/ADR-0007 的退路条款正当化；是否需要在 M4 承诺中单列。
2. G2 的"仓库外"实验被 `.tmp/` 替代是否可接受；作者文档是否真的足以让第三方不读源码。
3. `Plugin` trait 仍同步、`&'static str` leak 一次、无 `tool.chunk` 消费/并发=1——对 trait 冻结与 M4 兼容的影响。
4. chat.rs 的 L2 接线已通（`--selftest-l2` phase A+B），端到端证据是否足够；官方包验收（未修改装载/方法往返）尚未做，是否接受"装载绿+方法往返"口径；`event.*` 最小桥的 TODO 是否可接受。
5. storage-vs-systemPrompt 首批服务选择（0/27 官方工具调用方 vs 14/27）；证据是否支持当前决定。
6. 环境项（sidecar-guard/CI/ACL）是否被误计为代码缺陷。
