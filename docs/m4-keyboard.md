# M4 W2 — 键盘与无障碍基线

> 更新：2026-11-16。范围见 [ADR-0013](decisions/0013-m4-scope.md) W2；进度见 [m4-status.md](m4-status.md)。
> 本文记录**已实现的键位与无鼠标流程**，以及 G3 仍需人工/UI 自动化复验的项。

## 1. 快捷键（Windows Ctrl / macOS Cmd）

`cmd` 在 GPUI 里是平台主修饰键：Windows/Linux 为 Ctrl，macOS 为 Cmd。绑定注册在
`chat_key_bindings()`，语义在 `ChatApp::shortcut_*`，说明表 `CHAT_SHORTCUTS` 与提示文案同源。

| 键位 | 动作 | 适用场景 |
|---|---|---|
| `cmd-n` | 新建会话 | 任意焦点（输入框内也可用） |
| `cmd-k` | 聚焦输入框 | 焦点在面板/侧栏后回到输入 |
| `cmd-b` | 展开/收起侧栏 | 切换会话列表 |
| `cmd-j` | 打开/关闭插件面板 | 查看插件状态与 ADC 面板 |
| `cmd-shift-a` | 允许第一个待审批工具 | 审批卡片出现时 |
| `cmd-shift-d` | 拒绝第一个待审批工具 | 审批卡片出现时 |
| `escape` | 关闭插件面板 / 取消重命名 / 清除待删除 / 回到输入框 | 逐级退出 |
| `cmd-w` | 关闭应用 | 结束时 |

输入框内 `Enter` 发送仍是 `TextInput` 自己的绑定；生成中 `Button` 变为停止（鼠标），
键盘中断暂未绑定（记在 §4）。

## 2. 无障碍（a11y）基线

- 图标/文字按钮写 `Role` 与 `aria_label`：发送/停止（`send-button`）、允许/拒绝
  （`approval-allow` / `approval-deny`）、新建会话、侧栏开关、插件面板开关、
  两处主题切换、插件面板关闭。
- `TextInput` 根元素写 `Role::TextInput` 与占位符 `aria_label`，并带稳定
  `text-input-<placeholder>` id；ADC 面板继续走 `interactive_base` 的 role/label 规则。
- 审批可用全局快捷键完成，不依赖把焦点 Tab 到按钮；这是当前"键盘可达"的最小实现。

## 3. G3 键盘全流程复验清单（真实窗口，不用鼠标）

1. 启动 `chat.exe`，焦点应在输入框；
2. `Ctrl/⌘+N` 新建会话，标题变为新会话；
3. 直接输入一句话，`Enter` 发送；生成中界面出现状态；
4. 触发一个需要审批的工具（写文件/执行命令），审批条出现；
5. `Ctrl/⌘+Shift+A` 允许一次，或 `Ctrl/⌘+Shift+D` 拒绝；审批条消失、工具结果更新；
6. `Ctrl/⌘+B` 收起侧栏，再按展开；`Ctrl/⌘+J` 打开插件面板，`Esc` 关闭；
7. `Ctrl/⌘+K` 回到输入框，最后 `Ctrl/⌘+W` 关闭应用。

以上全部不碰鼠标即通过 G3；任一步需要鼠标都记为失败并修。

## 4. 已知限制（M4 剩余）

- **真实键盘事件未被自动化**：`M4-A11Y-001` 只验证快捷键表/绑定数量与 G3 键位覆盖；
  实际窗口的按键分发、焦点链与 IME 组合仍需要人工/UI 自动化（`M4-KEYBOARD-001` manual）。
- 没有完整的 Tab 焦点环与 Shift+Tab 逆序；侧栏会话列表、主题按钮等仍主要依赖鼠标。
- 生成中的"停止"只有鼠标按钮，没有 `Esc`/`cmd-.` 绑定；避免与取消重命名冲突后补。
- 屏幕阅读器（Windows UIA/VoiceOver）未实机验证；目前只保证 role/label 元数据齐全。
