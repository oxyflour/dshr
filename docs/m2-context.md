# 上下文工程（第一阶段：计量 + 裁剪，2026-11-11）

目标：请求前知道"这次要发多少 token"，超预算时**有损但合法**地缩小历史，
并能从 Session Log 回答"当时为什么裁"。

## 计量

`crates/agent-context` 的 `estimate_tokens`：

- CJK 字符按 1 token/字，其它按 1 token/4 字符，再叠加消息开销（保守估计，不引入 tokenizer）；
- `estimate_history_tokens` 汇总 `system/user/assistant(+tool_calls 的 name+arguments)/tool`；
- 窗口默认 131072（按模型名分档：`deepseek-chat`/`deepseek-reasoner` 128K、`deepseek-flash`/`v4-pro` 1M、OpenAI 128K、未知 128K），可用 `DSHR_CONTEXT_WINDOW` 覆盖。

## 计划与策略

`plan_with_window(log, next_user_text, window) -> ContextPlan`：

| 阶段 | 触发 | 动作 |
|---|---|---|
| 1. 判预算 | 历史 + 下一轮用户消息 > `window * 0.8` | 否则原样返回 |
| 2. 裁工具结果 | 单条 `tool` 内容 > 800 token | 保留头 1200 字符 + 尾 400 字符，中间标注"已裁剪 N 字符"；原结果仍在 Session Log |
| 3. 丢最旧 | 裁剪后仍超 | 从最旧的整轮开始丢，直到 ≤ `window * 0.6`；起点必须落在 `User` 上，绝不以 `Tool` 开头 |
| 4. 摘要压缩 | 阶段 3 真的丢了消息 | 把丢弃的前缀交给模型生成摘要（≤1200 字符），写摘要消息 + `context.compacted(from_seq, to_seq, summary_message_id)`；重建历史时该区间被替换成一条 System 摘要 |

`ContextPlan` 返回：实际消息、窗口、`tokens_before/after`、`ratio()`、
`pruned_tool_results`、`dropped_messages`，供事件与 UI 使用。

`chat` 在每轮组装请求前调用 `plan_context`，并写入 durable 事件
`context.measured`：

```json
{ "tokens": {"input": 3694, "output": 0, "total": 3694},
  "window": 8000, "ratio": 0.462,
  "pruned_tool_results": 20, "dropped_messages": 72 }
```

UI 回放的 Token 曲线直接取这个事件序列（规范 §6），不需要实时估算。

## 压缩（context.compacted）

当阶段 3 真的丢弃了旧轮次时：

1. 把丢弃的消息拼成带角色的对话稿（上限 8000 字符），让 provider 生成 ≤1200 字摘要；
   provider 失败时退回"截取历史开头"，压缩不会阻塞对话。
2. 追加 durable 事件：
   - `message.assistant.settled`：摘要正文（message_id 被引用，UI/模型都不单独展示它）；
   - `context.compacted`：`from_seq` / `to_seq` / `summary_message_id` / `tokens_before` / `tokens_after`。
3. 重建时（`agent-context` 与 `conversation-model` 同规则）：
   - `[from_seq, to_seq]` 内的事件跳过；
   - 摘要消息本身隐藏；
   - `context.compacted` 位置还原成一条 System 摘要（模型）或 `上下文已压缩 · seq a–b` 占位（UI）。
4. 本轮随即重新计算计划，用压缩后的历史发请求。

## 验证

- `agent-context` 新增测试：CJK 计量、未超预算不裁剪、超长工具结果头尾裁剪、
  超预算丢弃最旧且保持协议合法、任何情况下不以 `Tool` 开头、配对逐条可断言、
  **压缩事件重建为 System 摘要且区间被跳过**、**计划带出 dropped_range**。
- `conversation-model`：demo 的压缩区间不再渲染用户/助手原文，只留工具卡片与压缩占位。
- `dshr --selftest-context`：36814 token 的历史触发压缩（seq 1–72 → 一条摘要），
  断言 `context.compacted`、摘要消息、重建出的 System 摘要都存在，ratio ≤ 0.8，
  压缩后 `dropped_messages == 0`（不会每轮重复压缩）。
- UI 截图核对：`上下文已压缩 · seq 1–72` 占位、区间内的消息不再显示、头部标题截断正常。
- workspace 124 个测试全绿，clippy 干净。

## 未做

- **spill 外部化**：大工具结果只做头尾裁剪，没有落附件与 `spill.created`。
- **工具 schema / system prompt 的精确计量**：现在只给消息记 token，
  schema 占用用 0.2 窗口余量兜底。
- **token 估算精度**：按字符启发式，未接 tokenizer；对英文略保守、对代码可能偏差。
- **摘要质量**：摘要是单次生成，没有"压缩后再压缩"的层级；summary 失败时是截断兜底。
