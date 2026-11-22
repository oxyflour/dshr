use super::*;

pub(super) fn render_stack(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    row: bool,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let mut container = layout_box(node, row);
    for (index, child) in node.children.iter().enumerate() {
        container = container.child(render_node(theme, child, &child_path(path, index), handler));
    }
    container.into_any_element()
}

/// Stack/Row 共用的 flex 容器：gap/padding 只接受 spacing token，align/justify 映射 flex 属性。
pub(super) fn layout_box(node: &ElementNode, row: bool) -> Div {
    let mut element = div().flex().w_full().min_w(px(0.0));
    element = if row {
        element.flex_row()
    } else {
        element.flex_col()
    };
    if row && node.bool_prop("wrap").unwrap_or(false) {
        element = element.flex_wrap();
    }
    element.style().align_items = Some(match node.str_prop("align") {
        Some(value) => align_items(value),
        None => AlignItems::Stretch,
    });
    element.style().justify_content = Some(match node.str_prop("justify") {
        Some(value) => justify_content(value),
        None => AlignContent::Start,
    });
    element
        .gap(spacing_px(node.i64_prop("gap")))
        .p(spacing_px(node.i64_prop("padding")))
}

pub(super) fn render_grid(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let mut grid = div()
        .grid()
        .w_full()
        .grid_cols(grid_columns(node.i64_prop("columns")))
        .gap(spacing_px(node.i64_prop("gap")))
        .p(spacing_px(node.i64_prop("padding")));
    for (index, child) in node.children.iter().enumerate() {
        // 跨列写在网格项（包装 div）上，子元素本身不感知布局。
        let cell = div()
            .col_span(grid_span(child.span))
            .min_w(px(0.0))
            .child(render_node(theme, child, &child_path(path, index), handler));
        grid = grid.child(cell);
    }
    grid.into_any_element()
}

pub(super) fn render_divider(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let mut row = div().flex().flex_row().items_center().w_full().gap(px(8.0));
    row = row.child(divider_line(theme));
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row
            .child(
                div()
                    .flex_none()
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_tertiary))
                    .child(label.to_owned()),
            )
            .child(divider_line(theme));
    }
    row.into_any_element()
}

pub(super) fn divider_line(theme: &Theme) -> Div {
    let palette = theme.palette;
    div()
        .flex_1()
        .h(px(1.0))
        .bg(palette.color(palette.border_l2))
}

// --- 文本 -----------------------------------------------------------------

pub(super) fn render_card(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let border = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.border_l2),
    };
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.md)
        .border_1()
        .border_color(border)
        .bg(palette.color(palette.bg_layer_2))
        .overflow_hidden();
    if let Some(title) = node.str_prop("title").filter(|title| !title.is_empty()) {
        card = card.child(
            div()
                .w_full()
                .px(px(12.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .text_size(type_scale.body)
                .line_height(type_scale.body_line)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(palette.color(palette.label_primary))
                .child(title.to_owned()),
        );
    }
    let mut body = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .p(px(12.0))
        .gap(px(8.0));
    for (index, child) in node.children.iter().enumerate() {
        body = body.child(render_node(theme, child, &child_path(path, index), handler));
    }
    card.child(body).into_any_element()
}

pub(super) fn render_tabs(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let tabs = node.array_prop("tabs").map(Vec::as_slice).unwrap_or(&[]);
    let active = node.str_prop("activeKey").unwrap_or_default();
    let mut bar = div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(px(4.0))
        .border_b_1()
        .border_color(palette.color(palette.border_l1));
    for (index, tab) in tabs.iter().take(MAX_TABS).enumerate() {
        let key = tab
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let label = tab
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let is_active = !active.is_empty() && key == active;
        let mut chip = div()
            .id(format!("{path}-tab-{index}"))
            .debug_selector(|| format!("adc-{path}-tab-{index}"))
            .flex_none()
            .px(px(10.0))
            .py(px(5.0))
            .text_size(type_scale.secondary)
            .line_height(type_scale.secondary_line);
        chip = if is_active {
            chip.bg(palette.color(palette.bubble))
                .text_color(palette.color(palette.business_primary))
        } else {
            chip.text_color(palette.color(palette.label_secondary))
        };
        if let (Some(action @ Action::Call { .. }), Some(handler)) =
            (node.action("onChange"), handler)
        {
            let next = action_with_value(&action, key);
            let handler = handler.clone();
            let key_handler = handler.clone();
            let key_next = next.clone();
            chip = chip
                .role(Role::Button)
                .aria_label(format!("切换 {label}"))
                .cursor_pointer()
                .focusable()
                .on_click(move |_, window, cx| handler(&next, window, cx))
                .on_key_down(move |event, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_handler(&key_next, window, cx);
                    }
                });
        }
        bar = bar.child(chip.child(label.to_owned()));
    }
    let mut container = interactive_base(
        div().flex().flex_col().w_full().min_w(px(0.0)).gap(px(8.0)),
        node,
        path,
        Role::TabList,
    )
    .child(bar);
    // `Tabs` 的节点级 children 就是当前 active 页内容（spec §6.8）。
    for (index, child) in node.children.iter().enumerate() {
        container = container.child(render_node(theme, child, &child_path(path, index), handler));
    }
    container.into_any_element()
}

pub(super) fn render_accordion(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let sections = node
        .array_prop("sections")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let expanded: HashSet<&str> = node
        .array_prop("expanded")
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str())
                .collect::<HashSet<&str>>()
        })
        .unwrap_or_default();
    let mut container = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .overflow_hidden();
    if sections.is_empty() {
        return container
            .child(caption(theme, "（无 section）"))
            .into_any_element();
    }
    for section in sections.iter().take(MAX_ACCORDION_SECTIONS) {
        let key = section
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let title = section
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let open = expanded.contains(key);
        let glyph = if open { "▾" } else { "▸" };
        container = container.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .w_full()
                .px(px(10.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .child(
                    div()
                        .flex_none()
                        .w(px(12.0))
                        .text_size(type_scale.small)
                        .line_height(type_scale.small_line)
                        .text_color(palette.color(palette.label_tertiary))
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(type_scale.secondary)
                        .line_height(type_scale.secondary_line)
                        .text_color(palette.color(palette.label_primary))
                        .child(title.to_owned()),
                ),
        );
        if !open {
            continue;
        }
        // `sections[].children` 在数据层是 `nested_children[key]`（spec §6.8 的容器规则）。
        if let Some(children) = node.nested_children.get(key) {
            let mut body = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w(px(0.0))
                .px(px(10.0))
                .py(px(8.0))
                .gap(px(8.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1));
            for (index, child) in children.iter().enumerate() {
                body = body.child(render_node(
                    theme,
                    child,
                    &format!("{path}.sections[{key}].children[{index}]"),
                    handler,
                ));
            }
            container = container.child(body);
        }
    }
    container.into_any_element()
}

// --- 降级 -----------------------------------------------------------------
