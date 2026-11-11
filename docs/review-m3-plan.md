# M3「插件平台 Preview」计划评审

> 评审对象：[`PLAN.md`](PLAN.md) §8 M3（L692–709），并交叉核对 §3 CPP-v1、§4 ADC-v1、§5.1 L2、§7 安全、§9 工作量、§10 conformance、§11 风险、§13 未决决策。
> 代码/规范基线：HEAD `1ac851b`（master，工作区干净，2026-11-11）。
> 方式：只读抽查仓库现状 + 运行仓库自带校验（`node schema/check.mjs` → 20/20；`node tools/check-links.mjs`；`cargo test -p dshr-plugin-host -p dshr-plugin-api -p dshr-ui-contributions` → 6+0+0 测试全过）。未改代码/PLAN/提交，未联网。
> 标注：带「推断」的是估算或判断；其余均可按引用的 PLAN 章节 / spec / 代码路径核对。

## 0. 结论：有条件可执行

M3 的**方向**（先冻契约、再放第三方插件、UI 走声明式、运行时可替换）与 ADR-010 / §4.6 一致，**不需要重写范围**。但当前 8 条清单 + 4 条 exit gate 不能直接执行，原因有三类：

1. **"冻结"缺少工件定义**。ADC-v1 在 [`spec/adc-v1.md`](spec/adc-v1.md) 已有 285 行草案（12 个 slot、26 元素、绑定/动作/配额），但服务端 crate `ui-contributions` 只有 3 行、`element.schema.json` 的 `props` 无字段约束、`slot-registration.schema.json` 不含 `kind/scope`，且 `adc-v1 §12` 的 Q1–Q5 全部过期未决。按现状"冻结"只能冻在文档上。
2. **实现起点比 PLAN 假设低**。CPP-v1 的 `plugin-api` 只有 103 行（`crates/plugin-api/src/lib.rs:89-103`），无 inject/service/effect/event/config/capability/manifest；`plugin-host` 自述"不含 Service 依赖图与 Effect 撤销"（`crates/plugin-host/src/lib.rs:3-5`），cordis-rs 尚未进入依赖；WASM 无 WIT/wasmtime；WebView 只在 `tools/webview-probe`；信任链零代码。8 条里有 6 条是 0→1。
3. **范围有洞且工作量对不上**。M2 结束时正式后移到 M3 的 9 项（MCP/ACP、Loader 完整语义、工具并发、LLM 摘要压缩等）没有出现在 §8 M3；§9 也没有给 WASM 运行时/Script UI/HTML Widget 任何预算行。M3 增量是 `190–290 − 120–185 = 70–105` 工程日，本评审推断核心项就要 100–168 日、含 M2 债务 135–237 日（§2.2，推断）。

**条件 C1–C6**：C1 补范围（§2 末行 + §2.3 的替换草案）；C2 冻结工件与 conformance/CI 先行（§4/§6）；C3 L2 服务按证据重选（§2.1）；C4 Script UI 降为 spike（§2 第 3 条）；C5 WASM/信任链先补 spec（§8）；C6 §9 按里程碑重新分配（§2.2）。满足后可执行；不满足则按 PLAN 自己的降级路径执行并写 ADR。

## 1. 现状基线（"从现状到 M3"还差什么）

| 领域 | 现状（证据） | 对 M3 的含义 |
|---|---|---|
| CPP-v1 运行时 | `plugin-api` 只有 `Plugin{id,version,apply,call,unload}` + 工具/面板声明，`PluginContext` 无 config/service/effect/capability（`crates/plugin-api/src/lib.rs:42-103`） | §3.2/§3.3 的 8 类能力 0 实现 |
| Cordis 集成 | ADR-0002 选 `jincdream/cordis-rs`，但 `crates/plugin-host/Cargo.toml` 无该依赖、`Cargo.lock` 无 cordis 包；现有 host 是自写最小实现 | 先接入并 pin（或走 §3.5 自研退路），否则 Service/Effect 无法交付 |
| ADC 服务端 | `crates/ui-contributions/src/lib.rs` 全文 3 行；`plugin-host` 的 panel 注册无生产代码消费（`.panels(` 仅见 `crates/plugin-host/src/lib.rs:275-309` 测试） | 12 slot、kind/scope、generation、诊断全要写；"Tool + Panel" 目前只有元数据 |
| ADC 渲染 | `ui-gpui` 仅 diff/markdown/terminal/text_input/theme，无元素树渲染器；`chat.rs:519-527` 注册 panel 后未渲染 | 26 元素 × props × 绑定/动作是最大块 |
| Manifest/版本 | 只有 `schema/cpp-v1/manifest.schema.json`；无 Rust 解析器/版本检查/打包器/模板；workspace 版本 `0.0.0` | 外部作者无法依赖 `dshr-plugin-api`，SDK 需新建并给 semver |
| WASM | 无 `.wit`/`.wasm`，无 wasmtime/wasmer 依赖 | WASM 主机 + 配额测试从 0 |
| WebView | 仅 `tools/webview-probe`（独立 workspace，vendor gpui-kit/wry）；产品 crate 不依赖 gpui-kit/gpui-webview | HTML Widget 需新依赖决策 |
| 信任链 | 无验签代码（sha256 仅用于 spill/导出）；manifest 无签名/完整性/最低宿主字段；安装"不校验签名"（[`m2-plugins.md`](m2-plugins.md) L80-84） | 最小信任链从 0 |
| L2 | [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) 只有 `service.publish/invoke` 消息名（L110-111），无 host→JS 服务代理方向；sidecar 只 provide `tools`/`fs`/`systemPrompt`（`sidecar/dsh-official/sidecar-dsh.mjs:584-586`） | 需协议 v2 + 3 个 adapter |
| Conformance/CI | 仓库无 `.github`；代码无 test ID（grep `ADC-/RPC-/CPP-` 只命中注释）；现有 17 个 `chat --selftest*` | §10.1 的"CI 产物"不存在，gate 4 不可判定 |
| M2 后移债务 | 9 项已标 M3：[`m2-status.md`](m2-status.md) L187-202、[`m2-closeout.md`](m2-closeout.md) L56-63、[`m2-dsh-defaults.md`](m2-dsh-defaults.md) L177-183、[`m2-windows-launch.md`](m2-windows-launch.md) L147-157、[`spec/dsh-compat-mapping.md`](spec/dsh-compat-mapping.md) L115-124 | M3 清单未列，不处置会出现"M3 完成但产品仍差一截" |

## 2. M3 清单逐条评审

| # | 条目（PLAN L694–701） | 可判定性 | 现状差距 | 结论与建议 |
|---|---|---|---|---|
| 1 | CPP-v1 SDK、Manifest、版本检查、打包与模板 | 部分（"SDK 可用"未定义） | 0 起点（§1） | **拆分**：CPP-A 运行时（manifest→typed、api/WIT/min_host 版本检查、Fiber/Service/Effect、Config、错误码、capability）；CPP-B 可分发 SDK（`dshr-plugin-api` semver + 模板 + 示例）；CPP-C 打包（manifest+hash 清单）。验收：`CPP-MANIFEST-*`/`CPP-FIBER-*`/`CPP-SVC-*`/`CPP-EFF-*` 全绿 + 仓库外样例构建成功 |
| 2 | ADC-v1 冻结：slot 注册表 + 元素词汇表 + 兼容性测试 | 不可判定（"冻结"无工件/状态机） | spec Draft、§12 Q1–Q5 过期、schema 不完整、服务端 0 行 | **拆**：ADC-F 冻结工件（spec→Frozen + `slots.v1.json` + 元素 props + 未决清零 + ADR）；ADC-S 服务端；ADC-R 渲染器；ADC-T 兼容性测试。禁止把"实现"塞进"冻结"同一条 |
| 3 | Script UI：QuickJS/GPUI Shell 适配定案，或明确替代 | 不可判定（是决策不是交付） | PLAN M1 L641 要求"单独验证 Script UI 与 WebView"，仓库只有 WebView A3 结论（[`m1-log.md`](m1-log.md) L121-152），Script UI 无实验记录；产品不依赖 gpui-kit | **后移为 spike**：5 日时间盒 → ADR（选/不选/退路），不列为必达；若无"动态视图加载/卸载 + 错误隔离"演示，执行 PLAN 的 Native+WebView 退路 |
| 4 | WASM Component：Tool + 声明式 UI + 资源配额 | 可判定（超限隔离） | 无 wasmtime/WIT；manifest `fuel` 无 maximum（spec §3.5 上限 1e10）；fuel≠墙钟，阻塞 host call 需 epoch | **保留但后置**：先 `cpp-v1.wit` + 配额 ADR（fuel/epoch/内存/输出/并发五维），再做 Wasmtime host；三故障注入验收 |
| 5 | DSH L2：`storage`/`settings`/`fs` 三个 adapter + conformance | 可判定（矩阵全绿），但服务清单与协议方向未定 | sidecar 只有 3 个 service；`service.invoke` 无 payload；真实需求见 §2.1 | **修改**：按真实插件证据重选服务（建议 `workingDirectory` + `sessionProjections` + `storage`，推断）；先补 host→JS 代理（v2）；1–3 日/service 必须用实测回填 §5.1 |
| 6 | 最小信任链：签名（或来源校验）+ 首次安装风险提示 | 弱（"或"二义，无威胁模型） | 0 代码；无 `plugin-trust.md`（spec README L74 标 M3）；manifest 无签名字段 | **拆/改写**：先 ADR 定模型（建议签名可选 + 哈希强制 + 首次安装逐项确认 + 审计，不引入 CA 链），再写 spec，再实现验签/来源校验 + 降级拒绝；完整证书体系明确出 M3 |
| 7 | HTML Widget：隔离面板、权限、事件桥接、跨窗口生命周期 | 部分（隔离/权限可测），跨窗口过重 | 产品无 WebView 依赖；M1 A3：只能独立 Panel，GPUI 浮层不能压其上，需 `GPUI_DISABLE_DIRECT_COMPOSITION=true`（`m1-log.md` L131-152） | **拆分并设依赖**：先复用 ADC `workspace.panel` 做"独立 WebView Panel"最小闭环（导航/下载/新窗口/IPC 来源/权限禁止项），跨窗口生命周期后移；WebView 依赖决策未过则整项后移 M4 |
| 8 | UI 扩展兼容性测试；插件升级/恢复/数据迁移 | 兼容测试可判定；迁移无 spec 载体 | `adc-v1` 无插件持久化状态章节（仅 client-local state，L248）；PLAN §4.4 的"schema 版本+迁移钩子"未进 spec | **修改**：compat 测试保留并定义矩阵；迁移先补 `adc-v1` 插件状态章节，否则移出 M3 |

**缺项（C1）**：MCP/ACP 在 PLAN L675 明写"后移 M3"但 M3 清单没有；Loader `include`/group/`!!js`、工具并发、LLM 摘要压缩、reasoning effort、approval 模式切换、重复调用提醒、子会话并行/sidecar 工具继承、受限 token/AppContainer、pnpm 捆绑、per-tool timeout 透传也未列。建议在 §8 M3 增"M3 债务清单"表逐项标 M3/M4；本评审倾向 MCP 工具客户端进 M3、ACP 与受限 token 移 M4（推断）。

### 2.1 L2 服务选型的仓库证据（C3）

DSH 0.2.2-rc.1 的抽取物（`.tmp/dsh-tools-all` 等，gitignored）里有 27 个官方工具包。显式 `export const inject` 的服务集合是：`tools` / `fs` / `agents` / `workingDirectory` / `cordisInspect` / `sessionProjections` / `workflowEngine` / `systemPrompt` / `ptcRuntime`（present、todo、workflow、cordis、executor-ptc）；**没有工具包注入 `storage` 或 `settings`**，这两个在已抽取的包里只出现在 `dsh-client-ui-*` 的依赖里（`.tmp/dsh-ui`）。`dsh-tool-bash/package.json` 的 peerDependencies 还要求 `dsh-sandbox` / `dsh-sandbox-policy` / `dsh-user-approval` / `dsh-working-directory` / `dsh-jobs` / `dsh-system-prompt`，即解锁 bash/pwsh 要 5+ 服务。

因此 §5.1 "storage / settings / fs" 的首批选择与工具插件的一线需求错位：`fs` 已有 M2 的专用写裁决路径（`crates/compat-dsh-fs`，是 `fs.write.request` 而非 DSH `fs` 服务适配），`settings` 主要服务 Client 插件（L4/M5），`storage` 对第三方插件合理但当前官方工具无用例。建议把选择改成"证据驱动"（推断：`workingDirectory` + `sessionProjections` + `storage`），并在 `docs/m3-l2-services.md` 里逐服务记录真实插件、契约、实测成本。

### 2.2 工作量重估（推断；与 §9 的预算行对照）

| M3 工作项 | 工程日 | §9 现有预算行 |
|---|---:|---|
| CPP-A 运行时（manifest/版本/cordis 接入/Fiber/Service/Effect/Config/错误码） | 20–30 | Plugin SDK 25–45（已含 ADC 设计） |
| CPP-B/C SDK 发布、模板、示例、作者文档 | 3–6 | 同上 |
| ADC-F 冻结工件（spec/`slots.v1.json`/元素 props/未决清零） | 4–7 | 同上 |
| ADC-S 服务端（12 slot/generation/校验/诊断） | 8–12 | 同上 |
| ADC-R 渲染器（26 元素 + 绑定/动作 + a11y + Placeholder + 预算） | 20–30 | 同上（与设计共用 25–45，明显不够） |
| Script UI（若 spike 通过且实做） | 10–20 | **无预算行** |
| WASM 主机（WIT + Wasmtime + 配额） | 10–18 | **无预算行** |
| L2（协议 v2 + 3 adapter + conformance） | 8–15 | Node DSH 25–45 的 L2 部分（约 3–9） |
| 信任链（spec + 验签 + 安装 UI + 审计） | 6–10 | 打包/安全 32–55（属 M4 三平台项） |
| HTML Widget（独立 WebView Panel） | 6–12 | **无预算行** |
| 升级/迁移/兼容性测试 | 5–8 | 未知未知项 25–40（不得挪用） |
| **核心合计** | **100–168** | M3 可用 70–105 |
| M2 后移债务（MCP 8–15、ACP 8–15、工具并发 3–5、Loader 3–6、LLM 压缩 2–4、默认差异 2–5、子会话并行 3–6、受限 token 3–6、pnpm 2–5、timeout 1–2） | 35–69 | 未分配 |
| **含债务合计** | **135–237** | — |

结论：**即使 M2 债务全部后移，核心项 100–168 仍超过 M3 的 70–105**；§9 又没有任何一行承载 WASM/Script UI/HTML Widget。执行前必须重分配 §9 或缩小 M3（建议按 §4 的"未达标转向"预先定好砍哪部分）。

### 2.3 PLAN §8 M3 建议替换文本（草案，可直接用于改 PLAN）

```text
- [ ] CPP-v1 运行时：manifest→typed、api/WIT/min_host 版本检查、Fiber/Service/Effect/Config/
      错误码/capability；可分发 dshr-plugin-api（semver）+ rust-static/wasm-component 两套模板。
- [ ] ADC-v1 冻结工件：spec 状态 Frozen + schema/adc-v1/slots.v1.json + 元素 props +
      §12 未决清零 + 冻结 ADR（不把服务端/渲染器算进"冻结"）。
- [ ] ADC-v1 服务端 ui-contributions：12 slot、kind/scope、generation、撤销幂等、可读诊断。
- [ ] ADC-v1 GPUI 渲染器：26 元素 + 绑定/动作/事件 + Placeholder + a11y + 渲染预算测试。
- [ ] Script UI：5 日 spike + ADR（选/不选/退路），不作 M3 必达项。
- [ ] WASM Component：cpp-v1.wit + Wasmtime + fuel/epoch/内存/输出/并发配额 + 三故障注入。
- [ ] DSH L2：sidecar-rpc v2 host-service 代理 + 证据选出的 3 个 adapter + conformance。
- [ ] 最小信任链：plugin-trust.md + 签名/来源校验 + 首次安装逐项确认 + 审计 + 降级拒绝。
- [ ] HTML Widget：独立 WebView Panel（导航/下载/新窗口/IPC 来源/权限）+ 生命周期；
      WebView 依赖决策未过则整项后移 M4。
- [ ] 外部作者验收：plugin-authoring.md + 模板 + 仓库外样例 + "未读源码"checklist；
      插件升级/迁移（先补 adc-v1 插件状态章节，否则移出 M3）。
- [ ] M3 债务清单：把 M2 后移项逐条标 M3/M4（MCP 进、ACP/受限 token 建议 M4）。
```

## 3. 依赖图与建议顺序

```text
[schema/cpp-v1 manifest] ─► CPP-A 运行时核心（manifest/版本/lifecycle/service/effect/config）
cordis-rs 接入（ADR-0002 未落地）─┘        │
                                          ├─► CPP-B 可分发 SDK + 模板/示例 ─► G2（外部作者）
                                          └─► CPP-W WASM（WIT+Wasmtime+配额）

[adc-v1 spec Draft] ─► ADC-F 冻结工件 ─► ADC-S ui-contributions ─► ADC-R ui-gpui 渲染器
                                                                   ├─► Script UI（spike 通过才做）
                                                                   └─► HTML Widget（复用 panel slot）
[sidecar-rpc v1] ─► L2-P 协议 v2（host service proxy）─► L2-A 三个 adapter ─► L2-T conformance
[manifest schema] ─► TRUST（spec→签名格式→验签→安装 UI→回滚/降级拒绝）
[CI + test ID 骨架] ─────────────── 横切前置：所有 gate 的证据来源（当前不存在）
```

关键边：ADC-F 是 ADC-S/ADC-R/Script UI/兼容性测试的硬前置；CPP-A 是 WASM 与 SDK 的硬前置；L2 与信任链可独立并行；HTML Widget 必须等 ADC-S 的 panel 路径先通。**建议并行**：ADC-R（大头）与 L2/信任链两条线并行；Script UI spike 放在 W1–W2 先判死，避免拖到后期。

## 4. exit gate 修订建议（3 条，均可机检）

**G1 契约冻结（ADC）**：`spec/adc-v1.md` 状态按 [`spec/README.md`](spec/README.md) §1 流程改为 `Frozen`；`schema/adc-v1/slots.v1.json` 与 spec §5 的 12 个 slot 一一对应（kind/scope/注册契约/替换风险/key 域）；`element.schema.json` 对交互/数据类元素有字段级 props；§12 未决清零。`node schema/check.mjs` + 新增 `tools/adc-conformance.mjs` 全绿，覆盖 `ADC-ELEM-*`/`ADC-SLOT-*`/`ADC-BIND-*`/`ADC-SEC-*`；其中"未知元素"断言由 Rust 解析器单测保证：未知 type → Placeholder + 诊断 + 不 panic + 同树其余元素仍渲染。

**G2 外部作者可发布（可复现实验）**：仓库外目录只用 `docs/plugin-authoring.md` + `templates/` + 有 semver 的 `dshr-plugin-api` + `schema/` 构建 Tool + Panel 插件；经安装路径（首次风险提示 UI + 审计 + 签名/来源校验）后可见、可调用、可卸载；产出"作者未读 crates/ 源码"checklist + 命令日志/截图；反例：篡改包被拒、旧版本降级被拒（`min_host_version`）。

**G3 隔离与兼容（可机检）**：WASM 插件在 fuel 耗尽/内存超限/超时三例分别被隔离，宿主存活、Session 无脏写，`CPP-WASM-*` 全绿；三个 L2 service 用**未修改的官方插件**跑通 `RPC-L2-*`，并把每个 service 实际耗时回填 §5.1。

**未达标转向**：G1 若渲染成本超预算（§5）→ 元素集缩为只读子集（Text/Markdown/Code/Badge/Stack/Row/Grid/Divider/Spacer/List/Table/KeyValue/Card），交互元素延后 M4，**不冻结假的 v1**；G2 若外部作者仍受阻 → M3 降级为"内部作者可用 + Preview 标注"，不对外承诺生态；G3 若单 service 成本 >2× 预估（沿用 §12.2 的 2× 规则）→ 只保留证据最强的 1 个，其余 M4；WASM 若与线程模型冲突 → `external-process`（§3.3）作退路。Script UI 未通过 spike 则按 PLAN 现有退路执行，但**必须写 ADR 说明元素集不再对外承诺**。

## 5. 风险与降级方案

| 风险 | 证据 | 降级/控制 |
|---|---|---|
| ADC 渲染性能 | M1 A1c：单行 4 元素 draw p50 15.15ms vs 单文本 6.28ms，建议"单行 ≤3 文本段"（`m1-log.md` L82-100）；adc-v1 §10 只限节点数/深度 | 加渲染预算测试（典型树 10 例的 draw p95 + missed_refresh）；超限按 G1 转向砍元素 |
| Script UI | 上游实验路径（PLAN §14）；产品 crate 不依赖 gpui-kit，`gpui-shell` 在仓库内 grep 无命中（只在 `tools/webview-probe` 间接 vendor 了 gpui-kit 的 WebView 示例） | 5 日 spike + ADR；默认退路 Native+WebView |
| WASM 配额 | fuel 只计指令数，阻塞式 host 调用需 epoch；输出/并发也需上限（cpp-v1 §3.5 有数，manifest schema 未全约束） | 配额 ADR + 三故障注入；`external-process` 退路 |
| L2 成本 | `fs.write` 已暴露真实成本：错误码词表映射、version 格式、`sandboxMode` 语义（`sidecar-dsh.mjs` L137-157/262-269）；`dsh-tool-bash` peerDeps 需 sandbox/user-approval/working-directory/jobs 等 5+ 服务（`.tmp/dsh-tools-all/dsh-tool-bash/package.json`） | 证据驱动选服务；>2× 预估即缩减（§12.2） |
| 信任链 | 无密钥管理/吊销/回滚；安装仅 CLI 确认 | 先"哈希强制 + 可选签名 + 首次确认"；降级拒绝用 `min_host_version` |
| HTML Widget | GPUI 浮层不能压 WebView；DPI/焦点未验证（`m1-log.md` L139-152） | 只做独立 Panel；不做卡片内嵌（与 §4.2 D 一致） |
| SDK 可用性 | `plugin-api` 0.0.0；`fn call(&mut self)` 同步，无法取消/并发 | 冻结前改 async + CancellationToken + `ToolResult{content,value,meta,is_error}`，否则 M3 工具并发仍做不了 |
| 版本兼容 | spec 写 0.2.1-rc.3；`compat-dsh-protocol` 的 `hello()` 用 0.2.1-rc.3 而 `hello_with_caps()` 用 0.2.2-rc.1（L288 vs L149） | 单一事实源 + 里程碑升级窗口（ADR-0003 已有规则） |

## 6. 测试/conformance 缺口

现状：无 CI、无 test ID、无矩阵报告；schema 20 例；`plugin-host` 6 例、`plugin-api` 0 例、`ui-contributions` 0 例。补齐顺序与断言：

- `ADC-SLOT-*`：12 slot 每项的 kind/scope/契约；single 覆盖/拒绝；list `order` 稳定；keyed 闭合 key 拒绝；generation 失效撤销；逆序 + 幂等撤销；卸载超时强制撤销。
- `ADC-ELEM-*`：26 元素渲染快照逐个；**未知 type → Placeholder + 诊断 + 不 panic**（gate 2 的核心）；叶子带 `children` 拒绝；未知 props 忽略 + 诊断；深度/节点数/文本/图片配额；`a11yLabel` 缺失拒绝；Markdown 受控子集不执行 HTML。
- `ADC-BIND-*`：绑定前缀权限；未授权返回 null + 诊断；patch `seq` 乱序丢弃；>256 条改快照；渲染纯函数。
- `ADC-SEC-*`：`openExternal` 仅 https，`file:`/`javascript:` 拒绝；跨插件绑定拒绝；元素不触发网络。
- `CPP-MANIFEST-*`/`CPP-VER-*`：未知字段拒绝、api 不匹配、WIT 不匹配、`min_host` 拒绝、fuel/内存越界拒绝。
- `CPP-FIBER-*`/`CPP-SVC-*`/`CPP-EFF-*`：状态机、依赖重启、Effect LIFO/幂等/超时。
- `CPP-WASM-*`：fuel/内存/超时/输出超限各自隔离；宿主存活；Session 无脏写。
- `CPP-TRUST-*`：篡改包拒绝、未知来源提示、降级/回滚拒绝、审计字段。
- `RPC-L2-*`：每 service 用真实官方插件往返、取消/超时/崩溃、耗时回填。

**如何证明"外部作者不读主程序源码"**：把口号变成实验：① `dshr-plugin-api` 打包/发布（semver，非 0.0.0）+ rust-static/wasm-component 两套模板；② `docs/plugin-authoring.md` + 示例只引用 `docs/spec` 与 `schema/`；③ 仓库外目录按文档构建、安装（含风险确认 UI）、注册 Tool + Panel、调用、卸载；④ 记录"未读 crates/ 源码"checklist + 日志/截图，反例（篡改/缺字段）被拒。缺 ①–④ 时该 gate 永远不可判定。CI 侧至少加一个 workflow（或 `tools/conformance.mjs` 本地门禁）把 test ID 聚合成矩阵报告，兑现 §10.1。

## 7. 前两周行动清单（按依赖排序，产物可直接落盘）

| 时间 | 产物 | 验收 |
|---|---|---|
| W1D1–2 | `docs/decisions/0007-m3-scope.md`：M2 债务逐项处置（M3/M4）；`0008-script-ui-spike.md`：时间盒与判定标准 | 范围锁定，§8 M3 的 PLAN patch 有依据 |
| W1D1–3 | `tools/conformance.mjs` + CI workflow + 测试 ID 规范复用 `spec/README.md` §3；首个矩阵报告（现有 17 selftest + workspace tests 纳入） | 任一 gate 都有命令可复现 |
| W1D2–4 | `schema/adc-v1/slots.v1.json`（12 slot：kind/scope/契约/替换风险/key 域）；`slot-registration.schema.json` 对齐；`element.schema.json` 补交互/数据元素 props；`schema/check.mjs` 反面用例 | 冻结工件可机检 |
| W1D3–5 | `spec/adc-v1.md` 关闭 §12 Q1–Q5、状态改 `Review`；`spec/sidecar-rpc.md` 补 v2 host-service 代理（kinds/payload/错误码/流式）；envelope schema 与 `compat-dsh-protocol` 类型跟进 | 两份契约可进入实现 |
| W1D4–W2D4 | `plugin-host` 接 cordis-rs（pin/vendor）+ Fiber/Service/Effect 最小切片；`plugin-api` 或新 crate 加 typed Manifest/版本检查/capability | `CPP-FIBER-*` 首批绿 |
| W2D1–3 | `plugin-api` trait 冻结前评审：async `call` + `CancellationToken` + `ToolResult`；迁移 `chat.rs:504-560` 内联插件 | 改动面可控、无 M3 后返工 |
| W2D1–3 | Script UI 5 日 spike（独立探针目录）→ ADR 结论（选/不选/退路） | 高不确定性提前判死 |
| W2D2–4 | `spec/plugin-trust.md` 草案 + 信任 ADR；`manifest.schema.json` 增 `integrity`/`signature`/`min_host_version` 并修 `fuel.maximum`；`profile::install` 验签钩子 | 信任链可开工 |
| W2D3–4 | `docs/m3-l2-services.md`：真实插件 `inject`/peerDeps 证据 + 每服务契约与成本 → 定首批三个 | L2 不再凭假设选型 |
| W2D5 | 评审 W2 产物，**由实施方**提交 PLAN §8/§9/§10/§13 的修订 patch（含 §2.3 替换草案） | M3 启动前置就绪 |

## 8. 需要现在补的 ADR/规格

| 文件 | 必须回答的问题 | 截止 |
|---|---|---|
| ADR-0007 `m3-scope` | M2 债务逐项 M3/M4；MCP/ACP 去留；Script UI 是否必达 | W1D2 |
| ADR-0008 `script-ui` | 运行时选型（gpui-shell/QuickJS/自建）、与 raw gpui 的适配层、失败判定 | W2D3 |
| ADR-0009 `plugin-trust` | 签名可选/必选、来源与密钥、降级/回滚、Windows 证书是否 M4 | W2D4 |
| ADR-0010 `wasm-quota` | fuel/epoch/内存/输出/并发五维、阻塞调用、超限后的用户可见行为 | 开工 WASM 前 |
| `spec/plugin-trust.md` | manifest 签名载荷（canonical JSON/哈希）、验签时序、安装 UI 文案与审计字段 | W2D4 |
| `spec/adc-v1.md` 补章 | §12 清零 + 新增"插件持久化状态"（schema 版本/迁移/隔离） | 冻结前 |
| `spec/cpp-v1.wit` | WIT 世界定义（tool/effect/log/config + 配额导入） | WASM 前 |
| `spec/sidecar-rpc.md` v2 | host-service 代理方向、服务名单/方法表、错误码 | L2 前 |

## 9. 未验证与需要产品决策的问题

- **外部作者信任门槛**：不签名是否可接受？这直接决定"最小信任链"范围与 Windows 证书是否推迟 M4。
- **MCP/ACP 是否 M3**：ACP 完整 adapter 成本高（推断 8–15 日），建议 MCP 进、ACP 移 M4。
- **Script UI 与 HTML Widget 是否二选一**：两者都做会挤掉 ADC 渲染器；建议 HTML Widget 只做独立 Panel、Script UI 只 spike。
- **DSH 版本锁哪个**：0.2.1-rc.3（ADR-0003 首版）还是 0.2.2-rc.1（当前安装/`hello_with_caps`）？不统一则"版本检查"无法验收。
- **L2 价值排序**：解锁 bash/pwsh 要 5+ 服务（`dsh-tool-bash/package.json` peerDeps），是否值得 vs 先做 storage+workingDirectory 两个低成本服务。
- **是否公开发布 `dshr-plugin-api`**：不发布则"外部作者"只是内部团队。
- **ADC 元素集是否接受缩水**：渲染预算不达标时是否接受只读 10 元素（影响冻结承诺）。

## 10. 文档一致性问题（顺手修，不阻塞 M3）

1. `PLAN.md` §13 L878 仍写"Cordis 选择：dshbox 优先"，与 §3.5/ADR-0002 冲突。
2. `PLAN.md` §11 L829 说更新通道"提前到 M3"，与 §8 M4 L713 冲突；M3 清单只有最小信任链。
3. `PLAN.md` §8 M2 L675 写 MCP/ACP"按转向后移 M3"，M3 清单未列（见 §2 缺项）。
4. [`spec/README.md`](spec/README.md) L11-12 把 sidecar-rpc/session-log 冻结时点写 M2，但状态仍是 Draft；M3 的"冻结"应走同一状态机（Draft→Review→Frozen）。
5. `adc-v1.md` L281-285、`cpp-v1.md` L240-245、`sidecar-rpc.md` L277-282 的未决问题时点（M2/M1）已过。
6. `cpp-v1.md` L77 "缺省值见 §3.6"，但该文件没有 §3.6（默认值实际在 §3.5）。
7. `schema/cpp-v1/manifest.schema.json` 的 `fuel` 无 maximum（spec 上限 1e10）；`uiEntry` 缺 `location`，而 `adc-v1` §5 的 `workspace.panel` 契约含 `location`。
8. `docs/decisions/README.md` L35 把 0003 标 Proposed，文件头是 Accepted；ADR-0001 声称 `_probe/gpui-probe` "随仓库保留"，当前工作区不存在（`.gitignore` 已忽略）。
9. `schema/check.mjs` 把"未知元素"判为 schema invalid，而 spec P4 要求运行时降级为 Placeholder——两件事要分开：schema 作**作者期 lint**（严格），运行时解析器另做**前向兼容**（宽松 + 诊断）。

## 11. 复现命令（本次评审实际执行）

```powershell
git log --oneline -3; git status --porcelain
node schema/check.mjs
node tools/check-links.mjs
tools\build-env.cmd cargo test --offline -p dshr-plugin-host -p dshr-plugin-api -p dshr-ui-contributions
```

引用事实均可用 `Select-String`/编辑器定位到对应文件行；未重跑 `cargo test --workspace`（M2 测试状态以 [`m2-status.md`](m2-status.md) 为准，避免与 M2 实施评审重复）。
