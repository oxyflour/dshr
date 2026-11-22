use super::*;

pub(super) fn render_button(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let disabled = node.bool_prop("disabled").unwrap_or(false);
    let tone = node.tone.map(|tone| tone.as_str());
    let accent = match tone {
        Some("neutral") => palette.color(palette.tag),
        Some(tone) => tone_color(&palette, Some(tone)),
        None => palette.color(palette.business_primary),
    };
    let foreground = if disabled {
        palette.color(palette.label_tertiary)
    } else if tone == Some("neutral") {
        palette.color(palette.label_primary)
    } else {
        palette.color(palette.bg_base)
    };
    let label = truncate_text(node.str_prop("label").unwrap_or_default()).to_owned();
    let mut button = interactive_base(div(), node, path, Role::Button)
        .flex_none()
        .px(px(12.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .bg(if disabled {
            palette.color(palette.interactive_hover_solid)
        } else {
            accent
        })
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(foreground)
        .child(label);
    if disabled {
        button = button.opacity(0.6);
    } else if let (Some(action), Some(handler)) = (node.action("onPress"), handler) {
        // M4 W3：onPress 动作在这里回到宿主；启停/焦点由宿主状态驱动，渲染仍不持有状态。
        let handler = handler.clone();
        let key_handler = handler.clone();
        let key_action = action.clone();
        button = button
            .cursor_pointer()
            .focusable()
            .on_click(move |_, window, cx| handler(&action, window, cx))
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_handler(&key_action, window, cx);
                }
            });
    }
    button.into_any_element()
}

pub(super) fn render_input(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let multiline = node.bool_prop("multiline").unwrap_or(false);
    let raw_value = node.str_prop("value").unwrap_or_default().to_owned();
    let value = truncate_text(&raw_value);
    let placeholder = node.str_prop("placeholder");
    let role = if multiline {
        Role::MultilineTextInput
    } else {
        Role::TextInput
    };
    let mut input = interactive_base(div(), node, path, role)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line);
    if let Some(placeholder) = placeholder {
        input = input.aria_placeholder(placeholder);
    }
    input = input.aria_value(value);
    if value.is_empty() {
        let hint = placeholder.unwrap_or("（空）").to_owned();
        input = input
            .text_color(palette.color(palette.label_caption))
            .child(hint);
    } else {
        input = input
            .text_color(palette.color(palette.label_primary))
            .child(value.to_owned());
    }
    if let (Some(action), Some(handler)) = (node.action("onChange"), handler) {
        let edit = edit_action(path, &raw_value, multiline, action);
        let handler = handler.clone();
        let key_handler = handler.clone();
        let key_edit = edit.clone();
        input = input
            .cursor_pointer()
            .focusable()
            .on_click(move |_, window, cx| handler(&edit, window, cx))
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_handler(&key_edit, window, cx);
                }
            });
    }
    input.into_any_element()
}

pub(super) fn render_textarea(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let rows = node.i64_prop("rows").unwrap_or(3).clamp(1, 100);
    let raw_value = node.str_prop("value").unwrap_or_default().to_owned();
    let value = truncate_text(&raw_value);
    let min_height = type_scale.secondary_line.as_f32() * rows as f32 + 10.0;
    let mut textarea = interactive_base(div(), node, path, Role::MultilineTextInput)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .min_h(px(min_height))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(if value.is_empty() {
            palette.color(palette.label_caption)
        } else {
            palette.color(palette.label_primary)
        })
        .child(if value.is_empty() {
            "（空）".to_owned()
        } else {
            value.to_owned()
        });
    if let (Some(action), Some(handler)) = (node.action("onChange"), handler) {
        let edit = edit_action(path, &raw_value, true, action);
        let handler = handler.clone();
        let key_handler = handler.clone();
        let key_edit = edit.clone();
        textarea = textarea
            .cursor_pointer()
            .focusable()
            .on_click(move |_, window, cx| handler(&edit, window, cx))
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_handler(&key_edit, window, cx);
                }
            });
    }
    textarea.into_any_element()
}

pub(super) fn render_select(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let value = node.str_prop("value").unwrap_or_default();
    let options = node.array_prop("options").map(Vec::as_slice).unwrap_or(&[]);
    // 命中 options 时显示 label，否则原样显示 value（解析期不保证 value 在选项里）。
    let label = options
        .iter()
        .find(|option| option.get("value").and_then(|value| value.as_str()) == Some(value))
        .and_then(|option| option.get("label").and_then(|label| label.as_str()))
        .unwrap_or(value)
        .to_owned();
    // M4 W3：有 onChange(call) 时把选项展开成可点 chip，点击把 value 注入 args 后回传；
    // 没有 handler/action 时保持原来的只读展示。
    if let (Some(action @ Action::Call { .. }), Some(handler)) = (node.action("onChange"), handler)
    {
        let mut chips = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(4.0))
            .w_full();
        for (index, option) in options.iter().take(20).enumerate() {
            let option_value = option
                .get("value")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned();
            let option_label = option
                .get("label")
                .and_then(|label| label.as_str())
                .unwrap_or(&option_value)
                .to_owned();
            let selected = option_value == value;
            let next = action_with_value(&action, &option_value);
            let handler = handler.clone();
            let key_handler = handler.clone();
            let key_next = next.clone();
            let aria = format!("选择 {option_label}");
            let mut chip = div()
                .id(format!("{path}-option-{index}"))
                .debug_selector(|| format!("adc-{path}-option-{index}"))
                .role(Role::Button)
                .aria_label(aria)
                .px(px(8.0))
                .py(px(3.0))
                .rounded(theme.radii.sm)
                .cursor_pointer()
                .focusable()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(if selected {
                    palette.bg_base
                } else {
                    palette.label_primary
                }))
                .bg(palette.color(if selected {
                    palette.business_primary
                } else {
                    palette.input_major
                }))
                .on_click(move |_, window, cx| handler(&next, window, cx))
                .on_key_down(move |event, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_handler(&key_next, window, cx);
                    }
                });
            chip = chip.child(option_label);
            chips = chips.child(chip);
        }
        return interactive_base(div(), node, path, Role::ComboBox)
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .aria_value(label.clone())
            .child(
                div()
                    .text_size(type_scale.small)
                    .text_color(palette.color(palette.label_caption))
                    .child(label.clone()),
            )
            .child(chips)
            .into_any_element();
    }
    interactive_base(div(), node, path, Role::ComboBox)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(palette.color(palette.label_primary))
        .aria_value(label.clone())
        .child(div().flex_1().min_w(px(0.0)).child(label))
        .child(
            div()
                .flex_none()
                .text_color(palette.color(palette.label_tertiary))
                .child("▾"),
        )
        .into_any_element()
}

pub(super) fn render_checkbox(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let checked = node.bool_prop("checked").unwrap_or(false);
    let accent = palette.color(palette.business_primary);
    let mut box_mark = div()
        .flex_none()
        .size(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radii.xs)
        .border_1()
        .border_color(if checked {
            accent
        } else {
            palette.color(palette.border_l3)
        })
        .bg(if checked {
            accent
        } else {
            palette.color(palette.input_major)
        })
        .text_size(px(11.0))
        .line_height(px(11.0))
        .text_color(palette.color(palette.bg_base));
    if checked {
        box_mark = box_mark.child("✓");
    }
    let mut row = interactive_base(div(), node, path, Role::CheckBox)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_toggled(Toggled::from(checked))
        .child(box_mark);
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(label.to_owned()),
        );
    }
    if let (Some(action), Some(handler)) = (node.action("onChange"), handler) {
        let handler = handler.clone();
        let key_handler = handler.clone();
        let key_action = action.clone();
        row = row
            .cursor_pointer()
            .focusable()
            .on_click(move |_, window, cx| handler(&action, window, cx))
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_handler(&key_action, window, cx);
                }
            });
    }
    row.into_any_element()
}

pub(super) fn render_switch(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let checked = node.bool_prop("checked").unwrap_or(false);
    let accent = palette.color(palette.business_primary);
    let knob = div()
        .flex_none()
        .size(px(14.0))
        .rounded_full()
        .bg(palette.color(palette.bg_base));
    let mut track = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .w(px(32.0))
        .h(px(18.0))
        .px(px(2.0))
        .rounded_full()
        .bg(if checked {
            accent
        } else {
            palette.color(palette.border_l4)
        });
    track = if checked {
        track.justify_end()
    } else {
        track.justify_start()
    };
    let mut row = interactive_base(div(), node, path, Role::Switch)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_toggled(Toggled::from(checked))
        .child(track.child(knob));
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(label.to_owned()),
        );
    }
    if let (Some(action), Some(handler)) = (node.action("onChange"), handler) {
        let handler = handler.clone();
        let key_handler = handler.clone();
        let key_action = action.clone();
        row = row
            .cursor_pointer()
            .focusable()
            .on_click(move |_, window, cx| handler(&action, window, cx))
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    key_handler(&key_action, window, cx);
                }
            });
    }
    row.into_any_element()
}

struct SliderDragPreview;

impl Render for SliderDragPreview {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        div().size(px(1.0))
    }
}

pub(super) fn render_slider(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let value = node.f64_prop("value").unwrap_or(0.0);
    let min = node.f64_prop("min").unwrap_or(0.0);
    let max = node.f64_prop("max").unwrap_or(100.0);
    let step = node.f64_prop("step").unwrap_or(1.0);
    let step = if step > 0.0 { step } else { 1.0 };
    let fraction = slider_fraction(value, min, max);
    let accent = palette.color(palette.business_primary);
    // 填充/剩余两段都用 flex_grow 表达比例，不依赖百分比宽度。
    let mut track = div()
        .id(format!("{path}-track"))
        .flex()
        .flex_row()
        .flex_1()
        .min_w(px(0.0))
        .h(px(6.0))
        .rounded_full()
        .overflow_hidden()
        .bg(palette.color(palette.border_l3))
        .child(
            div()
                .flex_grow(fraction)
                .flex_basis(px(0.0))
                .h_full()
                .bg(accent),
        )
        .child(div().flex_grow(1.0 - fraction).flex_basis(px(0.0)).h_full());
    if fraction <= 0.0 {
        // flex_grow(0) 的段宽为 0；再垫一个 0 高元素避免空段触发布局歧义。
        track = track.child(div().flex_none().h(px(0.0)));
    }
    if let (Some(action @ Action::Call { .. }), Some(handler)) = (node.action("onChange"), handler)
    {
        let drag_action = action.clone();
        let drag_handler = handler.clone();
        track = track
            .debug_selector(|| format!("adc-{path}-track"))
            .on_drag((), |_, _, _, cx| cx.new(|_| SliderDragPreview))
            .on_drag_move(move |event: &gpui::DragMoveEvent<()>, window, cx| {
                let width = event.bounds.size.width.as_f32();
                if width <= 0.0 {
                    return;
                }
                let x = (event.event.position.x - event.bounds.origin.x).as_f32();
                let fraction = ((x / width).clamp(0.0, 1.0)) as f64;
                let raw = min + fraction * (max - min);
                let snapped = ((raw - min) / step).round() * step + min;
                let value = snapped.clamp(min, max);
                let action = action_with_json(&drag_action, serde_json::json!(value));
                drag_handler(&action, window, cx);
            });
    }
    let mut slider = interactive_base(div(), node, path, Role::Slider)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_numeric_value(value)
        .aria_min_numeric_value(min)
        .aria_max_numeric_value(max);
    if let (Some(action @ Action::Call { .. }), Some(handler)) = (node.action("onChange"), handler)
    {
        let lower = (value - step).max(min);
        let upper = (value + step).min(max);
        let dec = action_with_json(&action, serde_json::json!(lower));
        let inc = action_with_json(&action, serde_json::json!(upper));
        let dec_handler = handler.clone();
        let inc_handler = handler.clone();
        let key_dec_handler = handler.clone();
        let key_inc_handler = handler.clone();
        let key_dec = dec.clone();
        let key_inc = inc.clone();
        slider = slider
            .focusable()
            .on_key_down(
                move |event, window, cx| match event.keystroke.key.as_str() {
                    "left" | "down" => key_dec_handler(&key_dec, window, cx),
                    "right" | "up" => key_inc_handler(&key_inc, window, cx),
                    _ => {}
                },
            )
            .child(
                div()
                    .id(format!("{path}-dec"))
                    .debug_selector(|| format!("adc-{path}-dec"))
                    .role(Role::Button)
                    .aria_label("减少数值")
                    .flex_none()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(theme.radii.xs)
                    .bg(palette.color(palette.input_major))
                    .cursor_pointer()
                    .text_size(type_scale.small)
                    .text_color(palette.color(palette.label_primary))
                    .child("−")
                    .on_click(move |_, window, cx| dec_handler(&dec, window, cx)),
            )
            .child(track)
            .child(
                div()
                    .id(format!("{path}-inc"))
                    .debug_selector(|| format!("adc-{path}-inc"))
                    .role(Role::Button)
                    .aria_label("增加数值")
                    .flex_none()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(theme.radii.xs)
                    .bg(palette.color(palette.input_major))
                    .cursor_pointer()
                    .text_size(type_scale.small)
                    .text_color(palette.color(palette.label_primary))
                    .child("+")
                    .on_click(move |_, window, cx| inc_handler(&inc, window, cx)),
            );
    } else {
        slider = slider.child(track);
    }
    slider
        .child(
            div()
                .flex_none()
                .w(px(46.0))
                .text_align(TextAlign::Right)
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_secondary))
                .child(format_number(value)),
        )
        .into_any_element()
}

// --- 数据 -----------------------------------------------------------------
