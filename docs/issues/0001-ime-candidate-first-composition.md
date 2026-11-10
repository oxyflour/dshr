# ISSUE-0001 — 中文 IME 首次组合输入时候选窗落在屏幕右下角

- 类别：Platform（GPUI 0.9 / `gpui_windows`）
- 状态：**Open**（探针里已加缓解，实测无效）
- 严重度：中（不阻塞开发；阻塞"输入中文"这一核心体验）
- 首次记录：2026-11-09
- 关联：[`../m1-log.md`](../m1-log.md) A2、`tools/ime-probe`、commit `d13a092`（缓解尝试）
- 上游：`gpui_windows/src/events.rs`、`gpui/src/window.rs`（GPUI `d2a4fb6`）

## 现象

在 GPUI 自绘输入框（`EntityInputHandler`）里，**第一次**启用中文输入法并敲拼音时，
候选窗出现在桌面右下角；结束该次输入后，**第二次**起候选窗正常出现在光标旁。

用户实测（2026-11-09，微软拼音，Windows）：加缓解前后行为一致。

## 复现

```powershell
tools\build-env.cmd cargo build --manifest-path tools\ime-probe\Cargo.toml
.\tools\ime-probe\target\debug\ime-probe.exe
```

1. 启动后单击输入框；
2. **第一次**就切换到中文输入法并敲拼音（不要先敲英文）；
3. 观察候选窗位置。

## 根因

1. `gpui_windows/src/events.rs:152` 把 `WM_IME_STARTCOMPOSITION` 路由到 `handle_ime_position`，
   后者调用 `retrieve_caret_position()`，再用 `ImmSetCandidateWindow(CFS_CANDIDATEPOS)` 定位候选窗
   （`events.rs:658–690`）。
2. `retrieve_caret_position` 依赖 input handler 的 `selected_text_range()` 与 `bounds_for_range()`。
   上游示例的 `bounds_for_range` 在 `last_layout` 缺失、或 `last_layout.text != content` 时返回 `None`。
3. 首次组合输入时若拿到 `None`，`ImmSetCandidateWindow` **不会被调用**，IME 回落到默认位置。
4. 上游示例从未调用 `Window::invalidate_character_coordinates()`（`gpui/src/window.rs:6944`），
   即"主动告知 IME 光标位置"的入口。

## 已尝试

在 `tools/ime-probe` 中于**获得焦点时**与**鼠标按下时**各调用一次
`window.invalidate_character_coordinates()`（deferred 到下一帧，内部走
`selected_bounds` → `update_ime_position`）。

**结果：无效，现象不变。** 说明问题不在"位置没有被提前写入"，而更可能在：

- `update_ime_position` 在窗口尚未获得 IME 焦点（`ImmGetContext` 返回无效上下文）时被调用，写入了也不生效；
- 或 `WM_IME_STARTCOMPOSITION` 到达时，`with_input_handler_and_scale_factor` 取到的 handler
  仍是旧的/空的，与后续帧无关。

## 2026-11-11 更新：产品界面已换成真实输入组件

`crates/ui-gpui` 现在有 vendored 的 `TextInput`（`EntityInputHandler` + `ElementInputHandler`），
`chat` 的输入框已从"`div` + `key_down`"换成它。这意味着：

- 中文 IME 的组合输入路径**首次在产品界面里可用**（此前只在 `tools/ime-probe` 里验证过）；
- 焦点获得与鼠标按下时都会调用 `window.invalidate_character_coordinates()`（沿用探针的缓解）；
- 该问题仍**未关闭**：缓解在探针里实测无效，产品界面需要真人键盘复验一次。

**人工复验步骤**（自动化无法模拟输入法）：

```powershell
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat
```

1. 启动后直接切中文输入法（先不要敲英文）；
2. 敲一段拼音，观察候选窗是否仍在桌面右下角；
3. 再确认：组合态下划线与光标正常、上屏文本正确、退格不破坏 UTF-16 选区。

## 下一步（按成本排序）

1. 在 `update_ime_position` 处加日志（`ImmGetContext` 是否有效、`caret_position` 数值、调用时机），
   确认是"没调用"还是"调用了但上下文无效"。
2. 若确认需要在 `WM_IME_SETCONTEXT` / 焦点切换时补一次：给 `gpui_windows` 打本地补丁
   （fork 或 `[patch]`），并在补丁中把 `ImmSetCandidateWindow` 与 `ImmSetCompositionWindow`
   一起调用。
3. 兜底方案：输入框获得焦点后先插入一次零宽更新（重绘）再允许输入——不可靠，不推荐。

## 影响

- M2 的输入框方案必须回答"中文输入是否可用"；若上游短期不修，需要接受"第二次起正常"的退化，
  或在产品层做成"聚焦后再重绘一次"的隐式预热。
- 不影响 A3（WebView2）与后续后端工作，故先挂起。
