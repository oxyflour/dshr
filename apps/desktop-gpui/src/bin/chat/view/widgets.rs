use crate::*;

pub(crate) fn chrome_button(
    id: impl Into<ElementId>,
    label: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    let p = theme.palette;
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(theme.radii.sm)
        .cursor_pointer()
        .text_size(theme.type_scale.secondary)
        .text_color(p.color(p.label_secondary))
        .hover(|style| style.bg(p.color(p.interactive_hover)))
        .child(label)
}

pub(crate) fn desktop_icon(windows_glyph: &'static str, fallback: &'static str) -> Div {
    div()
        .flex_none()
        .text_size(px(14.0))
        .line_height(px(18.0))
        .when(cfg!(windows), |icon| icon.font_family("Segoe MDL2 Assets"))
        .child(if cfg!(windows) {
            windows_glyph
        } else {
            fallback
        })
}

struct ToolbarTooltip {
    label: gpui::SharedString,
    theme: Theme,
}

impl Render for ToolbarTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        let p = t.palette;
        div()
            .max_w(px(360.0))
            .rounded(t.radii.sm)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .text_color(p.color(p.label_primary))
            .text_size(t.type_scale.small)
            .px(px(8.0))
            .py(px(4.0))
            .shadow_sm()
            .child(self.label.clone())
    }
}

pub(crate) fn toolbar_tooltip(
    label: impl Into<gpui::SharedString>,
    theme: Theme,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let label = label.into();
    move |_, cx| {
        cx.new(|_| ToolbarTooltip {
            label: label.clone(),
            theme,
        })
        .into()
    }
}

pub(crate) fn toolbar_icon_button(
    id: impl Into<ElementId>,
    label: &'static str,
    windows_glyph: &'static str,
    fallback: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    chrome_button(id, "", theme)
        .aria_label(label)
        .focusable()
        .size(px(28.0))
        .px(px(0.0))
        .child(desktop_icon(windows_glyph, fallback))
        .tooltip(toolbar_tooltip(label, *theme))
}

pub(crate) fn attachment_image_button(
    id: impl Into<ElementId>,
    name: &str,
    image: Option<Arc<RenderImage>>,
    fallback: &'static str,
    width: f32,
    height: f32,
    t: &Theme,
) -> Stateful<Div> {
    let p = t.palette;
    div()
        .id(id)
        .role(Role::Button)
        .focusable()
        .aria_label(format!("预览图片 {name}"))
        .w(px(width))
        .h(px(height))
        .max_w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(t.radii.sm)
        .bg(p.color(p.tag))
        .cursor_pointer()
        .child(match image {
            Some(image) => img(image)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .text_size(t.type_scale.small)
                .text_color(p.color(p.label_secondary))
                .child(fallback)
                .into_any_element(),
        })
}

pub(crate) fn stored_image(file: &SpillRef) -> bool {
    AttachmentFormat::from_mime(&file.mime).is_some_and(|format| format != AttachmentFormat::Pdf)
}

pub(crate) fn file_size_label(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn composer_option(
    id: impl Into<ElementId>,
    label: &str,
    description: &str,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let p = theme.palette;
    div()
        .id(id)
        .role(Role::MenuItemRadio)
        .aria_label(label.to_owned())
        .aria_selected(selected)
        .flex()
        .flex_col()
        .px(px(9.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .cursor_pointer()
        .hover(|style| style.bg(p.color(p.interactive_hover)))
        .when(selected, |row| row.bg(p.color(p.business_tertiary)))
        .child(
            div()
                .text_size(theme.type_scale.secondary)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.color(if selected { p.link } else { p.label_primary }))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(theme.type_scale.small)
                .text_color(p.color(p.label_secondary))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(description.to_owned()),
        )
}

/// 侧栏的相对时间（不引入日期库）。
pub(crate) fn relative_time(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(updated_at);
    if diff < 60 {
        String::from("刚刚")
    } else if diff < 3_600 {
        format!("{} 分钟前", diff / 60)
    } else if diff < 86_400 {
        format!("{} 小时前", diff / 3_600)
    } else {
        format!("{} 天前", diff / 86_400)
    }
}
