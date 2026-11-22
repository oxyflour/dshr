# 稳定性基座（F，第一阶段，2026-11-11）

M2 gate 1 要求"20 步 turn 成功率 ≥99%、≥2 家 provider"。这一轮落：
第二 provider、统一重试/退避、故障注入自检。

## Provider 家族

| provider | 配置 | 流式 | 说明 |
|---|---|---|---|
| `deepseek` | `DEEPSEEK_API_KEY` / `DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL` | SSE 真流式 | M1 已有 |
| `openai` | `OPENAI_API_KEY`（或 `DSHR_OPENAI_API_KEY`）/ `OPENAI_BASE_URL` / `OPENAI_MODEL` | 非流式（整段一个 delta） | 新增，OpenAI Chat Completions 兼容；独立实现，验证 Agent 不依赖单一 API 细节 |
| `mock` | 无 | 8 字符切片 | 确定性自检 |

选择顺序：`DSHR_PROVIDER`（mock/deepseek/openai）显式指定 → 否则按 key 自动
（deepseek 优先，其次 openai）→ 都没有则 mock。真实 provider 统一包
`RetryingProvider`。

## 重试/退避

`crates/llm-retry`：

- 只重试**瞬时**错误：429 / 5xx / timeout / connection reset / unexpected eof /
  overloaded；`400/401/403`、参数错误不重试；
- 指数退避 `base * 2^(n-1)`，默认 200ms 起、封顶 2s、最多 3 次
  （`DSHR_RETRY_ATTEMPTS` / `DSHR_RETRY_BASE_MS` / `DSHR_RETRY_MAX_MS`）；
- **流式安全**：只有在还没有向 UI 交付任何 delta 时才允许重试；已经显示过部分文本
  就立即失败（否则会重复显示/错位）。

7 个单元测试：瞬时重试成功、永久错误不重试、到达上限放弃、流式未产出可重试、
已产出不重试、退避递增封顶、错误分类。

## 故障注入自检

`dshr --selftest-retry`：`FlakyProvider` 每第 3 次请求返回 `HTTP 503`，
外面套 max_attempts=4 的重试；每轮先返回一次工具调用（`probe_native`），
工具结果回来后再返回文本。连续跑 20 轮：

```text
[retry] turns=20 succeeded=20 failed=0 tool_executions=20
       provider_calls=59 (fault every 3rd call, max attempts 4)
```

59 次 provider 调用 = 20 轮 × 2 步 + 19 次被重试吃掉的故障；20 次工具执行全部成功，
20 轮零失败。这不能代替真实网络的 20 步成功率，但把"故障 → 重试 → 工具继续执行 →
成功收尾"的链路变成了可重复的本地断言。

## 真实网络 20 步成功率（Gate 1 实测，2026-11-11）

`dshr --selftest-live-20`：真实 DeepSeek（`deepseek-chat`，SSE 流式），每步要求模型调用
一次本地工具 `probe_native` 并确认结果；统计口径为"每步一次模型请求 + 至少一次真实工具调用，
`tool.call`/`tool.result` 配对且无错误/中止"。

```text
[live-20] provider=auto steps=20 ok=20 failed=0 rate=1.0000
         tool_calls=20 tool_results=20 tool_errors=0 p50_ms=2697 p95_ms=4847
[selftest-live-20] PASS
```

- 成功率 **20/20 = 100%**，达到 Gate 1 的 ≥99%；
- 每步 1.4–4.8 s（p50 2.7 s、p95 4.8 s），无工具错误、无重试耗尽；
- 需要 `DEEPSEEK_API_KEY`；没有 key 时自检返回 SKIP（exit 2），不伪装通过；
- 失败时退出码 1，可以进 CI（有 key 的环境）。

## Sidecar 瞬时错误分类（T3 补充）

`docs/spec/sidecar-rpc.md` §9.2 定义了 sidecar 瞬时错误分类与重试建议：
`E_TIMEOUT` / `E_BACKPRESSURE` / `E_INTERNAL` 可重试，写工具（`fs.write.request`）
**不参与自动重试**（避免重复写/半写）。宿主当前对 sidecar 工具失败仍按"显式失败卡片 +
`tool.aborted` 事件"处理，自动重试留给 Agent 决策（下轮可选）。

## 未做

- 工具层自动重试（sidecar 超时/崩溃时由宿主重放只读工具调用）；
- 真实 provider 上"重试后成功"的注入验证（需要伪造网络故障，目前只有本地 FlakyProvider）；
- SSE 中途断流后**已产出部分文本**的恢复（当前直接失败，语义上不重放）。
