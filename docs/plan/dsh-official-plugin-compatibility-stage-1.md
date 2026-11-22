# 阶段 1 报告 · DSH rc.5 文件工具

> 2026-11-22 完成。验收基线为本机 Windows 官方 DSH Desktop `0.2.2-rc.5`。

## 交付结果

- 建立了隔离的官方包验收入口和五包来源锁。验收从本机安装档抽取依赖，逐包核对版本与 SHA-256，然后执行 Rust 宿主、桌面自检和 headless 用例。错误版本与缺失包均被拒绝。 [验收脚本](../../tools/official-rc5-gate.ps1) · [来源锁](../../sidecar/dsh-official/official-rc5-packages.lock.json)
- 文件工具的相对路径按当前会话目录解析。两个会话可在各自子目录读取同名文件，headless 重启后恢复已保存的目录。预设内的 `working_directory` 和待办事件写入通过官方包用例。 [协议与宿主行为](../spec/sidecar-rpc.md#75-文件写入回调fswrite)
- 官方编辑器的 `view`、`create`、`str_replace`、`insert` 均通过。覆盖目录查看、审批拒绝、越界路径和取消。宿主在批准后再次核对文件版本，`createIfAbsent` 用原子创建保住等待批准期间出现的文件。
- 官方 `read_image` 接入会话附件存储和模型能力判断。端到端用例验证 PNG 缩放、图片载荷送达模型，以及删除原文件后重启重放相同图片。越界、损坏、超大输入及文本模型返回工具错误。 [图片附件契约](../spec/sidecar-rpc.md#77-官方工具图片附件)

## 验收

| 验收 | 结果 |
|---|---|
| `tools/official-rc5-gate.ps1` | 5 个锁定包通过来源校验，5 项专项测试通过 |
| `tools/official-l2-gate.ps1` | `rc.1` 的 10 项专项回归通过 |
| Rust 宿主、协议、文件、Agent、模型适配器和会话导出 | 受影响的测试套件全部通过，包含等待批准期间变更文件的回归 |
| 桌面与 headless 常规测试 | 桌面 38 项、headless 10 项通过。需官方包的 headless 用例由 `rc.5` 验收入口运行 |
| `node tools/conformance.mjs --quick` | 通过 Schema、文档链接和 21 项 Node 测试。恢复了现有 DeepSeek 请求测试与 `M3-DEFAULT-031` 的关联 |

本阶段使用显式 `rc.5` 依赖和测试 profile。桌面测试覆盖写入自检，图片输送和恢复经过 headless 及本地模型协议服务验证。矩阵与作者指南记录了相应启用条件。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵) · [启用指南](../plugin-authoring.md#启用-dsh-rc5-文件工具)
