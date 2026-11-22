# DSH Desktop 与 DSHR GPUI

## 获取参考源码

在 DSHR 仓库根目录运行。`tools/asar.mjs` 默认读取 Windows 本机安装的 DeepSeek Harness。可以通过进程环境变量 `DSHR_ASAR` 选择其他版本的安装包。先确认版本，再读取所需文件或抽取到忽略目录。

```powershell
node tools/asar.mjs cat package.json | ConvertFrom-Json | Select-Object name,version
node tools/asar.mjs ls dsh/node_modules/@deepseek-ai
node tools/asar.mjs cat dsh/node_modules/@deepseek-ai/dsh-client-ui-plugin-manager/src/client/PluginManagerPage.module.css
node tools/asar.mjs extract dsh/node_modules/@deepseek-ai/dsh-client-ui-workspace .tmp/ui-ref/workspace
```

`0.2.2-rc.5` 的入口如下。包名均位于 `dsh/node_modules/@deepseek-ai/` 下。其他版本先用 `ls` 或 `list` 确认路径。发布包只带编译产物时，从 `lib/client.js` 和 CSS 中查找组件、状态名和 token。

| 模块 | DSH 参考入口 | DSHR 实现入口 |
|---|---|---|
| 主题和圆角 | `dsh-client-ui-theme/src/styles/`，含 `corner-shape.css` | `crates/ui-gpui/src/theme.rs`、`apps/desktop-gpui/src/bin/chat/composer_surface.rs` |
| 会话与输入区 | `dsh-client-ui-conversation/src/client/skeleton/InputBar.module.css`、`ConversationRoot.module.css` | `apps/desktop-gpui/src/bin/chat.rs`、`crates/ui-gpui/src/text_input.rs`、`crates/ui-gpui/src/message_selection.rs` |
| 插件管理 | `dsh-client-ui-plugin-manager/src/client/PluginManagerPage.tsx` 和同名 CSS module | `apps/desktop-gpui/src/bin/chat/plugins_view.rs`、`crates/profile/src/lib.rs` |
| 工作区 | `dsh-client-ui-workspace/src/client/rows/WorkspaceBrowser.tsx`、`dsh-client-ui-workspace/src/client/WorkspacePicker.tsx` | `apps/desktop-gpui/src/bin/chat/workspace_view.rs`、`crates/session-store/src/session_index.rs`、`ChatState::switch_session` |
| 用户设置 | `dsh-client-ui-settings-general/src/client/SettingsRoot.tsx` 和同名 CSS module | `apps/desktop-gpui/src/bin/chat/settings_view.rs` |

现有对照与验证记录在 `docs/ui-dsh-alignment.md`。参考尺寸会随版本、可用内容区和缩放变化，先定位对应状态再调整目标。

## 容易遗漏的验收

- 输入卡片有空会话居中与活跃会话底部停靠两种布局。尺寸差异同时查外轮廓、内边距、输入行高和发送按钮位置。超椭圆与同半径的普通圆角可能有明显视觉差异。弹层比较触发器和弹层边缘，并验证窄窗收缩及点击外部关闭。
- 侧栏会话行在默认态显示更新时间，悬停才显示行操作。验证长标题、折叠态，以及弹层或页面切换后快捷键和焦点是否仍能工作。消息文字选中应覆盖助手 Markdown 和用户消息，且拖选后可复制。
- 插件启停、文件工具配置和 Loader 只读分别验证。切换到新工作区后检查会话日志的 `workspace_id`，再用两个临时目录验证读写工具实际使用对应根目录。设置弹窗从左下角打开，确认遮罩、Escape、关闭后焦点及重启后的本地偏好。
- DSHR 的用户设置范围以本地设置为准，按当前需求保留主题、已配置模型、工具权限和插件入口。DS 账号和反馈无需从参考界面迁入。

## 构建与窗口验证

```powershell
tools\build-env.cmd cargo build --offline --locked -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo test --offline --locked -p dshr-desktop-gpui -p dshr-profile -p dshr-ui-gpui --lib --bins
```

离线依赖已就绪时使用上述 `--offline` 命令。发布验证给构建命令加 `--release`，Windows 主程序名为 `dshr.exe`。UI 冒烟测试可用独立的 `DSHR_HOME`，设置 `DSHR_FORCE_MOCK=1` 和 `DSHR_PROVIDER=mock`。`DSHR_COMPOSER_FLOW=1`、`DSHR_TITLEBAR_FLOW=1`、`DSHR_KEYBOARD_FLOW=1` 分别用于输入框、标题栏和快捷键。工作区改到工具执行链路时，用准备好本地 DSH 依赖的隔离目录运行 `dshr.exe --selftest-dsh-write`。

需要截图时用 `tools/screen.ps1` 的 `windows` 找当前测试进程窗口，再用 `winshot` 按窗口句柄抓取。脚本使用 `PrintWindow`，可捕获普通桌面截图遗漏的 GPUI 窗口。按实际 DPI 核对 `-Scale`，将截图存到 `.tmp/`。
