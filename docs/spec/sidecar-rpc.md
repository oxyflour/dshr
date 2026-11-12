# Sidecar RPC — Rust ↔ Node DSH Sidecar 跨进程协议

> 状态：Review（**v1 行为在 M2 冻结，M3 复核**；**v2 host-service 代理为 M3 增补**，见 §13） · 协议版本：`2`（协商规则见 §3.2，v1 完全兼容） · 依赖：[`session-log.md`](session-log.md)、[`../m3-l2-services.md`](../m3-l2-services.md)
> 角色：Rust 是唯一权威；Sidecar 只执行 DSH JS 插件并把声明过的东西注册回来。Sidecar **不是**第二套 Session/Agent/权限系统。
>
> 版本策略：**v2 只做向后兼容的增补**——v1 的消息 kind、字段语义、错误码与 conformance 行为一律不变；
> v2 新增 `service_key` 可选字段与 4 个服务代理 kind（§13）。协商到 `1` 时双方**不得**发送任何 v2-only kind。


---

## 1. 边界

| 属于 Rust | 属于 Sidecar |
|---|---|
| Session 事件、工具审批、结果结算、审计 | JS 插件的加载、npm 解析、Cordis Fiber 生命周期 |
| 工具执行策略、超时、取消裁决 | 插件内部 Effect 清理 |
| `session_id` → `Agent` 的解析 | 只知道 `session_id`，不知道 `Agent` |
| 稳定 ID 与 generation 分配 | 按分配到的 ID 回调 |
| **Host Service 的真实实现**（`workingDirectory`、`storage`、Session Log 读面等，见 §13） | **服务方法表声明与代理转发**（v2）：把 JS 插件的 `ctx.<service>` 调用翻译成 `host.service.request`，把宿主实现翻译成插件可见的 Promise |

任何"把 JS 对象或 Rust trait 透传"的设计都是违规。v2 的服务代理同样只传有限 JSON：
**服务方法的 args / result / 错误码跨进程，函数、Promise、Proxy、类实例一律不跨进程**（§13.9）。

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

### 3.1 host_caps

| cap | 版本 | 含义 | Sidecar 行为 |
|---|---|---|---|
| `tool.stream` | v1 | Sidecar 可发 `tool.chunk` | 流式工具启用 |
| `tool.cancel` | v1 | Rust 会发 `tool.cancel` | 每个调用挂 `AbortController` |
| `fs.write` | v1 | Sidecar 可发 `fs.write.request`（见 §7.5） | **只有收到时才装载官方 `write`/`edit`/`apply_patch`**；未收到时强制只读 |
| `service.proxy` | v2 | 宿主实现了白名单内的 Host Service，并接受 `host.service.request`（§13.4） | 只有在收到时才允许把 `ctx.<service>` 注入插件；未收到时这些服务视为不存在，依赖它们的插件 fail-fast（不注册工具） |
| `service.expose` | v2 | 允许 Sidecar 用 `service.publish` 把 JS 实现的服务暴露给宿主（§13.3） | 未收到时收到 `service.publish` 回 `E_SERVICE_DENIED`；v2 **默认关闭**，首批三个服务都不需要它 |
| `service.attach` | v2 | 宿主支持服务结果的附件引用（§13.8） | 未收到时 > 768 KiB 的服务结果一律 `E_TOO_LARGE`；首批三个服务都声明了分页/小结果 |

未列入的能力一律视为"宿主不支持"；Sidecar 不得凭配置自行打开对应通道。
`runtime.ready.sidecar_caps` 回声本侧实际启用的能力（含 `fs.write`、`service.proxy` 的实现程度）。
v1 宿主不发 v2 的 cap，v1 Sidecar 也不认识它们——两边都按"未列入即不支持"处理，因此**混跑安全**。

### 3.2 版本协商（v1 ↔ v2）

| 宿主 `protocol_max` | Sidecar `protocol_max` | 协商结果 | 行为 |
|---|---|---|---|
| 1 | 1 | `1` | 纯 v1：不得发 `service_key`、不得发任何 v2-only kind |
| 2 | 1 | `1` | 同上；宿主必须降级（不发 `service.proxy`，不要求任何 L2 服务） |
| 1 | 2 | `1` | 同上；Sidecar 的 L2 服务全部不注入 |
| 2 | 2 | `2` | v2：发 `service_key`，按 §13 代理 Host Service |

规则：

1. 仍按 §3 的 `min(protocol_max)` 取版本；**没有交集即拒绝**，不得静默降级。
2. envelope 的 `version` 字段 = **本次协商出的版本**（不是"消息自身版本"）。协商为 1 时所有消息写 `1`；
   协商为 2 时所有消息写 `2`。因此 v2-only kind 只可能出现在 `version:2` 的消息里。
3. 协商为 1 时收到 `service.cancel`/`host.service.*`（v2-only kind）→ `E_PROTOCOL`，记诊断；
   协商为 2 时收到 v1 未列出的 kind 同理。
4. v2 的**增补不改变 v1 的任何 payload 形状**：`service_key` 是可选字段，v1 消息不带它，
   v1 实现按 `additionalProperties` 宽松处理即可（`schema/sidecar-rpc/envelope.schema.json` 已收录该字段）。


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

上例是**协商为 1** 的 v1 信封（v2 下同一消息只是 `version` 变成 `2`，字段不变）。
v2 服务代理消息另带 `service_key`，示例见 §13.4。

| 字段 | 必需 | 说明 |
|---|---|---|
| `version` | 是 | 本次协商出的协议版本（§3.2）：`1` 或 `2`。不是"消息自身版本" |
| `kind` | 是 | 见 §6 |
| `request_id` | 是 | 一次请求/响应对的关联 ID；响应必须回填 |
| `runtime_generation` | 是 | 发起方所属 generation；不匹配即丢弃 |
| `plugin_id` | 条件 | 与插件相关的消息必需 |
| `registration_id` | 条件 | 注册/执行类消息必需 |
| `service_key` | 条件 | **v2 服务代理消息必需**（`service.publish/revoke/invoke/result/cancel`、`host.service.*`，见 §13）；v1 消息一律缺省。形态 `name/vN`，见 [`README.md`](README.md) §3 |
| `session_id` | 条件 | 涉及会话的消息必需（工具、事件、会话级服务方法） |
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
   服务方法在需要会话时同样只收 `session_id`（§13.1），不得收 `Agent`/`Session` 对象或其句柄。
4. 插件卸载 → 该插件的所有 registration 立即失效；迟到回调按 §9 处理。
5. **`service_key` 是 v2 服务调用的路由锚点**：它决定"调谁"，`session_id` 仍决定"以谁的身份"；
   两者正交，服务方法可以只要 `service_key`（无会话，如 `storage-kv/v1` 的插件级域），
   但**只要方法按会话授权，就必须带 `session_id`**，否则 `E_SERVICE_ARGS`。
6. 服务调用的 generation 语义与工具一致：旧 generation 的请求与答复一律丢弃；在途调用全部
   以 `E_SERVICE_UNAVAILABLE` 结算，不重放（§13.5）。

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
| `service.publish` / `service.revoke` | Sidecar → host | 信封 `service_key` + `methods[]`、`generation`（§13.3） | key 冲突即 `E_SERVICE_CONFLICT`；需 `service.expose` |
| `service.invoke` | host → Sidecar | `method`, `args`（§13.4） | 读幂等；写按方法声明 |
| `service.result` | Sidecar → host | `result` 或 `content_ref`；错误走信封 `error` | 恰好一次答复 |
| `service.cancel` | host → Sidecar | `method`（可缺省） | 幂等；v2 新增 |
| `host.service.request` | Sidecar → host | `method`, `args`（§13.4） | 读幂等；写按方法声明；v2 新增 |
| `host.service.result` | host → Sidecar | `result` 或 `content_ref`；错误走信封 `error` | 恰好一次答复；v2 新增 |
| `host.service.cancel` | Sidecar → host | `method`（可缺省） | 幂等；v2 新增 |
| `ui.contribute` / `ui.retract` | → | slot 注册（见 [`adc-v1.md`](adc-v1.md)） | 同 `registration_id` 幂等 |
| `event.subscribe` / `event.unsubscribe` | Sidecar → host | `event`, `filter?` | 幂等 |
| `event.emit` | ↔ | `event`, `payload` | 不重放 |

服务相关的补充约定（v2）：

- **方向是协议的一部分**：`service.*` 是"宿主调 JS"（§13.1 A 路），`host.service.*` 是"JS 调宿主"
  （§13.1 B 路）。两类 kind 不得混用；`service.invoke` 只由宿主发起、`host.service.request` 只由
  Sidecar 发起。v1 只声明了 `service.invoke/result`，未定义方向也未实现（见 §13.10）。
- **JS 发布的服务不新增事件 kind**：服务事件复用 `event.emit`，`event` 名固定为 `service.event`，
  payload `{service_key, topic, data}`；订阅方用 `event.subscribe {event:"service.event", filter:{service_key}}`。
  因此 v2 **不引入** `service.event` 这个 kind（评估结论见 §13.7）。
- **会话事件推送**复用同一条通道：`event` 名为 `session.event`，payload
  `{session_id, seq, type, data}`（§13.7）。

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

## 7.6 服务调用（v2）

v2 把"Host Service"纳入同一条执行通道，规范全文见 §13。这里只钉死与工具执行共享的 4 条不变量：

1. **方向**：`host.service.request` 只由 Sidecar 发起（JS 插件消费宿主服务）；`service.invoke`
   只由宿主发起（宿主调用 Sidecar 发布的服务）。见 §13.1。
2. **depth 递增**：一次服务调用使 `depth + 1`，与工具回调同一上限（> 4 → `E_RPC_DEPTH`）。
   在 `tool.invoke` 处理器里发起的服务调用，`depth = tool.invoke.depth + 1`。
3. **不阻塞、不重入**：宿主处理 `host.service.request` 时不得阻塞协议读循环，也不得在同一
   调用链上反向发起同步的 `service.invoke`（只能通过事件通道，§13.7）；Sidecar 不得在等待
   服务答复时阻塞 `tool.result` 的发送（服务调用超时由 §13.6 裁决）。
4. **generation 失效**：在途服务调用随 generation 变化一律以 `E_SERVICE_UNAVAILABLE` 结算，
   迟到答复丢弃并记 `service.late_result` 诊断（§13.5）。

## 8. 取消

- 方向：Rust → Sidecar（`tool.cancel`），携带 `reason`。
- Sidecar MUST 在 5 s 内以 `tool.result`（`E_CANCELLED`）或 `tool.aborted` 响应。
- 取消后的迟到结果：Rust 丢弃、记 `tool.late_result` 诊断、**不重试**。
- 已结算调用再取消 → `E_ALREADY_SETTLED`。
- **服务调用（v2）**：
  - 宿主取消侧：`service.cancel`（host → Sidecar）带上 `request_id`；Sidecar MUST 在 5 s 内以
    `service.result`（`E_CANCELLED`）结算；迟到的 `service.result` 由宿主丢弃并记
    `service.late_result`。
  - 插件取消侧：工具被 `tool.cancel` 中断时，Sidecar MUST 对该调用链上在途的
    `host.service.request` 发 `host.service.cancel`；宿主 MUST 尽力中止实现（文件/存储 I/O
    至少不再写盘）并以 `E_CANCELLED` 结算，不得再回成功结果。
  - 已结算的请求再取消 → 幂等忽略（不报错、不计诊断）。
  - 取消是**建议性**的：宿主无法中止的同步 I/O 可以继续跑完，但结果必须按 `E_CANCELLED`
    丢弃，不得让插件看到"取消后成功"。


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
| `E_ALREADY_SETTLED` | 已结算的调用/请求被再次结算或取消（v1 正文已引用，v2 补记进闭集） | 否 |
| `E_SERVICE_UNAVAILABLE` | `service_key` 未发布/未协商/已 revoke；宿主没有该方法表；generation 已失效 | 否（换 generation 或重新发布后再试） |
| `E_SERVICE_METHOD` | 未知方法；方法 `kind` 不受支持（v2 的 `stream`） | 否 |
| `E_SERVICE_ARGS` | `args` 不符合 `args_schema`，或缺必需身份字段（`session_id`） | 是（修正后） |
| `E_SERVICE_TIMEOUT` | 超过方法 `timeout_ms`（裁决方见 §13.6） | 是（仅限 `idempotent:true`，需退避且换 `request_id`） |
| `E_SERVICE_DENIED` | manifest/白名单未授权；未协商 `service.proxy`/`service.expose` | 否 |
| `E_SERVICE_CONFLICT` | `service.publish` 的 `service_key` 冲突（v1 §6 已引用，v2 补记进闭集） | 否 |

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
| 瞬时（错误本身） | `E_TIMEOUT`、`E_BACKPRESSURE`、`E_INTERNAL`、`E_SERVICE_TIMEOUT` | 可退避后重试；`E_BACKPRESSURE` 必须退避 |
| 永久（协议/参数） | `E_PROTOCOL`、`E_VERSION`、`E_SCHEMA`、`E_RPC_DEPTH`、`E_TOO_LARGE`、`E_UNKNOWN_PLUGIN`、`E_DUPLICATE_REG`、`E_SERVICE_ARGS`、`E_SERVICE_METHOD`、`E_SERVICE_UNAVAILABLE`、`E_SERVICE_DENIED`、`E_SERVICE_CONFLICT` | 不重试；修配置/参数后重新发起 |
| 取消/已结算 | `E_CANCELLED`、`E_ALREADY_SETTLED` | 不重试 |
| 崩溃 | `runtime.crashed` / 管道 EOF | 在途调用全部 `aborted` 写日志，**不自动重试**；新 generation 重新注册后由用户/Agent 决定 |

工具幂等性约束：

- 只读工具（`read`/`load_workspace_dependencies` 等）幂等，可自动重试瞬时错误；
- **写工具（`write`/`edit`/`apply_patch`）不幂等**：重试会重复审批、重复落盘
  （`edit` 可能第二次找不到 old_string）。它们返回结构化 `tool.result` 错误，
  由模型重读文件后重新决策；宿主不做自动重试。
- 重试必须重新走 `tool.invoke`（新 `request_id`/`call_id`），不得复用已结算的调用；
  流式已交付部分内容时按 §7 的流式安全规则处理。

### 9.3 服务错误码的放置位置（v2）

- 服务代理的失败走**信封 `error`**（`{code,message,retryable}`），与 `tool.result` 一致；
  信封 `code` 只取 §9 主表里的 `E_*`（含 §9 新增的 `E_SERVICE_*`）。
- **服务实现自有的错误词表不进信封**：放进 `payload.detail`。例如 `storage` 的官方
  `StorageError` 码放 `detail.storage_code`（`version-mismatch` / `missing-key` / `closed` /
  `already-open` / `facet-unsupported` / `unknown-state` / `backend-not-found`），
  信封 `code` 仍是 `E_SERVICE_ARGS`/`E_SERVICE_UNAVAILABLE`/`E_INTERNAL` 之一。
- 例外：`fs.write.result` 沿用 §9.1 的 `FS_*` 词表放在 `payload.error`（v1 既定行为，不改）。
- `E_SERVICE_*` 与 `E_ALREADY_SETTLED` 目前**不在** `dshr_compat_dsh_protocol::ERROR_CODES`
  与 `is_retryable_sidecar_error` 的闭集里；实现 L2 时必须先同步这两个常量，否则 Rust 侧
  类型校验会拒绝新码。这是 L2 落地的第一个改动点（见 [`../m3-l2-services.md`](../m3-l2-services.md) §7）。

## 10. 崩溃与恢复

| 场景 | 行为 |
|---|---|
| 管道 EOF / 心跳 3×5 s 无响应 | 判定 `runtime.crashed`，Rust 立即失效该 generation |
| 在途调用 | 全部标记 `aborted` 写入 Session Log；不自动重试 |
| **在途服务调用（v2）** | 一律以 `E_SERVICE_UNAVAILABLE` 结算（不写 Session），不自动重试；迟到答复丢弃 |
| **已发布服务（v2）** | generation 失效即全部 revoke；重启后必须重新 `service.publish` |
| 已结算结果 | 不回退，不重放 |
| 重启 | 新 generation；插件重新 `plugin.load` + 重新注册；旧 registration 不可复用 |
| 连续崩溃 | 5 分钟内 3 次即进入 `failed`，需用户手动启用 |

## 11. 安全

1. **路径一律由 Rust 校验**：Sidecar 传来的路径只在 workspace 根下有效，且必须规范化后再比较。
2. 不序列化函数、Proxy、类实例；payload 必须是有限 JSON。
3. `depth` 限制阻断互递归；持有核心锁时**禁止**等待远端调用（代码审查 + 运行时断言）。
4. `tool.invoke` 的 `arguments` 在进入 Sidecar 前已通过 schema 校验；Sidecar 再校验一次属于冗余，不是依赖。
5. 安装第三方插件必须经过用户确认（见 PLAN §7），协议层不提供"静默安装"。
6. **服务白名单（v2）**：`ctx.<service>` 只有**同时**满足
   ① 协商到 `service.proxy`、② 宿主方法表（`runtime.ready.host_services`）列出该 `service_key`、
   ③ 该插件的 manifest/`inject` 声明允许注入，三条才注入；否则**装载期 fail-fast**
   （不注册任何工具，`plugin.failed`），运行期直接调用回 `E_SERVICE_DENIED`（§13.9）。
7. **方法表由宿主裁决（v2）**：`args_schema`/`result_schema` 只能来自宿主自己的方法表，
   Sidecar 不得用插件提供的 schema 覆盖、扩张或"借用"；宿主对每次调用**再校验一次** args。
8. **服务结果的最小暴露（v2）**：结果只含方法表声明的字段；`working-directory` 的 `cwd` 是
   显式授权的绝对路径例外，其余方法不得返回绝对路径、凭据或堆栈。
9. **写路径走 Rust**（§7.5）：绝对路径只接受工作区内的，相对路径拒绝 `..` 与符号链接
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
| `RPC-L2-*` | **v2 服务代理**：能力协商、方法表、超时/取消/幂等/背压、载荷上限、白名单 fail-fast；每服务一条真实往返（用例编号见 §12.1 与 [`../m3-l2-services.md`](../m3-l2-services.md) §5） |

### 12.1 `RPC-L2-*` 用例（协议级，0xx 段）

| ID | 断言 |
|---|---|
| `RPC-L2-001` | 协商为 1 时，宿主不发 `service.proxy`；Sidecar 不得发 `host.service.request`；主机收到 v2-only kind 回 `E_PROTOCOL` |
| `RPC-L2-002` | 协商为 2 且 `service.proxy` 在，宿主 `runtime.ready.host_services` 只列出白名单方法表；`capabilities.list` 与之一致 |
| `RPC-L2-003` | 未发布的 `service_key` → `E_SERVICE_UNAVAILABLE`；未知 `method` → `E_SERVICE_METHOD` |
| `RPC-L2-004` | args 违反 `args_schema`（多字段/缺字段/类型错）→ `E_SERVICE_ARGS`，宿主未执行任何副作用 |
| `RPC-L2-005` | 方法 `timeout_ms` 到期 → `E_SERVICE_TIMEOUT`（retryable=true）；取消路径按 §8 在 5 s 内结算 |
| `RPC-L2-006` | generation 变化 → 在途请求全部 `E_SERVICE_UNAVAILABLE`；迟到 `host.service.result` 丢弃并记 `service.late_result` |
| `RPC-L2-007` | `args` > 256 KiB → `E_TOO_LARGE`（宿主不执行）；结果 ≤ 768 KiB 内联，超过且无 `service.attach` → `E_TOO_LARGE`（有 `service.attach` → `content_ref` 往返） |
| `RPC-L2-008` | 每插件 4 个在途请求 / 每 Sidecar 16 个上限；超出 → `E_BACKPRESSURE`，Sidecar 必须暂停发起 |
| `RPC-L2-009` | `idempotent:true` 的同 `request_id` 重发命中 60 s 去重窗口，只执行一次；`idempotent:false` 重发 → `E_PROTOCOL` |
| `RPC-L2-010` | 服务调用 `depth + 1`；> 4 → `E_RPC_DEPTH`；同一链上不得反向同步 `service.invoke` |
| `RPC-L2-011` | 白名单外的服务注入 → 插件装载期 fail-fast（无 `tool.register`），运行期调用 → `E_SERVICE_DENIED` |
| `RPC-L2-012` | v1 回归：既有 20 条 schema 用例与 `RPC-EXEC/WRITE/CANCEL/CRASH` 行为不变（`node schema/check.mjs` 全绿） |

每服务的真实往返用例（1xx/2xx/3xx 段）在 [`../m3-l2-services.md`](../m3-l2-services.md) §5 定义；
编号仍属于 `RPC-L2-*` 前缀（`<AREA>-<CATEGORY>-<NNN>` 的 NNN 段按服务分块）。

## 13. v2 增补：Host Service 代理

> 版本：`2`（M3 增补） · 状态：Draft · 关联：[`../m3-l2-services.md`](../m3-l2-services.md)（首批服务选型与证据）
>
> 本节是**规范正文**；v1 的 §1–§12 语义在本节中不被修改，只被"增补方向"。

### 13.0 一句话模型

DSH 的 JS 插件通过 Cordis `inject` 拿到 `ctx.<service>`（`workingDirectory`、`sessionProjections`、
`storage`……）并直接调方法。v2 让这些方法调用**穿过进程边界**：Sidecar 把调用翻译成
`host.service.request`，宿主在其真实实现上执行并以 `host.service.result` 答复；反方向（宿主
调用 Sidecar 发布的服务）用 `service.invoke` / `service.result`。**函数、Promise、对象不跨进程，
只跨"方法名 + 有限 JSON"。**

### 13.1 方向矩阵（谁调用谁）

| 服务实现方 | 服务使用方 | kind | 方向 | v2 状态 |
|---|---|---|---|---|
| 宿主（Rust） | JS 插件 | `host.service.request` / `host.service.result` / `host.service.cancel` | Sidecar → host / host → Sidecar / Sidecar → host | **首批 3 服务的路径**，要求 `service.proxy` |
| JS 插件（Sidecar 内） | 宿主/原生插件 | `service.invoke` / `service.result` / `service.cancel` | host → Sidecar / Sidecar → host / host → Sidecar | 语义已定义，**默认关闭**，要求 `service.expose` |

两条路都不允许"透传对象"：

- 每端把 `session_id` 还原成自己的对象（宿主 → `Agent`，Sidecar → 会话句柄），
  **还原只发生在 adapter 边界**；
- 服务实现方只能看到方法表里声明的 JSON；
- 回调/流式回调（`systemPrompt.tools(provider)`、`sessionProjections` 的 `apply`）**不跨进程**：
  带函数的服务方法一律在 Sidecar 本地实现，宿主只提供事件与数据面（§13.7）。

### 13.2 `service_key` 与方法表

`service_key` 形态：`<name>/v<major>`，`name` 为 kebab-case（`[a-z][a-z0-9-]*`），
如 `working-directory/v1`、`session-log/v1`、`storage-kv/v1`。一个 major 内方法只增不改：
删除方法、改 args/result 语义、改 `kind` 都算破坏性变更，必须升 major。

方法表（`ServiceDescriptor`）：

```json
{
  "service_key": "working-directory/v1",
  "methods": [
    {
      "name": "get",
      "kind": "read",
      "args_schema": {
        "type": "object",
        "additionalProperties": false,
        "properties": {}
      },
      "result_schema": {
        "type": "object",
        "required": ["cwd"],
        "additionalProperties": false,
        "properties": { "cwd": { "type": "string" } }
      },
      "timeout_ms": 5000,
      "idempotent": true
    }
  ],
  "limits": { "max_inline_bytes": 262144, "max_result_bytes": 786432 }
}
```

| 字段 | 必需 | 规则 |
|---|---|---|
| `service_key` | 是 | 信封字段（§4），payload 不重复 |
| `methods[].name` | 是 | `[a-z][a-z0-9_]*`；同表内唯一 |
| `methods[].kind` | 是 | v2 只允许 `read` / `write`；`stream` 为**保留值**，v2 拒绝（`E_SERVICE_METHOD`） |
| `methods[].args_schema` | 是 | JSON Schema 子集，内联 ≤ 8 KiB；`type:"object"` 且 `additionalProperties:false`；**不含 `session_id`**（会话身份只在信封上） |
| `methods[].result_schema` | 是 | 同上；结果必须整体匹配 |
| `methods[].timeout_ms` | 是 | 整数，`[100, 120000]`；缺省 `30000`；越界 → `E_SCHEMA`（描述表校验失败） |
| `methods[].idempotent` | 是 | `true` = 可安全重试（读或幂等写）；`false` = 重试会重复副作用 |
| `limits.max_inline_bytes` | 否 | 缺省 262144（256 KiB），见 §13.8 |
| `limits.max_result_bytes` | 否 | 缺省 786432（768 KiB），见 §13.8 |

**会话身份不重复**：会话级方法的 `session_id` 只出现在信封（§4/§5），`args` 里不再携带；
无会话的服务（如 `storage-kv/v1`）只用 `plugin_id` + `service_key` 定作用域，不带 `session_id`。

方法表**声明在宿主侧**：宿主在 `runtime.ready.payload.host_services` 里给出自己愿意暴露的
方法表（白名单子集，不是全量；条目含 `service_key`），`capabilities.list` 的结果必须与之逐条一致。
Sidecar 发布服务时（§13.3，需 `service.expose`）payload 用同一形状但省略 `service_key`（走信封）。
Sidecar MUST 只按这份表建 `ctx.<service>` 代理：表里没有的方法在 JS 侧**不存在**（`undefined`
而不是"调用时报错"），这样官方插件的 `typeof`/可选调用分支不会被打乱。

### 13.3 发布与撤销（Sidecar → host 声明）

```json
{
  "version": 2, "kind": "service.publish", "request_id": "req-000501",
  "runtime_generation": "node-0007", "plugin_id": "example.dsh.tools",
  "service_key": "example-tools/v1", "depth": 0, "trace_id": "t-9f3c",
  "ts": "2026-11-12T10:00:00.000Z",
  "payload": {
    "methods": [
      {
        "name": "echo",
        "kind": "read",
        "args_schema": { "type": "object", "additionalProperties": false,
                         "properties": { "text": { "type": "string" } } },
        "result_schema": { "type": "object", "additionalProperties": false,
                           "properties": { "text": { "type": "string" } } },
        "timeout_ms": 5000,
        "idempotent": true
      }
    ]
  }
}
```

- 方法表与 §13.2 的 `ServiceDescriptor` 同形，只是 `service_key` 与 `generation` 由**信封**承载，
  payload 里不重复。
- `service.publish` 只在 `service.expose` 被协商时允许；`service_key` 已存在 → `E_SERVICE_CONFLICT`，
  且**不覆盖**旧服务（先 revoke 再 publish）。
- `service.revoke {service_key}` 幂等；撤销后在途 `service.invoke` 以 `E_SERVICE_UNAVAILABLE` 结算。
- 插件卸载/generation 失效 → 该插件发布的全部服务自动 revoke（§10）。
- host 侧对"表里声明但实现缺失"的方法返回 `E_SERVICE_UNAVAILABLE`（不 panic、不挂起）。

### 13.4 调用语义（关联、超时、幂等、背压）

**请求/答复**：

```json
{ "version": 2, "kind": "host.service.request", "request_id": "req-000412",
  "runtime_generation": "node-0007", "plugin_id": "example.dsh.tool",
  "service_key": "working-directory/v1", "session_id": "s-0001",
  "depth": 1, "trace_id": "t-9f3c", "ts": "2026-11-12T10:00:00.000Z",
  "payload": { "method": "set", "args": { "path": "src" } } }
```

```json
{ "version": 2, "kind": "host.service.result", "request_id": "req-000412",
  "runtime_generation": "node-0007", "service_key": "working-directory/v1",
  "depth": 1, "trace_id": "t-9f3c", "ts": "2026-11-12T10:00:00.031Z",
  "payload": { "result": { "cwd": "C:\\ws\\src" } }, "error": null }
```

规则：

1. **恰好一次答复**：每个请求恰好一个 `host.service.result`（成功或失败都算）；答复后同
   `request_id` 的再请求按本节第 4 条去重或报错。
2. **关联**：`request_id` 唯一（`^[A-Za-z0-9:_-]{1,64}$`），答复 MUST 回填同一 `request_id`
   与 `service_key`；`session_id`/`plugin_id` 由请求决定，答复不得替换。
3. **超时**：宿主按方法 `timeout_ms` 裁决，超时回 `E_SERVICE_TIMEOUT`（`retryable:true`）。
   Sidecar 侧另设 `timeout_ms + 2000` 的看门狗：先到者结算，另一侧迟到答复丢弃并记
   `service.late_result`。取消（§8）优先于超时。
4. **幂等与重试**：只有 `idempotent:true` 的方法允许自动重试，且只对
   `E_SERVICE_TIMEOUT`/`E_BACKPRESSURE`/`E_INTERNAL`；重试 MUST 换新 `request_id`。
   宿主对 `idempotent:true` 的 `(plugin_id, request_id)` 保留 **60 s 去重窗口**（重发同结果）；
   `idempotent:false` 的方法收到重复 `request_id` → `E_PROTOCOL`，不执行。
5. **背压**：在途 `host.service.request` 每插件 ≤ 4、每 Sidecar ≤ 16；超出 `E_BACKPRESSURE`，
   Sidecar MUST 暂停发起并退避（与 §7 第 4 条的工具并发上限独立计数）。
6. **不阻塞**：宿主收到请求后转入独立任务，不阻塞协议读循环；因此服务实现**不得**在
   自己的处理器里同步反向调用同一 Sidecar（防死锁），需要通知时走事件通道（§13.7）。
7. **重入**：一次服务调用使 `depth + 1`；`depth > 4` → `E_RPC_DEPTH`。

### 13.5 generation 失效

- 请求与答复的 `runtime_generation` 必须等于当前 generation；不等 → 丢弃 + `service.stale_generation` 诊断。
- generation 变化时：在途请求立刻以 `E_SERVICE_UNAVAILABLE` 结算（不等待真实结果），
  已发布服务全部 revoke，去重窗口清空。
- 重启后一切重建：`plugin.load` → `service.publish`（如有）→ 服务可用。

### 13.6 取消

见 §8 的服务条目。补充：`host.service.cancel` 的 `payload` 只有 `{method?}`，用信封 `request_id`
定位在途请求；取消是建议性（§8 最后一条）。

### 13.7 事件与"流式"（v2 结论：不做 chunk）

**结论：v2 不增加 `service.chunk`，也不做流式服务方法。** 理由（可核对）：

1. 首批 3 个服务的方法面全是请求/答复：`working-directory` 3 个方法、`storage-kv` 7 个方法、
   `session-log` 2 个方法；没有一个需要增量输出。
2. 唯一有"流"性质的需求是**会话事件推送**（`sessionProjections` 的驱动源），它是
   "有界、单调序号、可重放判定"的事件流，不是某次调用的分块输出——用 v1 的
   `event.subscribe`/`event.emit` 表达更准确，也复用了已有的重放/丢弃规则。
3. 引入 `service.chunk` 会带来"chunk 与 result 的结算竞态、取消时已交付前缀的语义、背压与
   序号"三类新问题，在 v1 的 `tool.chunk` 上已经付过一次成本（§7）；没有消费者就不重复付。

事件通道约定：

- 会话事件：`event` = `session.event`，payload `{session_id, seq, type, data}`。宿主按 `seq`
  单调发送，**不重放**；Sidecar 检测到 gap 时用 `session-log/v1.events.read` 补齐
  （`from_seq = 期望 seq`），补齐期间暂停 `apply`。
- 服务事件（JS 发布的服务）：`event` = `service.event`，payload `{service_key, topic, data}`；
  订阅用 `event.subscribe {event:"service.event", filter:{service_key}}`。
- 背压：事件积压超过 256 条或 1 MiB → 宿主暂停推送并回 `E_BACKPRESSURE` 语义的诊断
  （`event.backpressure`），待 Sidecar 追平后恢复；丢弃事件必须通过 `events.read` 补齐。

### 13.8 载荷上限与附件引用

| 位置 | 上限 | 超出行为 |
|---|---|---|
| `args` 内联 | 262144 B（不含信封字段） | 调用方 MUST 先拒绝（`E_TOO_LARGE`）；宿主再校验一次 |
| `result` 内联 | 786432 B | 宿主回 `content_ref`（需 `service.attach`）；未协商 → `E_TOO_LARGE` |
| 帧（NDJSON 行） | 1 MiB（§2 不变） | `E_TOO_LARGE` |
| 附件 | `attachment://<sha256>`（§2） | 单附件 ≤ 8 MiB；超限 `E_TOO_LARGE` |

- 附件引用只出现在 `result`：`payload = { "content_ref": "attachment://sha256:…" }`；
  `host.service.result` 不允许同时带 `result` 与 `content_ref`。
- **v2 不接受附件形式的 `args`**：大对象请用分页方法（`cursor`/`limit`）或 `storage` 域，
  避免把"传输层"塞进"调用语义"。
- 首批 3 个服务都按 768 KiB 设计：`session-log/v1.events.read` 必须分页；
  `storage-kv/v1.unit.loadAll` 超过上限直接 `E_TOO_LARGE`（v2 不做分页，记诊断，见
  [`../m3-l2-services.md`](../m3-l2-services.md) §6 风险）。

### 13.9 权限、白名单与错误

服务注入的三条件见 §11 第 6 条。补充：

| 场景 | 结果 |
|---|---|
| 未协商 `service.proxy` | 服务不注入；收到 `host.service.request` → `E_SERVICE_DENIED` |
| `service_key` 不在宿主方法表 | 服务不注入；直接调用 → `E_SERVICE_UNAVAILABLE` |
| 插件 manifest 未声明该服务可注入 | 装载期 fail-fast（`plugin.failed`，无工具注册） |
| 方法 `kind:"stream"` | `E_SERVICE_METHOD` |
| 方法 `args` 不合 schema | `E_SERVICE_ARGS`（`detail.path` 指向首个失败字段） |
| 实现内部错误（含存储官方词表） | `E_INTERNAL` + `payload.detail`（§9.3） |
| 结果过大且无 `service.attach` | `E_TOO_LARGE` |

审计：每次服务调用记一条 `diagnostics.snapshot` 可见的计数（`service_key`/`method`/耗时/结果码），
**不记录 args 内容**（可能含用户数据）；`working-directory` 的 `path` 与 `storage` 的 `key`
属于可记录的最小信息，记录方式与 `fs.write` 的审批审计一致。

### 13.10 与 v1 的兼容性

- v1 的 30 个 kind、全部字段与错误码语义**完全不变**。v2 协议只新增 4 个 kind 与 1 个可选字段
  （`service_key`）；schema 枚举另外补登记了 2 个 v1 正文已定义、实现已在用的 kind
  （`fs.write.request`/`fs.write.result`），那不是协议变更。
- v1 的 `service.publish/revoke/invoke/result` 在 v1 只出现在消息表里，**两侧都未实现**
  （`sidecar/dsh-official/sidecar-dsh.mjs` 只 provide `tools`/`fs`/`systemPrompt`，
  `compat-dsh-protocol` 只有 envelope + `fs.write` 类型）。因此 v2 钉死方向**不构成对既有行为的
  破坏**：v1 协商时这些 kind 仍不可用（发了也是 `E_PROTOCOL`）。
- schema 层面：`schema/sidecar-rpc/envelope.schema.json` 只做"字段/枚举"校验，
  **kind↔version 的配对是协议规则**（§3.2），schema 不表达（check.mjs 的校验子集不支持
  `if/then`）。建议后续补 2 条 fixture：`version:2 + service_key` 通过、`version:2 + 未知 kind` 拒绝。
- 顺手修一处 v1 漏登记：schema 的 `kind` 枚举原先缺 **`fs.write.request` / `fs.write.result`**
  （§7.5 早已定义、M2 已实现，`compat-dsh-protocol` 也有对应常量，只是 schema 漏了），
  v2 一并补上；这是 schema 与正文对齐，不改变任何消息语义。
- `compat-dsh-protocol` 需要跟进（不在本任务范围）：`PROTOCOL_VERSION = 2`、新增 4 个 kind 常量、
  `host_services` 方法表类型、`ERROR_CODES` 扩集、`CAP_SERVICE_PROXY/EXPOSE/ATTACH`。

## 14. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| Q1 | 控制通道选命名管道还是 stdio | 命名管道（Windows 上更稳），M1 实测 | M1 |
| Q2 | 心跳周期与判定阈值 | 5 s × 3 | M1 |
| Q3 | 是否允许 Sidecar 主动 `tool.invoke` 回调 Rust 工具 | 允许，但 depth 受限 | M2 |
| Q4 | 附件传输是否复用 HTTP 本地回环 | 倾向复用（已有 webServer），M1 定 | M1 |
| Q5 | 方法表放 `runtime.ready` 还是单独的 `service.catalog` 消息 | 先放 `runtime.ready.host_services`（少一个握手往返，v1 忽略未知字段）；表变大再加 catalog 消息 | M3 |
| Q6 | 服务白名单的**事实源**（manifest `inject` / profile patch / 宿主内置） | 宿主内置为默认，profile 只能收窄不能扩张；manifest `inject` 只做"声明需求" | M3 |
| Q7 | `storage-kv` 的介质（每插件 JSON 文件 / sqlite）与隔离粒度 | 先 JSON 文件（一个人可读、可备份），sqlite 留 M4 | M3 |
| Q8 | `session-log` 事件推送的 gap 补页上限（大 session 冷启动） | 单次 ≤ 2000 事件；超出分页并记 `projection.backfill` 诊断 | M3 |
| Q9 | 是否给 `service.attach` 一个传输实现，或直接删掉该 cap | 首批 3 服务不需要，倾向 M4 再定；v2 先保留 cap 与负向用例 | M4 |

