# 阶段 2 报告 · DSH rc.5 搜索与前台 Shell

> 2026-11-22 完成。验收覆盖 Windows headless、Windows 桌面自检和 Linux Rust 宿主。

## 交付结果

- 官方 `glob` 和 `grep` 通过 Rust 宿主启动 ripgrep，按保存的会话目录解析搜索范围。宿主拒绝工作区以外的路径。官方搜索工具可用于 headless，通用 Shell 工具由 headless 工具清单过滤。
- 桌面官方 `pwsh` 经 Rust 策略和审批执行，回传标准输出、标准错误和原生退出状态。Windows Job Object 和 Unix 进程组支持超时或取消时回收前台进程。Linux 宿主也通过原版 `bash` 验收。获准命令以当前用户权限运行。 [前台进程契约](../spec/sidecar-rpc.md#78-官方工具前台进程)
- `rc.5` 来源锁扩至八个官方包。提取工具可从安装档的解包目录补齐依赖，验收继续在隔离的资源目录运行。 [来源锁](../../sidecar/dsh-official/official-rc5-packages.lock.json) · [验收脚本](../../tools/official-rc5-gate.ps1)

## 验收

| 验收 | 结果 |
|---|---|
| Windows `tools/official-rc5-gate.ps1` | 8 个包通过版本与摘要检查，7 项执行用例通过。新增官方搜索和 PowerShell 桌面自检 |
| Linux `stage2_official_bash_runs_on_linux` | 在 WSL 内用 Linux Rust 宿主与 Node 运行官方包，验证会话目录、标准输出与错误、退出码 7、超时及取消 |
| Windows `tools/official-l2-gate.ps1` | `rc.1` 的 10 项专项回归通过 |
| Rust 宿主、工具运行时与 DeepSeek 模型适配器 | 常规测试通过 |
| `node tools/conformance.mjs --quick` | Schema 检查及 21 项 Node 测试通过 |

Linux 图形桌面与 macOS 待实测。后台执行、输出领取及持久终端属于阶段 4。启用方式已补入作者指南。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵) · [启用指南](../plugin-authoring.md#启用-dsh-rc5-搜索与前台-shell)
