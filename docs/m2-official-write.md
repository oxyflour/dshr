# M2 — 官方写工具解锁（`write` / `edit` / `apply_patch`）

> 状态：M2 `task-3`（W3）交付 · 2026-11-11
> 关联：[`m2-dsh-official.md`](m2-dsh-official.md)、[`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §7.5、
> [`m2-closeout.md`](m2-closeout.md) Gate 4

**结论**：官方 `@deepseek-ai/dsh-tool-fs@0.2.2-rc.1` 的 `write` / `edit` / `apply_patch`
在**不改官方源码**的前提下打开了。写入不发生在 Node 进程：Sidecar 只把官方 `ctx.fs`
的写意图翻译成 `fs.write.request`，由 Rust（`crates/compat-dsh-fs`）完成
**路径归一化 → 工作区边界 → ApprovalBroker 审批 → 写前快照 → 原子写 → 回版本**。
宿主未协商 `fs.write` 时，Sidecar 强制保持只读（`--selftest-dsh` 不回归）。

## 证据

离线 E2E（真实 Node sidecar + 未修改官方插件，`crates/compat-dsh-host/tests/fs_write_e2e.rs`）：

```text
[w3-e2e] read-only registrations=["read"]
[w3-e2e] writable registrations=["apply_patch", "edit", "read", "write"]
test read_only_when_fs_write_not_negotiated ... ok
test negotiated_write_capability_edits_and_patches_via_approval ... ok
```

E2E 依次验证：官方 `write` 创建文件（`Created file`）→ 官方 `edit` 替换并产生
`.dshr/snapshots/` 快照 → 官方 `apply_patch` 应用 hunk → 审批 Deny 时工具报错且文件不变。

```text
cargo test -p dshr-compat-dsh-fs          # 12 passed
cargo test -p dshr-compat-dsh-protocol    # 10 passed（含瞬时错误分类）
cargo test -p dshr-compat-dsh-host        # 12 passed（3 能力协商单元 + 9 集成，含 fs_write_e2e）
node --check sidecar/dsh-official/sidecar-dsh.mjs
```

## 1. 官方插件的真实调用链（实测 0.2.2-rc.1）

```text
write.execute(args, exec)
  ├─ sandbox.resolvePolicy("write", args, exec)        # FsSandboxController
  ├─ ctx.fs.resolve(file_path, {signal})               # → { targetKey, displayPath }
  ├─ ctx.waterfall("fs/write-intent", target, exec)    # 无监听者 → undefined
  └─ ctx.fs.writeText(target, content, intent, signal, sandboxPolicy)
        → { version, operation, before, after }

edit.execute
  └─ ctx.fs.editText(target, {oldString,newString,replaceAll}, intent, signal, policy)

apply_patch.execute
  └─ planPatch / preflight（fs.resolve / fs.stat / fs.lstat / fs.readText）
     └─ commitPatch：fs.writeText（add）/ fs.patchText（update）/
        fs.deleteFile（delete）/ fs.moveFile（move）
```

需要 seam 的精确形状（`ctx.fs`）：

| 官方方法 | 语义 |
|---|---|
| `resolve(path, {cwd?, signal})` | 返回 `{targetKey, displayPath}` 目标身份 |
| `processPath(target)` | 面向模型/UI 的进程路径 |
| `stat(target, signal)` | `{version, type, size}`；不存在返回 `undefined` |
| `lstat(path, {cwd?, signal})` | 路径本身类型（`apply_patch` 检查符号链接源） |
| `readText / streamText / readBytes` | UTF-8 / 分块 / 二进制读 |
| `writeText(target, content, intent, signal, policy)` | 全文 create-or-replace，返回 `{version, operation, before, after}` |
| `editText(target, {oldString,newString,replaceAll}, intent, signal, policy)` | 唯一匹配/全替换，返回 `{version, before, after}` |
| `patchText(target, hunks, intent, signal, policy)` | 应用官方 `dsh-fs-patch` hunks，返回 `{version, before, after}` |
| `deleteFile(target, intent, signal, policy)` | 删除，返回 `{before}` |
| `moveFile(target, destination, {hunks, expectedSource, expectedDestination}, signal, policy)` | 写目标 + 删源，返回 `{destination:{version,before,after}, sourceDeleted, sourceBefore?, sourceFailure?}` |

`apply_patch` 的 hunk 应用是官方纯函数：`@deepseek-ai/dsh-fs-patch` 的
`applyHunks(before, hunks) -> {ok:true, after} | {ok:false, failure}`，Sidecar 直接复用，
不重写补丁语义。

## 2. sandbox seam：`escalationModes=[]` 是官方自己给的

`dsh-tool-fs` 在 `apply` 时构造 `FsSandboxController`：

```js
const defaultMode = ctx.fs.sandboxMode            // 本侧车返回 undefined
this.escalationModes = defaultMode === undefined ? [] : ESCALATION_TARGETS
this.policy = defaultMode === undefined ? undefined : ctx.get("sandboxPolicy")
```

因此本组合里：

- 写工具的 `parameters` **不包含** `sandbox_permissions` / `justification`（无 escalate 模式）；
- `resolvePolicy` 返回 `undefined`（不依赖 `sandboxPolicy` 服务，也不依赖 `sessionProjections`）；
- `mapError` 本应原样返回错误——但它对 `FS_SANDBOX_DENIED` 会读 `policy.mode`
  （`policy` 为 `undefined` 时会崩）。所以 **Sidecar 把 Rust 的拒绝翻译成
  `FS_PERMISSION_DENIED`**（消息里保留 "denied / approval timed out" 细节），
  避免踩官方这条只在 confining 后端才成立的路径。

真实约束全部在 Rust：Node 只声明"我不做文件级沙箱"（`fs.sandboxMode === undefined`），
Rust 的 `FsWriteService` 才是审批与边界权威。

## 3. 能力协商（`host_caps`）

`runtime.hello.payload.host_caps` 新增 `fs.write` 的能力位（[`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §3.1）：

| host_caps | 行为 |
|---|---|
| 无 `fs.write` | Sidecar 强制 `write/edit/applyPatch=false`，**不注册**三个写工具；Rust 若收到写请求回 `E_FS_CAPABILITY` |
| 有 `fs.write` | 按 profile 配置装载：未显式配置的写开关缺省 `true`，显式 `false` 仍尊重 |

`--selftest-dsh` 走 `SidecarHost::spawn`（旧签名），host_caps 仍只有
`tool.stream/tool.cancel`，所以行为与 M2 之前完全一致。

## 4. 宿主接线（chat.rs）

```rust
use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{HostOptions, SidecarHost};
use dshr_tool_runtime::ApprovalBroker;
use std::sync::Arc;

let approvals = ApprovalBroker::new();                     // 与 ChatExecutor 共用
let fs_write = Arc::new(
    FsWriteService::open(workspace_root, approvals.clone()) // 默认审批超时 60s
        .expect("open workspace"),
);
let host = SidecarHost::spawn_with(
    script,
    plugin,
    HostOptions::default().with_fs_write(fs_write),
)?;
```

`HostOptions::with_fs_write` 自动：① hello 追加 `fs.write`；② 给 sidecar 子进程设置
`DSHR_WORKSPACE=<workspace_root>`（Sidecar 的 fs 根）。需要自定义 sidecar 环境
（自检/离线）用 `with_env`，注册收集窗口用 `with_registration_window`。

**注意**：`crates/profile` 的默认 dsh-tool-fs 配置必须把 `write/edit/applyPatch`
改为 `true`（由 T4/Lead 负责，属其写域）；否则 host 协商了能力，Sidecar 仍按
profile 显式 `false` 保持只读。

## 5. Rust 裁决链（`crates/compat-dsh-fs`）

`FsWriteService::handle(FsWriteRequest)`，顺序固定、每步都可能返回结构化错误：

1. **请求形状**：`write` 必须带 `content`、`delete` 不能带；空路径拒绝（`E_FS_REQUEST`）。
2. **workspace_root 一致性**：Sidecar 声明根与宿主 `Workspace::root()` 组件级比较
   （Windows 大小写不敏感），不一致直接 `FS_SANDBOX_DENIED`（防混淆代理）。
3. **路径归一化**：
   - 相对路径交给 `dshr_sandbox::Workspace::resolve`（拒绝绝对路径、`..`、父目录缺失）；
   - 绝对路径先 `std::path::absolute` 归一，按组件去掉工作区根前缀；不在根内 →
     `FS_SANDBOX_DENIED`；再走一次 `Workspace::resolve` 做父目录/符号链接逃逸检查。
4. **目标类型**：已存在且是目录 → `FS_NOT_REGULAR_FILE`。
5. **版本**：读 `mtime_ms:size`；请求带 `expected_version` 且不匹配 → `FS_STALE_VERSION`
   （在审批之前拒绝，不打扰用户）。
6. **审批**：`ApprovalBroker::request(call_id, tool, {path, operation, bytes, tool, sandbox}, timeout)`；
   Deny/超时 → `FS_SANDBOX_DENIED`；超时默认 60 s（`with_approval_timeout` 可调，
   必须 < 工具 timeout）。
7. **写前快照**：覆盖/删除已有文件先复制到 `<workspace>/.dshr/snapshots/<id>`；
   快照 ID 回给 Sidecar（`snapshot` 字段）。
8. **原子写**：同目录临时文件 + `rename`（既有无 `Workspace::write_text` 语义复用）；
   删除用 `remove_file`。
9. **回版本**：写入后重新 stat → `mtime_ms:size`；返回 `operation=create|update|delete`、
   `bytes`、`snapshot`。

`version` 与 Node 侧 `fs.stat().version` 采用同一格式：Rust `SystemTime::as_millis()`
与 Node `Math.floor(stat.mtimeMs)` 等价（Windows 100 ns 精度的分数部分都被向下取整）。

### 单元测试覆盖（12 个）

路径穿越（`../`、`sub/../../`）、工作区外绝对路径、workspace_root 不一致、超过 1 MiB、
缺 content / delete 带 content / 空路径、父目录缺失、目录目标、审批 Deny 不落盘、
审批 Allow 原子写、覆盖产生快照、`expected_version` 冲突、删除 + 快照、绝对路径在根内
可写、无临时文件残留、工作区根打开失败。

## 6. Node 侧写方法 → 协议映射

| 官方方法 | Sidecar 行为 |
|---|---|
| `writeText` | 本地读旧内容（`before`）→ `fs.write.request{operation:write}` → `{version,before,after,operation}` |
| `editText` | 读 + stat 版本 → 唯一/全量替换（`FS_EDIT_NOT_FOUND` / `FS_AMBIGUOUS_EDIT`）→ 带版本写 |
| `patchText` | 读 + 版本 → 官方 `applyHunks`（失败 `FS_PATCH_MISMATCH`）→ 带版本写 |
| `deleteFile` | stat 版本 → `operation:delete` → `{before}` |
| `moveFile` | 读源（+hunks 计算新内容）→ 写目标（带目标版本）→ 删源（带源版本）；删源失败按官方契约回 `sourceFailure`，不假装原子 |
| `lstat` | 直接本地 `lstat`（只读，供 `apply_patch` 拒绝符号链接源） |

所有写方法的调用者身份（`call_id` / 工具名）经 `AbortSignal → callContext` 关联，
审批提示里显示的 `call_id` 与 Session Log 的 `tool.call` 完全一致。

错误映射（Node 侧 → 官方 `FsError`）：

| Rust 码 | Node 最终码 | 说明 |
|---|---|---|
| `FS_SANDBOX_DENIED` | `FS_PERMISSION_DENIED` | 官方 `mapError` 在非 confining 后端会因 `policy.mode` 崩溃（见 §2） |
| `E_FS_CAPABILITY` | `FS_PERMISSION_DENIED` | 未协商能力 |
| `E_FS_REQUEST` / `E_PROTOCOL` | `FS_IO_ERROR` | 协议形状错误 |
| `FS_STALE_VERSION` / `FS_TOO_LARGE` / `FS_NOT_FOUND` / `FS_NOT_REGULAR_FILE` / `FS_IO_ERROR` | 原样 | 官方词表 |

Node 侧对 `FS_STALE_VERSION` 保留官方补救提示（"re-read the file, then retry"），
因为抛出的是真正的 `@deepseek-ai/dsh-fs` `FsError`。

## 7. 测试命令

```powershell
# Rust 单元 + 离线 E2E（E2E 依赖 `node tools/dsh-vendor.mjs` 的产物，缺失自动 SKIP）
tools\build-env.cmd cargo test -p dshr-compat-dsh-host -p dshr-compat-dsh-protocol -p dshr-compat-dsh-fs

# Node 语法
node --check sidecar/dsh-official/sidecar-dsh.mjs

# Lead 的最终 E2E（chat.rs）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --selftest-dsh-write
```

## 8. 边界与风险

1. **没有 escalate 模式**：`sandbox_permissions` 参数不出现；任何"更宽权限"的重试都
   没有通道。审批只有 AllowOnce/Deny 两种结果（沿用 `ApprovalBroker`）。
2. **version 冲突检测是 best-effort**：`editText`/`patchText`/`deleteFile` 在 Node 侧
   读版本、Rust 侧写前再校验；两次采样之间若有外部写入者，仍有极小的 TOCTOU 窗口
   （原子重命名保证不会写半截文件，但不保证"读-改-写"整体互斥）。
3. **`moveFile` 不是原子的**：写目标 + 删源是两次独立审批/快照。目标写成功、源删失败
   时按官方契约返回 `sourceFailure`，模型需要重读现场；没有回滚。
4. **快照只增不减**：`.dshr/snapshots/` 目前没有保留策略/清理命令；长会话大量覆盖会
   线性增长磁盘占用（后续交给保留策略/导出工作）。
5. **帧上限**：协议单行 ≤ 1 MB，而写入上限 1 MiB；JSON 转义后接近上限的内容可能
   触达帧上限。当前没有分块写，必要时把 `MAX_WRITE_BYTES` 降到 512 KiB 或改用附件。
6. **Sidecar 进程仍能直接调用 Node `fs`**：本设计保证"模型经官方工具发起的写入"走
   Rust 审批，但恶意/出错的 JS 插件仍可在进程内绕过（M2 靠代码审查；OS 级隔离见
   Job Object/沙箱工作，不是本任务的承诺）。
7. **`FS_SANDBOX_DENIED` 不能直接透给官方工具层**：官方 `FsSandboxController.mapError`
   在非 confining 组合里对它会解引用 `undefined.mode`；Node 已改映射为
   `FS_PERMISSION_DENIED`。若未来挂上真正的 confining fs 后端，这条映射要跟着改回。
8. **审批超时 < 工具超时**是硬约束：默认 60 s vs 120 s。把审批超时调到 > 工具超时会让
   Sidecar 的写入答复在宿主超时后无人消费，表现为 `fs.write.result` 迟到日志。
9. **只读回归的证明**在 `fs_write_e2e.rs::read_only_when_fs_write_not_negotiated`；
   `--selftest-dsh` 的最终人工验证由 Lead 在 chat.rs 编译通过后执行。
10. **一次写工具调用会经过两次审批**：工具级 `ToolPolicy`（放行这个工具调用）与
    写盘级 `FsWriteService`（批准这次文件变更）。两者共用同一个 `call_id`，但当前
    没有合并成一次 UI 提示；交互模式下用户会看到两次确认（`DSHR_APPROVAL_MODE`
    自检模式下两次都自动裁决，不影响 E2E）。这是偏保守的设计，后续可在 UI 层折叠。
11. **sidecar 瞬时错误分类**（`E_TIMEOUT`/`E_BACKPRESSURE`/`E_INTERNAL` 可重试，
    其余不可）在协议层 `dshr_compat_dsh_protocol::is_retryable_sidecar_error` 与
    [`spec/sidecar-rpc.md`](spec/sidecar-rpc.md) §9.2 落地；但**写工具不参与自动重试**
    （非幂等，重试会重复审批/重复落盘）。

## 9. 未做项

- `read_image` / `attachments`（仍关闭；需要附件服务）。
- `sandbox_permissions` escalate 审批（需要把审批 channel 接到 Rust 的 ApprovalBroker，
  并在 Node 侧提供真实 confining 语义）。
- 写入分块/附件（>1 MiB 文件）。
- 快照保留策略与恢复 UI。
- 多会话并发写同一文件的串行化（当前由单线程 host 的 invoke 顺序天然串行，
  多 agent 并发未验证）。
- **取消传播到进行中的写请求**：`tool.cancel` 只 abort Node 侧 `AbortController`，
  已经发往 Rust 的 `fs.write.request` 仍会走完审批（最长 approval timeout 60 s）；
  用户取消后待审批项仍可能在 UI 出现直到超时。
