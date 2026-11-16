# Native Agent Desktop — 项目实施计划

> **定位**：基于 GPUI 的原生 Agent Desktop 平台，Rust 是唯一权威应用运行时；Rust Native Plugin 为一等公民；通过 Node.js Sidecar 兼容 DSH Host 插件；通过独立 Client Extension API 构建长期 UI 生态。
>
> **状态**：Draft v0.2 · 2026-11-08（设计规范见 [`spec/`](spec/README.md)，决策记录见 [`decisions/`](decisions/README.md)）
> **目标平台**：Windows 优先，macOS / Linux 分阶段支持
> **工作量口径**：熟练 Rust/TypeScript/桌面工程师的工程日（8 小时），单人等效，非日历天数；预估值须经 M1 实测校准。

---

## 0. 一页摘要

### 0.1 产品目标

构建一套能够长期独立演进的 **Native Agent Desktop Platform**，而非 DSH Desktop 的界面复刻：

1. **Rust-first**：Agent Loop、Session、Tool Runtime、权限、插件生命周期与桌面业务状态由 Rust 管理。
2. **GPUI-native**：会话、工作区、Dock、终端、Diff、工具卡片和专业工具面板采用原生 GPUI 组件。
3. **Cordis-inspired**：沿用 DSH/Cordis 易理解的 `plugin / inject / service / event / effect` 心智模型，但定义比 DSH 更严格、更稳定的 Rust 插件子集。
4. **Compatibility, not captivity**：普通 DSH JavaScript Host 插件通过 Node Sidecar 使用；兼容层不得成为 Rust Core 的前置依赖。
5. **Client API 独立**：第三方 UI 插件依赖本平台的稳定 Client Extension API，而不是依赖 GPUI 内部类型。支持 Rust Native UI、脚本声明式 UI、隔离 WebView 三类呈现方式。
6. **ACP/MCP 原生集成**：ACP 用于接入外部 Agent，MCP 用于外部工具/服务；不强制所有能力包装为 DSH 插件。

### 0.2 核心架构决策

| ID | 决策 | 理由 |
|---|---|---|
| ADR-001 | **Rust Agent Runtime 是唯一主运行时** | 防止 Rust/Node 双 Agent Loop、双 Session 与双权限系统 |
| ADR-002 | **Rust Native Plugin 是产品规范，DSH 是兼容规范** | 长期技术主动权、降低 DSH 版本锁定 |
| ADR-003 | **插件公开 API 与 GPUI/Cordis 具体实现解耦** | 控制 ABI、框架升级与插件分发风险 |
| ADR-004 | **官方 DSH JS 插件在原版 Node Cordis 中执行** | 不重写 JS Proxy、Node/npm、Loader、Schemastery 等行为 |
| ADR-005 | **跨进程仅桥接声明过的 Service/Hook** | 避免任意 JS 对象和 Context 在两个 Runtime 之间透明代理 |
| ADR-006 | **GPUI 原生 UI 优先，HTML 作为有边界的兼容容器** | 保持原生交互体验，避免 WebView 变成全局 UI 依赖 |
| ADR-007 | **外部可安装插件优先选择 WASM/独立进程** | Rust ABI 不稳定，直接加载任意 `cdylib` 存在崩溃与安全风险 |
| ADR-008 | **Pinned dependencies + conformance tests** | GPUI 和 DSH 均在演进，必须锁定确切 commit/版本 |
| ADR-009 | **不走"Web 壳 + 现有 Web Client"的捷径** | 产品差异在原生交互与长期插件平台，不在把浏览器装进窗口；代价是 UI 核心必须自建，已计入估算 |
| ADR-010 | **ADC-v1 是 slot/factory 注册表，不是组件类型清单** | 组件类型无法覆盖真实挂载点，且会让 DSH 插件移植变成重写 |
| ADR-011 | **上下文工程是 Agent Core 的一等能力，不是 UI 优化** | 没有计量/压缩/裁剪/外部化，长会话不可用，事故无法回放 |

### 0.3 交付顺序

- **M0 — 基线与选型**（累计 10–15 工程日）：固定 GPUI / cordis-rs / DSH 版本，落盘三份 schema 与 ADR。
- **M1 — 垂直切片 PoC**（累计 30–50）：三个"判死实验"——GPUI 能否承载、cordis-rs 能否用、Sidecar 的真实单位成本。
- **M2 — Windows Alpha**（累计 120–185）：真实 Agent 工作流、上下文工程、原生 UI、工具审批、L1 DSH 兼容。
- **M3 — 插件平台 Preview**（累计 240–370）：CPP-v1 SDK、ADC-v1 冻结、WASM 插件、L2 首批 Service、最小签名。
- **M4 — 跨平台 Beta**（累计 290–470，另加 review 后追加候选 80–140，见 §9.2；不设硬时间上限）：三平台、无障碍、性能与安全回归。
- **M5 — 长期可选方向**：L3/L4、市场、云端，不计入 Beta 承诺。

### 0.4 术语对照表

三套规范并行，阅读时以本表为准。

| 名词 | 定义 | 运行位置 | 成熟阶段 |
|---|---|---|---|
| **CPP-v1** | Cordis Portable Profile v1，本项目拟议的 Rust 插件规范 | Rust 静态 / WASM Component / 独立进程 | M3 定型 |
| **ADC-v1** | Agent Desktop Client API v1，UI 扩展的公开契约（slot 注册表 + 声明式元素） | 插件 → Client Host RPC | M2 骨架、M3 冻结 |
| **DSH L1** | JS Tool 注册/执行/取消/结果 | Node Sidecar | M2 必须 |
| **DSH L2** | 白名单 Host Service 的逐服务适配 | Node Sidecar + Rust adapter | M3 按需 |
| **DSH L3** | 选定 Agent/Tool/Session Hooks | Node Sidecar | M5 |
| **DSH L4** | DSH React Client 插件在隔离 WebView | 独立 WebView | M5，实验性 |
| **DSH L5** | 完整 DSH Web Client | — | **不承诺** |

---

## 1. 背景与问题定义

当前 DSH 的 Host Plugin 机制基于 Cordis，使用服务注入、生命周期 Fiber、事件与可逆 Effects，这些概念可以相对自然地映射到 Rust。**真正的生态挑战在 Client Plugin**：官方 DSH Web Client 依赖浏览器中的 Cordis、动态模块加载、React Slots、Conversation Renderers、Host Boot Graph 与 Web RPC；它不是一套可以直接换成 GPUI Renderer 的纯数据 UI 协议。

GPUI 在原生多面板、高频文本和 IDE 类应用上具有实际优势，但尚不能假设它会形成 React 级别的事实标准。平台因此需要在 GPUI 之上建设 **Agent Desktop Client API**，保留 Renderer 可替换性，并提供低门槛的脚本扩展入口。

### 1.1 明确不做

- 不以完整复制 DSH Desktop UI、CSS 或 React 插件机制为目标。
- 不在 Rust 中重新实现完整 JavaScript/Node/Cordis。
- 不承诺任意现存 DSH 插件都能透明访问 Rust `ctx.agents`、`ctx.sessions`、`ctx.llm` 等对象。
- 不让 Node Sidecar 成为主 Session Store、权限中心或第二套 Agent Loop。
- 不把任意 Rust `trait`、`Arc<dyn Trait>`、GPUI `Entity<T>` 暴露成第三方动态插件的二进制 ABI。
- 不把 GPUI Shell/GPUIX 等新兴框架的实验 API 直接作为平台的公开稳定标准。
- 首版不追求完整 DSH React Client 插件映射、全功能多窗口 WebView 叠层或动态原生 DLL 热卸载。

### 1.2 核心成功标准

用户从 Desktop 发起一条会话，Rust Agent 调用由 Rust 和 Node Sidecar 分别注册的工具，将结构化结果写入**同一个 Rust Session Log**；GPUI 原生视图能够实时显示及历史回放；第三方插件失效或 Sidecar 重启不会破坏已有 Session 或本地工具。

### 1.3 为什么不走更便宜的路径（ADR-009）

评审要求把反面论证留在文档里，避免"因为能写所以要做"。

| 备选路径 | 成本 | 为什么不够 |
|---|---|---|
| **把现有 DSH Web Client 装进 Tauri/Electron 壳** | 10–25 日可出可用外壳 | 交互仍是 Web；无法提供原生多面板、原生终端与 GPU 级虚拟化；插件生态仍是浏览器 Cordis，长期被上游节奏绑定 |
| **只做 DSH Client 插件（不换壳）** | 5–15 日 | 受限于宿主 UI 的能力上限；无法定义自己的 Tool/Session/Policy 语义 |
| **自研 Rust Agent Core + 复用 Web UI（Tauri + 本仓库前端）** | 60–100 日 | 折中方案，代价是永久放弃 GPUI 的性能与原生交互优势；**若 M1 的 GPUI 判死实验不通过，这就是退路** |

结论：本项目选择最贵的路径，理由是差异必须落在"原生交互 + 可长期演进的插件平台 + 自有 Agent 语义"三者同时成立；若其中两者不成立，应回到折中方案，而不是继续加码。

---

## 2. 系统架构与所有权

```text
┌──────────────────────────────────────────────────────────────┐
│ GPUI Desktop Shell                                           │
│ Workspace / Multi-Agent Dock / Conversation / Tool Cards     │
│ Terminal / Diff / Inspector / Commands / Notifications       │
│ ┌──────────────────────────────────────────────────────────┐ │
│ │ ADC-v1 (versioned)                                       │ │
│ │ Slot Registry + Declarative Elements                     │ │
│ │ Native GPUI View | Script UI | Isolated HTML/WebView      │ │
│ └──────────────────────────────────────────────────────────┘ │
└──────────────────────────────┬───────────────────────────────┘
                               │ typed commands / snapshots
┌──────────────────────────────▼───────────────────────────────┐
│ Rust Application Core (sole authority)                       │
│ Agent / Context / Session Event Log / Tool Registry / Policy │
│ LLM Adapters / ACP / MCP / UI Contribution Registry / Jobs   │
│ Plugin Host (Cordis-style Fiber, Services, Effects, Events)  │
└──────────────────┬───────────────────────┬───────────────────┘
                   │                       │ versioned IPC
       ┌───────────▼──────────┐    ┌──────▼────────────────────┐
       │ Native Plugins       │    │ Node.js DSH Sidecar       │
       │ Rust built-ins       │    │ Original DSH/Cordis       │
       │ WASM / process       │    │ npm resolution / HMR      │
       │ CPP-v1               │    │ JS Host Plugin callbacks  │
       └──────────────────────┘    └───────────────────────────┘
```

### 2.1 状态所有权（必须唯一）

| 状态/能力 | 权威所有者 | Sidecar 权限 |
|---|---|---|
| Agent ID、Turn/Step、Inbox、任务调度 | Rust | 仅经公开 Adapter 查询/发送动作 |
| Session 事件、持久化、历史、回放 | Rust | 不能直接写磁盘或维护平行 Session |
| 模型选择、上下文组装与压缩、LLM Stream | Rust | L2 后可提供 Model Adapter，不自行调度主 Agent |
| Tool Registry、批准/拒绝/审计 | Rust | 注册远程 Tool；执行前通过 Rust Policy |
| Native 插件与 UI 贡献的**清单与生命周期** | Rust | 不管理 Rust Fiber |
| 窗口、Dock、布局、主题、渲染 | GPUI | 只提交声明式 Contribution，不直接改界面树 |
| JS 插件及其内部依赖/Effects | Node Cordis | 本地管理，跨进程通过 registration handles 同步 |
| 第三方文件/进程权限 | Rust Host / OS sandbox | npm/Node 插件需额外隔离，不能仅信任声明 |

> **UI 归属的精确表述**：Contributions 的**声明、校验、版本与撤销**属于 Rust（`ui-contributions`）；**布局、窗口、绘制、焦点**属于 GPUI（`ui-gpui`）。GPUI 不拥有插件清单，Rust 不拥有像素。

### 2.2 Rust Core 的推荐模块

- `agent-core`：模型请求、Turn/Step、Agent lifecycle、消息队列、取消和子 Agent 协调。
- `agent-context`：窗口计量、压缩、工具结果裁剪、外部化引用（见 §6.4）。
- `session-store`：append-only、版本化事件 Schema、索引、回放、迁移、崩溃修复。
- `tool-runtime`：Tool schema、调度、并发、取消、Policy、结果规范化和 UI metadata。
- `plugin-api`：公开的插件能力契约和稳定标识；不依赖 GPUI/Cordis 实现细节。
- `plugin-host`：Rust Cordis Adapter，依赖图、Fiber、Service、Effects、事件。
- `ui-contributions`：slot 注册表、Contribution 校验、元素词汇表（ADC-v1 的服务端）。
- `ui-gpui`：原生实现、主题、窗口、Dock、会话和虚拟化渲染。
- `compat-dsh-protocol`：IPC schema、错误码、版本协商、attachments。
- `compat-dsh-host`：Sidecar 生命周期、路由、重试、registration generation。
- `compat-acp` / `compat-mcp`：外部 Agent 和 Tool 接入。

建议先以**单 workspace + 明确 crate 边界**实现，不急于把每一层做成独立发布的 crate。

---

## 3. Rust Native Plugin：Cordis Portable Profile v1（CPP-v1）

### 3.1 目标

保留 DSH/Cordis 插件的主要心智模型，但建立 Rust 易于实现、可以测试且不会受 JavaScript 动态语义牵制的严格子集。CPP-v1 是**本项目拟议规范**，并非现有 DSH 官方规范。

### 3.2 支持与不支持

| 机制 | CPP-v1 | 约束 |
|---|---|---|
| Plugin `id` / `version` / `inject` | 支持 | 静态声明、版本化 Service 名称 |
| `apply(ctx, config)` 等价入口 | 支持 | Rust SDK 生命周期与配置类型校验 |
| Fiber `Pending/Loading/Active/Failed/Unloading/Disposed` | 支持 | 明确状态转换与失败清理 |
| Service publish / lookup / revoke | 支持 | 只能提供类型化服务或定义了 IDL 的 RPC 服务 |
| Reversible Effects | 支持 | 注册归属 Fiber；卸载按契约执行清理、等待完成 |
| Tool registration / execution | 支持 | 参数 Schema、canonical value、render/meta、取消 |
| Events | 受限支持 | v1 只开放命名的观察事件和少量有定义的顺序事件 |
| Config / Patch | 受限支持 | 纯 JSON/YAML；不得使用 `!!js` |
| Service isolation | 支持 | 明确 Realm、避免隐式继承/回退 |
| 任意 `waterfall(next)` | 不支持 v1 | 由特定 Policy / Hook 接口替代 |
| JS Proxy、装饰器、运行时任意对象 | 不支持 | 交给 Node Sidecar |
| 任意 `ctx.plugin()` 动态创建子插件 | 不支持 v1 | 插件树由 Host 管理；**该限制只约束 CPP-v1，JS 插件图完全由 Sidecar 内 Cordis 管理** |
| Web React Client Plugin | 不支持原生执行 | 单独 WebView 或 Legacy Workspace |

**特别强调：**使用上述 API 子集的 JavaScript 插件，并不会自动在 Rust 中执行。Rust Direct 要求 Rust 实现、WASM Component 或受支持的外部进程实现；未修改的 JS 插件仍在 Node Sidecar 中执行。

### 3.3 运行模式

1. **`rust-static`**：静态链接、可信系统级插件；可直接注册高阶 GPUI View、LLM Adapter。
2. **`wasm-component`**：面向独立分发、权限受控的第三方插件；使用 WIT + Wasmtime，宿主拥有真实的工具/文件/网络能力。**必须配套资源配额**（fuel/epoch 超时、线性内存上限、目录挂载白名单），否则一个死循环即可拖垮宿主。
3. **`external-process`**：对特定高性能进程或其他语言提供版本化 IPC；可独立崩溃、重启。
4. **`dsh-js`**：官方 DSH JavaScript 插件使用 Node Sidecar 运行，非 CPP-v1 原生插件。

可选的可信 `cdylib` 只作为专门的高级扩展通道，必须固定 ABI、版本、符号、内存所有权和卸载策略；首版不接受任意 DLL。

### 3.4 公开 Manifest 草案

```toml
[plugin]
id = "cad.validate"
version = "0.1.0"
api = "cordis-portable/v1"

[runtime]
kind = "wasm-component"
entry = "cad_validate.wasm"
# 资源配额（见 §3.3）
fuel = 2_000_000_000
max_memory_mb = 128
timeout_ms = 30_000

[inject]
required = ["tool-registry/v1", "ui-contributions/v1"]

[[tools]]
name = "cad_validate"
input_schema = "schemas/cad_validate.input.json"
output_schema = "schemas/cad_validate.output.json"

[permissions]
workspace_read = true
workspace_write = false
network = false
```

草案不是实际已实现的 `Cargo.toml` 或 DSH manifest。最终字段及版本规则由 `plugin-api` 的 schema、测试和示例包定义。

### 3.5 Rust Cordis 选型

- **已选定（ADR-0002，2026-11-09 实测）：** [`jincdream/cordis-rs`](https://github.com/jincdream/cordis-rs) —— 对冻结版上游 Cordis 的行为级移植，带 163 个场景映射；本机 Windows 上 111 个测试全绿，无原生 C 依赖。
- **参考实现：** [`dshbox/cordis-rs`](https://github.com/dshbox/cordis-rs)，类型化运行时（`inertia`/`era` 模型），167 个测试同样全绿，但依赖栈式协程（`alloca`，C 编译）且语义与 Cordis 有距离；只作为 effect 生命周期的测试参考。
- **动态 ABI 参考：** [`redoop/cordis-rust`](https://github.com/redoop/cordis-rust)，借鉴其将 Host 生命周期与 C ABI 计算入口隔离的模式，不直接复用其示例的 `unsafe` 设计作为生产 ABI。

**选型门槛（M0 已完成）：**在 Windows 上实测六项：Service Provider 失效后依赖 Fiber 重启、并发 `apply/dispose`、Effect 异步清理、取消安全性、Service isolation、逆序注册撤销。结果与证据见 [`decisions/0002-cordis-rs-selection.md`](decisions/0002-cordis-rs-selection.md)。**同时只依赖一个**：`plugin-host` 是唯一允许依赖它的 crate。

**未达标时的转向**：自研最小 Fiber（约 850–1500 行，只覆盖 CPP-v1 子集），并把第三方插件平台推迟到 M4。

---

## 4. Client Extension API：真正需要稳定的公开契约（ADC-v1）

### 4.1 原则

**GPUI 是内部 Renderer，不是第三方动态插件的标准 ABI。** 定义 `Agent Desktop Client API v1`（ADC-v1），独立于 GPUI 版本、Rust `Entity<T>`、JS Framework 和进程边界。

ADC-v1 的核心不是"六种组件"，而是**一个 slot 注册表加一套声明式元素词汇表**（ADR-010）。理由：

- 真实挂载点远比六种组件细（DSH 现有 Client 侧实测 **135 个 slot 节点**、`single/list/keyed/chain` 四种 kind、`root/session/session-maybe` 三种 scope，每槽带 registration 契约与替换风险标注）。
- 只有 slot 模型能给"一个按钮放进输入框左侧""一行设置放进通用页"这类需求一个**非破坏性**的落点。
- 若 ADC-v1 只提供六种组件，DSH Client 插件的移植是重写而不是改挂载点。

**slot 语义（v1 冻结）**

| 维度 | 取值 | 含义 |
|---|---|---|
| `kind` | `single` | 单席，后注册者替换（或按策略拒绝） |
| | `list` | 有序列表，按 `order` 排序 |
| | `keyed` | 按 key 分派，key 域由宿主声明（闭合或开放） |
| | `chain` | 链式，可拦截/改写宿主默认值 |
| `scope` | `root` | 全局唯一 |
| | `session` | 每个 Session 一份 |
| | `session-maybe` | 允许"尚未创建 Session"的空态 |
| 注册契约 | `id` / `order` / `label` / `key` | 由宿主按槽声明，校验失败即拒收并诊断 |

**六种 Contribution 的定位**：不再作为 API 的原子，而是**预置 slot 的语义分组**（见 §4.6.2）。

### 4.2 三条 UI 路线

**A. Native GPUI（产品核心）**

- 平台内置、可信同版本 Rust Plugin 可以直接提供 GPUI View。
- 常用工具视图（会话、终端、Diff、Markdown、Table、Tree）维护为官方 Native Components。
- 不将 GPUI 对象跨 DLL/WASM/IPC 传输。

**B. Script UI（第三方动态扩展的优先方向）**

- 研究 GPUI Kit 的 `gpui-shell` QuickJS 脚本扩展方案（上游为实验路径，见 §14）。
- 由平台提供 JavaScript/TypeScript SDK；脚本只调用 HostModule 与 ADC-v1 元素。
- 依赖 §4.6 的元素词汇表；`gpui-shell` 的对外接口不直接等于 ADC-v1。
- React-like GPUI Renderer（例如 GPUIX）只作为技术观察/备选，不加入首版核心依赖。

**C. HTML/WebView（兼容/高复杂可视化）**

- 使用独立的插件 Tab/Panel 或独立窗口承载需要 DOM/CSS/React 的组件。
- WebView 与主程序通过版本化 Action/State IPC 通信。
- Windows 上优先避免 GPUI popup/dialog 覆盖在 WebView 上；不承诺在任意 Tool Card 内嵌完美叠层。
- WebView 必须限制导航、下载、窗口创建、IPC 来源与权限；浏览器 JS 与 Native 功能显式授权。

**D. 工具卡内的动态区域（显式决策）**

DSH 已有"工具卡内部由包自己渲染一块交互区域"的 slot。ADC-v1 v1 的决策是：

- **不提供**工具卡内任意渲染（不内嵌任意 WebView、不接受任意 GPUI View）。
- **提供**两条受限路径：① 预置渲染器（Terminal / Diff / Table / Image / JSON）；② 声明式元素组成的 `tool.result.extra` 区域，只允许 §4.6.3 的元素集。
- 需要完整 DOM 的插件，走独立 Panel/窗口，而不是工具卡内嵌。

### 4.3 推荐的 Client API 风格（示意，不是已有 SDK）

```ts
export const inject = ['ui']

export function apply(ctx) {
  // 预置 slot：一个工作区右侧面板
  ctx.ui.declareView('cad.inspector', {
    title: 'CAD Inspector',
    render: (state) => h(Stack, { gap: 8 }, [
      h(Code, { source: state.selection.summary }),
      h(Button, { label: '校验', onPress: () => ctx.call('cad.validate') }),
    ]),
  })

  ctx.ui.register('workspace.panel', {
    id: 'cad.inspector',
    location: 'right',
    label: 'CAD Inspector',
    view: 'cad.inspector',
  })

  // keyed slot：按工具名挂结果渲染器
  ctx.ui.register('tool.call.toolview', {
    key: 'cad_validate',
    view: 'cad.validation-result',
  })
}
```

原生 Rust Plugin 可拥有更强的 typed API，但公开动态插件应使用相同的 slot 语义、权限模型和统一 Host 状态源。

### 4.4 UI 状态模型

- **Host-owned state**：Session、Tool Call、Artifacts、Workspace、审批结果、文件身份，Rust 是权威。
- **Client-local state**：面板尺寸/位置、焦点、选中项、折叠状态、未提交输入、临时表单。
- **Plugin state**：由插件声明可持久化字段，带 schema 版本与迁移钩子；禁止插件自行修改主 Session Log。
- **Render contract**：`snapshot + incremental updates + commands`；前端不依赖 DSH/Node 对象引用。
- **History replay**：Tool result + versioned presentation metadata 应能离线重建卡片；不需要重放实际 Tool callback。

### 4.5 优先验证的 GPUI 能力

按风险排序（M1 的判死实验直接取自第 1–3 项）：

1. Windows：多 Dock、多会话、上万条日志的虚拟化滚动与流式 Markdown。
2. 长 Tool Result、Diff、终端与中文 IME 输入。
3. WebView2 与 GPUI 原生浮层、焦点、输入、DPI 缩放、裁剪的真实表现。
4. Plugin Panel 注册/卸载、窗口 resize、主题切换与恢复布局。
5. GPUI Shell 动态插件冷/热加载、响应式数据、异常隔离。
6. 后续：macOS 菜单/快捷键、Linux X11/Wayland 与 WebView 限制。

### 4.6 声明式元素词汇表与 Slot 模型（M3 交付）

这是 ADC-v1 的真正主体，也是 v0.1 缺失的最大块。它本身是一个**独立交付物**（设计 2–3 周 + 实现，已计入 §9 的 `Plugin SDK / UI Contributions`）。

#### 4.6.1 设计约束

1. **没有任意布局引擎**：只有 `Stack`（纵向）、`Row`（横向）、`Grid` 三种容器与 `gap/padding/align`，不做绝对定位与自由漂移。
2. **没有 CSS**：样式只能来自主题 token（色彩、间距、字号阶、圆角、阴影），插件不得注入任意样式表。
3. **元素是数据不是代码**：`{ type, props, children }` 的纯 JSON 树（脚本侧可用 `h()` 构造），保证可跨进程传输、可静态校验、可在 WebView 与 GPUI 两条路线复用。
4. **前向兼容**：未知元素/未知属性必须渲染为占位块并记录诊断，**不得崩溃或静默丢弃**。

#### 4.6.2 预置 slot 与六种 Contribution 的映射

| 旧 Contribution | 落到的预置 slot | kind / scope |
|---|---|---|
| `panel` | `workspace.panel` | list / root |
| `tool_renderer` | `tool.call.toolview`（key = 工具名） | keyed / session |
| `widget` | `conversation.chat.node`（key = widget id） | keyed / session |
| `command` / `menu` | `command` 注册表 + `menu.*` | list / root |
| `viewer` | `viewer.tab`（key = 文件类型） | keyed / session |
| `settings` | `settings.section` + `settings.general.item` | list / root |
| （新增）工具卡附加区 | `tool.result.extra` | list / session |
| （新增）输入框两侧 | `conversation.input.left` / `.right` | list / session |

#### 4.6.3 元素集合 v1（冻结范围）

- **布局**：`Stack`、`Row`、`Grid`、`Divider`、`Spacer`
- **文本**：`Text`（可含行内强调）、`Markdown`（只读、受控子集）、`Code`、`Badge`
- **交互**：`Button`、`Input`、`Textarea`、`Select`、`Checkbox`、`Switch`、`Slider`
- **数据**：`List`、`Table`（列声明 + 行数据）、`Tree`、`KeyValue`
- **媒体**：`Image`、`Icon`、`Chart`（bars/line/donut 三种）
- **容器**：`Card`、`Tabs`、`Accordion`

不在 v1 范围（必须显式拒绝并提示替代）：`WebView`、`Canvas`/任意绘制、`Video`、任意 HTML、自由动画。

#### 4.6.4 绑定与动作

- **数据绑定**：`ctx.binding('session.summary')` 一类只读快照；更新通过 `snapshot + patch` 推送，插件不得持有 Host 对象引用。
- **动作**：`ctx.call(method, args)`（走 Host RPC）、`ctx.command(id)`（触发命令）、`openExternal(url)`（受限白名单）。
- **事件**：`onPress` / `onChange` / `onSubmit` 三个即可，避免把 DOM 事件模型带进来。

#### 4.6.5 版本策略

- 新增元素/属性 = minor，插件可探测 `adc.element.<name>` 能力位。
- 语义变更 = major，旧插件继续按旧元素集渲染至少一个 major 周期。
- 元素集冻结前（M3 之前），任何实现都必须假定会变，不要对外承诺。

### 4.7 与 DSH Client 的移植关系

| DSH Client 概念 | ADC-v1 对应 | 差异 |
|---|---|---|
| Slot 注册（`ctx.slots.register`） | `ctx.ui.register(slot, entry)` | 语义对齐；scope 由宿主声明而非运行时推断 |
| Factory | `declareView` + 元素树 | DSH 返回 React 元素，ADC 返回数据树 |
| `ctx.get(service)` | `ctx.service(name)` 白名单 | 跨进程只暴露声明过的服务 |
| `host.call` | `ctx.call` | 语义一致，改为版本化 envelope |
| 样式表注入 | 主题 token | 明确不支持任意 CSS |

目标不是"DSH Client 插件零改动运行"，而是**把移植工作从重写收敛为改挂载点与重写视图层**。

---

## 5. DSH 兼容 Sidecar

### 5.1 兼容策略

Node Sidecar 使用与锁定目标版本一致的**原版 DSH vendored Cordis**、Loader、npm 解析和插件生命周期。Sidecar 负责执行 JS 插件，但不运行第二套主 Agent Loop。

将兼容分为独立承诺等级：

| 级别 | 范围 | 实现形态 | 单位成本 | 阶段 |
|---|---|---|---|---|
| **L1 — Tool** | JS Tool 注册、Schema、执行、结果、取消、流式输出、Effect 清理 | 一套 register / invoke / result / chunk / cancel / unregister 协议 | 整体 10–15 日 | **M2 必须** |
| **L2 — Service** | 白名单 Host Service | **每个 service 一个手写 adapter**：`Agent` 身份 → session id、回调 → `event.subscribe`、流式端点 → 帧协议、生成 schema → Rust 类型 | 1–3 日/个 + conformance test | M3 按需 |
| **L3 — Hooks** | 明确选定的 Agent/Tool/Session 事件 | 每事件一个 adapter | 0.5–2 日/个 | M5 |
| **L4 — Client** | DSH React Client 插件在隔离 WebView/Legacy Client 中运行 | 独立 WebView + 最小 Browser Boot | 需独立 PoC | M5，实验性 |
| **L5 — Full DSH** | 完整 DSH Web Client、任意 Context/插件图 | — | — | **不承诺** |

**L2 口径（v0.2 修正，M3 证据复核）**：DSH 0.2.2-rc.1 的 `cordis_inspect` 目录列出 **107 项** Host Service（`dsh-tool-cordis` 的 `SERVICE_API`；v0.2 写的 98 项是旧采样），且大量方法以活的 `Agent` 对象、回调、流式端点作为参数（如 `approval.request`、`subagents.sendMessage(sender: Agent, …)`、`terminals.spawn(owner: Agent, …)`、`systemPrompt.tools(provider)`、`typertGateway.stream`）。因此 L2 **不是"配一份白名单"**，而是 N 个小型跨进程契约。**首批三个（证据驱动，逐项证据见 [m3-l2-services.md](m3-l2-services.md)）**：

| service | 真实调用方（证据） | 关键难点 |
|---|---|---|
| `workingDirectory` | 10/27 官方工具包注入；`dsh-tool-working-directory` 除 `tools` 外只差它 | 目录权威在 Session Log；per-session 串行化 |
| `sessionProjections`（配 `session-log` 数据面） | `dsh-tool-todo`、`dsh-tool-subagent` | 投影单元是 JS 函数：注册表留 Sidecar，宿主只给事件流 + 分页读 |
| `storage` / `storageDomain` | 官方**工具**无调用方（0/27，生态押注）；官方 `kv` backend 契约可复用 | unit/版本戳/布局；v2 无游标（768 KiB 上限） |

`settings`（0/27 工具包注入，只出现在 Client UI 的可选注入，属 L4）与 `fs`（M2 已有 `fs.write.request` 专用写路径，§7.5）**不进首批**；`systemPrompt`（14/27 注入）是 storage 的替补候选。协议正文见 [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §13。

L1 的 `Tool` 不是简单 `name -> string`：必须保留参数 Schema、执行 identity、取消、canonical JSON value、模型可见 content、错误标记和 presentation metadata。底层 Rust Policy 对来自 Sidecar 的 Tool **同样生效**。

### 5.2 双向 IPC 协议草案

使用本地双向 RPC（stdio / Unix domain socket / Windows named pipe 择一并抽象 transport），控制消息使用版本化 JSON，图片/大型文件用二进制附件或内容寻址引用，避免大量 Base64。

```json
{
  "version": 1,
  "kind": "tool.invoke",
  "request_id": "req-001",
  "runtime_generation": "node-0007",
  "plugin_id": "example.dsh.tool",
  "registration_id": "reg-0042",
  "session_id": "s-0001",
  "call_id": "call-123",
  "depth": 0,
  "payload": { "arguments": { "path": "part.step" } }
}
```

消息族（拟议）：

- 生命周期：`runtime.hello`、`runtime.ready`、`runtime.crashed`、`plugin.load`、`plugin.unload`。
- 注册：`service.publish/revoke`、`tool.register/unregister`、`ui.contribute/retract`。
- 执行：`tool.invoke/result/chunk/cancel`、`service.invoke/result`。
- 事件：`event.subscribe/unsubscribe/emit`（仅白名单）。
- 诊断：`health.ping`、`log.emit`、`capabilities.list`、`diagnostics.snapshot`。

**必须从首版设计进去的约束：**

- `protocol_version` + `DSH_runtime_version` + capability negotiation；不兼容时报错而不是静默降级。
- `plugin_id` / `generation` / `registration_id` / `session_id` 全链路携带；Sidecar 重启时撤销旧 generation 的一切注册。
- **`session_id` 是跨进程的身份锚点**：Rust 只在 adapter 边界把 `session_id` 还原为 `Agent`，任何反向的"传 Agent 对象"都视为协议违规。
- **流式工具输出**：`tool.chunk` 携带单调序号与累计字节数，超过上限切到附件引用；结算（`tool.result`）必须幂等，迟到结果按原 Tool policy 忽略或拒绝。
- 所有请求有 correlation id、timeout、取消传播和结构化错误；重复注册/结果有明确定义的幂等策略。
- **回调深度上限**：跨进程回调链带 `depth`，超过 4 层直接返回 `E_RPC_DEPTH`；持有核心锁时禁止等待远端调用。
- 服务/回调不序列化 JS Function 或 Rust trait object，只传稳定 ID + 明确的 RPC 方法。
- 大结果/流式输出提供大小上限、背压、独立的附件传输；日志不能混入协议 stdout。
- Sidecar Crash 不得导致 Rust 进程或 Native Plugin 崩溃，已提交 Session Event 不得回退；**崩溃时仍在执行的调用统一标记为 `aborted`**，不重试（由 Agent 决策是否重新调用）。

### 5.3 官方 DSH 插件的装载

- 通过 `package.json` 的 `dsh.bundle` 和 Profile/Patch 结构发现 JavaScript 插件。
- Sidecar 自行执行 npm/pnpm 安装与模块解析，版本由 sidecar runtime lockfile 固定。
- DSH 的 `!!js`、Schemastery、JS Proxy、HMR 等保留在 Node；Rust 不尝试模拟。
- Rust Core 维护**统一插件清单**与状态：`installed / enabled / loading / active / failed / unsupported`。
- 对混合 Profile 定义标准化配置层；首版可以选择"Rust Native Manifest + DSH Sidecar Profile"双清单，由 UI 汇总，不必强行复刻 DSH Patch 的全部复杂规则。

### 5.4 DSH Client Plugin 的处理

DSH Client Plugins 依赖浏览器侧的 Cordis、动态模块、React Slots 与 Host API，不能只靠 Node 执行 JS 就在 GPUI 中出现 UI。

分三类处理：

1. **工具类展示**：转换 DSH 的泛型/终端/Diff 等展示意图到 Native Tool Card；无专用 Renderer 时安全降级。
2. **可声明式改写的 UI**：插件作者基于 ADC-v1 提供新的 Script/Native UI 实现（§4.7 给出移植映射）。
3. **真正依赖完整 React/DSH Client 的插件**：运行在隔离的 WebView/Legacy DSH Workspace；不假设与原生 Rust Session 完全同构。

L4 必须通过独立 PoC 验证：官方 Browser Client Plugin 是否能够在最小 Browser Boot 环境中工作、其需要哪些 DSH Host Remote 以及是否值得桥接。不能为 L4 让整个 Agent Core 回退到 Node 所有权。

### 5.5 Node 运行时与依赖分发（M2 前置）

"可安装一个普通 DSH Tool 插件"这个 M2 退出条件依赖运行时分发，必须在 M2 前定案：

| 决策项 | 默认方向 | 备注 |
|---|---|---|
| Node 版本 | 捆绑固定版本（不依赖用户环境） | 与 sidecar lockfile 同批升级 |
| 包管理 | 捆绑 pnpm，安装到 profile 内目录 | 不使用全局缓存，避免污染与权限问题 |
| 离线 | `vendor/` 预置官方插件依赖；离线可运行 | 企业/内网场景必须支持 |
| 安装脚本 | 显式确认后执行，记录到审计日志 | npm 生命周期脚本是真实攻击面 |
| 首次启动 | 无网络时进入 Native-only 模式并提示 | 不阻塞 UI |

---

## 6. Agent Core 与会话模型

### 6.1 最小 Agent Loop

```text
user message
  -> session.append(user/message)
  -> assemble prompt + active tool schemas
  -> [context budget check] (见 §6.4)
        -> measure / prune / compact / spill
  -> model request / stream
  -> agent events (live)
  -> normalized assistant settlement (durable)
  -> [tool calls?]
       -> Rust Policy / Approval
       -> Native Tool OR Sidecar Tool OR MCP Tool
       -> normalized tool result (durable) [+ tool.chunk stream]
  -> next step / turn end
```

保留 `turn`（一次用户任务的完整执行）、`step`（一次模型请求及其 Tool 调用）概念。与 DSH 的相似性用于降低迁移认知成本，但 Session Log 的 Schema 独立定义，避免未来被 DSH 内部版本演进绑架。

### 6.2 Session 设计最低要求

- Append-only、版本化事件 Schema；只有 Rust 可提交权威事件。
- Live UI 事件与 Durable Event 区分；流式 chunk 可以临时显示，但 settled assistant/tool 结果必须可回放。
- 支持取消、重试、会话恢复与不完整 Tool Call 结果修复。
- Session IDs、Agent IDs、Tool Call IDs 稳定且与插件 generation 无关。
- Session Viewer 可以从 Event Log 独立重建，无须重放插件或 LLM 请求。
- 文件结果/大附件引用与 Session Log 分离，附带校验与保留策略。
- **保留与导出**：默认保留策略、单会话磁盘上限、导出（JSONL + 附件）与删除；敏感字段（凭据、绝对路径）不入 Log。
- **增长控制**：事件日志按 turn 分段索引，历史回放按段加载；超过阈值的会话提示归档而非静默变慢。

### 6.3 ACP / MCP

- ACP：外部 Agent 接入、独立会话代理、任务状态同步；不把每个 ACP Agent 当作内部 Rust Fiber。
- MCP：工具发现、Schema、请求/响应、权限与审计，统一进入 Tool Registry；远程 MCP 的 OAuth 与连接重建单独设计。
- 自己的原生插件可以实现 ACP/MCP Adapter，但 Rust Tool/Session/Policy 仍是核心边界。

### 6.4 上下文工程（ADR-011）

v0.1 把 Loop 写成"组装 prompt 就发请求"，这是不可行的。以下五项是 M2 交付物，不是优化项：

| 能力 | 要求 | 可观测产物 |
|---|---|---|
| **计量** | 每个 Session 维护 token 折算（消息、工具结果、附件分别计），请求前预估 | `token.measured` 事件；UI 显示当前占用 |
| **压缩** | 阈值触发、手动触发、区域压缩三种入口；压缩结果作为 durable 事件写入 Log | `context.compacted`（含被压缩的 seq 区间与摘要引用） |
| **裁剪** | 工具结果按 head/middle/tail 确定性裁剪，保留首尾与错误段 | `tool.result.pruned`（原始结果转为引用） |
| **外部化** | 超过阈值的输出落盘为内容寻址引用，模型只见引用 + 预览 | `spill.created`（大小、hash、保留期） |
| **一致性** | 压缩后"模型可见历史"与"UI 展示历史"的差异必须可解释、可回放 | 回放时按事件重建，不重放 LLM 请求 |

硬性规则：

1. 压缩不得丢失可回放性——原始事件仍在 Log 中，压缩只影响"下一次请求的输入"。
2. 任何裁剪/外部化都必须在 Log 中留痕，否则历史重建会失配。
3. 上下文预算用完时必须**降级而不是报错**：先裁剪、再压缩、最后拒绝新 Turn 并提示。
4. 费用只记录用量（token/时长），不在 Core 内嵌价格表——价格是易变数据。

### 6.5 会话树与子 Agent（v1 范围）

架构图里的 "Multi-Agent Dock" 必须有对应设计，否则 UI 做完后会被"顺手加"进来。

**v1 承诺**（M2）：

- **父子会话是一等对象**：子 Session 有自己的完整 Event Log，`parent_session_id` 与 `spawn_source` 写入 Log。
- **委派**：父 Agent 通过工具 `spawn_child(prompt, preset?)` 创建子会话，默认**单层**，深度上限可配置（默认 2）。
- **权限继承**：子会话默认继承父的工作区与策略，**只允许收紧不允许放宽**；放宽需显式审批。
- **中断传播**：中断父会话时按树向下传播（先 `interrupt`，超时后 `kill`），子会话标记 `interrupted_by_parent`。
- **等待语义**：父可等待全部子会话结束（`drain`），也可只等待直接子级；等待期间父保持 `active` 以免被误判闲置。
- **UI**：Dock 以树形展示，父/子可并排；回放子会话不依赖父会话在线。

**v1 不承诺**（M5）：团队任务板、跨会话共享内存、子会话之间直接通信、超过深度 2 的树、远程 Agent 参与同一棵树。

---

## 7. 安全和隔离

| 场景 | 约束 |
|---|---|
| 内置静态 Rust Plugin | 可信代码，使用细粒度 Host 权限但非进程沙箱 |
| 第三方原生动态插件 | 优先 WASM/独立进程，能力默认拒绝；**WASM 必须带 fuel/epoch 超时、内存上限、目录挂载白名单** |
| DSH npm 插件 | Node 安装脚本与运行时拥有执行代码能力，需独立权限/安装确认；Sidecar 不是沙箱 |
| Node Sidecar 进程 | **Windows Job Object** 限制 CPU 时间、内存、子进程数；独立 profile 目录；不继承宿主凭据环境变量 |
| DSH Tool 执行 | Rust 统一批准、拒绝、审计和超时策略，不允许绕开 Policy |
| HTML/React Widget | WebView 独立安全上下文，严格 URL/导航/IPC/下载权限 |
| 文件与命令 | 工作区边界、路径规范化、身份上下文与输出大小限制 |
| Secrets | Rust Credential Store（Windows DPAPI / Credential Manager），不直接提供给插件 JS 全局环境 |
| 凭据授权 | `authorization` 流程：按 provider 注册获取流程、一次一个尝试、可取消；插件只能申请"使用"，不能读取明文 |
| 插件故障 | Sidecar/独立进程可重启；Native `cdylib` 崩溃隔离不作保证 |

需要特别防范**跨层权限绕过**：JS 插件既可能调用 Rust 暴露的 Tool，也可能在 Node 进程里直接访问文件。Rust 的 Tool Policy 并不能自动约束 Node 进程自身的 OS 权限。

**因此 OS 级隔离是 M2 交付物，而不是提示语**：

1. Sidecar 以低完整性级别运行（Windows：Job Object + 受限 token；macOS：沙箱 profile；Linux：seccomp/bubblewrap，M4）。
2. 每个插件一个数据目录，跨插件读写默认拒绝。
3. 安装第三方插件时明确展示"该插件可在你的用户权限下执行任意代码"，并要求确认。
4. 审计只记录"调用与结果"，**不承诺记录进程实际副作用**——这一点必须写进用户文档。

---

## 8. 阶段路线图与验收

> 以下里程碑为**从项目启动算起的累计工程日**（单人等效），优先 Windows。每个阶段给出 2–3 个**可判定**指标；未达标时的转向方案见每节末尾。

### M0 — 基线与选型（累计 10–15 工程日）

- [ ] 固定 GPUI / GPUI Kit 精确 commit 与 Rust toolchain；完成 Windows 初始构建。
- [ ] 在 Windows 上实测 `dshbox/cordis-rs` 与 `jincdream/cordis-rs`（§3.5 六项），输出选型 ADR。
- [ ] 固定目标 DSH tag/commit 及其 vendored Cordis 版本；记录 license、更新政策。
- [ ] 定义 `plugin-api` / `ADC-v1` slot 契约 / Sidecar RPC 的首版 Schema。
- [ ] 写 ADR-009（不走 Web 壳）与一页"与上游桌面端的产品差异"。
- [ ] 完成 GPUI Kit Dock、Markdown、Tool Card、Terminal 的极简 UI 原型。

**Exit gate（可判定）**：
1. 干净环境 `cargo build && cargo test` 少于 10 分钟通过。
2. 三份 Schema 有 JSON Schema 文件 + 示例校验测试。
3. 五份 ADR 落盘（GPUI pin、cordis 选型、DSH pin、ADC slot 契约、不走 Web 壳）。

**未达标**：仅延期，不转向；但若 cordis 两候选都无法通过六项测试，M1 起改走"自研最小 Fiber"并同步下调 M3 插件平台目标。

### M1 — 垂直切片 PoC（累计 30–50 工程日）

三个判死实验（详见 §12）+ 一条端到端闭环：

- [ ] **实验 A（GPUI）**：10k 事件虚拟化 + 流式 Markdown + 中文 IME + WebView2 叠层，产出结论与录屏。
- [ ] **实验 B（Cordis）**：选定库跑通 Service 失效重启、并发 apply/dispose、Effect 异步清理。
- [ ] **实验 C（Sidecar 成本）**：用一个**带持久化 + 带审批**的真实 DSH 插件走完注册→调用→取消→卸载→杀进程→重注册，输出单位成本表。
- [ ] GPUI 展示工作区、单 Session 流式消息、Tool Call/Result。
- [ ] 一个 Rust Native Plugin 注册 Tool + Native Panel，并在卸载时清理。
- [ ] JS Tool 经 IPC 发布到 Rust Tool Registry，并由 Rust 调度器调用。
- [ ] Schema、取消、错误、结果/metadata、回放形成端到端闭环。
- [ ] 主动杀死 Sidecar，验证 Rust UI、Session 和 Native Plugins 继续可用。
- [ ] 单独验证 Script UI 与 WebView 的可行性，记录阻碍。

**Exit gate（可判定）**：
1. 统一 Rust Session Log 回放 Native/JS 两类工具调用，逐字节一致。
2. 杀 Sidecar 后 100% 会话完整、Native Tool 仍可用、无脏锁。
3. 三个实验结论落盘，且**每个结论都有可复现脚本**。
4. L1/L2 单位成本表给出"每个 service 的实际耗时"。

**未达标转向**：见 §12.2 的转向矩阵（GPUI 不达标 → 退到 Tauri + Web Client 折中方案；Sidecar 成本超预估 2× → L2 推迟到 M4）。

### M2 — Windows Alpha（累计 120–185 工程日）

> **状态：已完成（2026-11-11）。** 证据与剩余后移项见 [`m2-status.md`](m2-status.md)；
> 17 个 `chat` 自检 + workspace 测试全绿。MCP/ACP 按下方转向后移 M3。
>
> **复核补丁（2026-11-11 晚，真机双击复测）**：发现两处 M2 遗留并已修复——
> ① `chat.exe` 是 Console 子系统（双击弹命令行窗口），且会话/状态目录相对 CWD：
> 从桌面快捷方式（"起始位置"为空 → CWD=`C:\Windows\System32`）或只读目录启动会
> `os error 5` 退出，这就是"被 Windows 权限拦住"；同时打包资源根误用编译期源码树、
> sidecar 工具写死 5s 超时。② 默认配置与官方 DSH `0.2.2-rc.1` 有偏差，最严重的是
> `MAX_TOOL_CALLS=8`：一步发起 9 个工具调用即整轮 `TurnFailed`（官方 DSH 没有每轮
> 工具调用上限）。修复见 [`m2-windows-launch.md`](m2-windows-launch.md) 与
> [`m2-dsh-defaults.md`](m2-dsh-defaults.md)。

- [x] 真正可交互的 Agent Loop：LLM provider（≥2 家）、Turn/Step、流式、重试、取消、provider 归一化层。
- [x] Session Store：持久化、索引、重启恢复、未完整结果修复、保留与导出。
- [x] **上下文工程**：计量、压缩、裁剪、外部化、回放一致性（§6.4 五项全交付）。
- [x] **会话树**：spawn / 中断传播 / drain / 树形 Dock（§6.5 v1 范围）。
- [x] GPUI：多 Session Dock、输入框、Markdown、代码块、Diff、Terminal、Tool Cards。
- [x] Native Plugin SDK：Service/Effects/Tools/Event/Panel 的一等生命周期。
- [x] Tool Policy：批准/拒绝/问询、执行权限、审计。
- [x] DSH L1：工具注册、调用、取消、流式输出、卸载、Sidecar 重启测试。
- [x] **Node 运行时分发**（§5.5）：捆绑 Node（pnpm 待 M3）、离线 vendor、安装确认 + 审计日志。
- [x] **Sidecar OS 级隔离**（§7）：Job Object、独立 profile、环境变量隔离。
- [ ] MCP Tool 与基本 ACP Agent Adapter——按转向处置：**MCP 最小闭环进 M3，ACP 移 M4**
      （[ADR-0007](decisions/0007-m3-scope.md) §3）。
- [x] Windows 开发版打包、崩溃诊断和插件错误提示。
- [x] **启动面复核**：release 为 GUI 子系统（双击无控制台；CLI 用 `AttachConsole` 保留输出）、
      数据根与 CWD 解耦（`DSHR_HOME` / `%LOCALAPPDATA%\dshr` / 包内）、资源根 exe 相对、
      sidecar 工具超时改用工具声明（`ToolDefinition.timeout_ms`）。
- [x] **默认配置对齐 DSH 0.2.2-rc.1**：工具调用预算默认不限制（命中上限时降级而非
      `TurnFailed`，`DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS` 可收紧）、retry 6 次/500ms/
      10s/±10% jitter、temperature 默认不发送、context window 按模型分档。
- [x] **M2 独立 review 的 P1 修复**：会话日志原子写 + 损坏隔离 + `.bak` 恢复（防数据丢失）、
      取消语义（Agent `TurnCancelled` / provider token / 审批可取消 / sidecar `tool.cancel`）、
      工具错误改回 `tool.result(is_error)`（不再伪装成 `tool.aborted`）、Gate 2 frame-probe
      元数据与环境标注；见 [`m2-status.md`](m2-status.md)「P1 修复轮」。

**Exit gate（可判定）**：
1. 20 步 Turn 的工具调用成功率 ≥ 99%（故障注入用例除外）。→ 真实网络 20/20（100%）。
2. 10k 事件会话冷启动 < 1.5s；滚动帧率不低于基线（M1 记录值）。→ 147 ms；draw p95≈3 ms / 0 超预算。
3. 压缩触发后，模型上下文不超过窗口的 80%，且历史回放与 UI 一致。→ ratio 0.611；回放逐行一致。
4. 可安装/启用/禁用一个普通 DSH Tool 插件且不修改其源代码。→ 官方 read/write/edit/apply_patch 已装载。

**未达标转向**：砍 ACP/MCP 到 M3，优先保住"会话 + 工具 + 上下文"闭环。

### M3 — 插件平台 Preview（累计 240–370 工程日）

> **状态：完成（2026-11-15；独立 review 两轮 + 最终复核接受）。**
> 证据与缺口见 [m3-status.md](m3-status.md)；范围、债务处置、预算与砍单顺序见
> [ADR-0007](decisions/0007-m3-scope.md)；收尾口径见 [ADR-0012](decisions/0012-m3-gate-closure.md)；
> 评审记录见 [review-m3-implementation.md](review-m3-implementation.md) /
> [review-m3-implementation-2.md](review-m3-implementation-2.md)。
> M3 增量 **120–185 日**（重估区间 115–190，见 §9.1）；超出 v0.2 的 70–105 由 M2 后移债务与
> WASM/信任链/渲染器无预算项造成。**产品已确认（2026-11-16）：时间无硬指标，预算按 §9.2 追加**。

- [ ] **CPP-A 运行时**：manifest→typed、`api`/WIT/`min_host_version` 版本检查、capability、
      错误码；`plugin-host` 接 cordis-rs（pin/vendor）+ Fiber/Service/Effect/Config 最小切片。
      *（W2：manifest/capability/错误码/Fiber/Service/Effect 按 §3.5 自研退路实现；
      W3：`plugin.toml` TOML 解析 + external-process 运行时；cordis-rs 因离线未接入）*
- [ ] **CPP-B/C 可分发 SDK**：`dshr-plugin-api` 给 semver（非 0.0.0）、`rust-static` /
      `wasm-component` 两套模板、仓库外样例、作者文档。
      *（W3：`plugin-api` 0.1.0 + `plugin-sdk` 0.1.0 + external-process 模板/生成器/文档；
      `rust-static`/`wasm-component` 模板仍缺）*
- [x] **ADC-F 冻结工件**：`adc-v1.md` → `Frozen`、`schema/adc-v1/slots.v1.json`（12 slot）、
      元素 props 字段级、§12 未决清零、冻结 ADR。*（[ADR-0011](decisions/0011-adc-v1-freeze.md)；
      spec 已 `Frozen`）*
- [x] **ADC-S 服务端**（`ui-contributions`）：12 slot、kind/scope、generation、逆序幂等撤销、
      注册校验与可读诊断。*（56 tests：`ADC-SLOT/ELEM/SEC`）*
- [ ] **ADC-R 渲染器**（`ui-gpui`）：元素集 + 绑定/patch/动作 + 未知元素 `Placeholder` 降级 +
      a11y + 渲染预算测试。*（26 元素 + Placeholder + a11y + panel 入口（26+10 tests）；预算实测
      release 最差 total p95 3.78ms（[m3-render-budget](m3-render-budget.md)）；**W5 已接 chat 右栏
      ADC 面板**（`--selftest-adc-panel` PASS）；tool card renderer/交互/事件留 M4）*
- [ ] **WASM Component**：`cpp-v1.wit` + Wasmtime + fuel/epoch/内存/输出/并发五维配额 +
      三故障注入（超时/fuel/内存）。*（**W6/W8：wasmtime `=50.0.1` core-module ABI 已实现并接入
      宿主/产品链路**（`WasmPlugin` + loader + `--selftest-wasm`；`CPP-WASM-001..027` 含资源上限
      多 memory/大 table 负面测试）；`cpp-v1.wit` 仍草案，**Component Model/WIT canonical ABI 未做**
      ——见 [ADR-0012](decisions/0012-m3-gate-closure.md)、[`wasm-host.md`](wasm-host.md)；
      external-process 退路保留）*
- [ ] **DSH L2**：`sidecar-rpc` v2 host-service 代理 + 证据选出的 3 个 adapter + conformance。
      *（W4 端到端 + W5/W8 官方包验收 + W8 sessionProjections：未修改 `dsh-tool-working-directory`
      / `dsh-tool-todo` 装载绿 + `exec.agent` 债务证据、官方 `dsh-storage-domain` 经
      `defineDomain`/`DomainFacility` 与 storage-kv 后端真实往返；`session-projections/v1` +
      `RPC-L2-202..208`/`L2JS-021..028`；**官方包证据=manual gate** `tools/official-l2-gate.ps1`
      （缺 fixture 失败，默认 cargo 6 ignored），storage hub 本体（Cordis Service class）未按普通
      plugin 装载）*
- [x] **最小信任链**：`spec/plugin-trust.md` + 包级 `dshr-trust.json`（摘要强制 + 可选 ed25519）
      + manifest `min_host_version` + 首次安装逐项确认 + 审计 + 降级拒绝（ADR-0009）。
      *（18 tests + `chat --selftest-trust-install` PASS）*
- [ ] **外部作者验收**：`plugin-authoring.md` + 模板 + 仓库外样例 + "未读主程序源码" checklist；
      插件升级/迁移（先补 adc-v1 插件持久化状态章节，否则移出 M3）。
      *（文档/模板/生成器/打包器 + E2E 五种结果 PASS；chat 产品路径 self-test PASS；
      W8：SDK 自包含、生成器把 SDK 复制到 `<out>/sdk/`，样例不引用仓库路径；但 SDK 未发布、
      无 uninstall/真实签名安装 → **G2 按 [ADR-0012](decisions/0012-m3-gate-closure.md) 降级为
      "内部作者 Preview"**，不再宣称"外部作者可发布"）*
- [ ] **M3 债务**（[ADR-0007](decisions/0007-m3-scope.md) §3）：工具并行执行、子会话并行 /
      sidecar 工具继承、默认差异对齐（reasoning effort / LLM 摘要压缩 / 重复调用提醒 /
      `EMPTY_RESPONSE` / approval 模式切换）、`timeoutMs` 透传、Loader `include`/group 子集、
      MCP 最小闭环。*（MCP、默认差异（`M3-DEFAULT-*` 39+）、approval/重复提醒/timeout 接线、
      Loader include/group（`M3-LOADER-*`）、**工具并行（`M3-PARALLEL-*` 16 tests，3×2s 串行
      6.1s→并行 2.0s，sidecar 范围）均完成**；剩余：子会话并行 / sidecar 工具继承（M4 候选））*
- [ ] **条件项**（不计入必达）：Script UI 5 日 spike（[ADR-0008](decisions/0008-script-ui-spike.md)）；
      HTML Widget 独立 WebView Panel（WebView 依赖决策未过则移 M4）；pnpm 捆绑（体积评估未过则移 M4）。

**M3 预算（增量，工程日）**：

| 工作项 | 日 | 工作项 | 日 |
|---|---:|---|---:|
| CPP-A 运行时 | 20–30 | 最小信任链 | 6–10 |
| CPP-B/C SDK 与模板 | 3–6 | conformance 门禁 | 4–6 |
| ADC-F 冻结工件 | 4–7 | 外部作者验收 | 5–8 |
| ADC-S 服务端 | 8–12 | Script UI spike | 5 |
| ADC-R 渲染器 | 20–30 | M2 债务（M3 选做） | 20–39 |
| WASM 主机与配额 | 10–18 | **合计** | **115–190** |
| L2（协议 v2 + 3 adapter） | 8–15 | 计划口径 | **120–185** |

**Exit gate（3 条，均可机检/可复现；证据由 `tools/conformance.mjs` 聚合）**：

1. **G1 契约冻结**：ADC spec 状态 `Frozen`；`slots.v1.json` 与 spec §5 的 12 slot 一一对应；
   元素 props 字段级；§12 未决清零；未知元素 → `Placeholder` + 诊断 + 不 panic、同树其余元素仍渲染。
2. **G2 外部作者可发布**：仓库外目录只用文档 + 模板 + semver SDK + `schema/` 构建并安装
   Tool + Panel 插件；首次安装风险提示/审计/来源校验生效；篡改包与旧版本降级被拒。
3. **G3 隔离与兼容**：WASM 在 fuel 耗尽/内存超限/超时三例分别被隔离、宿主存活、Session 无脏写；
   3 个 L2 service 用**未修改的官方包**做到 ①插件装载绿（`inject` 满足）②至少一个方法经
   `host.service.request/result` 真实往返 ③超时/取消/参数错误各一例；实测耗时回填 §5.1。
   **工具 `execute` 级闭环不纳入 M3**（24/27 官方工具的 `execute` 读 `exec.agent`，属执行上下文债务）；
   `storage` 的"官方包"指官方 `dsh-storage` + `dsh-storage-domain`（无官方工具调用方）。

**未达标转向**（按 [ADR-0007](decisions/0007-m3-scope.md) §5 的砍单顺序，**不得砍 G1–G3 判定项**）：
HTML Widget（6–12 日）→ M4；插件升级/迁移（5–8 日）→ M4；Script UI 实做 → 只留 spike；
ADC 交互元素 → 只读子集冻结、交互延 M4；L2 减到 1 个服务。WASM 若与线程模型冲突 →
`external-process` 退路；外部作者验证受阻 → M3 降级为"内部作者可用 + Preview 标注"。

### M4 — 跨平台 Beta（累计 290–470 工程日 + review 后追加候选 80–140，见 §9.2；不设硬时间上限）

> **状态：进行中（2026-11-16 启动）。** 工作包与 gate 口径见
> [ADR-0013](decisions/0013-m4-scope.md)，证据与未验证项见 [m4-status.md](m4-status.md)。
> W1 完成（uninstall/密钥生命周期/作者签名安装/SDK 离线渠道）；W2 部分完成（快捷键与 a11y 基线，
> G3 待真机复验）；W3 开始（ADC Button onPress→宿主 copy/call/command，openExternal 确认条，
> tool card extra 渲染；Checkbox/Switch/Select/Input/Textarea/Slider 步进已接，拖拽未接）；W8 修复 node 缺失时
> SKIP 记 pass；G2 有 headless `--selftest-recovery` / `--selftest-sidecar-recovery` /
> `--selftest-crash-recovery`；W6 的 todo/working_directory execute 已闭环
> （official gate 6/6）；G4 有 `tools/perf-gate.mjs` + CI step（本机 PASS，Actions 未实跑）；
> W4 的 id/version leak 已收口、native 跨插件批并行已接；同插件/MCP 并发与 trait 冻结未做；三平台 CI 已加 contract/core
> matrix 文件，官方包来源/SHA-256 lock 已接 gate，WSL Linux 核心编译已通过；W3 窗口 draw 硬预算 PASS（RDP 相对基线未过），
> 但 Actions 未实跑、桌面 GPUI 未覆盖。

- [ ] Windows/macOS/Linux 编译与发布管线；签名和更新策略（差分、灰度、回滚）。
- [ ] 多窗口、快捷键、IME、辅助功能、DPI、窗口恢复、主题一致性。
- [ ] GPUI WebView 在各 OS 上的真实窗口/焦点/叠层回归。
- [ ] ACP/MCP 长连接、错误恢复、统一 tracing 与使用量诊断。
- [ ] 安全：Sidecar 权限加固、Secrets、沙箱、插件来源与安装风险提示（完整版）。
- [ ] 性能：大 Session、并行工具、Dock resize、低频闲置消耗、长期内存稳定性。
- [ ] 文档、SDK 示例、至少一个真实 CAD/EDA/电池分析类插件。

**Exit gate（可判定）**：
1. 三平台 CI 全绿；两类来源插件（Rust Native、DSH JS）跨平台回归通过。
2. 发生崩溃/插件失败后，数据可恢复且用户可继续操作。
3. 键盘全流程可达：新建会话→发送→审批→切换面板→关闭，不需要鼠标。
4. 性能基线进入 CI 并回归。

### M5 — 长期可选方向（不计入 Beta 承诺）

- `DSH L3`：选定 Agent Hooks / Session 插件能力。
- `DSH L4`：完整 React Client 插件的 Legacy WebView/Workspace 兼容。
- 更广泛的 `ADC-v1` 元素与 IDE/3D/EDA/仿真面板 SDK。
- 团队任务板、跨会话共享上下文、多用户/远端 Runtime、云端 Agent 工作区。
- 更稳定的 Wasm Component UI、组件市场。

---

## 9. 工作量基线（v0.2 复核）

| 工作流 | 估算工程日 | 说明 |
|---|---:|---|
| GPUI Shell、Dock、窗口与基本输入 | 22–35 | 多窗口、DPI、中文 IME、布局持久化、焦点顺序 |
| Conversation / Markdown / Terminal / Diff / Tool Cards | 40–65 | 终端仿真 12–20；流式 Markdown + 高亮 8–12；Diff 6–10；虚拟化与选择复制 |
| Agent Loop + Session 持久化（含上下文工程） | 35–55 | 计量/压缩/裁剪/外部化、崩溃修复、Schema 迁移、增长控制 |
| Plugin SDK、Lifecycle、UI Contributions（含 ADC-v1 词汇表） | 25–45 | 元素词汇表设计 2–3 周 + slot 注册表实现 |
| Node DSH Sidecar + L1/L2 Bridge | 25–45 | L1 整体 10–15；L2 每 service 1–3 日 |
| 配置、权限、审批、FS/Shell | 20–35 | 审批 answerer 链、策略预设、路径归一化、凭据与授权流程 |
| 打包、兼容、安全、跨平台回归 | 32–55 | 签名、更新通道与回滚、三平台 CI、E2E |
| **模块基准合计** | **199–335** | 相比 v0.1（112–197）约 1.7–1.8× |
| 未知未知项（PoC 推翻假设后的返工） | 25–40 | 单列，不得挪用 |
| **含集成风险的 Beta 预算** | **240–390** | v0.2 基线；M3 后重估 290–470 与 M4 追加候选 80–140 见 §9.1/§9.2，本行不再作为对外口径 |

**这些是判断，不是测量。** M1 结束后必须用实测表替换分项，尤其是 Terminal、Sidecar 与 ADC-v1 三行。

### 9.1 M3 增量预算（ADR-0007 重估，2026-11-12）

上表是 v0.2 的 Beta 全局基线，不承载 M3 的具体工作项。M3 的逐项重估见 §8 M3 预算表：
增量 **115–190 日**（计划口径 **120–185**），相对 v0.2 分配的 70–105 日增加 45–80 日，
来源是：M2 正式后移的 9 项债务（M3 选做 20–39 日）、WASM 主机（10–18 日）、信任链（6–10 日）、
ADC 渲染器（20–30 日）在 §9 均无预算行。对应累计：M3 240–370，M4 290–470（约 13–22 人月）；
2026-11-16 追加 §9.2 的 M4 候选 80–140，若全部纳入累计 **370–610**（约 17–28 人月）。

**对外口径已确认**：
- 2026-11-15：不设硬上限，预算不作为砍单依据；保留 290–470 估算作参考，按范围推进。
- 2026-11-16：**时间无硬指标，同意追加预算**；原 240–390 上限与 ADR-0007 §5 砍单顺序不再启用，
  M4 新增项按 §9.2 追加 80–140 日候选预算，**不得砍 G1–G3 判定项**。

**不含项（明确不计入）**：
- DSH L5 全兼容、任意 React 组件到 GPUI 自动映射。
- 任意 Rust DLL 动态热卸载。
- 团队任务板、跨会话共享内存、多用户/云端 Runtime。
- 3D/EDA/PCB 视口渲染器（单独 API，M4 后评估）。
- 组件市场与计费体系。

**并行说明**：单人等效口径。若 2 人并行，关键路径是 M0 选型 → M1 判死 → M2 Agent Core；UI 核心与 Sidecar 可并行，但**两人共享 M0 的三个结论**，不能各自假设。

### 9.2 M4 追加预算（review 后新增，2026-11-16）

产品确认时间无硬指标；以下为 M3 独立 review / [ADR-0012](decisions/0012-m3-gate-closure.md) 暴露、
且原 M4 基线**未单独预算**的项。按"追加候选"管理：不全部承诺，按优先级纳入 M4，纳入即从本表出账。

| 追加项 | 日 | 依据 |
|---|---:|---|
| Component/WIT canonical ABI + 工具/面板声明读取 | 15–25 | ADR-0012 §3；review "WASM core-module 偏离" |
| SDK 发布（registry/离线渠道）+ uninstall + 真实签名安装/密钥轮换 | 12–20 | review P1-4；G2 现为内部作者 Preview |
| 官方 `execute` 闭环（`exec.agent` 债务 / 官方工具适配） | 10–20 | review §3.3 / P1-5；G3 收窄口径 |
| native/MCP 并发 + `Plugin` trait 冻结（异步/取消/泄漏收口） | 10–18 | W7 只覆盖 sidecar；review P2-11 |
| ADC 交互/事件/tool card + 窗口 draw 预算 + a11y 焦点 | 15–25 | ADR-0011 降级清单；review "panel 只到 AnyElement" |
| 子会话并行 / sidecar 工具继承 / `tool.chunk` 消费 | 8–15 | M3 债务剩余；ADR-0007 §3 |
| 官方包 CI/vendor + GitHub Actions 实跑（含 manual gate 自动化） | 5–8 | review P0/P2-1；当前 CI 未实跑 |
| P2 余项（矩阵元数据、事件顺序文档、session_id chat 接线、flaky 治理） | 5–10 | review P2 表 |
| **合计** | **80–140** | |

> 2026-11-16 进度：该行中的 **uninstall + 密钥吊销/轮换 + 作者签名安装 + SDK 离线渠道**
> 已作为 M4 W1 交付（[m4-status.md](m4-status.md)）；registry、CA 证书体系与自动更新仍归 W7。

若全部纳入：Beta 累计 **290–470 + 80–140 = 370–610 工程日**（约 17–28 人月）。这仍是**估算，
不是对外承诺**；时间无硬指标，按范围与优先级推进。§9 的"不含项"（DSH L5、3D/EDA 视口、市场等）
仍不计入。

---

## 10. 测试、质量与版本策略

### 10.1 Conformance Matrix（CI 产物，不是文档表格）

| 测试领域 | Native | Node DSH | 重要断言 |
|---|---:|---:|---|
| 注册/注销 Tool | ✓ | ✓ | 无遗留注册、ID 冲突可诊断 |
| Tool args / output 校验 | ✓ | ✓ | canonical value/错误一致 |
| 取消/超时/迟到结果 | ✓ | ✓ | 无重复结算、无无限等待 |
| 流式输出与背压 | ✓ | ✓ | chunk 有序、超限转附件、结算幂等 |
| Service 失效/恢复 | ✓ | L2 | Fiber 按依赖规则停用/再激活 |
| Effect 清理/重复卸载 | ✓ | ✓ | 清理至多一次或幂等约束明确 |
| 跨进程崩溃恢复 | N/A | ✓ | generation 失效、在途调用标记 aborted、可重新注册 |
| 跨进程身份映射 | N/A | ✓ | 只传 session id，无 Agent 对象 |
| Session 事件回放 | ✓ | ✓ | UI 与模型历史由同一 Log 导出 |
| 上下文压缩一致性 | ✓ | N/A | 压缩后回放与 UI 一致 |
| Tool UI 展示/元信息 | ✓ | ✓ | Native Generic/Terminal/Diff fallback 正常 |
| 插件 UI 卸载、主题与焦点 | ✓ | L4 | 不残留失效 Panel，不阻塞主 UI |
| WASM 资源超限 | ✓ | N/A | 超时/超内存被隔离，宿主存活 |

实现要求：每个断言对应一个**测试 ID**（如 `L1-CANCEL-003`），CI 生成矩阵报告并按里程碑聚合；发布前必须全绿。

### 10.2 工程质量门槛

- CI：`cargo fmt/clippy/test`、TS 检查、IPC Schema 测试、至少 Windows E2E。
- **E2E 策略**：GPUI 没有 Playwright 级方案，采用"逻辑层测试接缝 + 截图回归"双层：所有 View 必须能在无窗口环境下构造状态并断言；关键界面做截图基线比对。
- 所有第三方依赖精确固定，并定期执行依赖/许可证审计。
- 跨 Runtime 有统一的 trace ID：Session、Agent、Turn、Step、Tool Call、Plugin Generation。
- 版本兼容测试应对一个明确的 DSH tag/commit 运行，而不是笼统声称"兼容 DSH"。
- 每个里程碑必须包括故障注入：杀进程、取消请求、卸载插件、配置错误、网络失败、坏数据、超时、内存超限。
- 性能先建立基线，再制定可审计目标；不在无测量前承诺具体 FPS、启动时间或内存占用。
- **可访问性基线从 M2 开始**：焦点顺序、控件 label、键盘可达；不留到 M4 补。

### 10.3 非功能重点

- 长对话高效虚拟化；尽量避免每个 token 触发整棵 UI 重绘。
- UI 线程不做阻塞 I/O；后台 Tokio 任务向 GPUI 安全发布快照/变更。
- 模型流、终端流和 Tool Output 分别设置背压与上限。
- JS Sidecar 未启动时 Desktop 仍可进入 Native-only 模式。
- 插件错误应被隔离并清楚显示，而不是使整个 Workspace 变空白。

---

## 11. 主要风险与控制策略

| 风险 | 等级 | 处理策略 |
|---|---|---|
| GPUI/GPUI Kit API 破坏性升级 | 高 | 精确 pin；公开 ADC API 与 GPUI 细节解耦；定期迁移窗口 |
| Dynamic GPUI UI 的 ABI/依赖耦合 | 高 | 内置静态 Native View；第三方走 WASM/Script/独立进程 |
| GPUI Shell 仍处于实验性阶段 | 高 | PoC 前不设为强制依赖，预留 HTML/声明式回退 |
| DSH Client 不可透明移植 | 高 | 分 L1–L5 兼容；L4 用独立 WebView/Legacy Workspace |
| Sidecar 直接访问 OS 绕开 Tool Policy | 高 | §7 的 Job Object/受限 token 是 M2 交付物；Rust Policy 不等于沙箱 |
| Cross-runtime Service/Hook 语义漂移 | 高 | 限定白名单接口、每服务 adapter、conformance tests |
| WebView 混排、焦点、叠层限制 | 中高 | 专用 Tab/独立窗口，不让原生 modal 覆盖 WebView |
| 双重 Agent/Session 状态 | 严重 | 架构禁止；Rust 单一权威，Sidecar 只做插件回调 |
| 多平台发布成本 | 中高 | Windows Alpha、再 macOS/Linux；逐平台 CI |
| 插件生态不足 | 中高 | 首版内置高价值工具、开源 SDK/模板和可复用组件 |
| **DSH 上游同时发布桌面端**（新增） | 严重 | M0 产出"与上游差异"一页纸；无法回答则重排优先级 |
| **GPUI 可访问性与 E2E 能力弱**（新增） | 高 | 逻辑层测试接缝 + 截图回归；a11y 基线提前到 M2 |
| **Windows Node 运行时分发**（新增） | 高 | §5.5 捆绑策略；M2 前定案，否则 L1 退出条件不成立 |
| **WASM 资源限制缺失**（新增） | 高 | 配额模型（fuel/内存/超时）随 §3.3 一起交付 |
| **流式工具输出协议缺失**（新增） | 中高 | `tool.chunk` + 背压 + 结算幂等，进 L1 |
| **Provider tool-call 格式漂移**（新增） | 中高 | provider 归一化层；M1 起至少两家验证 |
| **Session Log 增长与保留**（新增） | 中高 | 分段索引、归档、保留期、导出，M2 前定 |
| **自动更新通道**（新增） | 中 | 最小信任链 M3；完整更新通道（差分/灰度/回滚）与证书体系 M4（原"提前到 M3"仅指签名/来源校验） |
| **ADC-v1 词汇表设计不足**（新增） | 高 | 单列为独立交付物；未冻结前不对外承诺元素集 |

---

## 12. 第一个 30 天：三个判死实验（替代 v0.1 的两周清单）

v0.1 的两周清单实际约 3–4 周工作量，且把最危险的问题排在最后。v0.2 改为**先判死、再建设**。

### 12.1 实验安排

**第 1–5 日 · 实验 A：GPUI 能不能承载本产品**

| 子项 | 判定标准 |
|---|---|
| 10k 条日志虚拟化 + 流式 Markdown 滚动 | 稳定 60fps（记录基线值），无明显卡顿 |
| 中文 IME 在自绘输入框 | 候选词、组合态、光标、删除均正常 |
| WebView2 与 GPUI 面板的焦点/DPI/叠层 | 明确记录"能/不能"，不要求全部可用 |

产出：一页结论 + 录屏 + 基线数字。**不通过则不应投入后续 200 日。**

**第 6–12 日 · 实验 B：cordis-rs 能不能用**

只在 Windows 上跑两个候选仓库的 `cargo test`，逐项对照 §3.5 的六项。产出：选型 ADR（含"未通过项"清单）。

**第 13–22 日 · 实验 C：Sidecar 的真实单位成本**

选一个**带持久化 + 带审批**的真实 DSH 插件（不是玩具 Tool），走完：注册 → 调用 → 取消 → 卸载 → 杀进程 → 重注册。记录每个需要手写 adapter 的位置与耗时。产出：L1/L2 单位成本表，回填 §9。

**第 23–30 日 · 回填与决策**

用三个结论重写 M0–M2 的估算与退出条件，发布 `poc-report.md`；此时再决定是否进入 M2。

### 12.2 转向矩阵

| 实验结论 | 行动 |
|---|---|
| A 通过、B 通过、C 成本合理 | 按 M2 计划推进 |
| A 不通过（性能或 IME 硬伤） | 退到 ADR-009 的折中方案：Tauri + 现有 Web Client；Rust Agent Core 保留 |
| B 不通过 | 自研最小 Fiber；M3 的第三方插件平台推迟到 M4 |
| C 成本 > 预估 2× | L2 整体推迟到 M4；M2 只做 L1 + `storage` 一个 adapter |
| A 部分通过（如 WebView 叠层不可用） | 维持 GPUI，取消"工具卡内嵌任意渲染"，执行 §4.2 D 的受限方案 |

---

## 13. 待验证与未冻结的决策

| 问题 | 默认方向 | 决定时间 |
|---|---|---|
| Rust Cordis 选择 | `dshbox/cordis-rs` 优先，`jincdream` 对照 | M0 |
| 是否使用现有 Rust Agent Core | 优先复用经过测试的模块，但保持接口可替换 | M0/M1 |
| GPUI Kit 版本 | 精确 commit pin，防止 API 漂移 | M0 |
| DSH 版本 | 固定官方 DSH tag/commit + vendored Cordis 版本 | M0 |
| Sidecar Transport | stdio、named pipe 或 Unix socket；先以单机为主 | M1 |
| UI Script Runtime | 5 日 spike（ADR-0008）后定选/不选/退路；不作 M3 必达 | M3（W2） |
| ADC-v1 元素集边界 | §4.6.3 为草案；M3 冻结只读 + 交互子集，超预算按 ADR-0007 砍单 | M3（G1） |
| 动态 Native Plugin | WASM Component + WIT 优先；线程模型冲突时 `external-process` 退路 | M3（G3） |
| L2 具体 Service 清单 | 证据驱动：首批 `workingDirectory` + `sessionProjections`（配 `session-log`）+ `storage`；`settings`/`fs` 不进首批（[m3-l2-services.md](m3-l2-services.md)） | M3 |
| MCP / ACP | MCP 最小闭环进 M3；ACP 完整 adapter 移 M4 | M3/M4 |
| 插件信任链 | 哈希强制 + 可选签名 + 首装确认；CA/吊销/更新通道 M4 | M3/M4 |
| Beta 总预算 | 基线 290–470 + M4 追加候选 80–140（§9.2） | **已确认（2026-11-16）：时间无硬指标，预算为估算非对外承诺；不据此砍 G1–G3** |
| 复杂 CAD/EDA 3D Viewport | 单独 Renderer API；原生 GPU vs WebView 实测选择 | M4 |
| 是否集成完整 DSH React Client | 作为独立 Legacy Workspace，不阻塞主线 | M5 之后 |
| Linux Wayland 策略 | Native GPUI 优先；WebView 可能需要 X11/XWayland | M4 |
| 团队任务板 / 跨会话共享上下文 | 不进入 v1 | M5 |

---

## 14. 参考项目与来源

以下是设计调研线索，不等于已完成全部依赖的二进制/发布版验证；正式实施时在 M0 冻结精确版本并重跑测试。

- DeepSeek Harness: <https://github.com/deepseek-ai/deepseek-harness>
- DSH Architecture: <https://github.com/deepseek-ai/deepseek-harness/blob/main/docs/architecture.md>
- DSH Cordis Primer: <https://github.com/deepseek-ai/deepseek-harness/blob/main/docs/cordis-primer.md>
- DSH Web Client Architecture: <https://github.com/deepseek-ai/deepseek-harness/blob/main/docs/subsystems/web-client.md>
- DSH Desktop: <https://github.com/deepseek-ai/deepseek-harness/tree/main/apps/desktop>
- Zed/GPUI: <https://github.com/zed-industries/zed>
- GPUI Kit: <https://github.com/longbridge/gpui-kit>
- GPUI Kit WebView 限制: <https://github.com/longbridge/gpui-kit/blob/main/website/docs/webview.md>
- GPUI Shell（QuickJS 脚本运行时，上游实验路径）: <https://github.com/longbridge/gpui-kit/pull/2821>
- dshbox/cordis-rs: <https://github.com/dshbox/cordis-rs>
- jincdream/cordis-rs: <https://github.com/jincdream/cordis-rs>
- redoop/cordis-rust: <https://github.com/redoop/cordis-rust>
- Rust/Tauri + DSH Sidecar 实例: <https://github.com/FuqiangCraft/dsh-desktop>

---

## 15. 产品层面的判断

本项目的长期护城河不应是"我们使用了 GPUI"，也不应是"可以跑 DSH 插件"。它应该是**可长期复用的 Agent 工作台能力**：稳定的 Native Plugin SDK、跨语言 Tool/Session 契约、可扩展 Client UI、可信执行、可恢复的长期会话，以及高质量的专业工具面板。

因此，项目应把资源优先投向：

**第一**，Rust Agent Core（含上下文工程）与 Native Plugin 生命周期；**第二**，ADC-v1 与 GPU 原生交互；**第三**，通过 Sidecar 借用 DSH Host 插件生态；**第四**，按真实价值选择性扩展 React/WebView 兼容。

**架构红线：Rust 是主系统，GPUI 是可替换的原生渲染底座，DSH 是外部兼容对象，ADC-v1 才是面向长期生态的公开标准。**

---

## 16. 变更记录

### v0.2（2026-11-08，依据 v0.1 评审）

**结构性修改**

1. 删除 P0–P3 双编号，统一为 M0–M5；新增 §0.4 术语对照表（CPP-v1 / ADC-v1 / DSH L1–L5）。
2. 新增 ADR-009（不走 Web 壳捷径）、ADR-010（ADC-v1 是 slot 注册表）、ADR-011（上下文工程是一等能力），对应新增 §1.3 / §4.6 / §6.4。
3. §4.1 从"六种 Contribution"改写为 slot/factory 注册表（§4.6.2 保留六种作为预置 slot 分组）；新增 §4.6 声明式元素词汇表与 §4.7 DSH 移植映射。
4. §5.1 的 L2 改写为"每 service 一个手写 adapter（1–3 日/个）"，首批 `storage` / `settings` / `fs`；新增 §5.5 Node 运行时分发。
5. §5.2 补 `tool.chunk` 流式输出、`session_id` 身份锚点、回调 `depth` 上限、崩溃时在途调用 `aborted` 语义。
6. 新增 §6.4 上下文工程（计量/压缩/裁剪/外部化/一致性）与 §6.5 会话树与子 Agent（v1 范围）。
7. §7 补 Node Sidecar 的 OS 级隔离（Job Object/受限 token/独立 profile）与 WASM 资源配额，并明确"审计不承诺记录进程副作用"。
8. §8 每个里程碑改为 **2–3 条可判定指标 + 未达标转向**；M3 增加最小信任链；M4 增加可访问性判据。
9. §9 估算整体替换：模块基准 199–335（原 112–197），新增"未知未知项 25–40"，Beta 240–390（原 140–230），并单列不含项。
10. §10 将 conformance matrix 定位为 CI 产物（测试 ID + 里程碑聚合），补 E2E 双层策略与 a11y 基线时点。
11. §12 用"30 天三个判死实验 + 转向矩阵"替换原两周清单；原清单约 3–4 周工作量。

**一致性修正**

12. §2.1 明确 UI 归属：Contribution 的清单/生命周期属 Rust，布局/绘制/焦点属 GPUI。
13. §3.2 注明 `ctx.plugin()` 限制只约束 CPP-v1，JS 插件图由 Sidecar 内 Cordis 管理。
14. §8 累计区间不再交叠，M4 明确为"增量 50–100 日"。
15. §13 增补 ADC-v1 元素集边界、L2 服务清单、团队协作不进入 v1 等决策项。

### v0.3（2026-11-12，M3 启动，依据 M3 计划评审）

**结构性修改**

1. §8 M3 按 [ADR-0007](decisions/0007-m3-scope.md) 锁定范围：CPP-A/B/C、ADC-F/S/R、WASM、
   L2、最小信任链、外部作者验收、conformance 门禁；Script UI 降为 5 日 spike（ADR-0008）；
   HTML Widget / 插件迁移 / pnpm 捆绑为条件项。
2. §8 M3 exit gate 由 4 条改为 3 条可机检 gate（G1 契约冻结 / G2 外部作者 / G3 隔离与兼容），
   证据统一由 `tools/conformance.mjs` 聚合。
3. §8 M3 新增预算表（增量 115–190，计划口径 120–185）；§9 新增 §9.1 重估说明，Beta 累计上调为
   290–470；2026-11-16 产品确认时间无硬指标，新增 §9.2 M4 追加候选 80–140（若全部纳入 370–610），
   原 240–390 上限与砍单顺序不再启用。
4. §8 M2 的 MCP/ACP 后移项落账：MCP 最小闭环进 M3、ACP 移 M4；§8 M2 状态标注见 m2-status。
5. §13 更新 UI Script Runtime、ADC 元素边界、WASM、L2 选型，新增 MCP/ACP、信任链、Beta 预算行。
6. §11「自动更新通道」与 §8 M4 的冲突修正：M3 只做最小信任链，完整更新通道与证书体系 M4。

**一致性修正**

7. §8 M3 原 8 条中的"Script UI 定案"移出必达（决策不是交付）；"冻结"拆为 ADC-F（工件）与
   ADC-S/ADC-R（实现），禁止把实现塞进冻结。
8. §8 M3 补 M2 后移债务 6 项（工具并行、子会话并行、默认差异、timeout 透传、Loader 子集、MCP）；
   其余债务在 ADR-0007 §3 逐项标 M3/M4，不再散落在各 m2-*.md。
