# M3 L2 服务选型与契约（证据驱动）

> 状态：Draft（M3 W1D3–5） · 关联：[`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §13（协议正文）、[`PLAN.md`](PLAN.md) §5.1/§13、
> [`review-m3-plan.md`](review-m3-plan.md) §2.1、[ADR-0007](decisions/0007-m3-scope.md) §1 第 7 项
> 标注：带「推断」的条目是估算或判断；其余都能按文中引用的包路径/字段/行号核对。
> 证据来源是 gitignored 抽取物（`.tmp/dsh-tools-all`、`.tmp/dsh-ui`）与随仓库 vendored 的
> `sidecar/dsh-official/node_modules`（0.2.2-rc.1），复现命令见 §9。

## 0. 结论

| 项 | 结论 |
|---|---|
| 首批 3 个服务 | `working-directory/v1`（宿主实现，插件可见名 `workingDirectory`）、`session-log/v1`（宿主实现，配 `sessionProjections` 适配器）、`storage-kv/v1`（宿主实现，配 `storage` 适配器） |
| 方向 | 三个都是 **Sidecar → host**（JS 插件消费宿主服务）：`host.service.request/result/cancel`，见 spec §13.1 |
| 与 PLAN §5.1 的关系 | PLAN 的 `storage` / `settings` / `fs` **与工具插件的一线需求错位**：`settings` 是 Client 面（0/27 工具包注入）、`fs` 已有 M2 专用写路径（§5.1 原表把它当 L2 是重复计费）、`storage` 无官方工具调用方（生态押注）。建议改为 §4 的修正文本 |
| 成本（推断） | 3 个 adapter 6–9 日 + 协议 v2 落地 2–3 日 + conformance 1.5–2 日 = **9.5–14 日**，落在 PLAN §8 的 L2 预算 8–15 日内 |
| 最大风险 | **24/27 官方工具包的 `execute` 读 `exec.agent`**（§6.1）。服务到位 ≠ 工具可用：G3 的"用未修改的官方插件跑通"必须按 §6.2 的可判定定义执行，否则 gate 不可达 |

## 1. 证据与方法

| 证据物 | 内容 | 用途 |
|---|---|---|
| `.tmp/dsh-tools-all/` | 27 个官方工具包 `@deepseek-ai/dsh-tool-*`（0.2.2-rc.1），每个含 `package.json` + `lib/index.js` | 取 `inject`（Cordis 硬依赖）与 `peerDependencies`（包级依赖） |
| `.tmp/dsh-ui/` | 5 个官方 Client UI 包 `dsh-client-ui-*`（0.2.2-rc.1） | 判断 `settings`/`storage` 属于 L4（Client）还是 L2（Host） |
| `.tmp/dsh-tools-all/dsh-tool-cordis/lib/types/api-catalog.js` | DSH `cordis_inspect` 服务的机器可读目录：**107 个 Host Service** 及其方法签名（`SERVICE_API`） | 核对服务方法面与"98 项"口径 |
| `sidecar/dsh-official/node_modules/@deepseek-ai/` | 随仓库 vendored 的官方包（`dsh-working-directory`、`dsh-storage`、`dsh-storage-domain`、`dsh-session-projection`、`dsh-session`、`dsh-fs`、`dsh-workspace`、`cordis@4.1.0` …） | 判断哪些官方实现可以直接复用、哪些只能由 Rust 实现 |
| `sidecar/dsh-official/sidecar-dsh.mjs` | 当前 L1 实现（`PROTOCOL_VERSION = 1`，L39；只 `provide` `tools`/`fs`/`systemPrompt`，L584–586） | 现状基线：`service.*` 全部未实现 |

判据：

- **`inject` 是硬依赖**：Cordis Loader 按插件导出的 `inject` 注入 `ctx.<service>`；缺一个服务，插件
  Fiber 不会进入 `active`（在 DSH 里是装载失败，不是"调用时 undefined"）。因此 `inject` 是"这个插件
  需要 L2 提供什么"的**唯一权威证据**。
- **`peerDependencies` 不是注入证据**：它包含传递使用与工具库（如 `dsh-fs`、`dsh-subagent`），
  不等于 `ctx.<key>`。review-m3-plan §2.1 用 peerDeps 估"解锁 bash 要 5+ 服务"方向正确，
  但精确数字应看 `inject`（见 §2.3）。
- 本文所有 `inject` 取自各包 `lib/index.js` 的 `const inject = [...]`（编译产物等价于
  `export const inject`；`lib/types/index.js` 里的 `export const inject` 是同一份值的类型侧副本）。

## 2. 证据表

### 2.1 27 个官方工具包的 `inject`（全表）

`peerDeps` 只列 `@deepseek-ai/dsh-*` 中与"服务/能力"相关的包（省略 `cordis`、`dsh-llm`、`dsh-agent`、`dsh-tools`）。

| 包（`@deepseek-ai/` 前缀省略） | `inject` | 相关 `peerDependencies` |
|---|---|---|
| `dsh-tool-ask-user` | `tools`, `userQuestions` | dsh-user-questions |
| `dsh-tool-bash` | `tools`, `shell`, `systemPrompt`, `shellEnv`, `workingDirectory` | dsh-shell, dsh-shell-env, dsh-sandbox, dsh-sandbox-policy, dsh-system-prompt, dsh-jobs, dsh-user-approval, dsh-working-directory |
| `dsh-tool-bash-persistent` | `tools`, `terminals`, `workingDirectory` | dsh-output-retention, dsh-terminal, dsh-timeout, dsh-working-directory |
| `dsh-tool-call-timeout-policy` | `tools` | dsh-timeout |
| `dsh-tool-cordis` | `tools`, `cordisInspect` | dsh-app-boot, dsh-cordis-host-runner |
| `dsh-tool-executor-native` | `tools` | — |
| `dsh-tool-executor-ptc` | `tools`, `systemPrompt`, `ptcRuntime` | dsh-ptc-runtime, dsh-sandbox, dsh-sandbox-policy, dsh-scope, dsh-session, dsh-user-approval, dsh-working-directory |
| `dsh-tool-fs` | `tools`, `fs`, `systemPrompt`, `workingDirectory` | dsh-attachment, dsh-fs, dsh-sandbox, dsh-sandbox-policy, dsh-session, dsh-system-prompt, dsh-user-approval, dsh-working-directory |
| `dsh-tool-fs-search` | `tools`, `systemPrompt`, `subprocess`, `workingDirectory` | dsh-output-retention, dsh-session, dsh-spill, dsh-subprocess, dsh-system-prompt, dsh-timeout, dsh-working-directory |
| `dsh-tool-goal` | `agents`, `goals`, `tools`, `systemPrompt` | dsh-goal, dsh-session, dsh-system-prompt |
| `dsh-tool-jobs` | `tools`, `jobs`, `systemPrompt` | dsh-output-retention, dsh-system-prompt, dsh-jobs |
| `dsh-tool-present` | `tools`, `fs`, `agents`, `workingDirectory` | dsh-fs, dsh-session, dsh-working-directory |
| `dsh-tool-pwsh` | `tools`, `shell`, `systemPrompt`, `shellEnv`, `workingDirectory` | dsh-jobs, dsh-sandbox, dsh-sandbox-policy, dsh-shell, dsh-shell-env, dsh-system-prompt, dsh-user-approval, dsh-working-directory |
| `dsh-tool-pwsh-persistent` | `tools`, `terminals`, `workingDirectory` | dsh-output-retention, dsh-terminal, dsh-timeout, dsh-working-directory |
| `dsh-tool-ralph` | `tools`, `workflowEngine`, `subagents`, `systemPrompt` | dsh-subagent, dsh-system-prompt, dsh-workflow |
| `dsh-tool-schedule` | `tools`, `systemPrompt` | dsh-schedule, dsh-subagent, dsh-system-prompt |
| `dsh-tool-session-query` | `tools`, `systemPrompt`, `sessionQuery`, `agents` | dsh-session, dsh-session-query, dsh-system-prompt, dsh-timeout |
| `dsh-tool-skill` | `agents`, `tools`, `skills`, `workingDirectory` | dsh-skill, dsh-working-directory |
| `dsh-tool-str-replace-editor` | `tools`, `fs` | dsh-fs, dsh-output-retention, dsh-sandbox, dsh-sandbox-policy |
| `dsh-tool-subagent` | `tools`, `subagents`, `systemPrompt`, `sessionProjections` | dsh-agent-preset-registry, dsh-session, dsh-session-projection, dsh-subagent, dsh-system-prompt |
| `dsh-tool-subagent-control` | `tools`, `subagents`, `agents` | dsh-session, dsh-subagent |
| `dsh-tool-terminal` | `terminals`, `tools`, `systemPrompt`, `workingDirectory` | dsh-terminal, dsh-output-retention, dsh-system-prompt, dsh-jobs, dsh-working-directory |
| `dsh-tool-todo` | `tools`, `sessionProjections` | dsh-session, dsh-session-projection |
| `dsh-tool-web` | `tools`, `web`, `systemPrompt` | dsh-system-prompt, dsh-web |
| `dsh-tool-workflow` | `tools`, `workflowEngine`, `systemPrompt` | dsh-jobs, dsh-session, dsh-system-prompt, dsh-workflow |
| `dsh-tool-working-directory` | `tools`, `workingDirectory` | dsh-working-directory |
| `dsh-tool-workspace-dependencies` | `tools` | — |

### 2.2 注入频次（27 个包）

| 服务 | 包数 | 首个可解锁的官方插件（`inject` 除 `tools` 外只差这一个） |
|---|---:|---|
| `tools` | 27 | （L1 已提供） |
| `systemPrompt` | 14 | 无单点：其余 14 个都还要 1–3 个别的服务 |
| **`workingDirectory`** | **10** | **`dsh-tool-working-directory`（只差它）** |
| `agents` | 5 | `dsh-tool-goal` 还要 goals/systemPrompt |
| `terminals` | 3 | — |
| `fs` | 3 | `dsh-tool-str-replace-editor` 还要工具写路径 |
| `subagents` | 3 | — |
| `shell` / `shellEnv` | 2 / 2 | bash/pwsh 各还要 3 个 |
| `workflowEngine` | 2 | — |
| **`sessionProjections`** | **2** | **`dsh-tool-todo`（只差它）** |
| **`storage`** | **0** | **无官方工具调用方** |
| `settings` | **0** | 只出现在 Client UI 包（§2.4） |

### 2.3 关键差集（与 PLAN §5.1 的三处错位）

1. **`settings` 是 L4/Client 面，不是 L2**：27 个工具包无一注入 `settings`；`settings` 只出现在
   `.tmp/dsh-ui` 的 `dsh-client-ui-chat|conversation|theme` 的**宿主侧入口**里做**可选**
   `ctx.inject(["settings"], …)`（`chat/lib/index.js:69`、`conversation/lib/index.js:23`、
   `theme/lib/index.js:201`，均为 `useEffect` 式 `child.settings.configure({auto:false}, …)`）。
   这些包同时有 `lib/client.js`（浏览器侧），是 L4 路线（PLAN §5.4/M5）的对象。
2. **`fs` 已有 M2 专用写路径，重复计费**：官方 `dsh-tool-fs` 的读走 Sidecar 内 shim（已实现，
   M2）；写走 `fs.write.request` → Rust 裁决（spec §7.5、`crates/compat-dsh-fs`），**不是** L2
   服务适配。它注入的 `fs`/`systemPrompt`/`workingDirectory` 里只有 `workingDirectory` 是缺口。
3. **`storage` 在官方工具里没有调用方**：0/27。`.tmp/dsh-ui` 里唯一命中是
   `dsh-client-ui-conversation/lib/client.js` 的浏览器 `localStorage`（L4452/L4544–4563），
   与 `ctx.storage` 无关。选它的理由只能是"第三方插件需要持久化"（推断）与"官方 backend 契约
   可复用"（§3.3），**不能**说成"官方需求"。

### 2.4 `dsh-ui` 证据（为什么 `settings` 不属 L2）

| 包 | 宿主侧入口的注入 | 浏览器侧 `lib/client.js` 的 `inject` |
|---|---|---|
| `dsh-client-ui-chat` | 可选 `ctx.inject(["settings"])`（`lib/index.js:69`） | `slots`, `sessions`, `uiWorkspace`, `uiSession`, `uiConversation`, `locale`, `configForms`, `connection`, `remote`, `remote.session`, `sidebarRight`, `fileDrag`（`lib/client.js:15265–15279`） |
| `dsh-client-ui-conversation` | 可选 `ctx.inject(["settings"])`（`lib/index.js:23`） | `slots`, `sessions`, `fileUpload`, `uiSession`, `uiWorkspace`, `locale`, `configForms`（`lib/client.js:22490–22498`） |
| `dsh-client-ui-theme` | 可选 `ctx.inject(["settings"])`（`lib/index.js:201`） | `slots`, `locale`, `remote`, `configForms`（`lib/client.js:2292–2297`） |
| `dsh-client-ui-layout` | — | `slots`, `theme`, `locale`, `shortcuts`（`lib/client.js:655–660`） |
| `dsh-client-ui-renderer` | — | `[]`（`lib/client.js:1832`） |

结论：这些是 **Browser/Client 服务**（`slots`/`uiSession`/`configForms`/`remote`），要 L4 的
Client 运行时才有意义；把它们塞进 L2 会得到"实现了没人注入"的服务。

### 2.5 vendored 官方包：哪些能直接复用

`sidecar/dsh-official/node_modules/@deepseek-ai/`（均 0.2.2-rc.1，`cordis@4.1.0`）里有：

- `dsh-working-directory`、`dsh-storage`、`dsh-storage-domain`、`dsh-session-projection`、
  `dsh-session`、`dsh-session-persistence`、`dsh-fs`、`dsh-system-prompt`、`dsh-tools`、
  `dsh-tool-fs`、`dsh-workspace`、`dsh-agent`、`dsh-sandbox-policy`、`dsh-user-approval` 等。
- **没有** `dsh-storage-json` / `dsh-storage-sqlite`：storage 的介质后端本来就要由 Rust 实现
  （这正是"adapter"的位置），不是额外缺口。

两个直接影响设计的事实：

1. `dsh-working-directory` 的服务实现 `static inject = ["fs", "sessionProjections", "systemPrompt"]`
   （`lib/index.js:13–17`），且 `commit` 里 `agent.session.append("working-directory/change", …)`
   （L143）与 `agent.inject(notice)`（L152）。**因此首批不装载这个官方包**，而是由 Sidecar 提供
   行为等价的 `workingDirectory` shim（方法面见 §3.1）；否则要先补齐 3 个上游服务 + 可用的
   `Agent` 对象，成本翻倍。
2. `dsh-storage-domain` 直接 `inject = ["storage"]`（`lib/index.js:337`），打开域时
   `await backend.kv.open(descriptorOf(spec))`（L388）并调 unit 的
   `loadAll`/`putRecord`/`deleteRecord(s)`/`setGlobal`/`close`（L390/L272/L280/L290/L166/L208）。
   **所以 storage 的跨进程边界就是官方 `kv` backend facet**，语义（schema 校验、layout、版本戳、
   变更事件）全部留在官方 JS 代码里，Rust 只实现 7 个 unit 方法（§3.3）。

## 3. 首批 3 个服务

### 3.1 `working-directory/v1` → 插件可见 `ctx.workingDirectory`

**真实调用方（证据）**：10/27 个官方工具包注入 `workingDirectory`（§2.2）；最小的调用方是
`@deepseek-ai/dsh-tool-working-directory`：`const inject = ["tools", "workingDirectory"]`
（`.tmp/dsh-tools-all/dsh-tool-working-directory/lib/index.js:10`），`execute` 里
`ensure(exec.agent, signal)` / `set(exec.agent, args.cd, signal)`（L39–40）。bash/pwsh 是同一服务的
重用户，但它们各自还差 `shell`/`shellEnv`/`systemPrompt`（+ 动态 `ctx.inject(["jobs"])`，
`dsh-tool-bash/lib/index.js:720`），所以**不要**用 bash 做首个验收目标。

**语义**：目录的唯一权威是 Rust Session Log 的 `working-directory/change` 事件；Sidecar 的
`ctx.workingDirectory` 是薄代理，不缓存目录、不落盘。

| 方法 | kind | args | result | timeout_ms | idempotent |
|---|---|---|---|---|---|
| `get` | read | `{}`（会话来自信封 `session_id`） | `{cwd}` | 5000 | true |
| `ensure` | write | `{}` | `{cwd, recovered}` | 30000 | true |
| `set` | write | `{path}` | `{cwd}` | 30000 | true |

- `path`：绝对路径或相对**当前目录**的路径；空串 → `E_SERVICE_ARGS`；不存在/不是目录 → `E_SERVICE_ARGS`
  （`detail.reason = "not-a-directory"`）。
- `ensure` 的 `recovered=true` 表示原目录消失、已回落到 `SessionHeader.cwd ?? default_directory`，
  并已 append 事件 + 给 Agent 注入提示（提示在宿主侧，不进 Sidecar）。
- 三方法都幂等：`set` 到相同路径不产生事件（与官方实现一致，见 `dsh-working-directory:141–143`）。
- **返回形状对齐官方签名**：官方 `ensure`/`set` 都返回 `string`，`get` 返回 `string`；Sidecar 的
  shim 必须把 wire 的 `{cwd}`/`{cwd, recovered}` 解包成 `string` 再交给插件（`recovered` 只用于
  宿主诊断）。否则 `dsh-tool-working-directory` 的 `render`（`[{type:"text", text: value.cwd}]`）
  会拿到对象而渲染错。

**Rust adapter 映射**：

| 环节 | 映射 |
|---|---|
| 身份 | 信封 `session_id` → `Agent`（v1 既有边界，`compat-dsh-host`）；`get/ensure/set` 都要求 `session_id`，缺 → `E_SERVICE_ARGS` |
| 默认目录 | 宿主配置的 workspace root（当前 `DSHR_WORKSPACE`）；侧车不参与判定 |
| `get` | 读 Session Log 的最后一个 `working-directory/change`；没有则 `SessionHeader.cwd ?? default_directory`（**不碰 FS**，与官方"without filesystem I/O"一致） |
| `set` | Rust 侧校验目录存在（`fs.stat`）→ 归一化 → `session.append("working-directory/change", {cwd})` → 返回 canonical 绝对路径 |
| `ensure` | 同上 stat；目录消失 → 用 `header.cwd ?? default` 恢复 → append 事件；`recovered` 标志回传 |
| 并发 | 按 `session_id` 串行化（官方用 per-session promise 链，`dsh-working-directory:157–168`）；两请求同 session 时后到者排队 |
| 通知 | "目录已改变"的 user message 由宿主 agent-context 产生（M2 已有），不进协议 |

**错误码**：`E_SERVICE_ARGS`（参数/路径非法、缺 `session_id`）、`E_SERVICE_UNAVAILABLE`（未知/已销毁
session）、`E_SERVICE_TIMEOUT`、`E_SERVICE_DENIED`（白名单/未协商）、`E_INTERNAL`（FS 异常）。

**conformance（`RPC-L2-1xx`）**：

| ID | 断言 |
|---|---|
| `RPC-L2-101` | 未修改的 `dsh-tool-working-directory` 装载绿（`inject` 满足），`working_directory` 工具注册成功 |
| `RPC-L2-102` | `get` 往返：`session_id` 映射到 Agent，返回 committed cwd；无事件时回 `header.cwd ?? default_directory` |
| `RPC-L2-103` | `set` 后 Session Log 恰好一条 `working-directory/change`，再次 `get` 返回新值；幂等重放不新增事件 |
| `RPC-L2-104` | `set` 不存在/非目录/空串 → `E_SERVICE_ARGS`，Session Log 无写入 |
| `RPC-L2-105` | `ensure` 在目录被删后恢复 `header.cwd`，`recovered=true`，且事件已 append |
| `RPC-L2-106` | 跨会话隔离：s-1 的 `set` 不影响 s-2 的 `get` |
| `RPC-L2-107` | `set` 在途时 `tool.cancel` → `host.service.cancel` → 插件侧 `E_CANCELLED`，宿主不写事件 |
| `RPC-L2-108` | generation 变化：在途 `get` 以 `E_SERVICE_UNAVAILABLE` 结算，迟到结果丢弃 |

**成本（推断）**：1.5–2.5 日（adapter 1–2 + conformance 0.5）。**风险**：shim 与官方实现的
"事件顺序/提示语"漂移（用 §3.1 的两条硬断言锁住）；`ensure` 的 FS stat 可能触发慢盘（timeout
30 s，取消优先）。

### 3.2 `session-log/v1` + `ctx.sessionProjections` 适配器

**真实调用方（证据）**：2/27 注入 `sessionProjections`：`dsh-tool-todo`
（`const inject = ["tools", "sessionProjections"]`，`lib/index.js:12`，`register` 在 L81–95）与
`dsh-tool-subagent`（`lib/index.js:294–298`，`register` 在 L352）。`todo` 是最小目标——除 `tools`
外只差这一个服务。

**为什么不是"把 `sessionProjections` 整个代理到宿主"**：投影单元是 **JS 函数**
（`init(header)` / `apply(state, event)` / `wire.view`，见 `dsh-tool-todo:81–95`），而协议禁止
函数跨进程（spec §11.2/§13.1）。正确切分：

- **注册表留在 Sidecar**：直接复用 vendored `@deepseek-ai/dsh-session-projection@0.2.2-rc.1`
  （未修改）。它构造时监听 Cordis 事件 `session/created`（仅 `seq === 0`）与
  `session/event(session, event)`（`lib/index.js:67–81`），需要的是"会话对象 + 事件流"，
  而不是宿主方法。
- **宿主提供数据面**：`session-log/v1`（`session.describe` + `events.read`）与 v1 的
  `event.subscribe`/`event.emit`（spec §13.7 的 `session.event` 约定）。
- **Sidecar 造一个最小 `SessionView`**：`{id, header, seq, inheritedEventCount}`，作为注册表的
  WeakMap key 与 `handle.stateOf(session)` 的入参；不含 `Agent`，也不暴露 append（append 属于
  `exec.agent.session` 的写通道，见 §6.1）。

| 方法 | kind | args | result | timeout_ms | idempotent |
|---|---|---|---|---|---|
| `session.describe` | read | `{}`（会话来自信封 `session_id`） | `{header:{cwd?, created_at, version, parent?, delegation_depth?}}` | 5000 | true |
| `events.read` | read | `{from_seq, max_events}` | `{events:[{seq,type,data}], next_seq, eof}` | 10000 | true |

- `events.read` **必须分页**：`max_events ≤ 500`、单页序列化 ≤ 512 KiB；`eof=true` 表示已到
  Session Log 末尾。`events` 的 `seq` 严格递增，`next_seq = 最后一条事件 seq + 1`。
- 冷启动 fold：Sidecar 先 `session.describe` → 建 `SessionView(seq=0)` → 发 `session/created`
  → 循环 `events.read` 折叠（spec §13.7 的 gap 补齐同一路径）。
- 事件推送：宿主对 `event.subscribe {event:"session.event"}` 的订阅者按提交顺序发
  `event.emit {event:"session.event", payload:{session_id, seq, type, data}}`；**不重放**。

**Rust adapter 映射**：

| 环节 | 映射 |
|---|---|
| `session.describe` | 读 SessionHeader（`crates/session-store`；`id`/`created_at`/`cwd`/`parent_session_id`/`delegation_depth`，持久化来源是 [`spec/session-log.md`](spec/session-log.md) 的 `session.created` 事件） |
| `events.read` | 读 `SessionHandle.read(from_seq, max)`（M2 已有）；按 `max_events`/字节上限截断，返回 `next_seq`/`eof` |
| 推送 | 宿主在 Session 提交事件后，向订阅该 session 的 Sidecar 发 `event.emit`；序号单调，单会话内保序 |
| 隔离 | 只推订阅过的 `session_id`；越权订阅 → `E_SERVICE_DENIED` |
| generation | 新 generation 重新订阅；旧订阅失效，Sidecar 用 `events.read` 从最后 `seq+1` 续读 |

**错误码**：`E_SERVICE_ARGS`（`from_seq` 非法/非整数）、`E_SERVICE_UNAVAILABLE`
（未知/已销毁 session）、`E_TOO_LARGE`（单页超限或 `max_events` 越界）、`E_BACKPRESSURE`（事件积压）、
`E_SERVICE_TIMEOUT`、`E_SERVICE_DENIED`。

**conformance（`RPC-L2-2xx`）**：

| ID | 断言 |
|---|---|
| `RPC-L2-201` | 未修改的 `dsh-tool-todo` 装载绿（`inject` 满足），`todo_write` 注册成功，`sessionProjections.register({key:"todos"})` 的 handle 就绪 |
| `RPC-L2-202` | `session.describe` 返回 header，注册的 `init(header)` 能拿到它（`todo` 忽略 header；`working-directory` 的 `init` 用 `header.cwd`——见官方 `dsh-working-directory:38`） |
| `RPC-L2-203` | `events.read` 分页：seq 严格递增、`next_seq` 正确、`eof` 在最后一页为 true |
| `RPC-L2-204` | `event.emit(session.event)` 驱动 `apply`：一条 `todo/write` 事件后 `handle.stateOf(session)` 等于事件载荷 |
| `RPC-L2-205` | gap 检测：故意丢一条 `event.emit` → Sidecar 用 `events.read(from_seq)` 补齐，状态与不丢时一致 |
| `RPC-L2-206` | `max_events > 500` 或单页 > 512 KiB → `E_TOO_LARGE` 且不返回半页 |
| `RPC-L2-207` | 冷启动 fold 超 2000 事件 → 分页完成并记 `projection.backfill` 诊断（不阻塞其它工具） |
| `RPC-L2-208` | 插件卸载/revoke → 该 session 的推送停止，`ctx.sessionProjections` 的 handle 释放，不泄漏 Fiber |

**成本（推断）**：2.5–3.5 日（事件通道与 gap 补齐 1–1.5、`SessionView`/注册表接线 1、conformance 0.5–1）。
**风险**：冷启动 fold 成本（大 session 的 `events.read` 页数与并发）；事件保序/重入（`apply` 在
Committed 事件派发内同步执行，见官方 `onEventChanged` 注释）；**依赖 v1 `event.*` 通道先落地**——
它是 M2 原本未实现的 kind，属于 L2 的顺序前置。

### 3.3 `storage-kv/v1` → 插件可见 `ctx.storage`（+ 官方 `dsh-storage-domain`）

**证据（含反向证据）**：

- 0/27 官方工具包注入 `storage`（§2.2）。唯一相关的是 vendored 的**宿主侧**官方包
  `dsh-storage-domain`（`inject = ["storage"]`，`lib/index.js:337`）与 `dsh-workspace`
  （`static inject = ["storageDomain", "sessionPersistence"]`，`lib/index.js:355`）。
- `dsh-storage` README 的 "The backend contract" 明确：backend 拥有一种介质、暴露可选的数据形态
  facet，**`kv` 是唯一 facet**；unit/table 名受 `UNIT_NAME_RE` 约束；读接受当前或声明的
  `compatibleVersions`，不支持的版本戳拒绝 `version-mismatch`；写原子地打上当前版本戳。
- 因此 storage 的跨进程切面 = `kv` facet 的 unit 操作（7 个），**不是** `ctx.storage` 的
  `mount/form/backend` 三个注册表方法（那些留在 Sidecar，由官方包自己实现）。

| 方法 | kind | args | result | timeout_ms | idempotent |
|---|---|---|---|---|---|
| `unit.open` | write | `{unit:{name, version, compatible_versions?, layout:"single" / "per-record", tables?}}` | `{unit_id}` | 15000 | true（同 descriptor 重开返回同 unit） |
| `unit.loadAll` | read | `{unit_id}` | `{tables:{[table]:{[key]:value}}, global?}` | 30000 | true |
| `unit.putRecord` | write | `{unit_id, table, key, value}` | `{}` | 10000 | true（last-write-wins） |
| `unit.deleteRecord` | write | `{unit_id, table, key}` | `{}` | 10000 | true |
| `unit.deleteRecords` | write | `{unit_id, table, keys[]}` | `{}` | 10000 | true |
| `unit.setGlobal` | write | `{unit_id, value}` | `{}` | 10000 | true |
| `unit.close` | write | `{unit_id}` | `{}` | 5000 | true |

- **`unit_id` 是必需参数**（`unit.open` 除外，它返回 `unit_id`）：v2 是无状态 RPC，宿主不能靠
  「最近打开的 unit」推断目标；实现见 `crates/compat-dsh-host/src/storage_kv.rs`。JS 适配器必须
  在每个 `unit.*` 方法里带上 `unit_id`（官方 `kv` facet 是面向对象接口，适配层在 Sidecar 侧做映射）。
- `unit_id` 由宿主分配，作用域 `(plugin_id, unit.name)`；同一插件重复 `open` 同 descriptor 返回同
  `unit_id`（幂等），不同 descriptor 版本冲突 → `E_INTERNAL` + `detail.storage_code="version-mismatch"`。
- `unit.loadAll` 结果必须 ≤ 768 KiB（spec §13.8）；超限 → `E_TOO_LARGE`（v2 不做游标，风险见 §6.3）。
- **不实现** `backupRecord`：声明 `invalidRecords:"backup-and-skip"` 的域在遇到坏记录时会抛错
  而不是跳过（官方 `dsh-storage-domain:399` 的 `unit.backupRecord === void 0` 分支）。这是 v2
  的显式限制，记入 §6.3。

**Rust adapter 映射**：

| 环节 | 映射 |
|---|---|
| 介质 | 宿主 profile 目录下的按 `plugin_id/unit` 分文件 JSON（`storage-kv` 的 v2 介质选择，Q7）；布局 `single` = 一个文件，`per-record` = 每表一条记录一个 key |
| 版本戳 | unit 文件头写 `{version, compatible_versions}`；读时校验，不匹配 → `E_INTERNAL` + `detail.storage_code="version-mismatch"` |
| 原子性 | 单 unit 写串行化（Rust 侧 per-unit mutex）＋临时文件 + rename，保证"不丢已写记录" |
| schema | 校验只在 JS 侧（官方 `dsh-storage-domain` 用 zod）；宿主只保证"有限 JSON + 命名规则"（`UNIT_NAME_RE`） |
| 变更事件 | `ctx.storageDomain` 的域变更事件在 Sidecar 内产生（官方实现），跨进程不传（v2 不需要） |
| 作用域 | `(plugin_id, unit.name)`；插件 A 不能 open 插件 B 的 unit → `E_SERVICE_DENIED` |

**错误码**：`E_SERVICE_ARGS`（非法 unit 名/表名、值不是有限 JSON）、`E_SERVICE_UNAVAILABLE`
（unit 未打开/已关闭）、`E_SERVICE_DENIED`（跨插件作用域）、`E_TOO_LARGE`、`E_INTERNAL` +
`detail.storage_code`（`version-mismatch` / `missing-key` / `closed` / `already-open` /
`facet-unsupported` / `unknown-state`）。

**conformance（`RPC-L2-3xx`）**：

| ID | 断言 |
|---|---|
| `RPC-L2-301` | 官方 `dsh-storage` + `dsh-storage-domain` 装载绿，`ctx.storage.domain` 可解析（form mounted） |
| `RPC-L2-302` | 官方 `defineDomain` 声明的域 `open` 成功；同 descriptor 重开幂等 |
| `RPC-L2-303` | `putRecord` → `loadAll` 往返一致；`single` 与 `per-record` 两种 layout 各一例 |
| `RPC-L2-304` | 版本戳不支持（`version:2` 读 `version:1` 数据且未声明兼容）→ 打开失败 `version-mismatch` |
| `RPC-L2-305` | `deleteRecord` / `deleteRecords` 幂等且正确（不存在 key 不报错、计数正确） |
| `RPC-L2-306` | `setGlobal` / `global` 语义：未写过时 `null` 触发 `initial`；写过之后为存储值 |
| `RPC-L2-307` | `loadAll` 超 768 KiB → `E_TOO_LARGE`，已有记录不被截断 |
| `RPC-L2-308` | 同 unit 两个并发 `putRecord` 串行化，两条记录都在（不丢写） |
| `RPC-L2-309` | 跨插件作用域：插件 A 的 unit 不能被插件 B `open`/读写 |
| `RPC-L2-310` | `close` 幂等；generation 失效后重开能读回旧数据（持久化） |

**成本（推断）**：2–3 日（7 方法 + 版本戳/布局 + 串行化 + conformance）。

**风险**：

1. **没有官方工具调用方**——G3 字面要求"3 个 L2 service 用未修改的官方插件跑通"，storage 只能用
   **官方 storage 家族包**（`dsh-storage` + `dsh-storage-domain` 未修改装载、用官方 `defineDomain`
   定义域）来验证，而不能用工具插件。若 Lead 要求"每个服务都有未修改的官方**工具**调用方"，
   建议把 storage 换成 `systemPrompt`（14/27 注入，宿主→JS 方向，见 §5.2），storage 降到 M4。
2. 大域 `loadAll` 超 768 KiB 直接失败（v2 无游标）——首个真实第三方插件出现时再评估。
3. `invalidRecords:"backup-and-skip"` 不支持（§3.3 显式限制）。

## 4. 与 PLAN §5.1 / §13 的差异与建议修正文本（供 Lead 改 PLAN）

| 位置 | 现文 | 证据 | 建议 |
|---|---|---|---|
| §5.1 "L2 口径（v0.2 修正）" | "DSH 本机实测 Host Service 有 **98 项**" | `api-catalog.js` 的 `SERVICE_API` 在 0.2.2-rc.1 是 **107 项** | 改为"107 项（0.2.2-rc.1，`dsh-tool-cordis` 的 `SERVICE_API`）；数量随版本变，口径按版本记录" |
| §5.1 首批三个表 | `storage` / `settings` / `fs` | §2.3：settings 0/27、storage 0/27、fs 已有 §7.5 专用路径 | 换成 `workingDirectory` / `sessionProjections` / `storage`，理由各一行（见下方替换文本） |
| §13 决策行 | "L2 具体 Service 清单｜由真实插件需求决定，首批 storage/settings/fs｜M3" | 同上 | 改为"首批 `workingDirectory`+`sessionProjections`（有官方调用方）+`storage`（生态押注）；清单与证据见 `docs/m3-l2-services.md`" |
| §8 M3 清单 L2 行 | "`sidecar-rpc` v2 host-service 代理 + 证据选出的 3 个 adapter + conformance" | 已正确 | 追加"（协议正文 spec §13；服务契约与成本 `m3-l2-services.md`）" |
| §8 M3 G3 | "3 个 L2 service 用**未修改的官方插件**跑通 `RPC-L2-*`" | §6.1：24/27 的 `execute` 读 `exec.agent`，服务到位也跑不动工具执行 | 补一句判定口径（见下方替换文本） |

**建议替换文本（可直接贴）**：

```text
**L2 口径（v0.2 修正）**：DSH 0.2.2-rc.1 的 `cordis_inspect` 目录列出 107 项 Host Service，
且大量方法以活的 `Agent`、回调、流式端点作为参数。L2 不是"配一份白名单"，而是 N 个小型跨进程
契约。**首批三个（证据驱动，见 `docs/m3-l2-services.md`）**：

| service | 真实调用方（证据） | 关键难点 |
|---|---|---|
| `workingDirectory` | 10/27 官方工具包注入；`dsh-tool-working-directory` 只差它 | 目录权威在 Session Log；per-session 串行化 |
| `sessionProjections` | `dsh-tool-todo`、`dsh-tool-subagent` | 投影单元是 JS 函数：注册表留 Sidecar，宿主只给事件流 + 分页读 |
| `storage` | 官方工具无调用方（生态押注）；官方 `kv` backend 契约可复用 | unit/版本戳/布局；无游标（v2 限制） |

`settings`（Client 面，0/27 工具包注入）与 `fs`（M2 已有 `fs.write.request` 专用写路径）**不进
首批**：前者随 L4 走，后者在 §7.5 已交付。`systemPrompt`（14/27 注入）是 storage 的替补候选。
```

```text
G3 的 L2 部分判定：每个 service 用**未修改的官方包**做到 ①插件装载绿（`inject` 满足）
②至少一个方法经 `host.service.request/result` 真实往返 ③错误/超时/取消各一例；
工具 `execute` 级闭环不纳入 M3（24/27 官方工具的 `execute` 依赖 `exec.agent`，属执行上下文债务）。
storage 的"官方包"指官方 `dsh-storage`+`dsh-storage-domain`（无官方工具调用方）。
```

## 5. 不选 / 后移的服务

### 5.1 `fs`（不选，已在 M2 交付）

官方 `dsh-tool-fs` 的读面用 Sidecar 内的 shim（M2），写面走 `fs.write.request` → Rust 裁决
（spec §7.5、[`m2-official-write.md`](m2-official-write.md)）。把它再列为 L2 服务等于为同一能力建第二条路径。
真正缺的是它注入的 `workingDirectory`（§3.1）。

### 5.2 `systemPrompt`（替补候选，14/27 注入）

- 证据：14 个官方工具包注入 `systemPrompt`（§2.2）；当前 Sidecar 只提供 3 个方法的空壳
  （`sidecar-dsh.mjs:567–580`：`section`/`getSectionOrder`/`sections`）。
- 为什么不是首批：它的完整语义是"注册表 + 宿主装配"（`section/context/variable/tools/assemble`，
  见 `api-catalog.js` 的 `systemPrompt` 条目，9 个方法），**方向是 host→JS**（宿主装配时向
  Sidecar 取文本），需要 `service.expose` 那条腿，协议面比首批三个大。
- 如果 G3 要求"每服务都有未修改官方**工具**调用方"，用 `systemPrompt` 换掉 `storage` 更稳（推断：
  2–3 日 adapter，14 个官方包可直接验证注入与 section 文本）。

### 5.3 `settings`（后移 L4/M5）

证据见 §2.4：只有 Client UI 包的可选注入，服务面是 `describe/update/replace/mutate` 与
表单投影（`api-catalog.js` 的 `settings`，6 个方法），依赖 profile patch 文件与 Client 渲染。
放进 L2 会得到一个"没有 Host 消费者"的服务。PLAN §5.4 已把它归 L4。

### 5.4 其他高价值但高成本

`shell`/`shellEnv`/`jobs`/`terminals`（bash/pwsh 的组合）、`agents`/`subagents`（活的 Agent 对象 +
派生 Session）、`web`/`subprocess`、`cordisInspect`（调试面）、`workflowEngine`/`ptcRuntime`
（脚本运行时）：都需要"活对象/回调/流式端点"，属 L3/后续阶段；先做三个只读/薄写服务是证据下
风险最低的顺序（推断）。

## 6. 风险与验收定义

### 6.1 关键风险：服务到位 ≠ 工具可用（24/27）

`execute` 内读 `exec.agent` 的包：24/27（grep `exec.agent`，见 §9 复现）。其中：
`dsh-tool-working-directory` 直接 `throw`（L39）、`dsh-tool-todo` 直接 `throw`（L174）、
`dsh-tool-fs` 做了降级（`exec.agent === void 0 ? void 0 : await ctx.workingDirectory.ensure(...)`，
L175）——也就是说**同一个服务在不同工具里可能是硬依赖或软依赖**。

结论：即使 `workingDirectory`/`sessionProjections`/`storage` 全部到位，工具 `execute` 仍会因为
`exec.agent.session.append(...)`、`exec.agent.ctx.fiber.assertActive()` 等活对象调用而失败。
这是**执行上下文（L1）的债务**，不属于服务契约；M3 若要做工具闭环，必须先补
"`ToolRunContext` 的 agent/session facade"（新契约，建议在 `sidecar-rpc` 的 `tool.invoke` 桥里
加 `session_append` 能力位，属另一项工作）。

### 6.2 G3 的可判定定义（建议采纳）

| 判定 | 方法 | 证据 |
|---|---|---|
| 装载绿 | 未修改官方包 `plugin.load` 后 `plugin.loaded`，且 `tool.register` 成功 | `--selftest-dsh` 日志 + conformance 脚本 |
| 方法往返 | 每个服务至少 1 个方法经 `host.service.request/result`（真实 Session/真实存储） | 耗时/帧日志（回填 PLAN §5.1） |
| 失败面 | 超时、取消、参数错误各 1 例 | `RPC-L2-105/107/104` 等 |
| 不做的事 | 工具 `execute` 级闭环（依赖 §6.1 债务） | 在报告里写明"未纳入 M3" |

### 6.3 其他风险

| 风险 | 证据 | 控制 |
|---|---|---|
| `event.*` 通道是 M2 未实现的 kind | `sidecar-dsh.mjs` 的 `handle()` 没有 `event.*` 分支（L685–759） | 列为 L2 的第一个实现前置：先实现 `event.subscribe/unsubscribe/emit` + `session.event` 推送 |
| 冷启动 fold 成本 | `dsh-session-projection` 的 `init` 折叠全量 log | 单页 ≤ 500 事件/512 KiB；单次 ≤ 2000 事件；超出记 `projection.backfill` |
| 与官方实现漂移 | `dsh-working-directory` 的行为（事件名/提示语/幂等） | `RPC-L2-103/105` 锁事件与幂等；未来若可装载官方包，优先切回官方实现 |
| storage 无游标 | spec §13.8 | 768 KiB 上限 + `E_TOO_LARGE`；第三方出现真实大域再升 major |
| 协议错误码闭集未同步 | `crates/compat-dsh-protocol/src/lib.rs:260–272` 的 `ERROR_CODES` 只有 v1 码 | 实现顺序第 1 步：扩 `ERROR_CODES`/`is_retryable_sidecar_error`、`PROTOCOL_VERSION=2`、4 个 kind 常量 |
| 方法表与插件 `inject` 不一致 | 白名单三条件（spec §11 第 6 条） | 装载期 fail-fast：`plugin.failed`，无工具注册（`RPC-L2-011`） |

## 7. 实现顺序（第一个改动点）

1. **协议常量与类型**：`compat-dsh-protocol` 升 `PROTOCOL_VERSION=2`、加 4 个 kind 常量、
   `CAP_SERVICE_PROXY/EXPOSE/ATTACH`、方法表类型、`ERROR_CODES` 扩集（对齐 spec §9/§13.10）。
2. **v1 事件通道**：`event.subscribe/unsubscribe/emit`（`session.event` 推送是 §3.2 的前置）。
3. **`host.service.*` 路由骨架**：白名单方法表、args 二次校验、超时/取消/幂等窗口/背压、
   `service.late_result` 诊断（spec §13.4–§13.5）。
4. **三个 adapter**：`working-directory/v1` → `session-log/v1` → `storage-kv/v1`（依赖从薄到厚）。
5. **conformance**：`RPC-L2-0xx（协议）→ 1xx → 2xx → 3xx`；每服务实测耗时回填 PLAN §5.1。

## 8. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| L2-Q1 | `storage` 是否保留在首批（无官方工具调用方） | 保留（官方 backend 契约 + 第三方刚需，推断）；若 G3 严判则换 `systemPrompt` | W2D3–4 |
| L2-Q2 | 工具 `execute` 的 `exec.agent.session.append` 是否进 M3 | 不进（另一项契约，建议 M3 后债务或 M4） | 与 Lead 定 |
| L2-Q3 | `sessionProjections` 是否也做 `snapshot`/`checkpoint` 的宿主读面 | v2 不做（注册表在 Sidecar，宿主不需要 DSH 投影） | M3 实现时 |
| L2-Q4 | storage 介质（JSON 文件 vs sqlite） | JSON 文件；sqlite M4 | 实现前 |
| L2-Q5 | `workingDirectory` 的 `default_directory` 取 workspace root 还是 profile 配置 | workspace root（与 `DSHR_WORKSPACE` 一致） | 实现前 |

## 9. 复现命令

以下脚本一次性使用、**不提交**（`.tmp/` 已 gitignored）：

```powershell
# 1) inject 全表
Get-ChildItem .tmp\dsh-tools-all -Directory | ForEach-Object {
  Select-String -Path "$($_.FullName)\lib\index.js" -Pattern 'const inject\s*=' -Context 0,12 |
    Select-Object -First 1
}
# 2) 注入频次（node 一行版）
node -e "const{readdirSync,readFileSync,existsSync}=require('fs');const r='.tmp/dsh-tools-all';const f=new Map();for(const d of readdirSync(r)){const p=r+'/'+d+'/lib/index.js';if(!existsSync(p))continue;const m=readFileSync(p,'utf8').match(/const inject\s*=\s*(\[[\s\S]*?\])/);if(!m)continue;for(const s of eval(m[1]))f.set(s,(f.get(s)||0)+1)};console.log([...f].sort((a,b)=>b[1]-a[1]))"
# 3) execute 是否依赖 exec.agent
Select-String -Path .tmp\dsh-tools-all\*\lib\index.js -Pattern 'exec\.agent' -List
# 4) DSH Host Service 目录（107 项）
node -e "import('./.tmp/dsh-tools-all/dsh-tool-cordis/lib/types/api-catalog.js').then(m=>console.log('services:',m.SERVICE_API.length, m.SERVICE_API.filter(s=>['workingDirectory','sessionProjections','storage','settings','fs','systemPrompt'].includes(s.key)).map(s=>s.key)))"
# 5) 契约校验
node schema/check.mjs; node tools/check-links.mjs
```
