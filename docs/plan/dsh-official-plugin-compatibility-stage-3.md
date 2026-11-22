# 阶段 3 报告 · DSH rc.5 提问、Skills、网页与交付

> 2026-11-22 完成。验收使用 Windows 官方 DSH Desktop `0.2.2-rc.5` 的实际包和隔离的测试配置。

| 官方工具包 | 实测结果 |
|---|---|
| `ask-user` | 桌面运行时往返验证多选、自由填写、跳过和取消，拒绝跨会话回答和迟到操作。Headless 立即返回交互不可用 |
| `skill` | 用官方文件系统 provider 验证会话目录切换前后的不同技能目录、`skill` 加载，以及续跑会话的 `/技能名` 请求 |
| `web` | 官方 Web API provider 对本地 HTTP 服务完成搜索与抓取。验证缺凭据时隐藏工具、抓取超时，以及官方提示段落进入模型请求 |
| `present` | 相对路径跟随会话目录，拒绝工作区外文件，交付事件及工具结果可在重启后重放 |
| `office` | Windows 原生 LibreOffice Kit 将 ODT 转为 PDF、重算 ODS 中的公式，并渲染两张 PNG。验证两个文件可在会话重开后读取，两张 PNG 也能从附件记录恢复到模型上下文 |

Rust 绑定每次服务请求与会话和工具调用，负责授权、图像校验、附件落盘和交付引用。Sidecar 在当前绑定的预设范围内补充官方 `agent/pre-step`、`systemPrompt` 和 `tools/result` 行为。Office 依赖抽取保留了部分第三方包运行时引用的 `src/` 文件。 [Sidecar RPC 契约](../spec/sidecar-rpc.md#79-官方工具提问) · [作者指南](../plugin-authoring.md#启用-dsh-rc5-skills网页和文件交付)

## 验收

- `tools/official-rc5-gate.ps1` 验证 13 个工具包、7 个配套包的版本与摘要，13 项专项用例通过。
- `tools/official-l2-gate.ps1` 的 10 项 `rc.1` 回归通过。受影响的 Rust 常规套件通过，其中桌面 38 项、headless 10 项。
- `node tools/conformance.mjs --quick` 验证 Schema、文档链接和 21 项 Node 兼容用例。

实际桌面窗口的点击和键盘交互待复验，当前验收覆盖桌面运行时与编译后的界面接线。提问的 `timed` 模式仍缺超时后的答复投递。网页工具使用本地测试端点，生产账号、端点和直接 HTTP provider 待复验。Office 临时文件受引擎保留期约束。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵)
