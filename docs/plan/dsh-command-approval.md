# DSH rc.5 命令审批对齐

当前已完成单次授权、审批结果区分和本地子 Agent 的权限快照。命令在既有沙箱内自动运行的行为等待 Windows 写入隔离通过复验。[Windows 运行器问题](../issues/0002-windows-shell-sandbox-write-scope.md)

| 任务 | 状态 |
|---|---|
| 根据本机 `0.2.2-rc.5` 安装档核对 Shell、审批和本地子代理策略 | 完成 |
| Shell 和终端命令仅支持一次性同意，分别报告拒绝、取消和审批不可用 | 完成，重复命令会重新弹出审批 |
| 子会话启动时保存权限，恢复时重用，后台审批固定为 `never` | 完成，父会话切换到完整访问后，恢复的子会话仍按原模式拒绝需审批的命令 |
| 在已验证的进程沙箱内直接执行，仅对显式升权弹出一次性审批 | 等待隔离复验 |

本轮使用开发版暂存的官方包，运行 `stage2_official_shell_`、`stage5_official_subagents_`、`stage4_official_jobs_`、`stage4_official_terminals_` 和 `stage6_official_ptc_` 五个专项，通过 Schema 和文档链接检查。全量入口为 `tools/official-rc5-gate.ps1 -PackagedSidecar target/debug/dshr-debug-assets/sidecar/dsh-official`。隔离复验需要覆盖新目录、已授权目录、权限切换及跨工作区写入。[Sidecar 契约](../spec/sidecar-rpc.md#78-官方工具前台进程) · [DSH 审批参考](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/approval.md)
