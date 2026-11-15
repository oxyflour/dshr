# M2 — 对齐官方 DSH 默认配置（工具调用预算修复，2026-11-11）

**一句话**：`dshr` 的默认配置此前与官方 DSH 0.2.2-rc.1 有若干偏差，其中最严重的是
**"一轮最多 8 次工具调用，超了整轮 `TurnFailed`"**，把"一个 step 里并发发起 9 个
`probe_slow`"这种完全正常的场景判成失败。本轮把工具预算改成 **默认不限制**（与 DSH
一致，`@deepseek-ai/dsh-agent-loop` 没有每轮调用上限），并顺带对齐了
temperature / max_tokens / retry / context window 的默认值。压缩、裁剪、spill 的
常量**未改**（Gate 3 自检数据依赖它们），只在本文对比 DSH 口径。

- 目标版本：本机 DSH `0.2.2-rc.1`（`%LOCALAPPDATA%\Programs\DeepSeek Harness\resources\app.asar`）
- 只读证据：抽取后的包内容，未改动、未引入依赖、未联网
- 相关代码：`crates/agent-core`、`crates/agent-context`、`crates/llm-retry`、
  `crates/llm-deepseek`、`crates/llm-openai`

> **M3 更新（2026-11-15）**：表 1 中标注 "M3" 的差异已逐项收口（默认差异对齐见
> `crates/agent-core` 的 `ReasoningEffort`/重复提醒、`crates/llm-retry` 的 `EMPTY_RESPONSE`、
> `crates/agent-context` 的 `retainRatio=0.16`、`crates/tool-runtime` 的 `PermissionMode`/
> `ApprovalPolicy::from_env`、`crates/llm-openai` 的 `reasoning_effort`）。仍差：
> **每步并行工具数仍是 1**（工具并发改造未做）、DeepSeek Chat Completions 无
> `reasoning_effort` 字段（已解析但不发送）、compaction 的 `headroomTokens`/
> `compactionRetries`/`maxOverflowRetries` 未对齐。逐项状态以表 1 的"M3 状态"列与
> `docs/m3-status.md` 为准。

## 用户现场（修复前）

release 版 `chat.exe`（默认 probe sidecar）输入"从 1 数到 10，间隔 2 s"，模型在
**一个 step 里发起 9 个 `probe_slow` 调用**：

```text
assistant 文本：12345678处理失败：tool call budget exhausted (8)
落盘会话：s-1794404310974.jsonl 第 16–32 行（release 构建产物目录下，gitignored）
```

根因：`crates/agent-core/src/lib.rs` 的 `pub const MAX_TOOL_CALLS: usize = 8;`——
第 9 个调用命中后直接 `AgentEvent::TurnFailed`，整轮丢弃（前 8 个已经执行的结果也
只剩日志）。修复后同样的输入会执行完全部调用、正常给出最终回答。

## 表 1：默认差异总览

| 配置项 | DSH 0.2.2-rc.1 默认 | dshr 修复前 | dshr 修复后 | 影响 |
|---|---|---|---|---|
| 每轮工具调用总数 | **无上限**（`dsh-agent-loop` 只有 `maxParallelToolCalls`） | **8，超出即 `TurnFailed`** | **无上限**；可用 `DSHR_MAX_TOOL_CALLS` 显式收紧，命中后**降级**（未执行的调用仍发 `ToolCall` + `is_error` 的 `ToolResult`，随后 `TurnEnded`，不失败） | 修复用户现场故障；默认行为与 DSH 一致 |
| 每步并行工具数 | `maxParallelToolCalls = 10`，按批并发 | 串行 1 个（`Agent` 顺序 `for` 执行） | **M3 默认 10（sidecar 工具按波次并发）**：`ToolExecutor::execute_batch` + `SidecarHost::invoke_batch`；native/MCP/`spawn_child` 仍串行（native 是同步插件调用）；`DSHR_MAX_PARALLEL_TOOL_CALLS` 可收紧，默认 10。实测 3×2s probe 工具：串行 6.1s → 并行 2.0s（ratio 0.33）。**事件顺序**：同一 step 先发全部 `ToolCall`，再按原序发 `ToolResult`（UI/回放按 `call_id` 配对，不依赖相邻） | 功能与墙钟对齐 DSH；剩余差异仅 native/MCP 并发 |
| step 数上限 | 无 | 无（仅被 8 次调用间接限制） | 无；`DSHR_MAX_STEPS` 可显式收紧，命中后 `TurnEnded` | 与 DSH 一致 |
| context window | `deepseek-flash` / `deepseek-v4-pro`：`1_048_576`；`DEFAULT_CONTEXT_WINDOW = 1_000_000` | 固定 `64_000`（`DSHR_CONTEXT_WINDOW` 才能改） | `DSHR_CONTEXT_WINDOW` 优先；否则按模型名：`flash`/`v4-pro`/`v4.1` → 1M，`deepseek-chat`/`reasoner` → 128K，`gpt-4o`/`gpt-4.1`/`o3`/`o4` → 128K，其余 / 未配置模型 → 128K（两家默认模型口径） | 默认模型 `deepseek-chat` 不再被低估到 64K；选 1M 模型时自动吃满窗口 |
| output max tokens | `DEFAULT_MAX_TOKENS = 256_000`，每个请求都发 `max_tokens` | 不发送（provider 默认） | **仍默认不发送**；新增 `DEEPSEEK_MAX_TOKENS` / `OPENAI_MAX_TOKENS` 显式开启 | 有意差异：dshr 默认模型 `deepseek-chat` 与 DSH desktop 的 `deepseek-flash` 不同，不盲目照抄 256k |
| temperature | 默认**不发送**（`temperature === void 0 ? {} : { temperature }`） | 硬编码 `0.3` | 默认不发送；`DEEPSEEK_TEMPERATURE` / `OPENAI_TEMPERATURE` 显式设置才发送 | 采样行为回到服务端默认，与 DSH 一致 |
| reasoning effort | `off` / `low` / `high` / `max`；desktop profile 配 `high` | 不支持 | **M3 已实现**：`agent-core::ReasoningEffort`（共享档位/`DSHR_REASONING_EFFORT`/非法值警告）；OpenAI 请求体发 `reasoning_effort`（off 不发）；DeepSeek Chat Completions 无该字段 → 解析但不发送（`SUPPORTS_REASONING_EFFORT=false`）。**无显式配置时默认不发送**（不假设账号权限） | 与 DSH desktop `high` 的口径差异保留为"默认不发送"，显式 `DSHR_REASONING_EFFORT=high` 可对齐 |
| retry | `maxRetries=5`（总尝试 **6**）、`initialDelayMs=500`、`maxDelayMs=10000`、`jitterRatio=0.1`；可重试码含 `EMPTY_RESPONSE`/`RATE_LIMIT`/`SERVER`/`TIMEOUT`/`TRANSPORT` | 3 次尝试、200ms 起、2s 封顶、无抖动 | **6 次尝试、500ms 起、10s 封顶、±10% 抖动**；`DSHR_RETRY_*` 可覆盖；**M3 补 `EMPTY_RESPONSE`**（无 text 且无 tool_calls 归一化为可重试，受 `max_attempts` 上限；流式仅未交付 delta 时重试） | 瞬时错误更耐扛；抖动降低多客户端同刻重试风暴 |
| 历史条数 | 无条数上限（按 token 触发压缩） | `DEFAULT_MAX_MESSAGES = 60` 兜底 | 未改（兜底仍在，正常路径先按 token 触发裁剪） | 保持既有回放语义 |
| 压缩触发 / 保留 | `thresholdRatio=0.8`、`headroomTokens=65536`、`maxTokens=headroomTokens`、`retainRatio=0.16`（保留约 16% 消息）+ LLM 摘要；`compactionRetries=1`、`maxOverflowRetries=1` | 触发 `0.8`、裁到 `0.6`，**有 LLM 摘要**（`chat.rs summarize_dropped` 生成摘要消息 + `context.compacted` 留痕） | **M3 对齐 retainRatio=0.16**（`agent-context` 的 `DSH_RETAIN_RATIO`，可用 `DSHR_CONTEXT_RETAIN_RATIO` 覆盖/回退 0.6）；触发 0.8 不变；**headroomTokens/compactionRetries/maxOverflowRetries 未对齐** | 保留比与 DSH 一致；`headroom/retries` 仍差 |
| 工具结果裁剪 | `thresholdChars=8192`、`headChars=4096`、`tailChars=1024`（按字符） | `TOOL_RESULT_TOKEN_LIMIT=800` tokens，头 1200 / 尾 400 字符（按估算 token） | 未改 | 口径不同（token vs char），效果同量级 |
| 大结果 spill | `maxInlineTokens=12500` → 大结果外部化 | `SpillPolicy::default()`：**16 KiB 字节**阈值，内容寻址落盘 + 512 字符预览 | 未改 | 阈值口径不同，均为"超阈值落盘 + 模型只见引用" |
| 工具超时 | 每个工具声明 `timeoutMs`（`dsh-tool-call-timeout-policy` 按声明执行，无声明则不设限） | sidecar 工具固定 **5s** | 按工具声明 `timeout_ms`，clamp 到 1s–600s，`DSHR_TOOL_TIMEOUT_MS` 可整体覆盖（**由并行任务修复，见 `chat.rs`**）；**M3 补 tool-runtime 常量/`effective_timeout_ms()` 与注册表保留测试；sidecar wire 透传见 W5 接线** | 慢工具不再被 5s 误杀 |
| approval / sandbox | 默认 `workspace-write` + `ask`；`DSH_PERMISSION_MODE=danger-full-access` 时 `never` | 读类放行、写入/执行类 `Ask`（`ApprovalPolicy::desktop`）；写入限制在 workspace 根内；`DSHR_TOOL_POLICY=ask-all` 更严 | **M3 已实现 crate 层**：`tool-runtime::PermissionMode` + `ApprovalPolicy::{read_only,workspace_write,danger_full_access,for_mode,from_env}`（`DSHR_PERMISSION_MODE`）；chat 接线见 W5 | 默认安全口径不变；三模式可切换 |
| 重复工具调用提醒 | `repeat-tool-reminder thresholds: [3, 5, 8]`（软提醒，不是失败） | 无 | 无 | **M3 已实现 crate 层**：`agent-core` 默认开启、阈值 `[3,5,8]`（`DSHR_REPEAT_TOOL_REMINDERS` 可配/关），第 3/5/8 次注入 System 软提醒（不失败、不破坏 tool 配对），`take_repeat_reminders()` 供 chat 落 `tool.repeat`；不同参数/工具不触发 | 与 DSH 一致（chat 事件接线见 W5） |
| 默认 provider / model | desktop profile：`deepseek-account` / `deepseek-flash` / `reasoningEffort=high` | `DEEPSEEK_MODEL` 默认 `deepseek-chat`；`OPENAI_MODEL` 默认 `gpt-4o-mini`；provider 按 key 自动选 | 未改（避免假设 API key 权限） | 有意差异；要 1M 需 `DEEPSEEK_MODEL=deepseek-flash` + `DSHR_CONTEXT_WINDOW=1048576` |

## 表 2：证据索引

包版本均为 `0.2.2-rc.1`；路径相对 asar 抽取根，行号为抽取出的 `lib/*.js` / `cordis.patch.yml`。

| 证据 | 位置 | 关键内容 |
|---|---|---|
| 无每轮工具调用上限 | `@deepseek-ai/dsh-agent-loop` `lib/index.js:1309`、`:959-989` | `maxParallelToolCalls: z.number()...default(10)`；每步把 `message.content` 里所有 `tool-call` 交给 `tools.execute({..., maxParallelToolCalls})` 分批执行；全文无 `maxSteps` / 调用总数常量 |
| 默认模型与 1M 窗口 | `@deepseek-ai/dsh-llm-deepseek` `lib/index.js:17`、`:19-31` | `OFFICIAL_CONTEXT_WINDOW = 1048576`；`DEFAULT_MODELS = [{ id: 'deepseek-flash', contextWindow: OFFICIAL_CONTEXT_WINDOW }, { id: 'deepseek-v4-pro', ... }]` |
| 输出上限 256k | `@deepseek-ai/dsh-llm-deepseek` `lib/index.js:40`、`:329`、`:1732` | `DEFAULT_MAX_TOKENS = 256e3`；Config `maxTokens ... .default(DEFAULT_MAX_TOKENS)`；请求体 `max_tokens: options.maxTokens ?? model?.maxTokens ?? connection.maxTokens` |
| 默认不发 temperature | `@deepseek-ai/dsh-llm-deepseek` `lib/index.js:1736` | `...options.temperature === void 0 ? {} : { temperature: options.temperature }` |
| reasoning effort 档位 | `@deepseek-ai/dsh-llm-deepseek` `lib/index.js:323-330`、`:472-475` | Config `reasoningEffort` 支持 `off/low/high/max`（常量 `OFF/LOW/HIGH/MAX_REASONING_EFFORT`） |
| retry 默认 | `@deepseek-ai/dsh-llm` `lib/types/retry-policy.js:9-20` | `DEFAULT_MAX_RETRIES = 5`、`DEFAULT_INITIAL_DELAY_MS = 500`、`DEFAULT_MAX_DELAY_MS = 10_000`、`DEFAULT_JITTER_RATIO = 0.1`；retryable codes `EMPTY_RESPONSE/RATE_LIMIT/SERVER/TIMEOUT/TRANSPORT` |
| 压缩默认 | `@deepseek-ai/dsh-compaction-basic` `lib/index.js:15-17`、`:63-64`、`:82-83` | `DEFAULT_THRESHOLD_RATIO = .8`、`DEFAULT_RETAIN_RATIO = .16`；`headroomTokens ?? 65536`、`maxTokens ?? headroomTokens`；`compactionRetries ?? 1`、`maxOverflowRetries ?? 1` |
| 工具结果裁剪默认 | `@deepseek-ai/dsh-compaction-tool-result-pruner` `lib/index.js:11-13` | `thresholdChars: 8192, headChars: 4096, tailChars: 1024` |
| 工具超时按声明 | `@deepseek-ai/dsh-tool-call-timeout-policy` `lib/index.js:4`、`:123-130` | 工具声明 `timeoutMs`；`const timeoutMs = ctx.tools.get(...)?.timeoutMs; if (timeoutMs === void 0) return next();` |
| base 默认装配 | `@deepseek-ai/dsh-base` `cordis.patch.yml:90-92`、`:243-246`、`:260-262`、`:355`、`:408`、`:414-417`、`:425-428`、`:448-450` | `agent-default-model: provider deepseek-official, model deepseek-flash`；`sandbox-policy: mode = DSH_PERMISSION_MODE ?? 'workspace-write', workspaceRoot = cwd`；`approval: policy = danger-full-access ? 'never' : 'ask'`；`compaction-basic`（值取包默认）；`timeout-policy`；`spill-policy: maxInlineTokens 12500`；`tool-result-pruner`；`repeat-tool-reminder: thresholds [3,5,8]` |
| desktop profile 实际 | `%USERPROFILE%\.dsh\profiles\desktop\cordis.patch.yml` | `agent-default-model: { provider: deepseek-account, model: deepseek-flash, reasoningEffort: high }` |

## 修复细节

### 1. 工具预算（`crates/agent-core`）

```rust
pub struct ToolBudget {
    pub max_tool_calls: Option<usize>, // 默认 None = 不限制
    pub max_steps: Option<usize>,      // 默认 None = 不限制
}
```

- `Agent::new(provider)` 用 `ToolBudget::from_env()`（`DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS`，
  空/0/非法 = 不限制）；`Agent::with_budget(provider, budget)` 供测试与显式配置。
- 命中 `max_tool_calls` 后不再 `TurnFailed`：未执行的调用仍产出 `ToolCall` 事件和
  `is_error` 的 `ToolResult`（content 说明 `tool call budget exhausted (max): 本次调用未执行`），
  并记入本轮的 `tool` 消息；本步剩余调用逐条标记失败后结束该步并直接 `TurnEnded`
  （不再给模型额外发工具调用的机会，避免预算外的空转循环）。
- 命中 `max_steps` 同样直接 `TurnEnded`，不是失败。
- 删除了 `pub const MAX_TOOL_CALLS`（旧 `tool_budget_is_enforced` 固化的 8 次失败语义已重写）。

新增/重写测试（`cargo test -p dshr-agent-core`）：

- `tool_budget_is_enforced`：显式 `max_tool_calls=2` → 只执行 2 次、无 `TurnFailed`、
  第 3 个调用的结果 `is_error` 且事件配对完整；
- `default_budget_does_not_cap_tool_calls`：默认预算下 20 次循环调用全部执行；
- `many_tool_calls_in_one_turn_are_not_capped`：一步 10 个调用（用户现场）全部执行；
- `over_budget_calls_in_one_step_are_marked_failed`：一步 5 个调用 + 预算 2 → 3 个失败结果；
- `max_steps_ends_turn_without_failure`；
- `zero_or_unset_env_means_unlimited`：空/0/非法 env = 不限制。

### 2. LLM 采样参数（`crates/llm-deepseek` / `crates/llm-openai`）

- 去掉硬编码 `temperature: 0.3`；改为 `Option<f64>` 字段，默认从
  `DEEPSEEK_TEMPERATURE` / `OPENAI_TEMPERATURE` 读取，未设置时**请求体里不出现**该键。
- 新增 `DEEPSEEK_MAX_TOKENS` / `OPENAI_MAX_TOKENS`（`Option<u64>`），未设置时不发送。
- 提供 `with_temperature(Option<f64>)` / `with_max_tokens(Option<u64>)` 构造器便于显式配置与测试。
- 单测断言请求体有/无 `temperature`、`max_tokens` 两个键。

### 3. Retry（`crates/llm-retry`）

- `RetryPolicy::default()`：`max_attempts = 6`（DSH `maxRetries=5`）、`base_delay = 500ms`、
  `max_delay = 10s`、`jitter_ratio = 0.1`。
- `delay_for()` 保持确定性的指数退避（测试/封顶用）；新增
  `jittered_delay_for(attempt, unit)` 做 ±10% 抖动并再次封顶，`unit` 可注入以便断言上下界。
- `DSHR_RETRY_ATTEMPTS`（1–10）/`DSHR_RETRY_BASE_MS`/`DSHR_RETRY_MAX_MS` 继续可覆盖，
  新增 `DSHR_RETRY_JITTER`（0–1）。`RetryPolicy::immediate(n)` 保留
  （`chat --selftest-retry` 用的就是它，语义不变）。
- 已知差异：DSH 还会重试 `EMPTY_RESPONSE`，dshr provider 目前不产生该错误码（M3）。

### 4. 上下文窗口（`crates/agent-context`）

`context_window_from_env()` 优先级：

1. `DSHR_CONTEXT_WINDOW`（>= 4000）——Gate 3 自检的 `8000` 原样生效；
2. 否则读 `DSHR_MODEL` > `DEEPSEEK_MODEL` > `OPENAI_MODEL`，按模型名分档
   （`resolve_context_window` 纯函数 + 单测）：
   `flash` / `v4-pro` / `v4.1` → `1_048_576`；`deepseek-chat` / `deepseek-reasoner` → `131_072`；
   `gpt-4o` / `gpt-4.1` / `o3` / `o4` → `128_000`；其余 / 未配置 → `131_072`
   （两家默认模型都是 128K；更小窗口的模型请显式设 `DSHR_CONTEXT_WINDOW`）。
3. 压缩的 `TRIGGER_RATIO=0.8`、`TARGET_RATIO=0.6`、`TOOL_RESULT_TOKEN_LIMIT=800`、
   spill 阈值等**未改**（Gate 3 与既有测试数据依赖它们）。

要跑 DSH desktop 同款 1M：`DEEPSEEK_MODEL=deepseek-flash` + `DSHR_CONTEXT_WINDOW=1048576`
（不设后者的新默认也会给 1M，但显式更清晰）。

## 环境变量速查

| 变量 | 作用 | 默认 |
|---|---|---|
| `DSHR_MAX_TOOL_CALLS` | 每轮最多执行的工具调用数；命中降级不失败 | 不限制 |
| `DSHR_MAX_STEPS` | 每轮最多 step 数；命中 `TurnEnded` | 不限制 |
| `DEEPSEEK_TEMPERATURE` / `OPENAI_TEMPERATURE` | 显式发送采样温度 | 不发送 |
| `DEEPSEEK_MAX_TOKENS` / `OPENAI_MAX_TOKENS` | 显式发送输出 token 上限 | 不发送 |
| `DSHR_RETRY_ATTEMPTS` | 总尝试次数（1–10） | 6 |
| `DSHR_RETRY_BASE_MS` / `DSHR_RETRY_MAX_MS` | 退避起点/封顶 | 500 / 10000 |
| `DSHR_RETRY_JITTER` | 抖动比例（0–1） | 0.1 |
| `DSHR_CONTEXT_WINDOW` | 显式上下文窗口（>= 4000） | 按模型分档 |
| `DSHR_MODEL` / `DEEPSEEK_MODEL` / `OPENAI_MODEL` | 模型名（也参与窗口分档） | `deepseek-chat` / `gpt-4o-mini` |

## 验证

```text
tools\build-env.cmd cargo test -p dshr-agent-core -p dshr-agent-context -p dshr-llm-retry -p dshr-llm-deepseek -p dshr-llm-openai
tools\build-env.cmd cargo clippy -p dshr-agent-core -p dshr-agent-context -p dshr-llm-retry -p dshr-llm-deepseek -p dshr-llm-openai --all-targets -- -D warnings
tools\build-env.cmd cargo fmt --all -- --check
tools\build-env.cmd cargo build -p dshr-desktop-gpui --bin chat
.\target\debug\chat.exe --selftest
.\target\debug\chat.exe --selftest-retry
```

真实回归口径：默认预算下，一个 step 发 ≥9 个工具调用不再出现
`tool call budget exhausted (8)` / `TurnFailed`；`s-1794404310974.jsonl` 那类场景
应看到全部 `ToolResult` 后正常 `TurnEnded`。

## 结论

**已修的 bug**

1. 工具调用 8 次硬上限导致整轮失败（用户现场）——改为默认无上限，显式上限时降级而非失败。
2. context window 固定 64K 低估模型（DeepSeek 128K / DSH 1M）——改为按模型名分档，未配置兜底 128K。
3. temperature 硬编码 0.3、retry 3 次/无抖动——对齐 DSH 默认（不发送 / 6 次 + 500ms + 10s + ±10%）。

**有意的差异（不改）**

- 默认模型/Provider：`deepseek-chat` / `gpt-4o-mini` vs DSH desktop 的 `deepseek-flash`
  （不假设 API key 有 flash 权限）；要 1M 按上面的 env 显式切换。
- output `max_tokens` 不默认发送（不照抄 256k）。
- 工具执行仍串行（DSH 每步并发 10）；压缩已有 LLM 摘要但保留比/溢出重试与 DSH 不同；spill/裁剪阈值口径不同。

**M3 再做**

1. 每步工具并发执行（对齐 `maxParallelToolCalls=10`）。
2. reasoning effort（off/low/high/max）与 1M 模型目录。
3. LLM 摘要压缩（`context.compacted` 目前只有引用还原，没有摘要生成）。
4. 重复工具调用软提醒（`thresholds [3,5,8]`）。
5. `EMPTY_RESPONSE` 重试码；approval 的 `read-only` / `danger-full-access` 模式切换。
