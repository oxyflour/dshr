# M2 — 装载 DSH 官方插件（2026-11-11）

**结论**：官方 DSH 插件可以**不改一行代码**装进我们的 Sidecar，注册进 Rust `ToolRegistry`，
由真实 LLM 通过标准 tool_call 调用并拿到结果。已验证两个官方包：

- `@deepseek-ai/dsh-tool-workspace-dependencies@0.2.2-rc.1`（无额外服务依赖）
- `@deepseek-ai/dsh-tool-fs@0.2.2-rc.1`（只读 `read`，依赖 `fs` + `systemPrompt` 服务）

## 证据

```text
[selftest-dsh] items=3 registered(workspace=true, read=true)
  你    · 用工具试一下
  工具  · load_workspace_dependencies [sidecar] · 成功
      ↳ 通用 · load_workspace_dependencies · {"python":"C:\\...\\python.exe",...
  助手  · 工具返回 {"python": "...", "node": "...", "pnpm": "...", ...}
[selftest-dsh] read_tool_ok=true
[selftest-dsh] replay_equal=true
[selftest-dsh] PASS
```

真实 DeepSeek 经官方 `read` 工具读文件（`DSHR_SIDECAR=dsh`，`chat --live-chat`）：

```text
[live-chat] turn=1 ms=1916 stream_updates=16
             reply=第一行是：`# Native Agent Desktop — workspace`
[live-chat] persisted_turns=1 history_messages=4
```

`history_messages=4` 说明消息序列是 `user → assistant(tool_calls) → tool → assistant`，
即官方插件走的是我们 A15 实现的严格工具协议，而不是特例通道。

## 架构

```text
本机 DSH 安装 app.asar
   │  node tools/dsh-vendor.mjs（只读解析 asar，抽取依赖闭包）
   ▼
sidecar/dsh-official/node_modules        （gitignored，不提交）
   │  Cordis 4.1.0（与桌面版同版本）
   ▼
sidecar-dsh.mjs
   │  ① 提供 tools / fs / systemPrompt 服务
   │  ② await ctx.plugin(官方插件, 解析后的 config)
   │  ③ ctx.tools.register(def) → tool.register 信封
   │  ④ tool.invoke → def.execute(args, exec) → output.render → tool.result
   ▼
NDJSON over stdio（docs/spec/sidecar-rpc.md）
   ▼
Rust SidecarHost → ToolRegistry（origin=sidecar）→ Agent → GPUI
```

### 装载契约（实测）

| 环节 | 官方插件的形状 | 我们怎么处理 |
|---|---|---|
| 插件身份 | `export name / inject / Config / apply` | 包成 `{name, inject, Config, apply}` 交给 `ctx.plugin` |
| 配置 | Schemastery `Config` | **必须显式 `Config(raw)` 解析**：Cordis 直接收原始 config 不补默认值，插件会在 `apply` 里因 `undefined` 抛错 |
| 依赖 | `inject: ['tools', ...]` | 提供对应服务；部分官方插件在 `apply` 里访问**未声明**的服务（`systemPrompt`/`fs`），DSH Loader 用 entry 级 inject 覆盖，我们照做 |
| 工具定义 | `defineTool({name, description, parameters, output, execute})` | `parameters` 已是 JSON Schema，直接当 `input_schema`；`output.schema` 当 `output_schema` |
| 工具结果 | `execute` 返回值 + `output.render(args, value)` | content 用 `render`，失败退回 `[{type:'json',value}]`；`presentationMeta` 进 `meta` |
| 取消 | `exec.signal` | 每个调用一个 `AbortController`，`tool.cancel` 时 abort |
| 卸载 | Cordis `ctx.effect` 自动回收 | 装载失败只记 `log.emit`，不拖垮侧车 |

### 当前提供的服务子集

| 服务 | 实现 | 能兑现的语义 |
|---|---|---|
| `tools` | 映射到协议 | `register` / `get`；`presentAs`/`restrict`/`guard` 为空实现 |
| `fs` | Node `fs/promises` | `resolve` / `stat` / `readText` / `streamText` / `processPath`，**只读** |
| `systemPrompt` | 收集段落 | `section` / `getSectionOrder`；不做真实提示词拼装 |

**没有**的服务：`session`、`sessionProjections`、`sandbox`、`userQuestions`、`agent`、`llm`、
`attachments`、`subprocess`、`jobs`、`goal`。因此 `todo`/`web`/`bash`/`pwsh`/`write` 等插件
会在注册或执行时失败（失败会被隔离并记录，不影响已装载插件）。

## 复现

```powershell
# 1. 本机需安装 DeepSeek Harness 桌面版（0.2.2-rc.1），路径可用 DSHR_ASAR 覆盖
node tools/dsh-vendor.mjs

# 2. 官方插件自检（不需要 API key）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest-dsh

# 3. 真实模型 + 官方 read 工具
$env:DSHR_SIDECAR = 'dsh'
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --live-chat "用 read 工具读 README.md 前 3 行"
```

查看/抽取安装包里的其它文件：

```powershell
node tools/asar.mjs ls dsh/node_modules/@deepseek-ai
node tools/asar.mjs cat dsh/node_modules/@deepseek-ai/dsh-tool-fs/package.json
```

## 边界与已知限制

1. **写工具已解锁（2026-11-11 收尾轮）**：能力协商 `fs.write` 后，官方 `write`/`edit`/
   `apply_patch` 的写盘请求经 `fs.write.request` 回到 Rust，走沙箱 + 审批 + 写前快照 +
   原子写；无协商时保持只读。见 [`m2-official-write.md`](m2-official-write.md)。
   `bash`/`pwsh`/`todo` 等仍关闭（缺 `subprocess`/`session` 服务）。
2. **路径基准**由 `DSHR_WORKSPACE` 决定（host 在握手时下发给 sidecar）；
   多工作区/多 agent 场景需要 `workingDirectory` service 适配。
3. **没有 scope / restrict / guard**：官方多 agent 隔离语义不成立，单 agent 场景可用。
4. **插件列表**默认固定在 `sidecar-dsh.mjs`；可用 `DSHR_DSH_PLUGINS`（JSON 数组）
   覆盖，格式 `[{"name": "...", "config": {...}, "inject": [...]}]`。
5. **版本锁定**：依赖抽自本机桌面版（当前 `0.2.2-rc.1` / Cordis `4.1.0`）。
   跨版本升级时 `tools/dsh-vendor.mjs` 会重新抽取；若 API 变化，`tools`/`fs` 适配层需要跟进。
6. **未提交产物**：`sidecar/dsh-official/node_modules` 是抽取产物（gitignored）。
   官方包均为 MIT（`package.json` 已标注），若未来随安装包分发需收集 LICENSE。
7. **未接入 Loader YAML**：目前直接用 `ctx.plugin` 装单个插件，不解析
   `cordis.yml` / `cordis.patch.yml` 的 bundle 树；这是"装插件"与"复刻 DSH 宿主"的分界线。

## 下一步

1. `session` / `sessionProjections`：解锁 `todo_write`、`web`、`workflow`；
2. `subprocess` + 进程隔离：解锁官方 `bash`/`pwsh`（当前写入已可用，执行类仍关闭）；
3. `workingDirectory`：路径基准与多工作区；
4. Loader 兼容：`include`/group 展开、`!!js` 求值（当前只做 bundle/patch 派生）。
