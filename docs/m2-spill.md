# 上下文外部化（spill）与会话导出/保留（M2 W2，2026-11-11）

目标：补齐 PLAN §6.4「外部化」与 §6.2「保留与导出」的 Rust 核心，让**超大工具结果**
不再挤占模型窗口，同时保证"模型看到的历史"可从 Session Log 确定性回放、导出产物可离线校验，
磁盘清理只按显式计划执行。

- 代码：`crates/agent-context/src/spill.rs`（存储/阈值/回放）、`crates/agent-context/src/sha256.rs`
  （内置最小 SHA-256）、新 crate `crates/session-export`（导出 + 保留策略）。
- 本文档只描述**已实现行为**；`docs/spec/session-log.md` 由 W1/Lead 维护，本文件 §4
  列出建议补进规范的字段，供 Lead 汇总。

---

## 1. spill 存储格式

spill root 下的内容寻址布局：

```text
<spill_root>/
  objects/
    <hash 前 2 位>/
      <sha256 全 64 位小写 hex>
  tmp/                      # 原子写临时文件，成功后 rename 走
```

| 项 | 规则 |
|---|---|
| 地址 | `sha256(内容)` 的小写 hex，64 字符；`id = "spill:<hash>"`（`$attachment` 引用目标） |
| 内容 | `tool.result.payload.content` 的**规范 JSON 字节**（`serde_json::to_vec`，紧凑格式） |
| 元数据 | `size`（字节数）、`mime`（字符串结果 `text/plain; charset=utf-8`，其余 `application/json`）、`retention` |
| 去重 | 同一 hash 只存一份：`SpillStore::put` 已存在且长度一致返回 `Ok(false)`（不重写） |
| 原子写 | 先写 `<root>/tmp/<hash>.<pid>.<counter>` 再 `rename`，失败清理临时文件 |
| 无损取回 | `get` 校验 size；`get_verified` 再校验 sha256；`restore_json` 还原为原始 `content` JSON |
| 损坏处理 | 已存在对象长度不符视为损坏，重写修复；读取时 size/sha 不符返回 `InvalidData` |

**SHA-256 为什么自己写**：`Cargo.lock` 里的 `sha2 0.10.9` 是 GPUI 依赖树的传递依赖，
本机 cargo 缓存只有 `sha2 0.11.1`，引入 0.10.9 需要联网且会改 lock；仓库规则禁止新增未预置
依赖，因此内置约 100 行 FIPS 180-4 实现（`sha256.rs`，含标准测试向量），零新依赖。

## 2. 阈值策略

| 参数 | 默认 | 说明 |
|---|---|---|
| `SpillPolicy::threshold_bytes` | **16 KiB**（16384） | `content` 的规范 JSON 字节数**严格大于**该值才外部化 |
| `SpillPolicy::preview_chars` | **512** | 模型可见的开头预览字符数（UTF-8 字符，不是字节） |
| `SpillPolicy::retention` | **`90d`** | 写进 `spill.created.retention`；`session` 表示随会话删除 |
| 关闭 | `usize::MAX` | `SpillPolicy::disabled()` |

环境变量覆盖：`DSHR_SPILL_THRESHOLD_BYTES`、`DSHR_SPILL_PREVIEW_CHARS`、
`DSHR_SPILL_RETENTION`（`90d` / `session`）。

与已有裁剪的关系（**两层，不互相改写**）：

```text
tool.result.content
  ├─ 超过 spill 阈值（默认 16 KiB） → 整个结果落盘，模型只见 引用 + 预览
  └─ 未超过 spill 阈值，但超预算且 > 800 token → 仍走原有 head/tail 裁剪（行为不变）
```

- 只有 `tool.result` 且 `is_error == false` 的内容可外部化；`tool.aborted` / 错误结果不加引用
  （错误信息模型必须直接看到）。
- 外部化发生在预算判断**之前**：先外部化把 tokens 降下来，再执行原有"超预算→头尾裁剪→丢最旧"。
  低于 spill 阈值的长结果仍走原有裁剪/丢弃逻辑。
- `ContextPlan.tokens_before` 始终是**未外部化前**的估算，`tokens_after` 是计划后；
  `ContextPlan.spilled` 只包含"本次新产生"的外部化（已留痕的不会重复出现）。

### 模型可见的引用文本

`render_reference(spill, preview)` 是纯函数（无时间、无随机、无绝对路径）：

```text
[工具结果已外部化] id=spill:<sha256> sha256=<sha256> size=<n>B mime=application/json retention=90d
完整内容已落盘（会话附件/导出可离线取回），模型只看到引用与开头预览：
<前 512 字符>…
```

## 3. 事件字段（**给 Lead：需汇总进 `docs/spec/session-log.md` §3.5/§5**）

一次外部化写两条 durable 事件（先落盘 blob，成功后才写事件，符合 I7）：

### 3.1 `spill.created`

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `id` | string | 是（规范已有） | `spill:<64 hex>`，`$attachment` 引用目标 |
| `hash` | string | 是（规范已有） | sha256 小写 hex，也是对象文件名 |
| `size` | u64 | 是（规范已有） | 内容字节数 |
| `mime` | string | 是（规范已有） | 内容 MIME |
| `retention` | string | 是（规范已有） | `90d` / `session` |
| `call_id` | string | **新增（建议可选）** | 产生该结果的工具调用，回放直接关联；缺省时用 `tool.result.pruned.spill_ref` 关联 |
| `preview_chars` | u64 | **新增（建议可选）** | 模型可见预览字符数，回放按此截断（缺省 512） |
| `kind` | string | **新增（建议可选）** | 当前固定 `"tool.result"`，为未来附件类型留位 |

### 3.2 `tool.result.pruned`

| 字段 | 类型 | 说明 |
|---|---|---|
| `call_id` | string | 被外部化的工具调用 |
| `original_size` | u64 | 原始内容字节数（与 `spill.created.size` 一致） |
| `spill_ref` | string | 指向 `spill.created.id` |
| `mode` | string | **新增（建议可选）**：`"spill"`（未来区分 `"head_tail"` 等模式） |
| `preview_chars` | u64 | **新增（建议可选）**，同 `spill.created` |

示例（省略信封字段，实际由 `SessionLog` 分配 `seq`）：

```json
{"type":"spill.created","durability":"durable","payload":{
  "id":"spill:9f2c…","hash":"9f2c…","size":192042,"mime":"application/json",
  "retention":"90d","preview_chars":512,"call_id":"call-0042","kind":"tool.result"}}
{"type":"tool.result.pruned","durability":"durable","payload":{
  "call_id":"call-0042","original_size":192042,"spill_ref":"spill:9f2c…",
  "mode":"spill","preview_chars":512}}
```

> 注意：`retention` 只是**记录**，M2 没有到期自动删除附件；清理见 §6。

## 4. 回放一致性（确定性）

- `recorded_spills(log)` 从事件恢复 `call_id -> RecordedSpill`：
  1. `spill.created.call_id` 存在时直接关联；
  2. 否则用 `tool.result.pruned.spill_ref` 关联到同 `id` 的 `spill.created`；
  3. 只有 `pruned` 没有 `created`（日志残缺）→ 忽略，退化为完整内容，不阻塞回放。
- 引用文本 = `render_reference(事件里的 id/hash/size/mime/retention, 内容开头 preview_chars)`，
  与首次生成时逐字节一致；`plan_with_window_and_spill` 第一次（写事件前）与回放（写事件后）
  得到同一串模型消息（测试 `replay_after_recording_events_is_byte_identical` 断言）。
- 旧入口 `plan_with_window` / `plan_context` 在没有 spill 事件时与引入前**完全相同**；
  有事件时也按事件还原引用（`history_from_log_with_spill` 是模型可见历史的对应入口）。
- 附件文件缺失**不影响回放**：日志里仍有完整 `tool.result.content`，导出时记入
  `missing_blobs`（可离线查看的只有还在的内容）。

## 5. 与现有 `plan_context` 的集成

新增 API（`crates/agent-context`，均从 crate 根 re-export）：

```rust
// 计划：返回带 spilled 的 ContextPlan
pub fn plan_context_with_spill(log: &SessionLog, next_user_text: &str, policy: &SpillPolicy) -> ContextPlan;
pub fn plan_with_window_and_spill(log: &SessionLog, next_user_text: &str, window: usize, policy: &SpillPolicy) -> ContextPlan;

// 回放/展示：模型可见历史（已外部化处显示引用）
pub fn history_from_log_with_spill(log: &SessionLog) -> Vec<ChatMessage>;

// 存储
impl SpillStore {
    pub fn new(root: impl Into<PathBuf>) -> Self;
    pub fn with_policy(root: impl Into<PathBuf>, policy: SpillPolicy) -> Self;
    pub fn put(&self, spill: &SpillRef, bytes: &[u8]) -> io::Result<bool>;   // 去重
    pub fn put_pending(&self, pending: &PendingSpill) -> io::Result<bool>;
    pub fn get(&self, spill: &SpillRef) -> io::Result<Vec<u8>>;              // 校验 size
    pub fn get_verified(&self, spill: &SpillRef) -> io::Result<Vec<u8>>;     // 再校验 sha256
    pub fn restore_json(&self, spill: &SpillRef) -> io::Result<serde_json::Value>;
}

// 写事件（调用方先 put 成功再调用）
pub fn record_spill(log: &mut SessionLog, pending: &PendingSpill, ts: &str) -> Result<(u64, u64), LogError>;
```

`ContextPlan` 新增字段与访问器：

```rust
pub struct ContextPlan { /* 原有字段 */ pub spilled: Vec<PendingSpill> }
impl ContextPlan { pub fn spilled_tool_results(&self) -> usize; }
```

**不改变已有裁剪行为的保证**（有测试逐条覆盖）：

1. 没有 spill 事件且策略为 `disabled()` 时，`plan_with_window` 产出的
   `messages / tokens_before / tokens_after / pruned_tool_results / dropped_messages` 与改动前一致；
2. `prune_tool_result`（头 1200 + 尾 400 字符）与 `trim_to_budget` 实现未改；
3. 低于 spill 阈值的长结果仍按原规则裁剪/丢弃；
4. 外部化不修改日志里的原始 `tool.result`，只影响"下一次请求的模型输入"。

### chat.rs 接线建议（Lead；W2 不改 chat.rs）

```rust
use dshr_agent_context::{
    plan_context_with_spill, spill, SpillPolicy, SpillStore,
};

// 初始化一次（目录由 Lead 定，例如 <data_dir>/spill）：
let spill_policy = SpillPolicy::from_env();
let spill_store = SpillStore::with_policy(data_dir.join("spill"), spill_policy.clone());

// 组装请求前：
let mut plan = plan_context_with_spill(&self.log, "", &spill_policy);
if !plan.spilled.is_empty() {
    let ts = now_ts();
    for pending in &plan.spilled {
        // 先落盘再写事件（I7）；put 失败时不要写事件。
        if spill_store.put_pending(pending).is_ok() {
            let _ = spill::record_spill(&mut self.log, pending, &ts);
        }
    }
    self.save();
    // 重新计划：spilled 清空，历史从事件确定性还原（与回放完全一致）。
    plan = plan_context_with_spill(&self.log, "", &spill_policy);
}
// context.measured 建议追加（Lead 决定，属 spec 可选字段）：
//   "spilled_tool_results": plan.spilled_tool_results()
```

自检建议（`--selftest-spill`）：mock provider 返回 >16 KiB 工具结果 → 断言
`spill.created` / `tool.result.pruned` 各一条、附件文件存在且 `get_verified` 无损、
再次 `plan_context_with_spill` 的 messages 与首次一致且 `spilled.is_empty()`。

## 6. 导出（`crates/session-export`）

```text
<out_dir>/
  <session_id>.jsonl                    # 默认排除 ephemeral 事件
  <session_id>.attachments/<sha256>     # 日志里 spill.created 引用到的 blob（内容寻址、按 hash 去重）
  <session_id>.manifest.json            # 每个文件 size + sha256；缺失附件清单
```

```rust
pub fn export_session(
    log: &SessionLog,
    blobs: &dyn BlobSource,          // SpillStoreSource::new(&store) 或 NoBlobs
    out_dir: &Path,
    options: &ExportOptions,         // exported_at / include_ephemeral / fail_on_missing_blob
) -> Result<ExportManifest, ExportError>;

pub fn verify_export(out_dir: &Path, session_id: &str) -> Result<VerifyReport, ExportError>;
pub fn jsonl_for_export(log: &SessionLog, include_ephemeral: bool) -> (String, ExportCounts);
```

manifest 字段：`format`（`dshr-session-export/v1`）、`schema_version`、`session_id`、
`exported_at`（调用方提供，保证可复现）、`event_count` / `durable_event_count` /
`ephemeral_event_count`、`jsonl{path,size,sha256}`、
`attachments[{id,hash,size,mime,retention,sha256,path}]`、
`missing_blobs[{id,hash,size,reason}]`、`total_bytes`（JSONL + 附件，不含 manifest 自身）。

行为约定：

- **只读**：不修改 Session Log，不请求 LLM，不执行工具；导出后 JSONL 可被
  `SessionLog::from_jsonl` 原样恢复（默认排除 ephemeral 时即"可回放子集"）。
- blob 读取时校验 `size + sha256`；来源内容不符 → `ExportError::BlobIntegrity`；
  缺附件默认记录到 `missing_blobs` 继续导出，`fail_on_missing_blob(true)` 才失败。
- 同一 hash 只写一份附件；重复导出到同一目录会先校验已有附件再复用。
- `verify_export` 按 manifest 重新计算 sha256，返回问题清单（不修改文件）。
- 导出目录是自包含的：把 `<session_id>.jsonl + .attachments + .manifest.json` 一起拷走即可离线回放。

## 7. 保留策略（plan / apply 分离）

默认值对齐 `spec/session-log.md` §8：

| 常量 | 默认 |
|---|---|
| `DEFAULT_MAX_SESSION_BYTES` | 2 GiB（单会话磁盘上限） |
| `DEFAULT_MAX_AGE_DAYS` | 90 天 |
| `DEFAULT_KEEP_RECENT_SEGMENTS` | 1（最新一段永远保护） |

```rust
pub fn plan_retention(sessions: &[SessionUsage], policy: &RetentionPolicy, now_epoch_secs: u64) -> RetentionPlan;
pub fn apply_retention(root: &Path, plan: &RetentionPlan) -> Result<RetentionReport, RetentionError>;
```

算法（纯计算，不碰磁盘）：

1. 每个会话先保护最近 `keep_recent_segments` 段（时间相同按 `segment_id` 稳定排序）；
2. **按段年龄**：未被保护且最后修改早于 `max_age_days` 的段进入删除集（reason=`Age`）；
3. **单会话上限**：若会话总字节（段 + 附件）仍 > `max_session_bytes`，从最旧的未保护段
   继续删（reason=`OverCap`），直到达标或没有可删段；
4. 附件字节计入上限但**不自动删除**；删光可删段仍超限时记入 `over_cap` 由调用方决定
   （M2 不做附件 TTL 自动清理）。

`apply_retention` 只在显式调用时删计划里列出的段文件：

- `plan.is_empty()` → 不删任何东西；**没有任何"到期自动删除"路径**；
- 逐个校验：只接受相对于 root 的普通路径（拒绝绝对路径、`..`、根前缀），
  `canonicalize` 后必须仍在 root 内（防符号链接逃逸），非普通文件跳过；
- 单个文件失败记入 `RetentionReport.errors`，不中断其余删除；root 非法返回 `RetentionError`。

## 8. 验证

```powershell
tools\build-env.cmd cargo test -p dshr-agent-context -p dshr-session-export
tools\build-env.cmd cargo fmt -p dshr-agent-context -p dshr-session-export
```

覆盖：

- **spill**：SHA-256 标准向量与填充边界；超阈值触发（模型只见引用 + 截断预览）；
  小结果不触发；`disabled()` 无事件时与 `plan_with_window` 完全相同；
  低于 spill 阈值的长结果仍走原有头尾裁剪；store 去重（同内容只一个对象文件）；
  **写事件后回放逐字节一致**；**无损取回**（`get_verified`/`restore_json` 等于原始 content，
  篡改对象被校验拒绝）；引用文本确定性且不含绝对路径；`pruned` 边回退关联。
- **export**：JSONL 可反序列化回原日志；附件 + manifest + verify 全绿；
  缺附件记录不阻塞（`fail_on_missing_blob` 时失败）；同 hash 去重只导一份；
  ephemeral 默认排除/显式包含；篡改附件被 verify 发现；来源内容不符被拒；
  重复导出结果一致。
- **retention**：默认值对齐 spec；年龄计划保留最新段；超上限从最旧删到上限内；
  附件导致无法达标时 `over_cap` 报告且保护最新段；plan 不碰磁盘；apply 只删计划内文件；
  `../` 逃逸被拒且 root 外文件保留；非法 root 报错；同时间段按 `segment_id` 稳定排序。

## 9. 未做项 / 风险

- **服务端附件与云同步**：M2 只做本地内容寻址 blob；没有上传/下载、冲突与配额。
- **附件 TTL 自动清理**：`retention` 只写进事件与 manifest，没有后台 GC；spill objects 是
  跨会话共享的内容寻址存储，删除会话时如何回收未引用对象（GC/引用计数）未实现 → 
  目前靠导出/手工清理；后续应做"按 session 引用计数 + 全局 LRU"。
- **分段日志落地**：`RetentionUsage` 的段输入由调用方提供；session-store 还没有真正的
  分段文件布局，接上后需要一段"扫描 session 目录 → SessionUsage"的适配层。
- **加密**：导出与附件均明文；落盘加密留给存储层（spec §8 未实现）。
- **压缩摘要联动**：spill 阈值/预览与 `context.compacted` 没有联动（摘要里的工具结果不会
  被外部化成更小的引用）。
- **UI**：没有"查看完整工具结果 / 下载附件"入口；`conversation-model` 尚未渲染引用卡片。
- **阈值未实测调优**：16 KiB / 512 字符是按 token 估算与 DeepSeek 窗口取的保守值，
  没有按真实 provider 的计费/窗口回归；`context.measured` 也还没有 `spilled` 字段
  （是否加入由 Lead 决定，属 spec 可选字段）。
