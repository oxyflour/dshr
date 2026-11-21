# 工具审批与安全前置（2026-11-11）

目标：在解锁"写文件 / 执行命令"类工具之前，把 **策略 → 审批 → 审计** 这条链做实。
M1 的 `AllowAll` 在这轮被替换成桌面默认策略，审批由 UI 完成。

## 策略（ToolPolicy）

[`crates/tool-runtime/src/lib.rs`](../crates/tool-runtime/src/lib.rs) 新增 `ApprovalPolicy`：

- 判定顺序：显式 `deny` 名单 → 名字命中 `MUTATING_TOOL_NAMES`
  （write/edit/apply_patch/bash/pwsh/shell/delete/remove/mkdir/move/copy/rename/chmod/git_commit/git_push）
  → 展示类型是 `terminal` / `diff` → 其余 Allow；
- `ApprovalPolicy::desktop()` 是默认；`ask_all()` 供自检；
- `DSHR_TOOL_POLICY=ask-all` 可让所有工具走审批（演示/验证用）。

Registry 的 `authorize` 原本就会把 `Ask` 变成 `RegistryError::NeedsApproval`，这一轮只是
第一次真正消费它。

## 审批 broker

[`crates/tool-runtime/src/approval.rs`](../crates/tool-runtime/src/approval.rs)：

- 工作线程在 `execute` 里 `broker.request(call_id, tool, args, timeout)` **阻塞**；
- UI 线程轮询 `broker.pending()`，渲染审批卡片，点击"允许一次/拒绝"调用 `resolve`；
- Condvar 唤醒；超时（默认 300s）按拒绝处理；
- 非交互模式：`DSHR_APPROVAL_MODE=allow|deny` 直接决定并记录 `answerer=auto`；
- 审批痕迹 `ApprovalRecord` 在轮次结束后由 chat 落成
  `approval.asked` / `approval.answered` durable 事件（顺序上位于工具事件之后，
  不影响模型输入；审计/回放可配对）。

## UI

- 审批卡片在输入框上方：黄色边框、工具名、格式化参数（限高）、
  "拒绝 / 允许一次"按钮；多个待确认时显示"另有 N 个"。
- 生成中仍可点击；审批期间工作线程阻塞，UI 通过 16ms 轮询发现 pending 变化后重绘。
- 审批完成后卡片消失，工具继续执行，结果卡片正常出现。

## 官方写入工具为什么还没打开

官方 `write` / `edit` / `apply_patch` 需要 `sandbox` service（版本/审批语义），
`bash` / `pwsh` 需要进程隔离与允许清单。当前 sidecar 里这些能力仍是关闭的：
**审批 UI 是必要条件，不是充分条件**。下一步是补 `sandbox` seam：
路径根、写前快照、命令白名单/工作目录，再把官方写工具按"默认 Ask"接进来。

## 验证

- `tool-runtime` 16 个测试：策略顺序（deny > ask > presentation > allow）、
  broker 跨线程 resolve、超时视为拒绝、env 自动 allow/deny；
- `dshr --selftest-approval`：`ask-all` + deny 时工具不执行、落 approval 事件、
  工具结果是错误；allow 时工具真正执行；
- 截图核对：审批卡片（工具名/参数/两个按钮）、点"允许一次"后工具完成、
  `probe_echo` 结果卡片出现；
- workspace 全绿、clippy 干净。
