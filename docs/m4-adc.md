# M4 W3 — ADC 交互（进行中）

> 更新：2026-11-16。范围见 [ADR-0013](decisions/0013-m4-scope.md) W3；进度见 [m4-status.md](m4-status.md)。
> ADC-v1 的**契约**已在 M3 冻结；本文只记录 M4 的运行时接线，不改元素词汇表。

## 1. 已接线

- **动作解析/校验**（M3 已有，`dshr-ui-contributions::parse_action`）：`call` / `command` /
  `openExternal` / `copy` 四类，校验 method/args、https、copy ≤ 64 KiB；非法动作降级并诊断。
- **Button `onPress` 回调**（M4 W3）：
  - `dshr-ui-gpui` 新增 `adc_view_with_handler` / `adc_panel_body_with_handler`；
  - `handler` 只接收已校验的 `Action` + `Window`（Input/Textarea 编辑需要真实 `TextInput`），
    Button 未 `disabled` 且声明 `onPress` 时才注册点击；
  - 递归渲染（Stack/Row/Grid/Card/Tabs/Accordion）会把 handler 传到嵌套 Button；
  - `adc_view` / `adc_panel_body` 保持纯渲染，M3 的工具卡/自检路径不回归。
- **chat 插件面板**：Button 动作回到宿主：
  - `copy` → 写系统剪贴板（真实可用）；
  - `openExternal` → 弹确认条（显示 URL，取消/打开）；确认前不调用系统浏览器，
    `Esc` 或"取消"关闭确认条；
  - `call` / `command` → `PluginHost::dispatch_action` → external-process `ui.action`
    （fire-and-forget），随后 `refresh_adc_panels` 再发 `ui.render`；SDK 用
    `serve_with_views_and_actions` 的 action 闭包接收；
- `M4-ADC-003`：external fixture 收到 `ui.action` 后下一次 `ui.render` 看到计数器变化。
- **tool card extra**（M4 W3）：`tool.result.meta.ui` 会保留到 `ToolCard.extra`，chat 的工具卡
  用 `adc_panel_body` 渲染为受限 ADC 附加区；`M4-ADC-004`（conversation-model）通过。
- **Checkbox / Switch `onChange`**（M4 W3）：点击整行回传 `onChange` 动作（`call`/`command`），
  与 Button 共用 `AdcActionHandler`；插件更新状态后宿主再 render。
- `M4-ADC-001`：带 handler 的嵌套 Button 树渲染、0 诊断；`cargo test -p dshr-ui-gpui`。
- **Select `onChange`**（M4 W3）：有 handler 且动作为 `call` 时展开为选项 chip，点击把
  `args.value` 设为该选项后回传；无 handler 时保持只读盒。
- **Input/Textarea `onChange`**（M4 W3）：点击发保留动作 `__adc_edit`（带 `path/value/on_change`），
  宿主打开真实 `TextInput` 模态；保存时把值注入 `on_change.args.value` 再派发。
- `M4-ADC-006`：Select onChange 树渲染 + `action_with_value` 注入断言。
- **Slider `onChange`**（M4 W3）：有 handler 且动作为 `call` 时显示 −/+ 步进按钮，
  点击注入数值 `args.value`；**不是拖拽**，拖拽/元素 bounds 仍未做。
- `M4-ADC-008`：Slider 步进数值注入 + 树渲染。
- `M4-ADC-002`：`PluginHost::dispatch_action` 路由/未知插件；`cargo test -p dshr-plugin-host`。

## 2. 未接线（W3 剩余）

| 项 | 现状 | 下一步 |
|---|---|---|
| `openExternal` 的实机确认 | 确认条已实现，未真机点过 | `M4-ADC-101` 人工复验 |
| Slider 拖拽/键盘 | 只有 −/+ 步进，无拖拽 | 需要元素 bounds 与 pointer drag，后续做 |
| Tabs `onChange` | 仍只读 | 与 panel 状态回写一起做 |
| tool card extra 的真实数据 | 模型/渲染已接，尚无真实工具写 `meta.ui` | 给一个官方/样例工具写 `meta.ui` 做端到端 |
| 窗口 draw 预算 / a11y 焦点 | 未做 | 与 W2 焦点环、W7 性能基线合并 |

## 3. 手工验证（`M4-ADC-100`）

1. 用 external fixture 声明一个 `workspace.panel`，树里放
   `Button{ "label": "复制", "a11yLabel": "复制", "onPress": { "copy": "demo" } }`；
2. 启动 `chat.exe`，`Ctrl/⌘+J` 打开插件面板；
3. 点击"复制"，剪贴板应为 `demo`；stderr 出现 `[adc] copy 4 bytes`；
4. 把 `onPress` 换成 `call`，点击后宿主发 `ui.action`；external fixture 的 action 计数器
   增加，下一次 `ui.render` 的文本出现 `actions=1`；未知 action 只写 stderr，不影响界面。
5. 把 `onPress` 换成 `openExternal: "https://example.com"`，点击后出现确认条；
   点"取消"或按 `Esc` 不打开；点"打开"才调用系统浏览器。

当前 `--selftest-adc-panel` 只验证到 `AnyElement` 构造层；**M4-ADC-009..017** 用 GPUI
`TestAppContext` 在 headless 窗口里真实 `simulate_click`，覆盖 Button `copy`、Checkbox
`onChange`、Switch `onChange`、Select chip 注入 value、Slider + 注入 value、Input/Textarea
发 `__adc_edit`、Button `openExternal` 派发，以及 Button 点击获得焦点后 Space 再触发
`onPress`。真实窗口的剪贴板/确认条/IME 仍需人工复验。

## 9. 真实点击测试（headless）

- `interactive_base` 给交互元素加 `debug_selector("adc-<path>")`（仅测试 feature 记录 bounds）；
- `adc_click.rs` 渲染 Button/Checkbox/Switch/Select/Slider/Input/Textarea，`add_window_view` →
  `debug_bounds(...)` → `simulate_click` → 断言 handler 收到 `Copy`/`Call`（含注入 value/`__adc_edit`）；
  Button 还验证点击后 Space 的键盘激活。
- 覆盖"渲染树正确但点击没绑上"的回归；不替代真实窗口 IME/剪贴板/拖拽手测。
