# CPP-v1 — Cordis Portable Profile v1

> 状态：Draft（M3 定型） · 版本：`cordis-portable/v1` · 依赖：[`adc-v1.md`](adc-v1.md)、[`session-log.md`](session-log.md)
> 适用对象：Rust 静态插件、WASM Component 插件、独立进程插件。**DSH JavaScript 插件不走本规范**（见 [`sidecar-rpc.md`](sidecar-rpc.md)）。

---

## 1. 范围

本规范定义插件与 Rust Plugin Host 之间的契约：manifest、生命周期、Service、Effect、Event、Tool、Config 与错误模型。它是 DSH/Cordis 心智模型的**严格子集**——保留 `inject / service / effect / event`，去掉动态语义（Proxy、`!!js`、任意 `ctx.plugin()`、`waterfall`）。

## 2. 术语

| 术语 | 含义 |
|---|---|
| **Fiber** | 一个插件实例的生命周期容器；拥有自己的 Service 注册、Effect 与事件订阅 |
| **Realm** | 隔离命名空间；默认每个插件一个 Realm，只能看到本 Realm 与显式 export 的 Service |
| **Service** | 带版本的类型化能力，键形如 `tool-registry/v1` |
| **Effect** | 归属于 Fiber 的可逆副作用（注册、订阅、文件监听…），卸载时按逆序清理 |
| **Generation** | 插件的一次装载实例编号；重载即换代，旧代的一切注册作废 |

## 3. Manifest

### 3.1 位置与格式

插件产物根目录的 `plugin.toml`（UTF-8）。静态插件由 Host 在编译期嵌入同一份内容；WASM 插件随组件包分发。

### 3.2 示例

```toml
[plugin]
id = "cad.validate"
version = "0.1.0"
api = "cordis-portable/v1"
description = "Validate STEP parts against a rule set"

[runtime]
kind = "wasm-component"
entry = "cad_validate.wasm"
fuel = 2_000_000_000
max_memory_mb = 128
timeout_ms = 30_000

[inject]
required = ["tool-registry/v1", "ui-contributions/v1"]
optional = ["storage/v1"]

[permissions]
workspace_read = true
workspace_write = false
network = false

[[tools]]
name = "cad_validate"
description = "Validate a STEP file and return structured findings"
input_schema = "schemas/cad_validate.input.json"
output_schema = "schemas/cad_validate.output.json"
timeout_ms = 120_000
cancelable = true

[[ui]]
slot = "workspace.panel"
id = "cad.inspector"
label = "CAD Inspector"
view = "cad.inspector"
```

### 3.3 字段表

| 字段 | 必需 | 约束 |
|---|---|---|
| `plugin.id` | 是 | `vendor.name`，小写，长度 ≤ 64 |
| `plugin.version` | 是 | SemVer |
| `plugin.api` | 是 | 必须等于 `cordis-portable/v1` |
| `runtime.kind` | 是 | `rust-static` / `wasm-component` / `external-process` |
| `runtime.entry` | 条件 | `wasm-component` 与 `external-process` 必需 |
| `runtime.fuel` / `max_memory_mb` / `timeout_ms` | 条件 | `wasm-component` 必需；缺省值见 §3.6 |
| `inject.required` / `inject.optional` | 否 | Service key 列表；未知 key 视为配置错误 |
| `permissions.*` | 否 | 默认全部为 `false` |
| `tools[]` | 否 | 见 §7 |
| `ui[]` | 否 | 语法糖，等价于启动时调用 `ctx.ui.register`；校验在注册时进行 |

未知字段 MUST 导致装载失败（避免拼写错误被静默忽略）。

### 3.4 权限模型

| 能力 | 覆盖范围 |
|---|---|
| `workspace_read` | 读工作区文件（不含工作区外路径） |
| `workspace_write` | 写工作区文件；写入需经 Policy 审计 |
| `network` | 出站网络；默认拒绝，域名需在 `network_allow` 列出 |
| `process_spawn` | 启动子进程 |
| `credential_use` | 申请使用某凭据（不能读取明文） |

规则：默认拒绝；不支持通配符；`workspace_write` 隐含 `workspace_read`；权限变更视为**装载时**属性，运行中不可提升（需重启 Fiber）。

### 3.5 配额

| 项 | 默认 | 上限 |
|---|---:|---:|
| 单次 Tool 执行 | 120 s | 600 s |
| WASM fuel | 2e9 | 1e10 |
| WASM 线性内存 | 128 MB | 512 MB |
| 单次 Tool 输出 | 1 MB | 8 MB（超出须转为附件引用） |
| 并发 Tool 执行 | 4 | 16 |
| UI 元素提交 | 200 节点 | 500 节点 |

## 4. 生命周期

```text
Pending ──load──► Loading ──apply ok──► Active
   │                 │                    │
   │                 └─apply fail/timeout─►Failed
   │                                      │
   └──────────────unload──────────────────┴──► Unloading ──► Disposed
```

| From | 事件 | To | 副作用 |
|---|---|---|---|
| Pending | `load` | Loading | 解析 manifest、校验 API 版本、分配 generation |
| Loading | `apply` 成功 | Active | 服务已发布、Effect 已登记、UI 已提交 |
| Loading | `apply` 失败/超时（10s） | Failed | 回滚已登记的一切 Effect |
| Active | `unload` | Unloading | 拒绝新注册（`E_UNLOADING`） |
| Unloading | 清理完成 | Disposed | 逆序执行 Effect；撤销 UI；回收 Service |
| Unloading | 清理超时（15s） | Disposed | 强制回收并记录 `E_UNLOAD_TIMEOUT` 诊断 |
| Failed | `retry` | Pending | 退避 1s/2s/4s，最多 3 次 |

规则：`Disposed` 终态，不可复活；重启即新 generation。

## 5. Service

### 5.1 键

`<name>/v<major>`，例如 `tool-registry/v1`、`storage/v1`。major 变化即不兼容替换，不允许同时注入两个 major。

### 5.2 提供与撤销

```rust
let effect = ctx.publish::<dyn ToolRegistry>("tool-registry/v1", impl_)?;
// effect drop 即撤销；撤销是幂等的
```

- 同一 Realm 内同一 key 只能有一个提供者；重复提供返回 `E_SERVICE_CONFLICT`。
- 撤销后，依赖它的 Fiber 先停用（`Active → Pending`），恢复后自动重启，最多 3 次。
- 依赖重启期间拒绝新的 Tool 调用（`E_SERVICE_MISSING`），不排队。

### 5.3 注入语义

- `required` 全部可用后 Fiber 才进入 `Active`；等待上限 10 s，超时进 `Failed`。
- `optional` 缺失不影响启动；运行中通过 `ctx.try_get` 探测，缺失返回 `None`（不抛错）。
- Realm 隔离：未显式 export 的服务，其他插件不可见。

## 6. Effect

1. Effect 归属创建它的 Fiber；Fiber 卸载即全部回收。
2. 清理顺序为**逆注册序**（LIFO）。
3. 单个 Effect 清理超时 2 s：记诊断、继续下一个，不中断整体清理。
4. 清理必须幂等；重复清理不得产生副作用。
5. 卸载期间禁止登记新 Effect（`E_UNLOADING`）。

## 7. Tool

### 7.1 定义

| 字段 | 必需 | 说明 |
|---|---|---|
| `name` | 是 | `[a-z][a-z0-9_]*`，全局唯一；重名即 `E_TOOL_CONFLICT` |
| `description` | 是 | 模型可见，≤ 1 KB |
| `input_schema` / `output_schema` | 是 | JSON Schema 文件路径 |
| `timeout_ms` | 否 | 默认 120 s |
| `cancelable` | 否 | 默认 `true` |
| `presentation` | 否 | 结果卡片类型：`generic` / `terminal` / `diff` / `search` / `read` / `web`；未知值 MUST 退回 `generic`，不得丢弃结果 |

### 7.2 Canonical value

参数与结果必须是有限 JSON（禁止 `NaN`/`Infinity`）；数字按 IEEE-754 double 表示；键顺序无关；单值大小 ≤ 1 MB。超出时必须改为附件引用：`{"$attachment": "<id>", "size": 4194304}`。

### 7.3 结果

```json
{
  "is_error": false,
  "content": [
    { "type": "text", "text": "3 rules failed" },
    { "type": "json", "value": { "findings": [] } }
  ],
  "value": { "rules_failed": 3, "findings": [] },
  "meta": { "duration_ms": 812 },
  "metadata": { "duration_ms": 812 }
}
```

- `content` 非空；`text` 与 `json` 二选一或并存，`image` 必须引用附件。**`content` 是模型可见的**。
- `value` 是结构化返回值（对齐 DSH `ToolResult.value`）：展示与回放只用它，不要求模型可见。
- `meta` 是工具自带的展示元数据（对齐 DSH `presentationMeta`），可为 `null`。
- `is_error: true` 表示**工具执行失败**（模型可见），与协议错误（`E_*`）区分。
- 卡片由 `presentation` + `value` 折叠而来，折叠 MUST 确定性且有界（预览行数/单行长度上限）；形状不匹配时退回 `generic`。

### 7.4 取消

- 收到 `cancel` 后 MUST 在 5 s 内返回；超过则 Host 标记 `aborted` 并丢弃迟到结果。
- 取消不得产生半写入：副作用要么完成并留痕，要么回滚。
- 已结算的调用不可再取消（`E_ALREADY_SETTLED`）。

## 8. Config

- 只接受 JSON/YAML 纯数据；`!!js`、锚点展开后的非 JSON 值、任意构造器一律拒绝。
- 字段由 `config.schema`（JSON Schema）声明；未知字段拒绝。
- 校验失败 → Fiber `Failed`，不影响其他插件。
- 配置变更默认触发 Fiber 重启（`Disposed → Pending`）；插件可在 manifest 声明 `config_hot_reload = true`，改为收到 `on_config` 回调，由插件保证可重入。

## 9. 错误模型

| 码 | 含义 | 可恢复 |
|---|---|---|
| `E_MANIFEST` | manifest 解析/字段错误 | 否（需改包） |
| `E_VERSION` | `api` 不匹配 | 否 |
| `E_PERMISSION` | 缺少声明的能力 | 否（需改 manifest） |
| `E_QUOTA` | 超出配额（fuel/内存/输出） | 是（重试或降规模） |
| `E_SERVICE_CONFLICT` | 同 key 重复提供 | 否 |
| `E_SERVICE_MISSING` | 必需服务不可用 | 是 |
| `E_APPLY_TIMEOUT` | `apply` 超时 | 是（退避重试） |
| `E_UNLOAD_TIMEOUT` | 清理超时 | 是（诊断后可重载） |
| `E_TOOL_CONFLICT` | Tool 名冲突 | 否 |
| `E_TOOL_SCHEMA` | 参数不符合 schema | 是（模型可修正重试） |
| `E_CANCELLED` | 被取消 | 是 |
| `E_UNLOADING` | 卸载期间尝试注册 | 否 |
| `E_INTERNAL` | 插件内部错误 | 视情况 |

## 10. 版本与兼容

- `api` 只写 major；minor 通过 capability 探测（`ctx.capability("cpp.tool.stream")`）。
- Service key 的 major 变化即不兼容；宿主同时最多提供一个 major。
- WASM 插件额外固定 WIT 包版本；组件与宿主 WIT 版本不匹配即 `E_VERSION`。
  WIT 世界定义（草案）见 [`cpp-v1.wit`](cpp-v1.wit)；Wasmtime 主机接入时由 `wit-bindgen` 校验后冻结。
- `cdylib` 不作为公开扩展通道：不承诺 ABI 稳定，仅内部使用。
- 弃用流程：规范标记 `deprecated` → 至少一个 major 周期保留 → 删除。

## 11. 未决问题

| # | 问题 | 倾向 | 时点 |
|---|---|---|---|
| Q1 | 是否需要 `on_config` 之外的热更新钩子 | 只保留 `on_config` | M3 |
| Q2 | WASM 插件能否共享宿主内存做零拷贝 | 初期不做，先保证隔离 | M3 |
| Q3 | 独立进程插件的 transport 与 sidecar 是否复用 | **已定（ADR-0010）**：复用 NDJSON + envelope 语义，独立握手（见 §12） | M3 |
| Q4 | 静态插件的 manifest 是否允许编译期宏生成 | 允许，但产物必须等价于手写 | M3 |

---

## 12. `external-process` 运行时（`cpp-process/v1`，M3 退路）

> 状态：Review（M3 实现）。动机与配额见 [ADR-0010](../decisions/0010-isolation-runtime-quota.md)：
> 本机离线无法获取 Wasmtime，G3 的"隔离"由独立进程 + Windows Job Object 承担；WASM 主机就绪后
> 同一套配额/结算模型复用。

### 12.1 传输与进程边界

- 子进程由宿主启动（`runtime.kind = "external-process"`，`runtime.entry` 是包内相对路径的可执行文件）。
- **NDJSON over stdio**：stdout 只走协议（一行一条 JSON，≤1 MiB/行），stderr 是日志（宿主转发到
  `log.emit` 同级诊断），stdin 收宿主消息。
- 宿主用 `sidecar-guard` 的 Job Object 包裹：进程数、内存、CPU 时间、随宿主退出；cwd 是插件数据目录；
  环境变量清洗（不继承凭据，见 §3.4/PLAN §7）。
- 一份 generation 一个进程；崩溃 = generation 失效，在途调用 `aborted`，宿主不崩。

### 12.2 消息

帧形状与 [`sidecar-rpc.md`](sidecar-rpc.md) §4 的 envelope 同构（`version/kind/request_id/generation/
trace_id/ts/payload/error`），`version` 固定 `"cpp-process/v1"`：

| kind | 方向 | payload | 说明 |
|---|---|---|---|
| `process.hello` | host → child | `{protocol, host_version, plugin_id, generation, manifest}` | 启动后宿主先发，10 s 内必须收到 ready |
| `process.ready` | child → host | `{protocol, plugin_id, tools[], panels[]}` | `tools[]`/`panels[]` 见 §12.3；未知协议/插件 id 不符即拒绝 |
| `process.ping` / `process.pong` | ↔ | `{}` | 健康检查（宿主 15 s 一次） |
| `tool.invoke` | host → child | `{tool, arguments, timeout_ms, cancelable}` | 调用一次工具；同一 `request_id` 唯一 |
| `tool.chunk` | child → host | `{seq, text}` / `{seq, content}` | 可选流式；宿主只记诊断/转发，不阻塞结算 |
| `tool.result` | child → host | `{content[], value?, meta?, is_error}` | 恰好一次；形状对齐 §7.3 |
| `tool.cancel` | host → child | `{reason}` | child MUST 5 s 内以 result（`E_CANCELLED`）或 aborted 结算 |
| `ui.render` | host → child | `{view, state_seq}` | 声明过的 view 的元素树（ADC-v1 JSON） |
| `ui.result` | child → host | `{tree}` 或 `{error}` | 宿主用 `parse_tree` 校验/降级后交给 ADC 渲染器 |
| `ui.action` | host → child | `{view, action, payload}` | ADC 动作回传（`call`/`command`/`copy`；`openExternal` 由宿主处理） |
| `process.shutdown` | host → child | `{timeout_ms}` | 优雅退出；超时 `TerminateJobObject` |
| `process.bye` | child → host | `{}` | 退出确认（可选；进程退出即等价） |

错误统一走 envelope `error`（`E_*` 闭集）：`E_PROTOCOL`/`E_VERSION`/`E_SCHEMA`/`E_TIMEOUT`/
`E_CANCELLED`/`E_QUOTA`/`E_INTERNAL`。`error.message` 不得含凭据/绝对路径/堆栈。

### 12.3 声明（`process.ready`）

```json
{
  "tools": [{
    "name": "cad_validate",
    "description": "Validate a STEP file",
    "input_schema": { "type": "object", "required": ["path"], "properties": { "path": {"type":"string"} } },
    "output_schema": { "type": "object" },
    "timeout_ms": 120000,
    "cancelable": true,
    "presentation": "generic"
  }],
  "panels": [{ "slot": "workspace.panel", "id": "cad.inspector", "label": "CAD Inspector",
               "view": "cad.inspector", "location": "right", "order": 100 }]
}
```

- `input_schema`/`output_schema` **内联**（≤8 KiB），必须是 JSON Schema 子集；宿主按 §7.1 校验。
- `panels` 走 ADC-v1 slot 契约（`workspace.panel` 等），注册校验由 `ui-contributions` 完成；
  未声明 view 的 panel 拒绝。`view` 的树由 `ui.render` 提供，宿主不缓存旧树。
- 工具名全局唯一（与 Native/Sidecar/MCP 同表）；冲突 → `plugin.failed`（`E_TOOL_CONFLICT`）。

### 12.4 配额（与 Wasmtime 主方案同构，ADR-0010 §1）

| 维度 | 来源 | 超限行为 |
|---|---|---|
| 墙钟超时 | tool 声明 `timeout_ms` / `process.ready` 10 s | 杀 Job → `E_TIMEOUT`，会话无脏写 |
| CPU 时间 | manifest `fuel` 折算或 `cpu_ms`（缺省 60 s/调用） | 杀 Job → `E_QUOTA` |
| 内存 | manifest `max_memory_mb`（1–512） | Job 内存上限触发 → `E_QUOTA` |
| 输出 | 单 result ≤ 1 MB（上限 8 MB，超出转附件） | 截断/`E_TOO_LARGE`，不阻塞其它调用 |
| 并发 | 每插件 4 / 全局 16 | `E_BACKPRESSURE`（退避可重试） |

### 12.5 生命周期

```text
spawn → process.hello → process.ready → apply(tools/panels 注册) → Active
  → tool.invoke* / ui.render* → process.shutdown → bye/exit → Disposed
```
- `process.ready` 超时/协议不符/插件 id 不符 → `Failed`，杀进程；不影响其它插件。
- 撤销（unload）幂等；重启即新 generation。
- 宿主退出时 Job Object 兜底杀子进程。

### 12.6 安全

- `entry` 必须是包内相对路径（拒绝 `..`、绝对路径、符号链接）；不在 PATH 里找可执行文件。
- 不经过 shell；参数由 manifest/宿主固定，插件不得让模型注入命令行。
- 子进程拿不到宿主凭据环境变量；文件权限由 Agent 工具策略（Rust）裁决，不信任子进程自报。
- `ui.render` 的树按 ADC-v1 作者期严格校验 + 运行时降级（未知元素 Placeholder）；元素树不得触发网络。

