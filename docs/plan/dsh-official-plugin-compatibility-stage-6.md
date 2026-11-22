# 阶段 6 报告 · DSH rc.5 执行钩子与创造模式

> 2026-11-22 完成工具侧验收。Windows 使用 `0.2.2-rc.5` 官方工具包、捆绑 Node 24 和本机进程沙箱。

| 工具包 | 实测 |
|---|---|
| `call-timeout-policy` | 声明 80 ms 的用例收到官方 `tools/execute` 钩子的协作取消信号，并报告该预算。Rust 的调用预算和取消继续生效 |
| `executor-native` | 官方 Native 执行器运行实际注册的工具并返回预期结果，验收同时覆盖超时钩子 |
| `executor-ptc` | `run_code` 在官方 Node PTC 进程启动。程序直接调用当前步骤 SDK 内的读和写工具。模型直接调用程序专用工具被拒绝，拒绝审批的写入无文件副作用，批准写入持久化。会话日志记录 Rust 授权的父子调用 ID、注册 ID 和代次 |
| `cordis` | 列出本机 Host 检查 provider。`Tool.listTools` 读取当前步骤的 Sidecar 工具 Schema，其他会话选择器被参数 Schema 拒绝 |

预设测试将 PTC 挂载于单独修订。删除预设定义时已绑定会话可继续使用该修订。释放会话修订后撤销 `run_code`，使用旧注册 ID 的调用被拒绝。 [执行与检查契约](../spec/sidecar-rpc.md#721-官方执行钩子执行器与-cordis-检查) · [专项用例](../../apps/desktop-gpui/src/bin/chat.rs)

## 验收

- `official-rc5-gate.ps1` 校验 29 个官方工具包与 21 个配套包的版本及摘要，Windows 27 项专项用例通过。阶段 6 的四项用例在最后的超时错误码调整后再次通过。
- `official-l2-gate.ps1` 的 10 项 `rc.1` 回归通过。
- Linux Rust 宿主复验三个 Bash 用例，分别覆盖前台进程、后台任务和原生 PTY 持久终端。Linux 使用 Node `24.14.0` 和 Linux x64 `node-pty` 组件。
- Rust 宿主、文件服务及工具运行时常规测试通过。桌面对话 38 项和 headless 10 项通过。Schema 83 项和 Node 21 项通过。

PTC 当前为 TypeScript 单次执行。Windows 文件沙箱的覆盖范围由官方本机后端报告。Cordis 检查当前提供 Host 的工具 Schema 和协商服务元数据。Cordis Client 槽位、动态 `cordis_run` 交互卡和 bundle 启停及配置管理仍需独立适配。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵) · [作者指南](../plugin-authoring.md#启用-dsh-rc5-执行与检查插件)
