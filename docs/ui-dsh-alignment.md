# UI 对齐 DSH Desktop（2026-11-11）

**目标**：GPUI 桌面端的界面结构、配色、圆角与排版按 DSH Desktop 的**实际设计 token** 对齐，
而不是"看起来像"。视觉参考来自本机安装的 DSH 桌面版 `0.2.2-rc.1`（`app.asar`）。

## 来源与提取方法

| 参考物 | 路径（asar 内） | 用途 |
|---|---|---|
| 设计 token | `@deepseek-ai/dsh-client-ui-theme/lib/client.js` 内的 `design-platform.css` 等 | 调色板、语义别名、圆角、字体、阴影 |
| 桌面壳样式 | `lib/welcome/design-platform.css`、`renderer/*.css` | Windows 标题栏/侧栏底色、圆角窗口 |
| 对话布局 | `@deepseek-ai/dsh-client-ui-chat` 的 CSS modules | 内容列宽、消息间距、Turn 指示 |
| 侧栏 | `@deepseek-ai/dsh-client-ui-sidebar` | 宽 280、8px 内边距、38px 新建按钮 |
| 输入区 | `@deepseek-ai/dsh-client-ui-conversation` / chat | 卡片 max-width、panel 圆角 28、34px 圆形发送键 |

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
| 内容列宽 | 748（`--dsh-chat-content-width` 回退值） | `Layout::content_width`，居中 |
| 输入卡片 | max-width 780（内容宽 + 32）、圆角 28、`input-major` 底色 | `composer_max_width` / `radii.panel` |
| 发送键 | 34×34 圆形、`button-info-fill` | `rounded_full` + `button_info` |
| 用户消息 | 右对齐气泡、深蓝 50 底、圆角 16、最宽 70.2% 内容宽 | `bubble` + `radii.lg` |
| 助手消息 | 无气泡、14/24 正文、左侧小标记 | 首字母方块（DSH 是鲸鱼图标） |
| 工具调用 | 圆角 8 的紧凑行 + 来源标签 + 状态色 | 工具卡片行 |
| 头部 | `min-height: 76px`、面包屑、右侧操作 | `header_height` + 会话面包屑 |
| 字体/行高 | 正文 14/24、secondary 13/20、small 12/18 | `Typography::DSH` |
| 边框 | 低透明度 0.5–1px（`border-l1..l4`） | 对应 RGBA token |
| 主题 | 跟随 DSH 的 `ui-theme.preference`（当前 profile 为 light） | 默认浅色，侧栏/头部可切换深色 |

## 未对齐（明确边界）

1. **消息正文仍是纯文本**：DSH 用 Markdown 渲染（标题/列表/表格/代码块/引用），
   我们还没接 Markdown 渲染器；代码块样式（`--dsw-alias-markdown-code-block`）暂未使用。
2. **侧栏只有当前会话**：DSH 的会话列表按时间分组、支持搜索/重命名/删除；
   我们还没有多会话后端（M1 遗留），侧栏只显示 `s-chat` 一行。
3. **没有命令面板 / 模型选择器 / 附件 / 审批 / Turn 导航 / 消息反馈**：
   这些是 DSH 输入区与对话区的完整能力，涉及后端状态，未用假 UI 硬凑。
4. **图标用文字/首字母近似**：DSH 的鲸鱼 logo、面板图标是 SVG 资源；
   我们的品牌方块与 `DM` 是占位，避免引入未授权资源。
5. **Windows 自定义标题栏 / 圆角窗口**：DSH 用 `--dsh-windows-titlebar` 与
   `corner-shape` 做系统级圆角；GPUI 侧暂用默认标题栏。
6. **无过渡动画**：DSH 有 0.1–0.3s 的 hover/侧栏动画；GPUI 版先保证静态一致。
7. **长会话未虚拟化**：`ScrollHandle + overflow_y_scroll` 是普通滚动容器；
   上千条消息需要回到 `uniform_list` 或分段渲染（M2 性能）。

## 验证与限制

- `cargo test -p dshr-ui-gpui`：token 数值断言（浅/深底色、侧栏色、圆角、布局常量）。
- `cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 全绿。
- `chat --selftest` / `--selftest-stream` / `--selftest-cordis` / `--selftest-dsh` 全部 PASS
  （UI 改动没有改变会话/工具/回放行为）。
- **视觉截图无法自证**：本机是 Microsoft Remote Display Adapter，GPUI 窗口不会被
  桌面截图捕获（试过 `tools/screen.ps1 shot`，只能截到前台浏览器）。因此"是否像 DSH"
  需要你在本机肉眼看一次；token 与结构已在代码里锁定。
