# 桌面对话交互对齐

已完成。参考本机 DSH Desktop `0.2.2-rc.5` 的模型菜单、输入区、过程折叠和消息操作源码。参考入口见[界面对齐记录](../ui-dsh-alignment.md)，行为见[桌面对话控制](../spec/desktop-conversation.md)。

| 优先级 | 改动 | 验收 |
|---|---|---|
| P1 | 生成中切换工具权限，宿主按当前模式授权 | 等待本地 SSE 回复时切到只读。同一轮的官方 `read` 成功，`write` 被拒，工作区无写入 |
| P2 | 连续工具和模型推理归入可展开的过程 | 发布窗口默认收起两个调用，展开可查看思考与工具结果。下一次请求包含模型此前返回的推理内容 |
| P2 | 模型菜单增加连接级推理等级 | 选择最高，核对请求的 `thinking.type=enabled` 与 `reasoning_effort=max`。重启后再次发送，值保持一致 |
| P2 | 停止使用 10 px 圆角方形与状态反馈 | 本地 SSE 暂停供观察，点击后立刻显示「正在停止…」，随后记录取消，保留停止前已显示的内容 |
| P3 | 复制按钮显示勾选 | 发布窗口点击复制，检查按钮状态和剪贴板原文 |

Windows 发布构建使用 `tools\build-env.cmd cargo build --offline --locked --release -p dshr-desktop-gpui --bin dshr`。实窗验证使用隔离数据目录、本地 SSE 和临时工作区，显示缩放为 175%。相关 Rust 单测和 headless 集成测试通过，`node tools/check-links.mjs` 与 `node schema/check.mjs` 通过。

审批回归已复核。在工具审批和官方文件写入审批处分别切换到「工具免审批」，当前写入与同轮后续写入均成功，工作区外写入被拒。待审批时切到「只读」也结束了等待，目标文件没有创建。会话日志中的审批答复记录为 `permission_mode`。

审批卡片参考 DSH Desktop `0.2.2-rc.5` 的 `dsh-client-ui-approval/src/client/ApprovalCard.tsx` 添加「始终允许」。发布窗口分别在 `write` 的工具审批和 `edit` 的文件写入审批上完成授权。重启后，同一会话的 `write` 自动通过两层审批。不同工具及不同会话各自弹出审批，工作区外写入被拒。单测覆盖授权键的会话、工作区和工具定义隔离，以及只读策略优先于已保存授权。
