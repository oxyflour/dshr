# 会话树 v1（2026-11-11）

PLAN §6.5 的 M2 承诺：父子会话是一等对象、`spawn_child` 委派、权限继承只收紧、
中断按树传播、`drain` 两种等待语义。本文记录**已实现的语义/事件/API**与**明确未做项**；
事件格式的规范定义见 [`spec/session-log.md`](spec/session-log.md) §3.6 / §7。

## 1. 范围与边界

**v1 已实现（Rust 核心 + 测试）**

| 能力 | 位置 |
|---|---|
| `parent_session_id` / `spawn_source` 写入 `session.created`，回放可重建 | `crates/session-store` |
| 目录索引暴露树字段；`children` / `descendants` / `depth` / 无环校验 / 深度上限 | `crates/session-store/src/session_tree.rs` |
| `spawn_child` 工具定义（name/description/schema） | `crates/agent-core/src/session_tree.rs` |
| 子会话独立 Event Log + 父链接 + 来源 | `SessionTreeRuntime::spawn_child` |
| 默认深度上限 2（可配置），子会话只能收紧 | `SessionTreeRuntime::new(max_depth)` / `SessionPolicy` |
| 子会话继承父工作区与策略且只允许收紧 | `SessionPolicy::descend` |
| 中断向子会话传播，`interrupted_by_parent`，超时 kill 升级 | `interrupt_tree` / `escalate_interrupt` |
| drain：直接子级 / 全部后代；等待期间父保持 active | `begin_drain` / `poll_drain` / `is_idle` |
| 重启后从 Event Log 重建运行时树 | `SessionTreeRuntime::from_logs` |

**v1 明确不做（M5）**：团队任务板、跨会话共享内存、子会话之间直接通信、超过深度 2 的树、
远程 Agent 参与同一棵树。UI 树形 Dock 的 GPUI 呈现由集成轮完成（见 §7）。

## 2. 数据模型

### 2.1 会话元数据

`session.created` 是树结构的唯一权威来源，写在**子会话自己的** Event Log 第一条：

```json
{
  "type": "session.created",
  "payload": {
    "workspace_id": "ws-1",
    "parent_session_id": "s-1794329101557",
    "spawn_source": "agent",
    "preset": "reviewer"
  }
}
```

- `spawn_source`：`user`（UI 新建根会话）/ `agent`（父 Agent 通过 `spawn_child` 派生）。
  读取端对未知来源返回 `None`，不拒绝日志。
- `preset`：可继承父预设；子会话显式传入时覆盖。
- 旧日志没有 `session.created` 时按根会话处理（`parent_session_id = None`），不视为损坏。
- 深度从 0 开始（根 0 / 子 1 / 孙 2），默认上限 2。

`SessionIndex::scan()` 返回的 `SessionMeta` 新增 `parent_session_id` / `spawn_source`；
`*.meta.json` 仍只保存 UI 标题，不参与树结构（避免第二份真相）。

### 2.2 事件

| 事件 | 写在哪 | 语义 |
|---|---|---|
| `session.created` | 子会话自己的日志（第 1 条） | 父链接 / 来源 / 预设，不可改 |
| `child.spawned` | 父会话日志 | 记录派生了哪个子会话、prompt、触发消息 |
| `child.finished` | 父会话日志 | 子会话结束（`completed`/`failed`/`cancelled`/`interrupted`/`killed`） |
| `session.interrupted` | 中断发起方日志 | 我发起了中断（`by_parent: false`） |
| `session.interrupted_by_parent` | 被中断的子会话日志 | 被父会话中断（`interrupted_by_parent`） |
| `session.killed` | 被中断的子会话日志 | 中断超时后按 kill 语义结束（审计） |

父日志与子日志的 `seq` 空间完全独立；父日志只保存**引用事件**，因此删除/移动子会话文件
不会破坏父会话回放（子会话离线时树显示为"子会话（不可用）"由 UI 决定，v1 不实现）。

## 3. session-store API

```rust
// crates/session-store
pub enum SpawnSource { User, Agent }

pub struct SessionCreated {
    pub workspace_id: String,
    pub parent_session_id: Option<String>,
    pub spawn_source: Option<SpawnSource>,
    pub preset: Option<String>,
}
impl SessionCreated {
    pub fn root(workspace_id) -> Self;
    pub fn child(workspace_id, parent_session_id, spawn_source) -> Self;
    pub fn with_preset(self, preset) -> Self;
    pub fn with_optional_preset(self, Option<String>) -> Self;
    pub fn payload(&self) -> serde_json::Value;
    pub fn from_payload(&Value) -> Result<Self, LogError>;
    pub fn append_to(&self, log: &mut SessionLog, ts) -> Result<u64, LogError>;
}

impl SessionLog {
    pub fn created(&self) -> Option<&SessionCreated>;
    pub fn parent_session_id(&self) -> Option<&str>;
    pub fn spawn_source(&self) -> Option<SpawnSource>;
    pub fn append_created(&mut self, ts, &SessionCreated) -> Result<u64, LogError>;
    pub fn append_child_spawned(&mut self, ts, child_id, prompt, parent_message_id) -> Result<u64, LogError>;
    pub fn append_child_finished(&mut self, ts, child_id, outcome) -> Result<u64, LogError>;
    pub fn append_interrupted(&mut self, ts, reason) -> Result<u64, LogError>;
    pub fn append_interrupted_by_parent(&mut self, ts, parent_session_id, reason) -> Result<u64, LogError>;
    pub fn append_killed(&mut self, ts, reason) -> Result<u64, LogError>;
    pub fn interrupted_by_parent(&self) -> bool;
    pub fn was_killed(&self) -> bool;
}

pub struct SessionView {  // replay() 的结果，新增：
    pub created: Option<SessionCreated>,
    pub interruptions: Vec<InterruptionView>,  // { reason, by_parent, killed }
    pub children: Vec<ChildView>,              // { child_session_id, prompt, outcome }
}

// 静态树（纯计算）
pub struct SessionTree { /* children/descendants/depth/... */ }
impl SessionTree {
    pub fn from_metas(&[SessionMeta]) -> Result<Self, TreeError>;
    pub fn from_nodes(impl IntoIterator<Item = SessionNode>) -> Result<Self, TreeError>;
    pub fn roots(&self) -> Vec<&SessionNode>;
    pub fn children(&self, id: &str) -> Vec<&SessionNode>;
    pub fn descendants(&self, id: &str) -> Vec<&SessionNode>;
    pub fn depth(&self, id: &str) -> Option<u32>;
    pub fn is_ancestor(&self, ancestor: &str, id: &str) -> bool;
    pub fn flatten(&self) -> Vec<(u32, &SessionNode)>;       // 前序，UI 树形列表用
    pub fn validate_max_depth(&self, limit: u32) -> Result<(), TreeError>;
    pub fn check_child_depth(&self, parent_id: &str, limit: u32) -> Result<u32, TreeError>;
}

impl SessionIndex {
    pub fn tree(&self) -> io::Result<SessionTree>;
    pub fn root_sessions(&self) -> io::Result<Vec<SessionMeta>>;
    pub fn children_of(&self, id: &str) -> io::Result<Vec<SessionMeta>>;
    pub fn descendants_of(&self, id: &str) -> io::Result<Vec<SessionMeta>>;
    pub fn depth_of(&self, id: &str) -> io::Result<Option<u32>>;
    pub fn create_root(&self, id, workspace_id, ts) -> io::Result<SessionMeta>;
    pub fn create_child(&self, id, parent_session_id, workspace_id, ts) -> io::Result<SessionMeta>;
    pub fn create_with_created(&self, id, &SessionCreated, ts) -> io::Result<SessionMeta>;
    pub fn write_log(&self, &SessionLog) -> io::Result<PathBuf>;  // 子会话日志落盘
}
```

`SessionTree::from_metas` / `from_nodes` 构造时拒绝重复 ID 与父链成环；父会话已删除的
孤儿按根展示但保留 `parent_session_id`。同层排序按 `updated_at` 倒序、再按 id 升序。

## 4. agent-core API

```rust
// crates/agent-core
pub const SPAWN_CHILD_TOOL: &str = "spawn_child";
pub const DEFAULT_MAX_DEPTH: u32 = 2;

pub fn spawn_child_spec() -> ToolSpec;   // prompt 必需，preset 可选
pub fn is_spawn_child_tool(name: &str) -> bool;

pub struct SpawnChildArgs { pub prompt: String, pub preset: Option<String> }
impl SpawnChildArgs {
    pub fn from_json(&Value) -> Result<Self, SpawnError>;
    pub fn from_call(&ToolCallRequest) -> Result<Self, SpawnError>;
}

pub struct SessionPolicy {
    pub workspace_id: String,
    pub max_depth: u32,
    pub allowed_tools: Option<BTreeSet<String>>,
    pub denied_tools: BTreeSet<String>,
}
impl SessionPolicy {
    pub fn root(workspace_id) -> Self;
    pub fn with_max_depth(self, u32) -> Self;
    pub fn with_allowed_tools(self, ...) -> Self;
    pub fn with_denied_tools(self, ...) -> Self;
    pub fn allows_tool(&self, tool: &str) -> bool;
    pub fn descend(&self, &PolicyTightening) -> Result<Self, SpawnError>;  // 放宽 → PolicyWidening
}

pub struct SessionTreeRuntime { /* max_depth + 运行态 + drain 等待 */ }
impl SessionTreeRuntime {
    pub fn new(max_depth: u32) -> Self;
    pub fn with_default_max_depth() -> Self;
    pub fn register_root(&mut self, id, workspace_id) -> Result<(), SpawnError>;
    pub fn register_root_with_log(&mut self, id, workspace_id, SessionLog) -> Result<(), SpawnError>;
    pub fn spawn_child(&mut self, child_id, parent_id, &SpawnChildArgs, &SpawnContext)
        -> Result<SpawnedChild, SpawnError>;
    pub fn finish_child(&mut self, child_id, outcome, ts) -> Result<(), SpawnError>;  // 幂等
    pub fn mark_finished(&mut self, session_id, outcome) -> Result<(), SpawnError>;
    pub fn interrupt_tree(&mut self, session_id, reason, ts)
        -> Result<InterruptPlan, SpawnError>;                 // 第一段：interrupt
    pub fn escalate_interrupt(&mut self, &InterruptPlan, ts)
        -> Result<Vec<InterruptTarget>, SpawnError>;          // 第二段：kill
    pub fn begin_drain(&mut self, parent_id, DrainMode) -> Result<DrainWait, SpawnError>;
    pub fn poll_drain(&mut self, parent_id) -> Option<DrainWait>;  // None = 已满足
    pub fn cancel_drain(&mut self, parent_id) -> bool;
    pub fn is_active(&self, session_id) -> bool;   // 含 drain 等待
    pub fn is_idle(&self, session_id) -> bool;     // 等待期间 MUST 为 false
    pub fn children(&self, id) -> Vec<&TreeSession>;
    pub fn descendants(&self, id) -> Vec<&TreeSession>;
    pub fn session(&self, id) -> Option<&TreeSession>;
    pub fn sync_log(&self, session_id, &mut SessionLog) -> bool;  // 宿主日志同步
    pub fn to_tree(&self) -> Result<SessionTree, SpawnError>;
    pub fn from_logs(logs, max_depth) -> Result<Self, SpawnError>;  // 重启恢复
}
```

`SpawnedChild` 返回子会话的 `log`（已含 `session.created`），调用方负责：
1. 用 `SessionIndex::write_log`/`create_child` 落盘；
2. 以 `prompt` 跑子会话的普通轮次（`Agent::run_turn` 会写 `message.user`，避免重复写）；
3. 结束后调用 `finish_child(child_id, outcome, ts)` 写父日志并推进 drain。

`spawn_child` **不**自己写子会话的 `message.user`：prompt 只作为运行参数返回，避免与
正常轮次的用户消息重复。

## 5. 时序

### 5.1 spawn

```text
父 Agent 产生 spawn_child(prompt, preset?)
  -> ChatExecutor 拦截（spawn 是核心能力，不经过插件注册表）
  -> SessionTreeRuntime::spawn_child(child_id, parent, args, ctx)
       ├─ 校验：父 active、深度 <= min(runtime.max_depth, parent.policy.max_depth)
       ├─ 策略：parent.policy.descend(ctx.tightening)  （只收紧）
       ├─ 子日志：session.created{workspace_id,parent_session_id,spawn_source:"agent",preset}
       └─ 父日志：child.spawned{child_session_id,prompt,parent_message_id}
  -> child_id 由宿主生成（SessionIndex::new_id），落盘 write_log/create_child
  -> 子会话跑独立轮次；结束 -> finish_child -> 父日志 child.finished
```

### 5.2 中断传播

```text
用户中断父会话
  -> interrupt_tree(parent, reason, ts)
       ├─ 父日志：session.interrupted{reason, by_parent:false}
       ├─ 每个活跃后代日志：session.interrupted_by_parent{parent_session_id, reason}
       └─ 返回 InterruptPlan（层序：直接子级在前）
  -> 调用方对目标请求 cancel（cancel token）
  -> 超时仍未停止：escalate_interrupt(plan, ts)
       └─ 仍 Interrupting 的后代日志：session.killed{reason:"interrupt_timeout"}
  -> finish_child(..., "interrupted") 汇总到父日志
```

### 5.3 drain

```text
begin_drain(parent, DirectChildren | AllDescendants)
  -> 收集开始等待时仍 active 的子/后代，pending 集合
  -> 每 finish_child / mark_finished 自动从 pending 移除
  -> poll_drain(parent)：Some = 继续等；None = 已满足（已清等待标记）
等待期间 is_idle(parent) == false（父保持 active，避免被误判闲置）
```

## 6. 回放与重启

- 父链接/来源：`session.created`。
- 中断：`session.interrupted*` / `session.killed`（多条的读取端按"发生过"处理）。
- 子会话结束：父日志的 `child.finished`。
- `SessionTreeRuntime::from_logs(logs, max_depth)` 会重建父子关系、深度、prompt 与状态，
  然后可继续 `spawn_child` / `interrupt_tree` / drain。
- `SessionIndex::tree()` 直接由目录索引构造静态树（不要求加载全部 Event Log 内容）。

已知限制：`SessionPolicy` 的收紧项（白名单/黑名单/更小深度）v1 **不持久化**，
重启后按 `workspace_id + 运行时 max_depth` 重建；跨重启仍要收紧策略需要新的
`session.created` 字段（见 §8）。

## 7. 与 chat.rs 的接线（已落地，2026-11-11）

产品面通过 `TreeCoordinator` 和 `ChatExecutor` 接入会话树。

1. `ChatExecutor::specs()` 在 `allow_spawn` 时追加 `spawn_child_spec()`；`execute()` 在查
   注册表**之前**拦截 `call.tool == SPAWN_CHILD_TOOL`，交给 `TreeCoordinator::spawn_child`。
2. 由于流式回调与执行器都要写父日志，`ChatState::stream_turn` 把父日志临时放进
   `Rc<RefCell<SessionLog>>`（回调写 `tool.*`，执行器写 `child.spawned/finished`），
   轮次结束后还原；这样不会出现 seq 分叉。
3. 运行态树**每次派发时重建**：父会话用内存日志（最新），其余会话从磁盘读，
   再 `SessionTreeRuntime::from_logs` + `register_root_with_log`。不需要在 `ChatState`
   里长期持有运行时，也就不会与磁盘/内存两份日志不同步。
4. 子会话：`SessionIndex::write_log` 落盘 → `ChatState::open(..., allow_spawn=false,
   with_sidecar=false)` 跑一轮 → `finish_child` → `sync_log(parent)` → 父日志落盘。
   默认单层（子会话工具集里没有 `spawn_child`）；子会话不再起第二个 Node 进程。
5. 中断传播：父子共享同一个 `cancel` 句柄；`stop()` 置位后，执行器在子轮次结束时
   调 `interrupt_tree`，并给子日志补 `session.interrupted_by_parent`；
   `stream_turn` 结束时给父日志补 `session.interrupted`。
6. `ordered_sessions()` 用 `SessionTree::flatten()` 的前序和深度表排序。启动和 `reload_sessions` 共用该逻辑。
   桌面在会话标题旁的子智能体菜单按会话深度展示。 [桌面对话控制](spec/desktop-conversation.md)
7. `--selftest-tree` 实测（全部通过）：
   - 子日志有 `session.created.parent_session_id`、父日志有 `child.spawned`/`child.finished`；
   - 子会话回答落盘（`message.assistant.settled`）；
   - 子会话再派发被拒（默认单层）；
   - 真实链路：mock 模型选择 `spawn_child` → 执行器派发 → 子会话完成 → 模型拿到 tool.result；
   - 父取消后：父 `session.interrupted` + 子 `session.interrupted_by_parent`，outcome=interrupted；
   - 落盘后 `from_logs` 重建树：深度=1、中断状态一致、JSONL 回放无损。

## 8. 未做项与风险

| 项 | 说明 | 时点 |
|---|---|---|
| 子会话异步/并行调度 | 当前 `spawn_child` 是同步模型（父工具调用内跑完子轮次）；后台/并行子会话未做 | M3 |
| 子会话 sidecar 工具集 | 子会话只用原生工具（避免每次派发再起 Node）；继承 sidecar 工具待 M3 | M3 |
| 策略收紧持久化 | `SessionPolicy` 收紧项只在内存；重启按 workspace + max_depth 重建 | M3 视需要 |
| 超过深度 2 的树 | 上限可配置，但 UI/调度未验证更深 | M5 |
| 团队任务板 / 跨会话共享内存 / 子会话直接通信 | PLAN §6.5 明确 M5 | M5 |
| 远程 Agent 参与同一棵树 | ACP 后移 M3 | M5 |
| 子会话文件缺失的 UI 占位 | 父日志只存引用；子文件被删时 UI 如何显示未定义 | M3 |
| 跨会话搜索/引用 | 不属于树 v1 | M3 |

## 9. 测试与验收

```powershell
tools\build-env.cmd cargo test -p dshr-session-store -p dshr-agent-core
```

覆盖：

- `session-store`：`session.created` 必须第一条/父链接/来源/预设；`interrupted_by_parent`
  与 kill 升级事件；JSONL 往返重建树字段；`SessionTree` 的 children/descendants/depth/
  前序 flatten/无环/深度上限；`SessionIndex` 的 `scan` 树字段、`create_root`/`create_child`、
  `children_of`/`descendants_of`/`depth_of`/`tree`、`write_log` 往返。
- `agent-core`：`spawn_child` schema 与参数校验；独立日志 + 父链接 + 工作区继承；
  默认/可配置深度上限；策略只收紧（白名单子集、黑名单并集、深度不放大）；
  中断传播 + `interrupted_by_parent` + 超时 kill；drain 两种语义；等待期间父保持 active；
  `finish_child` 幂等；`from_logs` 重建树/深度/状态。
