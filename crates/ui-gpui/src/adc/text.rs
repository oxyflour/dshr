use super::*;

pub(super) fn render_text(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let metrics = text_metrics(theme, node.str_prop("style"));
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_primary),
    };
    let text = truncate_text(node.str_prop("value").unwrap_or_default()).to_owned();
    let mut element = text_block(text, metrics, color).w_full();
    if metrics.mono {
        element = element.font(code_font(theme));
    }
    if let Some(lines) = max_lines(node) {
        element = element.line_clamp(lines);
    }
    element.into_any_element()
}

pub(super) fn render_markdown(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let source = truncate_text(node.str_prop("source").unwrap_or_default());
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_primary),
    };
    let mut element = div()
        .w_full()
        .child(markdown_view(source, theme, &Font::default(), color));
    // 降级：`maxLines` 按文本块逐块 line_clamp（Markdown 是多块元素，无法整段裁剪）。
    if let Some(lines) = max_lines(node) {
        element = element.line_clamp(lines);
    }
    element.into_any_element()
}

pub(super) fn render_code(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let source = truncate_text(node.str_prop("source").unwrap_or_default());
    let show_numbers = node.bool_prop("lineNumbers").unwrap_or(true);
    let mut container = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(theme.radii.md)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.code_block))
        .overflow_hidden();
    if let Some(language) = node.str_prop("language").filter(|value| !value.is_empty()) {
        container = container.child(
            div()
                .flex_none()
                .w_full()
                .px(px(10.0))
                .py(px(3.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .bg(palette.color(palette.code_block_banner))
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_tertiary))
                .child(language.to_owned()),
        );
    }
    // 横向滚动容器必须带 id（GPUI 用 id 维持滚动状态）。
    let mut body = div()
        .id(format!("adc-code-{path}"))
        .flex()
        .flex_col()
        .w_full()
        .p(px(8.0))
        .overflow_x_scroll()
        .overflow_y_hidden();
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().take(MAX_CODE_LINES).enumerate() {
        let mut row = div()
            .flex()
            .flex_row()
            .items_start()
            .w_full()
            .whitespace_nowrap();
        if show_numbers {
            row = row.child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .text_align(TextAlign::Right)
                    .pr(px(8.0))
                    .font(code_font(theme))
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_caption))
                    .child((index + 1).to_string()),
            );
        }
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .font(code_font(theme))
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_primary))
                .child(if line.is_empty() {
                    " ".to_owned()
                } else {
                    (*line).to_owned()
                }),
        );
        body = body.child(row);
    }
    let total = lines.len();
    if total > MAX_CODE_LINES {
        body = body.child(caption(
            theme,
            format!("（仅渲染前 {MAX_CODE_LINES} 行，共 {total} 行）"),
        ));
    }
    container.child(body).into_any_element()
}

pub(super) fn render_badge(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let tone = node.tone.map(|tone| tone.as_str());
    let foreground = tone_color(&palette, tone);
    let background = if tone.is_some() {
        foreground.opacity(0.12)
    } else {
        palette.color(palette.tag)
    };
    div()
        .flex_none()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(theme.radii.xs)
        .bg(background)
        .text_size(type_scale.small)
        .line_height(type_scale.small_line)
        .text_color(if tone.is_some() {
            foreground
        } else {
            palette.color(palette.label_secondary)
        })
        .child(truncate_text(node.str_prop("label").unwrap_or_default()).to_owned())
        .into_any_element()
}

// --- 交互（只画视觉，不接事件） -------------------------------------------
