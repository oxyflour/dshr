use crate::*;

impl ChatApp {
    pub(crate) fn jump_to_turn(&mut self, row: usize, cx: &mut Context<Self>) {
        if !matches!(self.rows.get(row), Some(Row::User { .. })) {
            return;
        }
        self.activity_open = false;
        self.turn_menu_open = false;
        self.initial_scroll_done = true;
        self.follow_transcript = false;
        self.visible_limit = self.visible_limit.max(self.rows.len() - row);
        self.turn_jump = Some(row);
        self.turn_focus = Some(row);
        cx.notify();
    }

    pub(crate) fn turn_rail(
        &self,
        focus_row: usize,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let anchors: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, Row::User { .. }).then_some(index))
            .collect();
        if anchors.is_empty() {
            return None;
        }
        let active = anchors
            .partition_point(|row| *row <= focus_row)
            .saturating_sub(1);
        let p = t.palette;
        Some(
            div()
                .id("turn-rail")
                .aria_label("会话轮次")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(3.0))
                .w(px(22.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .children(turn_rail_window(anchors.len(), active).map(|turn| {
                    let row = anchors[turn];
                    let selected = turn == active;
                    let label = format!("跳到第 {} 轮", turn + 1);
                    let preview: String = turn_preview(&self.rows[row])
                        .unwrap_or_default()
                        .chars()
                        .take(50)
                        .collect();
                    let tooltip = if preview.is_empty() {
                        label.clone()
                    } else {
                        format!("{label} · {preview}")
                    };
                    div()
                        .id(("turn-marker", turn))
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_selected(selected)
                        .focusable()
                        .w_full()
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .pr(px(4.0))
                        .rounded(t.radii.xs)
                        .cursor_pointer()
                        .hover(|style| style.bg(p.color(p.interactive_hover)))
                        .child(
                            div()
                                .w(px(if selected { 14.0 } else { 8.0 }))
                                .h(px(2.0))
                                .rounded_full()
                                .bg(p.color(if selected { p.link } else { p.label_secondary })),
                        )
                        .tooltip(toolbar_tooltip(tooltip, *t))
                        .on_click(cx.listener(move |chat, _, _, cx| chat.jump_to_turn(row, cx)))
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn turn_navigator(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let anchors: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, Row::User { .. }).then_some(index))
            .collect();
        let p = t.palette;
        let mut menu = div()
            .id("turn-navigator")
            .role(Role::ListBox)
            .aria_label("轮次导航")
            .absolute()
            .top(gpui::relative(1.0))
            .right(px(12.0))
            .w(px(312.0))
            .max_w_full()
            .occlude()
            .p(px(5.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm();
        if anchors.is_empty() {
            menu = menu.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(t.type_scale.secondary)
                    .text_color(p.color(p.label_secondary))
                    .child("此会话暂无可跳转的轮次"),
            );
        } else {
            let height = 36.0 * anchors.len().min(6) as f32;
            let theme = *t;
            menu = menu.child(
                gpui::uniform_list(
                    "turn-anchors",
                    anchors.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|turn| {
                                let row = anchors[turn];
                                let preview = turn_preview(this.rows.get(row)?)?;
                                Some(
                                    div()
                                        .id(("turn-anchor", turn))
                                        .role(Role::ListBoxOption)
                                        .aria_label(format!("跳到第 {} 轮 {preview}", turn + 1))
                                        .h(px(36.0))
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .px(px(8.0))
                                        .rounded(theme.radii.sm)
                                        .cursor_pointer()
                                        .text_size(theme.type_scale.secondary)
                                        .text_color(p.color(p.label_secondary))
                                        .hover(|style| style.bg(p.color(p.interactive_hover)))
                                        .children([
                                            div().flex_none().child(format!("第 {} 轮", turn + 1)),
                                            div()
                                                .min_w(px(0.0))
                                                .overflow_hidden()
                                                .text_color(p.color(p.label_primary))
                                                .text_overflow(TextOverflow::Truncate("…".into()))
                                                .child(preview.to_owned()),
                                        ])
                                        .on_click(cx.listener(move |chat, _, _, cx| {
                                            chat.jump_to_turn(row, cx)
                                        })),
                                )
                            })
                            .collect()
                    }),
                )
                .h(px(height))
                .w_full(),
            );
        }
        gpui::deferred(menu).with_priority(1).into_any_element()
    }

    pub(crate) fn header(
        &self,
        t: &Theme,
        collapsed: bool,
        compact: bool,
        show_window_bar: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let hero = self.show_empty_composer();
        let sidebar_label = if collapsed {
            "展开侧栏"
        } else {
            "收起侧栏"
        };
        div()
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .justify_end()
            .gap(px(4.0))
            .px(if compact { px(12.0) } else { px(20.0) })
            .pt(px(6.0))
            .when(!hero, |header| {
                header
                    .min_h(t.layout.header_height)
                    .border_b_1()
                    .border_color(p.color(p.border_l2))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .when(!show_window_bar, |header| {
                        header.child(
                            toolbar_icon_button(
                                "sidebar-toggle",
                                sidebar_label,
                                "\u{e700}",
                                "☰",
                                t,
                            )
                            .aria_expanded(!collapsed)
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.toggle_sidebar(window, cx);
                                },
                            )),
                        )
                    })
                    .when(!hero, |header| {
                        header.child(self.session_heading(t, compact, cx))
                    })
                    .child(div().flex_1().min_w(px(0.0)))
                    .when(self.busy && !compact, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .child("生成中…"),
                        )
                    })
                    .when(!hero, |header| header.child(self.header_actions(t, cx)))
                    .when(!self.right_sidebar_open, |header| {
                        header.child(
                            toolbar_icon_button(
                                "expand-right-sidebar",
                                "展开右侧栏",
                                "\u{e89f}",
                                "▥",
                                t,
                            )
                            .aria_expanded(false)
                            .on_click(cx.listener(|chat, _, _, cx| {
                                chat.header_menu = None;
                                chat.composer_menu = None;
                                chat.workspace_picker_open = false;
                                chat.turn_menu_open = false;
                                chat.right_sidebar_open = true;
                                cx.notify();
                            })),
                        )
                    }),
            )
            .children((!hero).then(|| {
                div().flex().items_center().gap(px(6.0)).children(
                    [
                        ("conversation-tab", "对话", false),
                        ("activity-tab", "轨迹", true),
                    ]
                    .into_iter()
                    .map(|(id, label, activity)| {
                        let selected = self.activity_open == activity;
                        chrome_button(id, label, t)
                            .role(Role::Tab)
                            .aria_selected(selected)
                            .focusable()
                            .h(px(30.0))
                            .rounded(px(0.0))
                            .border_b_2()
                            .border_color(p.color(if selected { p.link } else { 0x0000_0000 }))
                            .when(selected, |tab| {
                                tab.text_color(p.color(p.link))
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activity_open = activity;
                                cx.notify();
                            }))
                    }),
                )
            }))
            .when_some(self.header_notice.as_ref(), |header, (error, notice)| {
                header.child(
                    div()
                        .pb(px(4.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(if *error {
                            p.state_error
                        } else {
                            p.label_secondary
                        }))
                        .child(notice.clone()),
                )
            })
            .when(self.turn_menu_open, |header| {
                header.child(self.turn_navigator(t, cx))
            })
            .into_any_element()
    }
}
