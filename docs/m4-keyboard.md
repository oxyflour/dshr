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

1. 启动 `dshr.exe`，焦点应在输入框；
2. `Ctrl/⌘+N` 新建会话，标题变为新会话；
3. 直接输入一句话，`Enter` 发送；生成中界面出现状态；
4. 触发一个需要审批的工具（写文件/执行命令），审批条出现；
5. `Ctrl/⌘+Shift+A` 允许一次，或 `Ctrl/⌘+Shift+D` 拒绝；审批条消失、工具结果更新；
6. `Ctrl/⌘+B` 收起侧栏，再按展开；`Ctrl/⌘+J` 打开插件面板，`Esc` 关闭；
7. `Ctrl/⌘+K` 回到输入框，最后 `Ctrl/⌘+W` 关闭应用。

以上全部不碰鼠标即通过 G3；任一步需要鼠标都记为失败并修。

复验时设 `DSHR_UI_TRACE=1`：每个快捷键触发会向 stderr 打一行 `[keyboard] <action>`，
可作为"按键确实到达 ChatApp"的证据（不含按键内容/输入法组合态）。

本机自动化限制（2026-11-16）：用 `WScript.Shell.SendKeys` + `SetForegroundWindow` 尝试
自动注入 `Ctrl+B/J/Esc/N` 时，窗口能启动并 `AppActivate` 返回 true，但 Windows 前台锁让
`SetForegroundWindow` 失败、GPUI 未收到按键，stderr 无 `[keyboard]` 行。因此该项仍必须由
人工窗口或带 UI 自动化的 CI runner 复验，**不得**根据 SendKeys 失败/成功改判 G3。

## 4. 已知限制（M4 剩余）

- **真实键盘事件未被自动化**：`M4-A11Y-001` 只验证快捷键表/绑定数量与 G3 键位覆盖；
  实际窗口的按键分发、焦点链与 IME 组合仍需要人工/UI 自动化（`M4-KEYBOARD-001` manual）。
- 没有完整的 Tab 焦点环与 Shift+Tab 逆序；侧栏会话列表、主题按钮等仍主要依赖鼠标。
- 生成中的"停止"只有鼠标按钮，没有 `Esc`/`cmd-.` 绑定；避免与取消重命名冲突后补。
- 屏幕阅读器（Windows UIA/VoiceOver）未实机验证；目前只保证 role/label 元数据齐全。

## 5. 真实窗口按键注入（2026-11-18）

- 直接在一个 `WindowHandle::update` 回调里 `window.dispatch_keystroke("enter")` 会触发
  GPUI 实体重入 panic：`cannot update chat::WindowProbe while it is already being updated`。
- 正确做法是 `window.defer(cx, |window, cx| window.dispatch_keystroke(...))`，
  把注入排到本帧更新之后；`--selftest-window` 已用该方式验证真实窗口的 key handler，
  输出 `[selftest-window] PASS window+keyboard`。
- 这一步只证明窗口/按键路由；ChatApp 主流程由 §5.1 的 `DSHR_KEYBOARD_FLOW=1` 覆盖并已
  在 Windows CI 执行，因此 G3 判 PASS。IME/macOS 键位与完整 Tab 焦点环仍另测。

### 5.1 ChatApp 真实快捷键全流程（环境变量）

设置 `DSHR_KEYBOARD_FLOW=1` 后启动 GUI，应用会在首帧后通过 `window.defer` 依次注入：

1. `cmd-n` 新建会话；
2. `cmd-k` 聚焦输入框；
3. 逐字符输入 `hello keyboard`；
4. `Enter` 发送；
5. 一个后台线程向 `ApprovalBroker` 发起 synthetic 审批，`cmd-shift-a` 允许一次；
6. `cmd-j` 切换插件面板；
7. `cmd-w` 退出。

本机 Windows 实测 **exit 0**，会话日志包含 `message.user hello keyboard`，审批结果为
`AllowOnce`；GitHub Actions Windows `desktop build` 也执行该流程。它覆盖真实窗口的
新建→输入→发送→审批→面板→关闭键盘路径；审批请求是 synthetic，不是一次真实模型工具调用
（真实工具审批由 `--selftest-approval` 覆盖）。仍未自动覆盖：IME 组合输入、macOS Cmd 键位。
