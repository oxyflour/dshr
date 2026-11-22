use super::*;

pub(super) fn render_list(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let items = node.array_prop("items").map(Vec::as_slice).unwrap_or(&[]);
    let total = items.len();
    let shown = clip_list_items(total);
    let mut list = interactive_base(div(), node, path, Role::List)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0));
    if shown == 0 {
        return list.child(caption(theme, "（空列表）")).into_any_element();
    }
    for item in items.iter().take(shown) {
        let mut row = div()
            .flex()
            .flex_col()
            .w_full()
            .px(px(6.0))
            .py(px(4.0))
            .rounded(theme.radii.xs);
        let primary = item
            .get("primary")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        row = row.child(
            div()
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(primary.to_owned()),
        );
        if let Some(secondary) = item.get("secondary").and_then(|value| value.as_str()) {
            row = row.child(
                div()
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_tertiary))
                    .child(secondary.to_owned()),
            );
        }
        list = list.child(row);
    }
    if total > shown {
        list = list.child(caption(
            theme,
            format!("（仅渲染前 {shown} 项，共 {total} 项）"),
        ));
    }
    list.into_any_element()
}

pub(super) fn render_table(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let columns = node.array_prop("columns").map(Vec::as_slice).unwrap_or(&[]);
    let rows = node.array_prop("rows").map(Vec::as_slice).unwrap_or(&[]);
    let column_count = clip_table_columns(columns.len());
    let row_count = clip_table_rows(rows.len());
    let mut table = interactive_base(div(), node, path, Role::Table)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .overflow_hidden();
    if column_count == 0 {
        return table
            .child(caption(theme, "（表格无列）"))
            .into_any_element();
    }
    // 列宽策略简化：按列均分（`width` 先忽略，见实现说明）。
    let mut header = div()
        .flex()
        .flex_row()
        .w_full()
        .bg(palette.color(palette.tag))
        .border_b_1()
        .border_color(palette.color(palette.border_l2));
    let visible_columns: Vec<&serde_json::Value> = columns.iter().take(column_count).collect();
    for column in &visible_columns {
        let title = column
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        header = header.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .px(px(8.0))
                .py(px(4.0))
                .whitespace_nowrap()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(palette.color(palette.label_secondary))
                .child(title.to_owned()),
        );
    }
    table = table.child(header);
    if row_count == 0 {
        return table
            .child(caption(theme, "（无数据行）"))
            .into_any_element();
    }
    for (row_index, row_value) in rows.iter().take(row_count).enumerate() {
        let mut row = div()
            .flex()
            .flex_row()
            .w_full()
            .border_b_1()
            .border_color(palette.color(palette.border_l1));
        if row_index + 1 == row_count {
            row = row.border_b_0();
        }
        for column in &visible_columns {
            let key = column
                .get("key")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let text = row_value.get(key).map(cell_text).unwrap_or_default();
            row = row.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_primary))
                    .child(text),
            );
        }
        table = table.child(row);
    }
    let total_rows = rows.len();
    if total_rows > row_count {
        table = table.child(caption(
            theme,
            format!("（仅渲染前 {row_count} 行，共 {total_rows} 行）"),
        ));
    }
    table.into_any_element()
}

pub(super) fn render_tree(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let nodes = node.array_prop("nodes").map(Vec::as_slice).unwrap_or(&[]);
    let expanded: HashSet<&str> = node
        .array_prop("expanded")
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str())
                .collect::<HashSet<&str>>()
        })
        .unwrap_or_default();
    let mut tree = interactive_base(div(), node, path, Role::Tree)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0));
    let mut rendered = 0usize;
    for tree_node in nodes {
        render_tree_node(theme, tree_node, &expanded, 0, &mut tree, &mut rendered);
        if rendered >= MAX_TREE_ROWS {
            break;
        }
    }
    if rendered == 0 {
        return tree.child(caption(theme, "（空树）")).into_any_element();
    }
    if nodes.len() > 1 && rendered >= MAX_TREE_ROWS {
        tree = tree.child(caption(
            theme,
            format!("（仅渲染前 {MAX_TREE_ROWS} 个节点）"),
        ));
    }
    tree.into_any_element()
}

pub(super) fn render_tree_node(
    theme: &Theme,
    value: &serde_json::Value,
    expanded: &HashSet<&str>,
    depth: usize,
    tree: &mut Stateful<Div>,
    rendered: &mut usize,
) {
    if *rendered >= MAX_TREE_ROWS {
        return;
    }
    *rendered += 1;
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let key = value
        .get("key")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let label = value
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let children: &[serde_json::Value] = value
        .get("children")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let open = expanded.contains(key);
    let glyph = if children.is_empty() {
        "·"
    } else if open {
        "▾"
    } else {
        "▸"
    };
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(px(4.0))
        .pl(px(8.0 + depth as f32 * 14.0))
        .py(px(2.0))
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
                .child(label.to_owned()),
        )
        .into_any_element();
    tree.extend(std::iter::once(row));
    if open {
        for child in children {
            render_tree_node(theme, child, expanded, depth + 1, tree, rendered);
            if *rendered >= MAX_TREE_ROWS {
                return;
            }
        }
    }
}

pub(super) fn render_key_value(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let pairs = node.array_prop("pairs").map(Vec::as_slice).unwrap_or(&[]);
    let shown = clip_table_rows(pairs.len());
    let mut list = div().flex().flex_col().w_full().gap(px(4.0));
    if shown == 0 {
        return list
            .child(caption(theme, "（无键值对）"))
            .into_any_element();
    }
    for pair in pairs.iter().take(shown) {
        let key = pair
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let value = pair.get("value").map(cell_text).unwrap_or_default();
        list = list.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .w_full()
                .py(px(2.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(96.0))
                        .text_size(type_scale.small)
                        .line_height(type_scale.small_line)
                        .text_color(palette.color(palette.label_tertiary))
                        .child(key.to_owned()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(type_scale.secondary)
                        .line_height(type_scale.secondary_line)
                        .text_color(palette.color(palette.label_primary))
                        .child(value),
                ),
        );
    }
    if pairs.len() > shown {
        list = list.child(caption(
            theme,
            format!("（仅渲染前 {shown} 组，共 {} 组）", pairs.len()),
        ));
    }
    list.into_any_element()
}

// --- 媒体 -----------------------------------------------------------------

pub(super) fn render_image(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let attachment = node.str_prop("attachmentId").unwrap_or_default();
    let alt = node.str_prop("alt").unwrap_or_default();
    // v1 不做网络/解码：只画占位框 + alt（spec §6.7 / §10.1，附件由 Host 解析后替换本元素）。
    let mut placeholder = div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(CHART_HEIGHT))
        .gap(px(4.0))
        .rounded(theme.radii.md)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.tag))
        .child(
            div()
                .text_size(px(22.0))
                .line_height(px(26.0))
                .text_color(palette.color(palette.label_caption))
                .child("▢"),
        );
    let label = if alt.is_empty() { "图片" } else { alt };
    placeholder = placeholder.child(
        div()
            .text_size(type_scale.small)
            .line_height(type_scale.small_line)
            .text_color(palette.color(palette.label_secondary))
            .child(label.to_owned()),
    );
    if !attachment.is_empty() {
        placeholder = placeholder.child(
            div()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_caption))
                .child(format!("附件 {attachment}")),
        );
    }
    interactive_base(div(), node, path, Role::Image)
        .aria_label(label)
        .child(placeholder)
        .into_any_element()
}

pub(super) fn render_icon(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let name = node.str_prop("name").unwrap_or_default();
    let glyph = icon_glyph(name);
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_secondary),
    };
    let label = node.a11y_label.clone().unwrap_or_else(|| name.to_owned());
    interactive_base(div(), node, path, Role::Image)
        .flex_none()
        .text_size(px(16.0))
        .line_height(px(18.0))
        .text_color(color)
        .aria_label(label)
        .child(glyph)
        .into_any_element()
}

pub(super) fn render_chart(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let accent = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.business_primary),
    };
    let data = node.array_prop("data").map(Vec::as_slice).unwrap_or(&[]);
    let series = node.array_prop("series").map(Vec::as_slice).unwrap_or(&[]);

    let mut labels: Vec<String> = Vec::new();
    let mut values: Vec<f64> = Vec::new();
    if !data.is_empty() {
        for point in data.iter().take(clip_chart_points(data.len())) {
            labels.push(
                point
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
            values.push(
                point
                    .get("value")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
            );
        }
    } else if let Some(first) = series.first() {
        // 多条 series 只画第一条（降级）；其余只出现在图例里。
        values = series_points(first);
        labels = (1..=values.len())
            .map(|index| format!("#{index}"))
            .collect();
    }

    let mut container = div().flex().flex_col().w_full().gap(px(8.0));
    if values.is_empty() {
        return container
            .child(caption(theme, "（图表无数据）"))
            .into_any_element();
    }
    let body = match node.str_prop("kind").unwrap_or("bars") {
        // 降级：GPUI 此 rev 没有折线绘制原语，line 用细柱近似（见实现说明）。
        "line" => chart_columns(theme, &values, accent, Some(4.0)),
        // 降级：donut 用按比例分段的横条 + 图例表达占比（无弧线绘制原语）。
        "donut" => chart_donut(theme, &values, accent),
        _ => chart_columns(theme, &values, accent, None),
    };
    container = container.child(body);
    container = container.child(chart_legend(theme, &labels, &values));
    container.into_any_element()
}

pub(super) fn series_points(series: &serde_json::Value) -> Vec<f64> {
    let values = series
        .get("values")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    values
        .iter()
        .take(clip_chart_points(values.len()))
        .map(|value| value.as_f64().unwrap_or(0.0))
        .collect()
}

pub(super) fn chart_columns(
    theme: &Theme,
    values: &[f64],
    accent: Rgba,
    bar_width: Option<f32>,
) -> Div {
    let heights = chart_bar_heights(values, CHART_HEIGHT);
    let mut row = div()
        .flex()
        .flex_row()
        .items_end()
        .w_full()
        .h(px(CHART_HEIGHT))
        .gap(px(6.0));
    for height in heights {
        let mut bar = div().h(px(height)).rounded(theme.radii.xs).bg(accent);
        bar = match bar_width {
            Some(width) => bar.w(px(width)),
            None => bar.flex_1().min_w(px(2.0)),
        };
        row = row.child(bar);
    }
    row
}

pub(super) fn chart_donut(theme: &Theme, values: &[f64], accent: Rgba) -> Div {
    let palette = theme.palette;
    let positive: Vec<f64> = values
        .iter()
        .map(|value| {
            if value.is_finite() && *value > 0.0 {
                *value
            } else {
                0.0
            }
        })
        .collect();
    let total: f64 = positive.iter().sum();
    let mut row = div()
        .flex()
        .flex_row()
        .w_full()
        .h(px(18.0))
        .rounded_full()
        .overflow_hidden()
        .bg(palette.color(palette.tag));
    if total <= 0.0 {
        return row;
    }
    for (index, value) in positive.iter().enumerate() {
        let ratio = (value / total) as f32;
        let tint = if index % 2 == 0 {
            accent
        } else {
            accent.opacity(0.55)
        };
        row = row.child(div().flex_grow(ratio).flex_basis(px(0.0)).h_full().bg(tint));
    }
    row
}

pub(super) fn chart_legend(theme: &Theme, labels: &[String], values: &[f64]) -> Div {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let mut legend = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(10.0))
        .w_full();
    for (index, label) in labels.iter().enumerate() {
        let value = values.get(index).copied().unwrap_or(0.0);
        let text = if label.is_empty() {
            format_number(value)
        } else {
            format!("{label} {}", format_number(value))
        };
        legend = legend.child(
            div()
                .flex_none()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_tertiary))
                .child(text),
        );
    }
    legend
}

// --- 容器 -----------------------------------------------------------------
