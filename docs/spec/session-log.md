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
| `session.created` | durable | `workspace_id`, `parent_session_id?`, `spawn_source?`, `preset?` |
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
| `tool.call` | durable | `call_id`, `tool`, `arguments`, `origin`(`native`/`sidecar`/`mcp`), `registration_id?`, `presentation?` |
| `tool.chunk` | ephemeral | `call_id`, `seq`, `text` |
| `tool.result` | durable | `call_id`, `is_error`, `content[]`, `value`, `meta?`, `metadata` |
| `tool.aborted` | durable | `call_id`, `reason`(`crash`/`cancel`/`timeout`/`unload`) |
| `tool.late_result` | durable | `call_id`, `discarded: true`（诊断用） |
| `tool.result.pruned` | durable | `call_id`, `原始大小`, `spill_ref`（见 §5） |

不变量：一个 `call_id` 至多一条 `tool.result`；`tool.aborted` 与 `tool.result` 互斥（先到者胜）。

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
| `child.spawned` | durable | `child_session_id`, `parent_message_id`, `prompt` |
| `child.finished` | durable | `child_session_id`, `outcome` |
| `session.interrupted` | durable | `by_parent: true`, `reason` |

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

违反 I1–I5 视为数据损坏：停止加载该会话并提示导出，不猜测修复。

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
| 子会话树 | `child.*` + `session.created.parent_session_id` |

回放 MUST NOT 触发 LLM 请求或工具执行。所有展示元数据（tool card 的 `presentation`）随事件持久化，不依赖插件当前版本。

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

## 7. 会话树

- 子会话是完整会话：自己的 `seq` 空间、自己的日志文件、可独立导出。
- `parent_session_id` 只在 `session.created` 写入一次，不可改。
- 中断传播由父会话发起：写 `session.interrupted`，子会话状态机进入 `aborted`；子会话自己的日志也要各写一条。
- 回放子会话不要求父会话在线；跨会话引用通过 `session_id` 文本表示。

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
| `LOG-MIGRATE-*` | 旧版本读取、未知类型跳过 |

## 11. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| Q1 | 存储形态（SQLite vs 分段 JSONL + 索引） | 分段 JSONL + SQLite 索引 | M2 |
| Q2 | `context.compacted` 是否携带完整摘要文本 | 只带引用，正文在消息事件里 | M2 |
| Q3 | 跨会话搜索的索引来源 | 从事件派生，独立索引 | M3 |
| Q4 | 审计摘要的保留期 | 与合规要求对齐，默认 1 年 | M2 |
