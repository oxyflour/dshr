# Session Log — 事件日志与会话模型

> 状态：Draft（M2 冻结） · 版本：`1` · 关联：[`sidecar-rpc.md`](sidecar-rpc.md)、[`adc-v1.md`](adc-v1.md)、PLAN §6
> 定位：**Rust 是唯一权威**。UI、模型历史、回放、审计全部从这一份日志导出；不存在第二份"真相"。

---

## 1. 范围

定义事件信封、事件类型、不变量、回放语义、上下文工程事件、会话树字段与保留策略。不定义存储引擎（append-only 文件、SQLite 或分段对象存储均可），只定义可观察行为。

## 2. 事件信封

```json
{
  "session_id": "s-0001",
  "seq": 42,
  "type": "tool.result",
  "durability": "durable",
  "ts": "2026-11-08T10:00:00.000Z",
  "actor": { "kind": "plugin", "plugin_id": "example.dsh.tool", "generation": "node-0007" },
  "trace_id": "t-9f3c",
  "schema_version": 1,
  "payload": {}
}
```

| 字段 | 必需 | 说明 |
|---|---|---|
| `session_id` | 是 | 会话稳定 ID，与插件 generation 无关 |
| `seq` | 是 | 会话内单调递增，从 1 开始，无空洞 |
| `type` | 是 | 见 §3 |
| `durability` | 是 | `durable` / `ephemeral` |
| `ts` | 是 | RFC 3339 UTC（仅用于展示，排序一律按 `seq`） |
| `actor` | 是 | `user` / `agent` / `plugin` / `system` |
| `trace_id` | 是 | 跨 Runtime 追踪，可与 Sidecar RPC 对齐 |
| `schema_version` | 是 | 该 type 的载荷版本 |
| `payload` | 是 | 类型相关，必须是有限 JSON |

## 3. 事件类型

### 3.1 会话与轮次

| type | durability | 载荷要点 |
|---|---|---|
| `session.created` | durable | `workspace_id`（必需）, `parent_session_id?`, `spawn_source?`(`user`/`agent`), `preset?` |
| `session.title` | durable | `title`, `source` |
| `session.archived` / `session.unarchived` | durable | — |
| `turn.started` | durable | `turn_id`, `prompt_message_id` |
| `turn.ended` | durable | `turn_id`, `reason`(`completed`/`cancelled`/`error`) |
| `step.started` | durable | `step_id`, `turn_id`, `model` |
| `step.ended` | durable | `step_id`, `stop_reason`, `usage` |

### 3.2 消息与模型

| type | durability | 载荷要点 |
|---|---|---|
| `message.user` | durable | `message_id`, `content[]`（引用附件用 `$attachment`） |
| `message.assistant.delta` | ephemeral | `message_id`, `text`（流式，可丢） |
| `message.assistant.settled` | durable | `message_id`, `content[]`, `finish_reason` |
| `model.request` | durable | `step_id`, `provider`, `model`, `token_estimate` |
| `model.response.meta` | durable | `usage{input,output,cached}`, `latency_ms` |

规则：`delta` 永不进入回放；`settled` 必须完整。当前实现（M1）把 delta 只送给 UI、不落盘（规范允许丢）；M2 若需要诊断首字延迟，再按 ephemeral 记录。

### 3.3 工具

| type | durability | 载荷要点 |
|---|---|---|
| `tool.call` | durable | `call_id`, `tool`, `arguments`, `origin`(`native`/`sidecar`/`mcp`/`unknown`), `registration_id?`, `plugin_id?`, `generation?`, `presentation?` |
| `tool.chunk` | ephemeral | `call_id`, `seq`, `text` |
| `tool.result` | durable | `call_id`, `is_error`, `content[]`, `value`, `meta?`, `metadata`；`meta.ui` 可带受限 ADC 附加区（M4 W3，仅展示，不参与模型输入） |
| `tool.aborted` | durable | `call_id`, `reason`(`crash`/`cancel`/`timeout`/`unload`) |
| `tool.late_result` | durable | `call_id`, `discarded: true`（诊断用） |
| `tool.result.pruned` | durable | `call_id`, `原始大小`, `spill_ref`（见 §5） |
| `tool.repeat` | durable | `tool`, `count`, `threshold`, `message`（M3：同一工具+参数在本轮第 N 次调用触发软提醒；actor=`system`；**不失败、不改变工具结果**；读取端必须容忍未知 type，UI 可不渲染，用于自激诊断/审计） |

新事件保留模型返回的 `call_id`，用可选的 `call_seq`（对应 `tool.call` 的事件序号）配对调用、终态和外部化结果。历史事件按 `call_id` 配对。
每次调用至多有一个终态（`tool.result` 或 `tool.aborted`）。
`tool.repeat` 是旁路诊断事件，不参与工具结算，也不影响 `tool.call/tool.result` 的配对。

**并行工具的顺序（M3）**：一个 step 里有多个工具调用且宿主按批并发时，事件序为
`ToolCall×N → 终态×N`（`tool.result` / `tool.aborted` 按**原调用顺序**发出），不同 `call_id` 的终态
**不再保证相邻**于自己的 `tool.call`。UI/回放按 `call_seq` 或历史 `call_id` 配对；
`tool.chunk` 仍按各自 `call_id` 归属。

### 3.4 审批与策略

| type | durability | 载荷要点 |
|---|---|---|
| `approval.asked` | durable | `call_id`, `request`, `policy` |
| `approval.answered` | durable | `call_id`, `outcome`(`allow`/`deny`/`allow_once`), `answerer` |
| `policy.changed` | durable | `scope`, `from`, `to`, `by` |

### 3.5 上下文工程（见 §5）

| type | durability | 载荷要点 |
|---|---|---|
| `context.measured` | durable | `tokens{input,output,total}`, `window`, `ratio` |
| `context.compacted` | durable | `from_seq`, `to_seq`, `summary_message_id`, `tokens_before/after` |
| `spill.created` | durable | `id`, `hash`, `size`, `mime`, `retention` |

### 3.6 会话树

| type | durability | 载荷要点 |
|---|---|---|
| `child.spawned` | durable | `child_session_id`, `parent_message_id?`, `prompt` |
| `child.finished` | durable | `child_session_id`, `outcome`(`completed`/`failed`/`cancelled`/`interrupted`/`killed`) |
| `session.interrupted` | durable | `reason`, `by_parent: false`（中断发起方自己的日志） |
| `session.interrupted_by_parent` | durable | `parent_session_id`, `reason`（子会话被父会话中断，即 `interrupted_by_parent`） |
| `session.killed` | durable | `by_parent: true`, `reason`(`interrupt_timeout`)（中断超时后升级为 kill） |

规则：

- `child.spawned` / `child.finished` 写在**父会话**日志里；子会话自己的日志以
  `session.created` 开头，`parent_session_id` 只在那里写一次。
- 中断按树向下传播：发起方写 `session.interrupted`；每个被传播到的子会话**各写一条**
  `session.interrupted_by_parent`，状态机进入 `aborted`。
- 执行器先请求 `interrupt`，超时仍未停止时升级为 kill 语义，并补一条 `session.killed`
  （审计用，不改变"被父中断"的事实）。读取端 MUST 容忍一个会话出现多条中断类事件，
  以最后一条为准。

## 4. 不变量

| # | 不变量 | 检查方式 |
|---|---|---|
| I1 | append-only；任何事件不可修改/删除（保留期清理除外） | 存储层只追加 |
| I2 | `seq` 连续无空洞；并发写入由单一提交者串行化 | 启动时校验 |
| I3 | 每个 `tool.call` 至多一次结算（`result` XOR `aborted`） | 回放校验 |
| I4 | `message.assistant.settled` 每个 `message_id` 恰好一次 | 回放校验 |
| I5 | 压缩/裁剪必须引用原始 `seq` 区间，不得丢弃可回放性 | §5 |
| I6 | 日志中不得出现凭据明文、绝对路径（工作区外）、原始堆栈 | 写入时脱敏 |
| I7 | 事件写入成功后才对外可见（先落盘后推送） | 提交路径 |
| I8 | `session_id`/`call_id` 与插件 generation 无关 | 重载后 ID 不变 |
| I9 | `session.created` 是会话第一条事件且至多一条；`parent_session_id` 不可变、不能自引用 | 启动/回放校验 |

违反 I1–I5 视为数据损坏：停止加载该会话并提示导出，不猜测修复。
旧日志（M2 之前）可能没有 `session.created`：读取端必须把它当作根会话（`parent_session_id = None`），
不得因此拒绝加载。

## 5. 上下文工程事件

1. **计量**：每个 `step.started` 前写 `context.measured`；UI 显示的占用取自该事件，不实时估算。
2. **压缩**：`context.compacted` 记录被替换的 `[from_seq, to_seq]` 与摘要消息 ID；回放时该区间渲染为"已压缩"占位，模型历史取摘要。
3. **裁剪**：`tool.result.pruned` 记录原始大小与 `spill_ref`；原结果保留在附件中可展开查看。
4. **外部化**：`spill.created` 的 `id` 作为 `$attachment` 引用目标；保留期到期后引用变为"已过期"占位，不阻塞回放。

硬性要求：任何压缩/裁剪都不得让 `model.request` 的输入无法重建。若无法重建，说明该事件缺少必要字段，属于规范缺陷。

## 6. 回放

回放输入：单一会话的完整事件序列（含 durable 与 ephemeral 的历史片段，后者可缺省）。

| 目标 | 规则 |
|---|---|
| 会话列表/时间线 | 只用 `session.*`、`turn.*` |
| 对话气泡 | `message.user` + `message.assistant.settled`；`delta` 仅用于"进行中"动画 |
| 工具卡片 | `tool.call` + 终态（`result`/`aborted`）+ `tool.chunk` 可缺省 |
| 审批痕迹 | `approval.asked` ↔ `approval.answered` 配对 |
| Token 曲线 | `context.measured` 时间序列 |
| 子会话树 | `child.*` + `session.created.parent_session_id`；跨日志由 `session_id` 文本引用，不要求父会话在线 |
| 中断痕迹 | `session.interrupted` / `session.interrupted_by_parent` / `session.killed`，按事件顺序 |

回放 MUST NOT 触发 LLM 请求或工具执行。所有展示元数据（tool card 的 `presentation`）随事件持久化，不依赖插件当前版本。

桌面模型步骤的 `tool.call` 从该步提供的定义获取来源、卡片类型与 Sidecar 注册身份。模型请求中没有公开该工具时，事件使用 `origin: "unknown"`，并记录配对的工具错误。

### 6.1 工具卡片的折叠规则

卡片是**事件的确定函数**：`(presentation, value, content, 终态)` → 卡片，不读插件、不读当前时间、不随机。

| 卡片 | 数据形状 | 摘要 |
|---|---|---|
| `terminal` | `{exit_code, stdout, stderr}` | `exit N` + 输出/错误行数 |
| `diff` | `{path?, diff\|patch, added?, removed?}` | 路径 + `+a −r` |
| `search` | `{query, matches[]\|count}` | 查询 + 匹配处数 |
| `read` | `{path, text\|lines[]}` | 路径 + 行数 |
| `web` | `{url, title?, snippet?}` | 标题 + URL |
| `generic` | 任意 `value` | 工具名 + 紧凑 JSON 前若干行 |

约束：

- 预览最多 6 行、单行最多 120 字符，超出以"… 还有 N 行"收尾（UI 不会被输出撑爆）。
- 形状与 `presentation` 不匹配时 MUST 退回 `generic`；缺少顶层 `value` 时回退到 `content[0].value`（旧插件兼容）。
- `is_error: true` 或 `tool.aborted` 生成**错误卡片**（标题分别 `失败` / `已中止`），不套用插件声明的卡片类型。
- 卡片只派生不落盘：同一份 JSONL 必须折叠出完全相同的卡片（可逐项断言）。

### 6.2 模型历史的重建

下一轮请求的消息历史**从 durable 事件重建**（`agent-context`），不依赖内存状态，因此重开应用也能接上：

| 事件 | 重建出的消息 |
|---|---|
| `message.user` | `user` |
| `message.assistant.settled` | `assistant`（空文本跳过） |
| `tool.call` | 追加进**前一条** `assistant` 的 `tool_calls`；上一条不是 assistant 时补一条空 assistant |
| `tool.result` / `tool.aborted` | `tool`，`call_id` 必须等于对应 `tool_calls[].id`；失败内容加 `ERROR:` 前缀 |

约束：

- `role: tool` 的消息必须能反查到同 id 的 `tool_calls`，否则回放直接丢弃该条（宁可少一条，也不产生非法序列）。
- 历史截断按**消息条数**（默认 60），且不得从 `tool` 消息开始——那会切掉它的调用方。
- 工具结果传给模型的是文本投影；结构化 `value` 只给 UI/回放。

## 7. 会话树

v1 语义（PLAN §6.5，实现见 `crates/session-store` 与 `crates/agent-core`）：

- 子会话是完整会话：自己的 `seq` 空间、自己的日志文件、可独立导出回放。
- `parent_session_id` / `spawn_source` / `preset` 只在子会话的第一条 `session.created` 写入一次，
  不可修改；父会话**不保存**子会话树结构，只通过 `child.spawned` 留引用痕迹。
- 桌面根会话仅含 `session.created` 时，可在写锁下原子更新该事件的 `preset`。会话开始后按原预设执行。官方 Sidecar 的创造根会话写入 `preset: "cordis"`，旧版和诊断入口使用 `"dshr-creator"`。
  进程存续期间保留该会话的试用实例，删除会话或退出应用时清理。预设会话持久化的是 ID，进程内修订由 Sidecar 保留。
  [创造模式](creator-mode.md) · [Agent 预设](agent-presets.md)
- 深度从 0 开始（根 0 / 子 1 / 孙 2），默认上限为 2；上限可配置，子会话只能收紧不能放大。
- 子会话继承父的工作区与策略，且只允许收紧（更小的深度上限、工具白名单子集、黑名单追加）；
  放宽必须走显式审批，v1 不支持。
- 中断传播由父会话发起：发起方写 `session.interrupted`，每个被传播到的子会话各写
  `session.interrupted_by_parent`，子会话状态机进入 `aborted`；先请求 `interrupt`，
  超时后升级为 kill 语义并补 `session.killed`。
- 父会话等待子会话（drain）有两种语义：只等直接子级、或等待全部后代。等待期间父会话
  保持 `active`，不得被 UI/调度器判定为闲置。
- 回放子会话不要求父会话在线；跨会话引用一律通过 `session_id` 文本表示。
- 运行态（active / interrupted / drain 等待）不落盘，由事件重建：父链接来自
  `session.created`，中断状态来自 §3.6 的事件，子会话结束来自父日志的 `child.finished`。

## 8. 保留、导出与加密

| 项 | 默认 |
|---|---|
| 单会话磁盘上限 | 2 GB（超出提示归档） |
| 附件保留期 | 90 天（`spill.created.retention`） |
| 删除 | 会话删除即连同附件引用清理；保留审计摘要 |
| 导出 | `logs/<session_id>/events.jsonl` + `attachments/`，可离线回放 |
| 加密 | 落盘加密由存储层负责；密钥不由插件可见 |

## 9. 版本与迁移

- 每个事件类型独立 `schema_version`；新增可选字段不升版本，语义变化升版本。
- 迁移器只能**新增派生事件**，不得原地改写历史事件（违反 I1）。
- 读取端必须容忍未知字段；遇到未知 `type` 时跳过并计数，不中断回放。

## 10. Conformance

| 前缀 | 覆盖 |
|---|---|
| `LOG-ORDER-*` | seq 连续、并发提交串行化 |
| `LOG-SETTLE-*` | 结算唯一性、aborted 互斥 |
| `LOG-REPLAY-*` | 各视图可离线重建、不触发副作用 |
| `LOG-CTX-*` | 压缩/裁剪区间可重建 |
| `LOG-TREE-*` | 树字段写入/回放、深度上限、成环拒绝、中断传播与 drain 语义 |
| `LOG-MIGRATE-*` | 旧版本读取、未知类型跳过 |

## 11. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| Q1 | 存储形态（SQLite vs 分段 JSONL + 索引） | 分段 JSONL + SQLite 索引 | M2 |
| Q2 | `context.compacted` 是否携带完整摘要文本 | 只带引用，正文在消息事件里 | M2 |
| Q3 | 跨会话搜索的索引来源 | 从事件派生，独立索引 | M3 |
| Q4 | 审计摘要的保留期 | 与合规要求对齐，默认 1 年 | M2 |
