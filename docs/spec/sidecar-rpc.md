# Sidecar RPC — Rust ↔ Node DSH Sidecar 跨进程协议

> 状态：Draft（M2 冻结） · 协议版本：`1` · 依赖：[`session-log.md`](session-log.md)
> 角色：Rust 是唯一权威；Sidecar 只执行 DSH JS 插件并把声明过的东西注册回来。Sidecar **不是**第二套 Session/Agent/权限系统。

---

## 1. 边界

| 属于 Rust | 属于 Sidecar |
|---|---|
| Session 事件、工具审批、结果结算、审计 | JS 插件的加载、npm 解析、Cordis Fiber 生命周期 |
| 工具执行策略、超时、取消裁决 | 插件内部 Effect 清理 |
| `session_id` → `Agent` 的解析 | 只知道 `session_id`，不知道 `Agent` |
| 稳定 ID 与 generation 分配 | 按分配到的 ID 回调 |

任何"把 JS 对象或 Rust trait 透传"的设计都是违规。

## 2. Transport

| 层 | 方案 | 说明 |
|---|---|---|
| 控制通道 | NDJSON（一行一条 JSON） | 命名管道优先，M1 实测后再定；抽象为 `Transport` trait |
| 二进制/大对象 | 内容寻址附件（`attachment://<hash>`） | 不进控制通道，不做 base64 |
| 日志 | **仅 stderr** | stdout 保留给协议；任何非协议输出即 `E_PROTOCOL` |

帧上限：单行 ≤ 1 MB；超出须改用附件引用或分块。

## 3. 握手与版本协商

```text
Rust                                    Sidecar
 │  runtime.hello {protocol_max, dsh_version, host_caps[]}  │
 │ ──────────────────────────────────────────────────────► │
 │ ◄──────── runtime.ready {protocol, sidecar_caps[], generation} │
 │  （不兼容时：runtime.reject {reason, supported[]}，随后退出）  │
```

规则：

- 双方取 `min(protocol_max)`；无交集即拒绝，**不得静默降级**。
- `generation` 由 Rust 分配（`node-0007`），Sidecar 不得自增。
- 握手超时 10 s → 判定启动失败，Rust 继续以 Native-only 模式运行。

## 4. Envelope

```json
{
  "version": 1,
  "kind": "tool.invoke",
  "request_id": "req-000123",
  "runtime_generation": "node-0007",
  "plugin_id": "example.dsh.tool",
  "registration_id": "reg-0042",
  "session_id": "s-0001",
  "call_id": "call-123",
  "depth": 0,
  "trace_id": "t-9f3c",
  "ts": "2026-11-08T10:00:00.000Z",
  "payload": {},
  "error": null
}
```

| 字段 | 必需 | 说明 |
|---|---|---|
| `version` | 是 | 协议主版本，整数 |
| `kind` | 是 | 见 §6 |
| `request_id` | 是 | 一次请求/响应对的关联 ID；响应必须回填 |
| `runtime_generation` | 是 | 发起方所属 generation；不匹配即丢弃 |
| `plugin_id` | 条件 | 与插件相关的消息必需 |
| `registration_id` | 条件 | 注册/执行类消息必需 |
| `session_id` | 条件 | 涉及会话的消息必需（工具、事件） |
| `call_id` | 条件 | 执行类消息必需 |
| `depth` | 是 | 跨进程回调深度，从 0 起；> 4 即 `E_RPC_DEPTH` |
| `trace_id` | 是 | 跨 Runtime 统一追踪 |
| `ts` | 是 | RFC 3339 UTC |
| `payload` | 是 | 业务字段（可为空对象） |
| `error` | 否 | `{code, message, retryable}` |

## 5. 身份与 generation

1. `generation` 单调递增；旧 generation 的任何消息一律丢弃并记诊断。
2. `registration_id` 由 Rust 分配；Sidecar 只能回填，不得自拟。
3. **`session_id` 是唯一身份锚点**：Rust 在 adapter 边界把它还原成 `Agent`；反向传对象即违规。
4. 插件卸载 → 该插件的所有 registration 立即失效；迟到回调按 §9 处理。

## 6. 消息族

| kind | 方向 | 关键 payload | 幂等 |
|---|---|---|---|
| `runtime.hello` / `runtime.ready` / `runtime.reject` | ↔ | 版本与能力 | — |
| `runtime.crashed` | → | `reason`, `exit_code` | — |
| `plugin.load` / `plugin.loaded` / `plugin.failed` | ↔ | `plugin_id`, `entry` | load 幂等（同 id 返回已加载） |
| `plugin.unload` / `plugin.unloaded` | ↔ | `plugin_id`, `timeout_ms` | unload 幂等 |
| `tool.register` / `tool.unregister` | → | `definition`, `registration_id` | 重复注册返回同一 id |
| `tool.invoke` / `tool.chunk` / `tool.result` / `tool.cancel` | ↔ | 见 §7 | `result` 恰好一次 |
| `service.publish` / `service.revoke` | → | `service_key`, `methods[]` | key 冲突即 `E_SERVICE_CONFLICT` |
| `service.invoke` / `service.result` | ↔ | `method`, `args` | 读操作幂等；写操作由方法声明 |
| `ui.contribute` / `ui.retract` | → | slot 注册（见 [`adc-v1.md`](adc-v1.md)） | 同 `registration_id` 幂等 |
| `event.subscribe` / `event.unsubscribe` | → | `event`, `filter?` | 幂等 |
| `event.emit` | ↔ | `event`, `payload` | 不重放 |
| `health.ping` / `health.pong` | ↔ | `rss_mb`, `uptime_ms` | 幂等 |
| `log.emit` | → | `level`, `message`, `plugin_id` | 不保证投递 |
| `capabilities.list` / `diagnostics.snapshot` | ↔ | — | 幂等 |

## 7. 执行语义

```text
Rust ──tool.invoke(req)──► Sidecar
Rust ◄──tool.chunk #0────  Sidecar
Rust ◄──tool.chunk #1────  Sidecar
Rust ◄──tool.result──────  Sidecar
```

规则：

1. `tool.chunk` 的 `seq` 从 0 单调递增；`bytes_total` 单调不减。
2. `tool.result` 必须恰好一次；`result` 之后的 `chunk` 丢弃并记诊断。
3. 流式累计超过 8 MB 时，后续内容转附件引用。
4. 并发上限：默认 4 个 in-flight 调用/插件；超出返回 `E_BACKPRESSURE`，Sidecar 必须暂停发起新调用。
5. 每个调用都有 `timeout_ms`（Rust 侧裁决，默认 120 s）；超时即 `E_TIMEOUT`，随后发送 `tool.cancel` 兜底。

## 8. 取消

- 方向：Rust → Sidecar（`tool.cancel`），携带 `reason`。
- Sidecar MUST 在 5 s 内以 `tool.result`（`E_CANCELLED`）或 `tool.aborted` 响应。
- 取消后的迟到结果：Rust 丢弃、记 `tool.late_result` 诊断、**不重试**。
- 已结算调用再取消 → `E_ALREADY_SETTLED`。

## 9. 错误模型

| 码 | 触发 | 可重试 |
|---|---|---|
| `E_PROTOCOL` | 非法 JSON/未知 kind/缺字段/非协议 stdout | 否 |
| `E_VERSION` | 版本无交集 | 否 |
| `E_UNKNOWN_PLUGIN` | 插件未加载或已卸载 | 否 |
| `E_DUPLICATE_REG` | 注册冲突 | 否 |
| `E_SCHEMA` | 参数/结果不符合 schema | 是（修正后） |
| `E_TIMEOUT` | 超过 `timeout_ms` | 视工具语义 |
| `E_CANCELLED` | 被取消 | 否 |
| `E_RPC_DEPTH` | `depth > 4` | 否 |
| `E_TOO_LARGE` | 超过帧/结果上限 | 否（改用附件） |
| `E_BACKPRESSURE` | 并发/窗口超限 | 是（退避） |
| `E_INTERNAL` | 未分类异常 | 视情况 |

`error.message` 面向开发者，不得包含凭据、绝对路径或堆栈原文（堆栈进诊断通道）。

## 10. 崩溃与恢复

| 场景 | 行为 |
|---|---|
| 管道 EOF / 心跳 3×5 s 无响应 | 判定 `runtime.crashed`，Rust 立即失效该 generation |
| 在途调用 | 全部标记 `aborted` 写入 Session Log；不自动重试 |
| 已结算结果 | 不回退，不重放 |
| 重启 | 新 generation；插件重新 `plugin.load` + 重新注册；旧 registration 不可复用 |
| 连续崩溃 | 5 分钟内 3 次即进入 `failed`，需用户手动启用 |

## 11. 安全

1. **路径一律由 Rust 校验**：Sidecar 传来的路径只在 workspace 根下有效，且必须规范化后再比较。
2. 不序列化函数、Proxy、类实例；payload 必须是有限 JSON。
3. `depth` 限制阻断互递归；持有核心锁时**禁止**等待远端调用（代码审查 + 运行时断言）。
4. `tool.invoke` 的 `arguments` 在进入 Sidecar 前已通过 schema 校验；Sidecar 再校验一次属于冗余，不是依赖。
5. 安装第三方插件必须经过用户确认（见 PLAN §7），协议层不提供"静默安装"。

## 12. Conformance

| 前缀 | 覆盖 |
|---|---|
| `RPC-HANDSHAKE-*` | 版本协商、拒绝路径、超时 |
| `RPC-REG-*` | 注册/撤销/重复注册/generation 失效 |
| `RPC-EXEC-*` | invoke/chunk/result 顺序、幂等、大小上限 |
| `RPC-CANCEL-*` | 取消传播、迟到结果、已结算 |
| `RPC-CRASH-*` | 崩溃检测、在途 aborted、重注册 |
| `RPC-LIMIT-*` | 背压、depth、帧上限 |

## 13. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| Q1 | 控制通道选命名管道还是 stdio | 命名管道（Windows 上更稳），M1 实测 | M1 |
| Q2 | 心跳周期与判定阈值 | 5 s × 3 | M1 |
| Q3 | 是否允许 Sidecar 主动 `tool.invoke` 回调 Rust 工具 | 允许，但 depth 受限 | M2 |
| Q4 | 附件传输是否复用 HTTP 本地回环 | 倾向复用（已有 webServer），M1 定 | M1 |
