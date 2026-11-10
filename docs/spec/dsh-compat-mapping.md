# DSH 兼容映射（`docs/spec/dsh-compat-mapping.md`）

> 状态：Draft · 2026-11-10
> 用途：回答"我们的协议和 DSH 一致吗"，并把"哪些必须对齐、哪些刻意不对齐"钉死。
> 证据来源：本机运行中的 DSH 0.2.1-rc.3（`ctx.tools` 服务契约、`hostProtocolVersion: 4`）+ 仓库内探针实现。

## 1. 结论

**不一致，而且不应一致。**

- DSH 没有"sidecar 协议"这个概念：它的插件（含工具）在 **同一个 Node 进程**里通过 Cordis
  `ctx.tools.register(definition)` 注册；Host 与浏览器端之间走的是 `hostProtocolVersion: 4`
  的 Remote/typert RPC（HTTP/WS），不是 NDJSON。
- 我们的 `sidecar-rpc.md` 是**本项目自定义的兼容层协议**：进程边界是 ADR-004/005 的直接结果
  （不重写 Cordis、不透传 JS 对象），目的不是"像 DSH"，而是"能承载 DSH 插件的语义"。
- 因此判断标准不是"字节一致"，而是：**一个 DSH 工具的定义与结果，能不能无损穿过我们的协议层**。
  目前**不能无损**，差在 `output` / `presentCall` / `presentResult` / 作用域这几个字段上。

## 2. 分层对照

| 层 | DSH 0.2.1-rc.3 | 本项目 | 关系 |
|---|---|---|---|
| 插件装载 | Cordis `ctx.plugin()`，进程内 | Rust `PluginHost`（native）/ Node Sidecar（JS） | **刻意不同**（ADR-004） |
| 工具注册 | `ctx.tools.register(ToolDefinition)`，可带 agent scope，scope 内遮蔽全局 | `ToolRegistry.register(ToolDefinition)`，`ToolOrigin` 区分来源；无 scope | **缺 scope** |
| Host↔Client | `hostProtocolVersion 4`，生成的 Remote RPC | 无（自绘 GPUI，不桥接 DSH Client） | 不同层，不要求对齐 |
| Host↔Sidecar | 不存在 | NDJSON over stdio（自研） | 本项目新增 |
| Session 存储 | `dsh-session-format-v0…v4` 迁移链 | 自有事件 Schema（`session-log.md`） | **刻意不同**（PLAN §6.1） |

## 3. 工具契约：字段级对照

DSH 侧（实测类型声明，节选）：

```ts
interface ToolSchema { deferLoading?: true; name: string; description: string; parameters: Record<string, unknown> }
interface ToolDefinition extends ToolSchema {
  readonly output: ToolOutputDefinition
  execute(args: unknown, exec: ToolRunContext): Promise<unknown>
  projectContent?(exec, result): ContentBlock[] | undefined
  finalizeContent?(exec, result): ContentBlock[] | undefined
  timeoutMs?: number
  isConcurrencySafe?(args: unknown): boolean
  presentCall?(args: unknown): ToolCallView | undefined
  presentResult?(args: unknown, result: ToolResult): ToolResultView | undefined
}
interface ToolOutputDefinition { readonly schema: JsonSchemaNode; render(args, value): ContentBlock[]; presentationMeta?(args, value): JsonValue }
```

| DSH 字段 | 本项目（`tool-runtime` / `sidecar-rpc`） | 差距 |
|---|---|---|
| `name` / `description` / `parameters` | 有（`name`/`description`/`input_schema`） | 字段名不同，语义一致 |
| `output.schema` | **无** | 结果 schema 不参与校验 |
| `output.render` | 无（sidecar 直接回 `content[]`） | 渲染约定由插件自带 |
| `output.presentationMeta` | **无** | 卡片元数据丢失 |
| `execute(args, exec)` | 分离：`ToolExecutor` + sidecar `tool.invoke` | 可映射 |
| `timeoutMs` | 有（`timeout_ms`） | 一致 |
| `isConcurrencySafe` | **无** | 并发调度无法判定 |
| `presentCall` / `presentResult` | **无** | 只能用通用卡片 |
| `deferLoading` | 无 | 懒加载工具不可表达 |
| scope（agent 级遮蔽/restrict） | **无** | 多 agent 场景无法隔离 |
| `ToolCallView` 卡片类型：`generic`/`terminal`/`diff` | 只有 `origin` 与文本 content | 终端/差异卡片会退化成文本 |

**结论**：当前 L1 只能"跑通"简单工具；DSH 深度插件（终端、diff、搜索、read 视图）会**丢失展示语义**，
但**不会丢数据和执行**。这与 PLAN §5.1 的分级承诺一致（L1 只承诺 Tool 执行），
但文档必须写明"展示降级"，否则会被误读为兼容。

## 4. 结果与视图

DSH 的 `ToolExecutionResult` 是 `{ isError, value, content, meta?, additionalContexts?, concludesTurn? }`，
且 `content` 是 `ContentBlock[]`。我们目前：

- `tool.result` 只有 `call_id` / `is_error` / `content` / `metadata`；
- **没有** `value`（结构化返回值）与 `meta` 的稳定位置——我们把 value 塞进了 `content[0].value`；
- **没有** `additionalContexts`（工具向对话注入消息）与 `concludesTurn`（工具结束整轮）。

这三项影响的不只是展示：`additionalContexts` 是 DSH 工具改变后续模型上下文的正规通道，
`concludesTurn` 影响 Agent Loop 的终止条件。**M2 接真实 DSH 工具前必须补齐**。

## 5. 会话事件

DSH 的 session format 有 v0→v4 的迁移链（`dsh-session-format-*` 包），事件类型与字段是内部契约。
我们的 `session-log.md` 是独立 Schema（PLAN §6.1 明确"避免被 DSH 内部版本绑架"）。
两者**不做逐字段映射**；只在需要导入 DSH 会话时写一次性转换器（M5 可选）。

## 6. 优先级（按"影响真实插件"排序）

| 优先 | 事项 | 原因 |
|---|---|---|
| P0 | `output.schema` 进入 `ToolDefinition`，随 `tool.register` 传输 | 没有结果 schema，校验与展示都无据可依 |
| P0 | `value` / `meta` 在 `tool.result` 独立成字段 | 现在塞在 content block 里，DSH 侧读不到 |
| P1 | `presentResult` 的卡片类型（generic/terminal/diff/search/read/web） | 终端与 diff 工具的体验 |
| P1 | `isConcurrencySafe` → 调度器 | 并发安全无法判定时只能串行 |
| P2 | scope / restrict / guard | 多 agent 与审批的隔离 |
| P2 | `additionalContexts` / `concludesTurn` | 改变上下文与提前结束轮次 |

## 6.1 已完成（2026-11-10）

| 事项 | 状态 |
|---|---|
| `output.schema` 进入 `ToolDefinition`，随 `tool.register` 传输 | ✅ `ToolDefinition.output_schema` + `l1_roundtrip` 断言 |
| `presentResult` 卡片类型字段 | ✅ `ToolDefinition.presentation`（仅承载，渲染未接） |
| `tool.result` 拆出 `value` / `meta` | ✅ 事件载荷为 `{call_id, is_error, content, value, meta}`；`ToolOutcome` 同步 |

## 7. 下一步落地

1. `ToolDefinition` 增加 `output_schema`、`presentation`（卡片类型）字段，并让
   `sidecar-rpc` 的 `tool.register` 携带它们（字段名对齐 DSH，减少映射损耗）。
2. `tool.result` 增加 `value` 与 `meta`；`content` 保留给模型可见文本。
3. 在 `spec/sidecar-rpc.md` 的字段表里标注"对齐 DSH 的 X 字段"，让差异可追踪。
