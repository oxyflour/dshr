# UI 对齐 DSH Desktop（2026-11-11）

**目标**：GPUI 桌面端的界面结构、配色、圆角与排版按 DSH Desktop 的**实际设计 token** 对齐，
而不是"看起来像"。视觉参考来自本机安装的 DSH 桌面版 `0.2.2-rc.1`（`app.asar`）。

## 来源与提取方法

| 参考物 | 路径（asar 内） | 用途 |
|---|---|---|
| 设计 token | `@deepseek-ai/dsh-client-ui-theme/lib/client.js` 内的 `design-platform.css` 等 | 调色板、语义别名、圆角、字体、阴影 |
| Windows 主窗口 | `lib/main.js` 的 `titleBarStyle`、`titleBarOverlay`，以及 `@deepseek-ai/dsh-client-ui-layout/lib/client.js` | 隐藏系统标题栏，菜单和窗口按钮位于顶部，窗口栏底色随主题变化 |
| 桌面壳样式 | `lib/welcome/design-platform.css`、`renderer/*.css` | Windows 标题栏/侧栏底色、圆角窗口 |
| 对话布局 | `@deepseek-ai/dsh-client-ui-chat` 的 CSS modules | 内容列宽、消息间距、Turn 指示 |
| 侧栏 | `@deepseek-ai/dsh-client-ui-sidebar` | 宽 280、8px 内边距、38px 新建按钮 |
| 输入区 | `@deepseek-ai/dsh-client-ui-conversation` / chat | 卡片 max-width、panel 圆角 28、34px 圆形发送键 |
| 输入区复核（0.2.2-rc.5） | `dsh-client-ui-conversation/src/client/skeleton/InputBar.module.css`、`ConversationRoot.module.css`、`dsh-client-ui-theme/src/styles/corner-shape.css` | 初始居中与会话底栏两种布局、卡片宽度、编辑区行高和 28px 超椭圆轮廓 |
| 插件管理（0.2.2-rc.5） | `dsh-client-ui-plugin-manager/src/client/PluginManagerPage.tsx` | 主内容区的已安装列表、开关和设置详情 |
| 工作区（0.2.2-rc.5） | `dsh-client-ui-workspace/src/client/rows/WorkspaceBrowser.tsx`、`src/client/WorkspacePicker.tsx` | 可折叠分组和新会话的工作区选择器 |
| 用户设置（0.2.2-rc.5） | `dsh-client-ui-settings-general/src/client/SettingsRoot.tsx` | 左下角入口、居中模态、分类导航 |
| 新会话模式（0.2.2-rc.5） | `dsh-client-ui-agent-preset/src/client/AgentPresetSeat.tsx`、`seat-store.ts` | 工作区旁选择模式，首轮前允许改选 |
| 模型设置（0.2.2-rc.5） | `dsh-client-ui-settings-models/src/client/ModelsSection.tsx`、`ProviderEditor.tsx`、`ApiKeyInput.tsx`、`ModelListEditor.tsx` | 连接列表、新增模型、就地编辑与掩码密钥输入 |
| 对话交互（0.2.2-rc.5） | `dsh-client-ui-chat/src/client/chat/TurnProcessNodeView.tsx`、`ReasoningRow.tsx`、`MessageIconActions.tsx`，`dsh-client-ui-model-selection/src/client/ModelSelect.tsx`，`dsh-client-ui-conversation/src/client/skeleton/InputBar.tsx` | 过程折叠、推理等级、停止图标、生成中权限选择和复制反馈 |

提取步骤（可复现）：

```powershell
node tools/asar.mjs ls dsh/node_modules/@deepseek-ai
node tools/asar.mjs cat dsh/node_modules/@deepseek-ai/dsh-client-ui-theme/lib/client.js > .tmp/theme-client.js
# 解析 CSS 里的 --dsw-* 定义，展开 var() 链，得到 light / body[data-ds-dark-theme] 两套值
```

结果落在 [`crates/ui-gpui/src/theme.rs`](../crates/ui-gpui/src/theme.rs)：
`Palette::LIGHT` / `Palette::DARK`、`Radii::DSH`、`Typography::DSH`、`Layout::DSH`，
并用单元测试锁住关键数值（改错会挂 CI，而不是靠肉眼）。

## 已对齐

| 项 | DSH 值 | GPUI 实现 |
|---|---|---|
| 侧栏宽 | 280（`layoutInfo.sidebar \|\| 280`） | `Layout::sidebar_width` |
| 侧栏底色 | `--dsw-specific-sidebar-fill`（浅 `#f9fafb` / 深 `#1b1b1c`） | 侧栏 `bg` |
| 对话列宽 | 同尺寸桌面窗口实测约 680 px | 聊天正文列 680 px，居中，窄窗随可用宽度收缩 |
| 输入卡片 | 同尺寸桌面窗口实测约 713 px，圆角 28 | 桌面目标宽 712 px，窄窗随内容收缩 |
| 发送键 | 34×34 圆形、向上箭头 | 圆形图标键，发送、停止与执行指令沿用现有操作 |
| 用户消息 | 右对齐气泡、深蓝 50 底、圆角 16、最宽 70.2% 内容宽 | `bubble` + `radii.lg` |
| 消息操作 | 悬停或聚焦时显示操作行，复制后显示勾选 | 用户消息和已完成的助手回复支持复制原文并短暂显示勾选反馈 |
| 思考与工具调用 | 过程可展开，工具行显示状态 | 连续工具调用与推理文本默认收起，展开后可查看工具卡片，轨迹页可直接查看工具调用 |
| 头部 | `min-height: 76px`、会话标题、下划线标签页 | 会话标题、下划线标签页和带提示的工具栏图标 |
| 字体/行高 | 正文 14/24、secondary 13/20、small 12/18 | `Typography::DSH` |
| 消息正文 | Markdown（标题/列表/表格/代码块/引用/行内样式） | `ui-gpui::markdown` 子集渲染：标题 h1–h4、段落、粗体/斜体/行内代码/链接、围栏代码块、列表、引用、表格、分隔线 |
| 边框 | 低透明度 0.5–1px（`border-l1..l4`） | 对应 RGBA token |
| 主题 | 跟随 DSH 的 `ui-theme.preference`（当前 profile 为 light） | 默认浅色，在左下角设置切换深色 |

### 插件、工作区与本地设置

- 侧栏「插件」打开主内容区管理页。已安装列表支持搜索和启停，文件工具提供独立的权限开关和 JSON 编辑。其他侧车插件可编辑配置 JSON，Loader 配置可查看。外置插件详情显示清单路径，启停在重启后生效。已注册的 ADC 视图位于「插件面板」。
- 侧栏按工作区折叠会话。工作区行首使用文件夹图标，点击行即可展开或收起。新会话的工作区选择器在输入卡左上方，菜单从选择器下缘打开（参考 `ConversationContent.tsx` 的 `heroWorkspaceRow`）。会话记录工作区，切换会话时工具目录随之恢复。
- 新会话输入卡上方的模式菜单紧邻工作区选择器。标准模式、创造模式和已发布的其他可用预设可在首轮前改选，保留会话和草稿。输入区继续使用 dshr 的菜单样式。[模式与绑定](spec/agent-presets.md)
- 左下角「设置」打开本地设置，侧栏折叠时保留齿轮入口。通用和模型页保存外观、工具权限和模型选择，插件页提供管理入口。
- 模型页可在已有 OpenAI 兼容连接上沿用同一服务的密钥来添加模型，也可填写独立连接。输入区菜单用 API 根地址区分同名模型。见[桌面模型连接设置](plan/desktop-model-settings.md)。
- 对话过程、模型推理等级、生成中权限、停止与复制的发布窗口验收见[桌面对话交互对齐](plan/desktop-conversation-alignment.md)。

## 未对齐（明确边界）

1. **Markdown 只覆盖常用子集**：语法高亮、图片、嵌套列表的层级渲染、表格列宽自适应、
   代码行号还没做；围栏代码块只有语言标签与等宽字体。未闭合的标记按字面量渲染。
2. **会话侧栏**：按工作区分组，标题与更新时间同排。工作区标题旁的搜索按钮可展开搜索框，
   支持按会话标题或工作区路径筛选。会话可新建、切换、行内重命名和二次确认删除。
   每个工作区默认显示五条主会话。其余会话可按批展开，搜索可直接定位。会话行默认显示更新时间，悬停时显示归档和「⋯」。点击「⋯」或右键点击会话行可打开同一个操作菜单。工作区标题旁可筛选已归档会话。
   会话置顶、按时间分组、跨会话正文搜索待实现。
   会话操作见 [`m2-sessions.md`](m2-sessions.md)。
3. **聊天与输入区**：顶部在会话标题旁显示模式和子智能体入口。右侧提供会话操作和插件视图侧栏，下方有「对话」「轨迹」。输入区可切换本机配置的模型
   和推理等级。生成中可更改工具权限，新授权和派发的子会话读取当前值。没有可用模型时，「配置模型」直达模型设置。
   测试可用 `DSHR_PROVIDER=mock` 选择本地模拟，或用 `DSHR_FORCE_MOCK=1` 限定为本地模拟。
   `DSHR_TOOL_POLICY=ask-all` 锁定逐次审批。只读模式即时拒绝新的官方侧车写入请求。
   可用文件选择器、拖入输入区或粘贴文件和剪贴板图片来添加附件，每条消息最多 4 个，
   UTF-8 文本总计 32 KiB，PNG、JPEG、GIF、WebP 和 PDF 总计 12 MiB。图片和 PDF
   需要 DeepSeek Flash 或支持对应内容类型的 OpenAI 兼容模型。会话日志保留文件名和引用，
   原始图片和 PDF 随会话导出。在构建多轮模型请求时，优先恢复较新的图片和 PDF。
   空会话中标识与引导文字同排，隐藏会话标题和标签。发送首条消息后显示会话头部，输入卡从居中切换到底部停靠。
   草稿随内容自动换行并增高，达到高度上限后内部滚动。Windows 和 Linux 用 `Ctrl+V` 粘贴，macOS 用 `Cmd+V`。粘贴多行文本时保留换行。
   Shift+Enter 换行，回车发送。「+」打开附件和指令菜单，权限和模型分别位于工具栏两侧。
   权限菜单沿触发按钮左侧对齐，模型菜单沿按钮右侧对齐，空间不足时调整到视口内。
   输入区菜单支持按 Escape 或点击外部关闭，触发按钮支持再次点击关闭。
   草稿和已发消息显示图片缩略图，点击可放大，按 Escape 关闭。动图预览使用首帧。
   PDF 文件卡片显示文件名和大小。
   输入 `/` 打开宿主指令菜单，
   可用方向键选择，回车执行。「轮次导航」（`/turns`）打开完整列表，可定位已落盘的早期轮次。宽窗在消息区右缘显示轮次标记，当前轮次高亮。
   用户消息和助手的 Markdown 正文可拖选并使用复制快捷键。消息操作行支持复制原文。
   输入框下方显示「回车发送」，悬停可看快捷键。
   插件注册的斜杠指令待实现。
4. **品牌与图标**：保留 DSHR 品牌。Windows 工具栏与复制键使用系统图标字体，工具卡片沿用现有样式。
5. **Windows 窗口栏**。顶部放置侧栏按钮、「应用」和「编辑」菜单，以及 Windows 窗口按钮。
   拖动区和最小化、最大化/还原、关闭按钮使用 GPUI 的原生窗口命中区。菜单调用现有会话和文本编辑操作。
   `DSHR_TITLEBAR_FLOW=1` 自检查验窗口栏下方的布局，以及通过菜单全选输入文本和新建会话。
6. **无过渡动画**：DSH 有 0.1–0.3s 的 hover/侧栏动画；GPUI 版先保证静态一致。
7. **长会话**：聊天流和轨迹使用变高行虚拟化。旧版性能基线见
   [`m2-performance.md`](m2-performance.md)。

## 窗口尺寸与高 DPI（踩过的坑）

GPUI 的 `WindowBounds` 是**逻辑像素**，平台再乘 `scale_factor` 变成物理窗口。本机是
1.75 缩放，最初写死 `1180x780` 逻辑 = `2065x1365` 物理，超过可见视口——窗口右侧和底部
（包括输入框）被裁掉，看起来就像"agent 输出文本消失了"。

修复：先用保守尺寸开窗，拿到 `window.display()` 与 `scale_factor()` 后按
`visible_bounds / scale` 重设尺寸，保证物理窗口不超过可见区域；窗口逻辑宽 < 600 时自动收起
侧栏，避免 280 的侧栏把正文挤出窗口。历史会话首次渲染后滚到最新一条（与 DSH 打开会话一致）。

## 验证与限制

- `cargo test -p dshr-ui-gpui`：token 数值断言（浅/深底色、侧栏色、圆角、布局常量）。
- `DSHR_COMPOSER_FLOW=1` 检查空态居中、点击浮层外部关闭并聚焦输入框，以及发送后输入卡停靠底部。Windows 发布构建在 175% 缩放下对照 DSH Desktop `0.2.2-rc.5`，另用深色窄窗验证菜单未遮挡标题栏和输入卡。会话菜单的「⋯」及右键入口均已实测，重命名写入测试会话元数据，取消删除后测试会话日志保持原样。
- 对 10,000 事件测试会话设置 `DSHR_VISIBLE_LIMIT=24` 和 `DSHR_NAVIGATION_FLOW=1`，
  可在真实 GPUI 窗口验证从斜杠菜单键盘进入轮次列表，并点击跳到第一轮。
- 附件已在真实 GPUI 窗口以本地模拟模型验证多选和发送，并对会话导出的两个原文件验证 SHA-256。
  已在浅色和深色窗口核对图片放大、关闭及重启后加载缩略图。
  测试还覆盖重启回放、剪贴板位图转 PNG、DeepSeek 的 PDF 上传与文件 ID 复用，以及 OpenAI 的内联图片和 PDF。
  [DeepSeek 视觉理解](https://api-docs.deepseek.com/guides/vision/)、
  [DeepSeek 文档解析](https://api-docs.deepseek.com/guides/document_parsing/)、
  [OpenAI 文件输入](https://developers.openai.com/api/docs/guides/file-inputs)。
- `dshr --selftest` / `--selftest-stream` / `--selftest-cordis` / `--selftest-dsh` 全部 PASS
  （UI 改动没有改变会话/工具/回放行为）。
- **截图已可用**：桌面截图抓不到 GPUI 窗口，但 `PrintWindow(PW_RENDERFULLCONTENT)` 可以：

  ```powershell
  # 进程不是 DPI-aware，窗口矩形是虚拟化尺寸，用 -Scale 1.75 抓完整物理窗口
  node tools/asar.mjs ls dsh/node_modules/@deepseek-ai   # （示例：确认工具可用）
  tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin dshr
  # 另开终端：先 windows 拿 hwnd，再 winshot
  $env:TMP='C:\Projects\dshr\.tmp'; $env:TEMP=$env:TMP
  powershell -ExecutionPolicy Bypass -File tools\screen.ps1 windows -ProcessId <pid>
  powershell -ExecutionPolicy Bypass -File tools\screen.ps1 winshot -Hwnd <hwnd> -Scale 1.75 -Path .tmp\chat.png
  ```

  已用该方式核对：侧栏、头部、用户气泡、长回复换行、输入卡片与发送键都在正确位置。
  在 175% 缩放的浅色窗口中复核了复制按钮和工作区选择器，在深色窄窗中复核了复制按钮和选择菜单。
  已点击复制并核对剪贴板原文，也验证了菜单关闭和切换后新会话的 `workspace_id`。

