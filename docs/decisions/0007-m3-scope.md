# ADR-0007 — M3 范围锁定与 M2 后移债务处置

- 状态：Accepted
- 日期：2026-11-12
- 关联：`PLAN.md` §8 M3 / §9 / §13、[`../review-m3-plan.md`](../review-m3-plan.md)、`spec/adc-v1.md`、`spec/cpp-v1.md`、`spec/sidecar-rpc.md`

## 背景

M2 已完成（四条 exit gate 达标，见 [`../m2-status.md`](../m2-status.md)）。M3 计划评审
（[`../review-m3-plan.md`](../review-m3-plan.md)）判定 M3 **有条件可执行**：

1. PLAN §8 M3 的 8 条清单没有可判定的"冻结"工件，也没有承载 M2 后移的 9 项债务；
2. 实现起点比计划假设低（`plugin-api` 103 行、`ui-contributions` 3 行、无 cordis 依赖、无 WIT/Wasmtime、无信任链代码）；
3. §9 没有给 WASM / Script UI / HTML Widget 任何预算行，核心项 100–168 工程日 > M3 可用的 70–105 日。

本 ADR 锁定 M3 的范围、顺序、退出条件与砍单路径，作为改 PLAN 与开工的依据。**不重写
M3 方向**（先冻契约、再放第三方插件、UI 走声明式、运行时可替换）——方向与 ADR-0004 /
ADR-010 一致，只把"做什么、什么时候停"变得可判定。

## 选项

| 选项 | 内容 | 结论 |
|---|---|---|
| A. 按 PLAN 原 8 条全做 | CPP + ADC + Script UI + WASM + L2 + 信任链 + HTML Widget + 兼容/迁移 | 否决：预算 100–168 日起步，超 M3 分配 2×；Script UI/HTML 无预算行 |
| B. 只做 ADC 一条竖切 | 冻结 + 服务端 + 渲染器，其余后移 | 否决：无法兑现 WASM 隔离、3 个 L2、外部作者三条 gate，M3 名存实亡 |
| **C. 冻结契约 + 多竖切并行** | CPP-A 运行时、ADC 冻结+服务端+渲染器、WASM、L2、最小信任链；HTML 条件化、Script UI 只 spike、MCP 进/ACP 移 M4；同步重分配预算与砍单顺序 | **采用** |

## 决定

### 1. M3 交付清单（必达）

| # | 工作项 | 交付物 | 验收（测试 ID 前缀） |
|---|---|---|---|
| 1 | **CPP-A 运行时** | `plugin-api` manifest→typed、api/WIT/`min_host_version` 版本检查、capability、错误码；`plugin-host` 接 cordis-rs（pin）后的 Fiber/Service/Effect/Config 最小切片 | `CPP-MANIFEST-*` / `CPP-VER-*` / `CPP-FIBER-*` / `CPP-SVC-*` / `CPP-EFF-*` |
| 2 | **CPP-B/C 可分发 SDK** | `dshr-plugin-api` 给 semver（非 0.0.0）、`rust-static` + `wasm-component` 两套模板、仓库外样例、作者文档 | `G2` 可复现实验 |
| 3 | **ADC-v1 冻结工件（ADC-F）** | spec 状态 `Frozen` + `schema/adc-v1/slots.v1.json`（12 slot）+ 元素 props + §12 未决清零 + 冻结 ADR | `ADC-SLOT-*` / `ADC-ELEM-*`（gate 1） |
| 4 | **ADC-v1 服务端（ADC-S）** | `ui-contributions`：12 slot、kind/scope、generation、逆序幂等撤销、注册校验与可读诊断 | `ADC-SLOT-*` |
| 5 | **ADC-v1 渲染器（ADC-R）** | `ui-gpui`：元素集 + 绑定/patch/动作/事件 + 未知元素 Placeholder + a11y + 渲染预算 | `ADC-ELEM-*` / `ADC-BIND-*` / `ADC-SEC-*` |
| 6 | **WASM Component** | `cpp-v1.wit` + Wasmtime 主机 + fuel/epoch/内存/输出/并发五维配额 + 三故障注入（超时/fuel/内存） | `CPP-WASM-*`（gate 3） |
| 7 | **DSH L2** | `sidecar-rpc` 协议 v2 host-service 代理 + 证据选出的 3 个 adapter + conformance | `RPC-L2-*`（gate 3） |
| 8 | **最小信任链** | `spec/plugin-trust.md` + manifest `integrity`/`signature`/`min_host_version` + 哈希强制 + 可选签名 + 首次安装逐项确认 + 审计 + 降级拒绝 | `CPP-TRUST-*`（gate 2） |
| 9 | **外部作者验收** | `docs/plugin-authoring.md` + 模板 + 仓库外样例 + "未读主程序源码" checklist | `G2` 可复现实验 |
| 10 | **conformance 门禁** | `tools/conformance.mjs` + 测试 ID 注册表 + 矩阵报告 + CI workflow（作为所有 gate 的证据来源） | 矩阵全绿 |

### 2. 条件项（未达标即转，不计入必达）

| 项 | 条件 | 未达标转向 |
|---|---|---|
| Script UI | 5 日 spike 通过（动态加载/卸载、错误隔离、元素树渲染三演示）→ ADR-0008 结论 | 按 PLAN 退路：只保留 Native + WebView；元素集不再对外承诺 |
| HTML Widget | 复用 ADC `workspace.panel` 的最小独立 WebView Panel（导航/下载/新窗口/IPC 来源/权限） | WebView 依赖决策未过 → 整项移 M4 |
| 插件升级/数据迁移 | 先补 `adc-v1` 插件持久化状态章节（schema 版本 + 迁移钩子 + 隔离） | 章节未落 → 移出 M3 |
| MCP | 工具发现/Schema/调用/权限映射的最小闭环，统一进 Tool Registry | 超出 12 日或与 L2 抢工 → 只保留发现+调用，OAuth/重连移 M4 |
| pnpm 捆绑 | 体积与离线 lockfile 评估通过 | 超预算 → 继续用本机 Node/pnpm 探测，捆绑移 M4 |

### 3. M2 后移债务处置（9 项 + 评审补项）

| 债务 | 证据 | 处置 |
|---|---|---|
| Loader 完整语义（`include`/group/`!!js`） | [`m2-status.md`](../m2-status.md) §剩余缺口 1、[`m2-loader.md`](../m2-loader.md) | **M3 按证据做子集**：`include`/group 展开进 M3；`!!js` 只留在 Node（Rust 不模拟），M4 再评估 |
| 安装依赖树/生命周期脚本/交互式确认 UI | 同上 2、[`m2-plugins.md`](../m2-plugins.md) | **M3 只做**首次安装确认 UI + 安装审计 + 哈希校验；npm 脚本执行策略与依赖树解析 M4 |
| 打包分发签名/更新通道/干净机器 | 同上 3、[`m2-runtime.md`](../m2-runtime.md) | **最小信任链进 M3**；完整签名证书体系、更新通道（差分/灰度/回滚）**M4** |
| 子会话异步/并行调度 + sidecar 工具继承 | [`m2-session-tree.md`](../m2-session-tree.md) | **M3**（gate 1/6 的前置） |
| 虚拟化锚点/等高校验/元素内存细分 | [`m2-ui-views.md`](../m2-ui-views.md) | **M4**（非 gate，优化项） |
| 中文 IME 人工复验 | [`issues/0001`](../issues/0001-ime-candidate-first-composition.md) | **M3 人工项**（需真人复验，不计入自动 gate） |
| 工具并行执行（DSH `maxParallelToolCalls=10`） | [`m2-dsh-defaults.md`](../m2-dsh-defaults.md) | **M3**（依赖取消接线，M2 P1-2 已修） |
| 默认差异：reasoning effort / LLM 摘要压缩对齐 / 重复调用软提醒 / `EMPTY_RESPONSE` / approval 模式切换 | [`m2-dsh-defaults.md`](../m2-dsh-defaults.md) | **M3**（配置面小，逐项对齐 DSH；不改协议） |
| Windows 签名/安装器、sidecar `timeoutMs` 透传、受限 token/AppContainer | [`m2-status.md`](../m2-status.md) §剩余缺口 9 | **`timeoutMs` 透传进 M3**；受限 token/AppContainer 与代码签名 **M4** |
| ACP Agent Adapter | `PLAN.md` §8 M2 L675 | **M4**（完整 adapter 8–15 日，收益低于 MCP/L2） |

### 4. 退出条件（替换 PLAN §8 M3 原 4 条）

| Gate | 判定 | 证据命令 |
|---|---|---|
| **G1 契约冻结** | `adc-v1.md` 状态 `Frozen`；`slots.v1.json` 与 spec §5 的 12 slot 一一对应；元素 props 字段级；§12 未决清零；未知元素 `Placeholder` + 诊断 + 不 panic，同树其余元素仍渲染 | `node schema/check.mjs` + `tools/conformance.mjs` 的 `ADC-*` 全绿 |
| **G2 外部作者可发布** | 仓库外目录**只用** `plugin-authoring.md` + 模板 + semver 的 `dshr-plugin-api` + `schema/`，构建并安装 Tool + Panel 插件；首装风险提示/审计/来源校验生效；篡改包与旧版本降级被拒 | 仓库外构建日志 + 截图 + "未读 crates/ 源码" checklist |
| **G3 隔离与兼容** | WASM 在 fuel 耗尽/内存超限/超时三例分别隔离且宿主存活、Session 无脏写；3 个 L2 service 用**未修改的官方插件**跑通 `RPC-L2-*`，并把实测耗时回填 PLAN §5.1 | `tools/conformance.mjs` 的 `CPP-WASM-*` / `RPC-L2-*` 全绿 |

### 5. 预算重分配

M3 增量按 **120–185 工程日** 执行（重估区间 115–190，逐项见 PLAN §8 M3 预算表），对应累计
240–370；M4 增量 50–100 → 累计 290–470。相对 v0.2 的 70–105 增加 45–80 日，来源是 M2 后移
债务（M3 选做 20–39 日）与 WASM/信任链/ADC 渲染器的无预算项。

**实施更新（2026-11-16，产品确认）**：时间无硬指标、不设硬上限；Beta 规划基线保留 290–470，
并按 PLAN §9.2 追加 M4 候选预算 **80–140**（若全部纳入累计 370–610）。原 240–390 上限与
下方砍单顺序**不再启用**，但"不得砍 G1–G3 判定项"仍然有效。

若必须维持原 240–390 上限，按以下顺序砍（**不得砍 G1–G3 的判定项**）：
1. HTML Widget（6–12 日）→ M4；
2. 插件升级/数据迁移（5–8 日）→ M4；
3. Script UI 实做（10–20 日）→ 只留 spike；
4. ADC 交互元素（Button 以外）→ 只读子集冻结，交互延 M4；
5. L2 减到 1 个服务（证据最强者），其余 M4。

## 后果

**好消息**：范围与 gate 一一对应；每条 gate 都有可执行命令/可复现实验；冻结工件先行使
后续实现不再返工；M2 债务有明确归属，不会出现"M3 完成但产品仍差一截"。

**坏消息**：M3 仍是 M2 的 ~1.5–2 倍工作量；信任链不引入 CA/吊销，只到"哈希强制 + 可选签名 +
首装确认"；Script UI/HTML Widget 不作为承诺；预算上修需产品确认。

## 反悔条件

- WASM 与宿主线程模型/GPUI 生命周期冲突 → `external-process` 作退路（`cpp-v1.md` §3.3），WASM 移 M4；
- ADC 渲染预算超限（典型树 draw p95 超 M1 基线）→ 只读 10 元素冻结，交互元素延 M4，**不冻假 v1**；
- 外部作者验证仍受阻 → M3 降级为"内部作者可用 + Preview 标注"，不对外承诺生态；
- 单 L2 service 实测 > 2× 预估（§12.2 规则）→ 只保留证据最强的 1 个；
- 若 cordis-rs 接入失败 → 自研最小 Fiber（§3.5 退路），CPP 范围不变。

## 验证

- `node schema/check.mjs`、`node tools/check-links.mjs`；
- `tools/conformance.mjs --quick`（契约 + 文链 + 注册表）与 `--workspace`（workspace 测试）；
- G1–G3 按 §4 的表逐条取证；每条 gate 的失败项必须写明"未达标转向"的哪一条被触发。
