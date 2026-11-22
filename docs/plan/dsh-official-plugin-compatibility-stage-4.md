# 阶段 4 报告 · DSH rc.5 后台任务与持久终端

> 2026-11-22 完成。使用官方 `0.2.2-rc.5` 工具包与本机 PTY 后端。

| 范围 | 实验结果 |
|---|---|
| Windows 后台 PowerShell | 经过宿主审批启动。输出按游标领取，重复领取为空。能请求停止并回读终态，重启后可以查看任务和领取未读输出，完成提示只投递一次 |
| Linux 后台 Bash | 在 Linux Rust 宿主中启动真实 Bash，领取输出并确认重复领取为空 |
| Windows 通用终端和持久 PowerShell | 工作区外打开被拒绝。实测收发、滚动回读、变量跨调用保留、跨会话拒绝、取消及关闭。重开日志后旧终端显示已结束 |
| Linux 持久 Bash | Linux x64 在 Node 24 和原生 PTY 上验证终端输入、跨调用变量保留、其他会话读取拒绝及关闭记录 |

Rust 负责后台任务身份、进程执行、持久记录和输出游标。持久终端通过官方本地 PTY 后端运行，Rust 负责打开边界、调用授权、会话所有权与重启状态回放。 [后台任务和终端契约](../spec/sidecar-rpc.md#714-官方后台任务) · [作者指南](../plugin-authoring.md#启用-dsh-rc5-后台任务与持久终端)

## 验收

- Windows `official-rc5-gate.ps1` 检查 17 个工具包与 10 个配套包，15 项专项用例通过。
- Windows `official-l2-gate.ps1` 的 10 项 `rc.1` 回归通过。Rust 宿主及工具运行时常规套件通过。
- Linux 宿主使用已校验的 Node `24.14.0` 和 `node-pty 1.2.0-beta.15` 的 Linux x64 附加组件，两个 `stage4_official_bash_` 用例通过。

后台 Shell 使用显式启动及 15 分钟宿主上限，输出在结算后领取。前台超时自动转后台和终端后台发送待适配。Linux 图形桌面与 macOS 待实测。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵)
