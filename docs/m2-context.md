# 上下文工程（第一阶段：计量 + 裁剪，2026-11-11）

目标：请求前知道"这次要发多少 token"，超预算时**有损但合法**地缩小历史，
并能从 Session Log 回答"当时为什么裁"。

## 计量

`crates/agent-context` 的 `estimate_tokens`：

- CJK 字符按 1 token/字，其它按 1 token/4 字符，再叠加消息开销（保守估计，不引入 tokenizer）；
- `estimate_history_tokens` 汇总 `system/user/assistant(+tool_calls 的 name+arguments)/tool`；
- 窗口默认 64000（DeepSeek `deepseek-chat`），可用 `DSHR_CONTEXT_WINDOW` 覆盖。

## 计划与策略

`plan_with_window(log, next_user_text, window) -> ContextPlan`：

| 阶段 | 触发 | 动作 |
|---|---|---|
| 1. 判预算 | 历史 + 下一轮用户消息 > `window * 0.8` | 否则原样返回 |
| 2. 裁工具结果 | 单条 `tool` 内容 > 800 token | 保留头 1200 字符 + 尾 400 字符，中间标注"已裁剪 N 字符"；原结果仍在 Session Log |
| 3. 丢最旧 | 裁剪后仍超 | 从最旧的整轮开始丢，直到 ≤ `window * 0.6`；起点必须落在 `User` 上，绝不以 `Tool` 开头 |

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

## 验证

- `agent-context` 新增 6 个测试：CJK 计量、未超预算不裁剪、超长工具结果头尾裁剪、
  超预算丢弃最旧且保持协议合法、任何情况下不以 `Tool` 开头、配对逐条可断言。
- `chat --selftest-context`：构造 36814 token 的长会话，断言写入 `context.measured`、
  `ratio <= 0.8`、裁剪/丢弃发生、重建计划仍合法。
- workspace 测试与 clippy 全绿。

## 未做（下一阶段）

- **摘要压缩**（`context.compacted`）：当前是"丢最旧的整轮"，不是 LLM 摘要；
  消息仍在日志里可见，只是不再进模型历史。下一步做：对丢弃区间生成摘要、
  写 `context.compacted` + 摘要消息，回放时该区间渲染"已压缩"占位。
- **外部化（spill）**：大工具结果目前只做头尾裁剪，没有落附件与 `spill.created`。
- **工具 schema / system prompt 的精确计量**：现在只给消息记 token，
  schema 占用用 0.2 窗口余量兜底。
- **token 估算精度**：按字符启发式，未接 tokenizer；对英文略保守、对代码可能偏差。
