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

### 3.1 host_caps（M2）

| cap | 含义 | Sidecar 行为 |
|---|---|---|
| `tool.stream` | Sidecar 可发 `tool.chunk` | 流式工具启用 |
| `tool.cancel` | Rust 会发 `tool.cancel` | 每个调用挂 `AbortController` |
| `fs.write` | Sidecar 可发 `fs.write.request`（见 §7.5） | **只有收到时才装载官方 `write`/`edit`/`apply_patch`**；未收到时强制只读 |

未列入的能力一律视为"宿主不支持"；Sidecar 不得凭配置自行打开对应通道。
`runtime.ready.sidecar_caps` 回声本侧实际启用的能力（含 `fs.write`）。

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
| `tool.register` / `tool.unregister` | → | `definition`（含 `output_schema?` / `presentation?`，对齐 DSH `output.schema` / `presentResult` 卡片类型）, `registration_id` | 重复注册返回同一 id |
| `tool.invoke` / `tool.chunk` / `tool.result` / `tool.cancel` | ↔ | 见 §7 | `result` 恰好一次 |
| `fs.write.request` / `fs.write.result` | ↔ | 见 §7.5 | `result` 恰好一次（按 `request_id` 关联） |
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
6. `tool.result` 载荷为 `{call_id, is_error, content[], value, meta?}`（对齐 DSH `ToolExecutionResult`；字段语义见 [`cpp-v1.md`](cpp-v1.md) §7.3）。Host 只把 `value`/`meta` 落进 Session Log 的事件载荷，卡片由 `presentation` 折叠，不在传输层做渲染。

## 7.5 文件写入回调（`fs.write`）

官方 `@deepseek-ai/dsh-tool-fs` 的 `write`/`edit`/`apply_patch` 不在 Sidecar 直接落盘：
它们经官方 `ctx.fs` 服务发出写入意图，Sidecar 翻译成 `fs.write.request` 交给 Rust
裁决（路径边界、审批、写前快照、原子写、版本），结果用 `fs.write.result` 返回。

```text
Sidecar ──fs.write.request(req)──► Rust（在等待 tool.result 的读循环里同步服务）
Sidecar ◄──fs.write.result────────── Rust
```

请求 payload：

| 字段 | 必需 | 说明 |
|---|---|---|
| `call_id` | 是 | 发起写入的 `tool.invoke.call_id`；审批审计使用同一身份 |
| `tool` | 是 | `write` / `edit` / `apply_patch`（审批展示用） |
| `operation` | 是 | `write`（create-or-replace，内容即全文）或 `delete` |
| `path` | 是 | 工作区相对路径，或工作区内绝对路径；Rust 负责归一化与边界校验 |
| `workspace_root` | 否 | Sidecar 认为的工作区根；与宿主不一致 → `FS_SANDBOX_DENIED`（防混淆代理） |
| `content` | 条件 | `write` 必填（UTF-8 全文，≤ 1 MiB）；`delete` 必须缺省 |
| `expected_version` | 否 | 版本守卫 `mtime_ms:size`；不匹配 → `FS_STALE_VERSION` |
| `intent` | 否 | 官方 `fs/write-intent` 水位；仅透传审计，不参与裁决 |
| `mode` | 否 | 说明性字段（当前固定 `workspace-write`；真实约束在 Rust） |

成功答复 payload：`{ok:true, path, operation:create|update|delete, version?, bytes?, snapshot?}`。
`version` 为写入后的 `mtime_ms:size`；`snapshot` 为覆盖/删除前落进
`.dshr/snapshots/` 的快照 ID。失败答复：`{ok:false, error:{code,message,retryable}}`，
错误码见 §9.1。

约束：

1. 每个请求恰好一个答复；Sidecar 在收到答复前**不得**再发同 `request_id` 的请求。
2. Rust 在 `tool.invoke` 的等待循环里服务该请求（`compat-dsh-host`），因此不会出现
   "Sidecar 等写入答复、宿主等 tool.result" 的死锁。
3. Rust 的审批/写盘是同步阻塞的：宿主侧 approval timeout（默认 60 s）必须小于
   `tool.invoke` 的 `timeout_ms`（默认 120 s），否则工具超时后答复无人消费。
4. 未协商 `fs.write` 时 Rust 一律回 `E_FS_CAPABILITY`，不做任何写入。
5. Sidecar 的写方法只在 `host_caps` 含 `fs.write` 时对插件可见（写工具未注册）；
   即使被直接调用也返回结构化错误。

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

### 9.1 `fs.write` 错误码

`fs.write.result.payload.error.code` 采用官方 `@deepseek-ai/dsh-fs` 的 `FS_*` 词表
（Rust 直接返回；Node 侧再翻译成官方 `FsError`，其中 `FS_SANDBOX_DENIED` 会转成
`FS_PERMISSION_DENIED`，原因见 [`../m2-official-write.md`](../m2-official-write.md)）：

| 码 | 触发 |
|---|---|
| `FS_SANDBOX_DENIED` | 路径越界 / `workspace_root` 不一致 / 审批拒绝或超时 |
| `FS_PERMISSION_DENIED` | OS 权限拒绝（也是 Node 侧收到的最终码） |
| `FS_STALE_VERSION` | `expected_version` 不匹配 |
| `FS_TOO_LARGE` | 内容超过 1 MiB |
| `FS_NOT_FOUND` | 目标/父目录不存在，或删除不存在的文件 |
| `FS_NOT_REGULAR_FILE` | 目标是目录 |
| `FS_IO_ERROR` | 其它 I/O / 请求失败 |
| `E_FS_REQUEST` | payload 形状错误（协议层，非官方词表） |
| `E_FS_CAPABILITY` | 未协商 `fs.write` 却收到写请求 |

### 9.2 Sidecar 瞬时错误分类与重试建议

`RpcError.retryable` 与协议实现 `dshr_compat_dsh_protocol::is_retryable_sidecar_error`
是**建议值**，最终是否重试由工具执行器按"错误分类 × 工具幂等性"裁决：

| 分类 | 错误码 | 建议 |
|---|---|---|
| 瞬时（错误本身） | `E_TIMEOUT`、`E_BACKPRESSURE`、`E_INTERNAL` | 可退避后重试；`E_BACKPRESSURE` 必须退避 |
| 永久（协议/参数） | `E_PROTOCOL`、`E_VERSION`、`E_SCHEMA`、`E_RPC_DEPTH`、`E_TOO_LARGE`、`E_UNKNOWN_PLUGIN`、`E_DUPLICATE_REG` | 不重试；修配置/参数后重新发起 |
| 取消/已结算 | `E_CANCELLED`、`E_ALREADY_SETTLED` | 不重试 |
| 崩溃 | `runtime.crashed` / 管道 EOF | 在途调用全部 `aborted` 写日志，**不自动重试**；新 generation 重新注册后由用户/Agent 决定 |

工具幂等性约束：

- 只读工具（`read`/`load_workspace_dependencies` 等）幂等，可自动重试瞬时错误；
- **写工具（`write`/`edit`/`apply_patch`）不幂等**：重试会重复审批、重复落盘
  （`edit` 可能第二次找不到 old_string）。它们返回结构化 `tool.result` 错误，
  由模型重读文件后重新决策；宿主不做自动重试。
- 重试必须重新走 `tool.invoke`（新 `request_id`/`call_id`），不得复用已结算的调用；
  流式已交付部分内容时按 §7 的流式安全规则处理。

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
6. **写路径走 Rust**（§7.5）：绝对路径只接受工作区内的，相对路径拒绝 `..` 与符号链接
   逃逸，写前落快照；Sidecar 直接调用 Node `fs` 写文件属于越权（M2 靠代码审查，
   OS 级隔离见后续 Job Object/沙箱工作）。

## 12. Conformance

| 前缀 | 覆盖 |
|---|---|
| `RPC-HANDSHAKE-*` | 版本协商、拒绝路径、超时 |
| `RPC-REG-*` | 注册/撤销/重复注册/generation 失效 |
| `RPC-EXEC-*` | invoke/chunk/result 顺序、幂等、大小上限 |
| `RPC-WRITE-*` | `fs.write` 能力协商、审批/越界/版本/快照、读循环不死锁 |
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
