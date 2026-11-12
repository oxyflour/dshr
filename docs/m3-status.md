# M3 状态 — 插件平台 Preview

> 更新：2026-11-12（M3 起步批次 W1）。范围、gate 与砍单顺序见
> [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)；计划基线见 [`PLAN.md`](PLAN.md) §8 M3 / §9.1；
> 计划评审见 [`review-m3-plan.md`](review-m3-plan.md)。
> 本文是**活文档**：每完成一个 gate 项就更新证据命令与结果，不写"计划中的证据"。

## 1. 当前结论

M3 已从"计划评审"进入"契约冻结 + 首个实现切片"，**G1–G3 尚未达标**（见 §2）。
起步批次的产出把四条前置从 0 变成可执行：①范围与预算锁定（ADR-0007/0008 + PLAN v0.3）；
②ADC-v1 冻结工件可机检（`slots.v1.json` + 26 元素 props + 57 条 schema 用例）；
③conformance 门禁（测试 ID 注册表 + 矩阵 + CI workflow）；④CPP typed manifest 与 29 个契约测试；
⑤sidecar-rpc v2 host-service 代理与证据驱动的 L2 选型。

## 2. Exit gate 对账

| Gate | 判定 | 当前状态 | 缺口（下一步） |
|---|---|---|---|
| **G1 契约冻结** | ADC `Frozen`；12 slot 一一对应；元素 props 字段级；§12 清零；未知元素 Placeholder + 诊断 + 不 panic | **部分**：spec 为 `Review`；冻结工件齐（`schema/adc-v1/slots.v1.json`、`element.schema.json` 26 元素、`slot-registration.schema.json`、§12 Q1–Q5 已关闭）；Placeholder/降级只有 spec + schema 用例，**运行时解析器未实现** | `ui-contributions` 未知元素 → Placeholder + 诊断（`ADC-ELEM-*`）；冻结 ADR；spec 状态改 `Frozen` |
| **G2 外部作者可发布** | 仓库外只用文档+模板+semver SDK 构建并安装 Tool+Panel；首装确认/审计/来源校验；篡改/降级被拒 | **未开始**：`plugin-api` 仍 0.0.0；模板/作者文档/安装 UI/信任链未做 | `dshr-plugin-api` semver、模板、`plugin-authoring.md`、`plugin-trust.md` + 验签钩子 |
| **G3 隔离与兼容** | WASM 三故障注入隔离且宿主存活；3 个 L2 service 用未修改官方插件跑通 | **未开始**：无 WIT/Wasmtime；L2 只有协议 v2 草案 | `cpp-v1.wit` + Wasmtime + 配额；3 个 adapter + `RPC-L2-*` |

## 3. 起步批次（W1）交付与证据

| 交付 | 文件 | 证据 |
|---|---|---|
| 范围/预算/gate 锁定 | [`decisions/0007-m3-scope.md`](decisions/0007-m3-scope.md)、[`PLAN.md`](PLAN.md) §8 M3 + §9.1 | 3 条 gate 可判定；M2 债务逐项 M3/M4；砍单顺序明确 |
| Script UI 判死时间盒 | [`decisions/0008-script-ui-spike.md`](decisions/0008-script-ui-spike.md) | 5 日、三演示、退路；不作必达 |
| ADC-v1 冻结工件 | `schema/adc-v1/slots.v1.json`（12 slot）、`element.schema.json`（26 元素字段级 props）、`slot-registration.schema.json`（4 种载荷形状） | `node schema/check.mjs` → **65/65**；spec §5 12/12、§6 26 元素逐字段一致 |
| ADC spec 未决清零 | [`spec/adc-v1.md`](spec/adc-v1.md)（`Review`；§5.1 目录唯一事实源；§8.1 插件持久化状态；§12 Q1–Q5 关闭） | 状态机 `Draft → Review`；Q5 WebView 明确排除（v1/v2） |
| conformance 门禁 | `tools/conformance.mjs`、`docs/conformance.d/registry.json`（57 ID：25 implemented / 31 pending / 1 manual）、[`conformance.md`](conformance.md)、[`conformance-matrix.md`](conformance-matrix.md)、`.github/workflows/conformance.yml` | `--quick` 全绿（schema+links+注册表）；`--workspace` 见 §4；CI **未在 GitHub Actions 实跑** |
| CPP typed manifest | `crates/plugin-api/src/manifest.rs` + `tests/manifest.rs`；`schema/cpp-v1/manifest.schema.json`（fuel 上限、`min_host_version`、`ui[].location`） | `cargo test -p dshr-plugin-api` → **29 passed**；`CPP-MANIFEST-001..022` / `CPP-VER-001..007` |
| sidecar-rpc v2 / L2 选型 | [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md)（host→JS service proxy）、[`m3-l2-services.md`](m3-l2-services.md) | 见 §5（l2-contract 报告） |
| M2 债务处置 | ADR-0007 §3 表（9 项 + ACP） | 每项标 M3/M4 与理由；PLAN §8 M3 只列 M3 选做 6 项 |

## 4. Conformance 现状

- **契约**：`node schema/check.mjs` → 65/65（原 20 条不回归；新增 slots 目录自检、元素 props 正反例、
  注册 kind 差异、L2 v2 envelope 与 CPP manifest 新字段）。
- **文档互链**：`node tools/check-links.mjs` → 228 链接 0 broken。
- **Rust 单测**：`tools\build-env.cmd cargo test --offline --workspace` → **26 suites / 316 passed / 1 failed**。
  唯一失败：`dshr-sidecar-guard process::tests::drop_terminates_the_guarded_process`
  （本会话沙箱的进程创建/终止限制导致；隔离复跑同样失败，非本轮改动引入，需在正常桌面会话复验）。
- **矩阵**：`docs/conformance-matrix.json` / `.md` 由 `tools/conformance.mjs --quick` 生成；
  57 个 ID 中 `implemented 25 / pending 31 / manual 1`，pending 全部是 ADC/CPP/RPC 的运行时项
  （占位不失败），`manual` 为 G2 外部作者实验。
- **CI**：`.github/workflows/conformance.yml` 已定义（Windows：schema + links + cargo + 矩阵 artifact），
  **未在 GitHub Actions 实跑**，本机证据以上面命令为准。

## 5. L2 选型（摘要）

详见 [`m3-l2-services.md`](m3-l2-services.md)。证据：27 个官方工具包的 `inject` 频次
（`workingDirectory` 10、`sessionProjections` 2、`storage` 0、`settings` 0）与 DSH 0.2.2-rc.1
的 `SERVICE_API`（**107 项**，v0.2 写 98）。首批三个：`working-directory/v1`（1.5–2.5 日）、
`session-log/v1` + `sessionProjections`（2.5–3.5 日）、`storage-kv/v1`（2–3 日），合计 9.5–14 日，
落在 L2 预算 8–15 日内。协议 v2 是**向后兼容增补**：新增 `service_key` 可选字段与
`host.service.*` / `service.cancel` 四个 kind；`event.*` 通道是 L2 的顺序前置（v1 未实现）。
**关键风险**：24/27 官方工具的 `execute` 读 `exec.agent`——服务到位 ≠ 工具可执行，G3 的 L2
判定已按"装载绿 + 方法往返 + 超时/取消/参数错误"收口，工具 `execute` 级闭环不进 M3。

## 6. 未决与风险

| 项 | 影响 | 处理 |
|---|---|---|
| Beta 对外口径 290–470（原 240–390） | 产品/商业承诺 | **待产品确认**；若维持原上限按 ADR-0007 §5 砍单（不得砍 G1–G3） |
| `menu.item` 闭合 key 域的具体菜单名 | ADC 注册校验 | 目录标 `source: "host"`，待 Host 菜单目录回填；不擅造值 |
| `dshr-sidecar-guard` 进程测试在受限会话失败 | workspace 测试 1/317 | 记录为环境项；在正常桌面会话复验；不阻塞 quick gate |
| `plugin-api` 版本仍 0.0.0 | `min_host_version` 校验基准不可用；G2 不成立 | W2 起给 SDK semver，并让 `Manifest::validate` 用宿主真实版本 |
| GitHub Actions 未跑 | CI 证据缺口 | 首次推送后补跑；矩阵可由 `--quick` 本地复现 |
| Script UI / HTML Widget 未验证 | 条件项 | spike 5 日（ADR-0008）；HTML Widget 依赖 WebView 决策 |

## 7. 下一步（W2 起，按依赖排序）

1. `ui-contributions`（ADC-S）：12 slot、kind/scope、generation、逆序幂等撤销、未知 slot/契约诊断；
   冻结 ADR 后把 `adc-v1.md` 改 `Frozen`（G1）。
2. `ui-gpui`（ADC-R）：元素渲染器 + 未知元素 Placeholder + a11y + 渲染预算（G1）。
3. `plugin-host` 接 cordis-rs（pin/vendor）+ Fiber/Service/Effect 最小切片（`CPP-FIBER-*`）。
4. `plugin-api` trait 冻结前评审（async `call` + CancellationToken + `ToolResult`），迁移 `chat.rs` 内联插件。
5. WASM：`cpp-v1.wit` + 配额 ADR-0010 + Wasmtime 主机 + 三故障注入（G3）。
6. 信任链：`plugin-trust.md` + ADR-0009 + manifest `integrity`/`signature` + 安装确认/审计（G2）。
7. L2：按 [`m3-l2-services.md`](m3-l2-services.md) 实现首批 adapter + `RPC-L2-*`（G3）。
8. 外部作者：`dshr-plugin-api` semver + 两套模板 + `plugin-authoring.md`（G2）。
9. Script UI 5 日 spike（ADR-0008）；工具并行 + 子会话并行 + 默认差异对齐（M3 债务）。

## 8. 复现命令

```powershell
node schema/check.mjs
node tools/check-links.mjs
node tools/conformance.mjs --quick
node tools/conformance.mjs --quick --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md
tools\build-env.cmd cargo test --offline -p dshr-plugin-api
tools\build-env.cmd cargo test --offline --workspace
```
