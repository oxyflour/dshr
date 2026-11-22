use crate::*;

impl ChatApp {
    pub(crate) fn render_row(
        &self,
        row: &Row,
        index: usize,
        t: &Theme,
        base_font: &Font,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        match row {
            Row::User { text, attachments } => div()
                .group("message-row")
                .flex()
                .flex_col()
                .items_end()
                .gap(px(2.0))
                .when(!attachments.is_empty(), |row| {
                    row.child(self.message_attachments(attachments, index, t, window, cx))
                })
                .when(!text.is_empty(), |row| {
                    let text_view = gpui::SharedString::from(text.clone());
                    row.child(
                        div()
                            .max_w(gpui::relative(0.702))
                            .px(px(14.0))
                            .py(px(8.0))
                            .rounded(t.radii.lg)
                            .bg(p.color(p.bubble))
                            .text_size(t.type_scale.body)
                            .line_height(t.type_scale.body_line)
                            .text_color(p.color(p.label_primary))
                            .child(self.message_selection.fragment(
                                index,
                                0,
                                text_view.clone(),
                                gpui::StyledText::new(text_view),
                                p.color(p.business_primary).into(),
                            )),
                    )
                    .child(self.copy_message_button(index, text, t, cx))
                })
                .into_any_element(),
            Row::Assistant(text) | Row::Streaming(text) => {
                let streaming = is_streaming_row(row);
                div()
                    .group("message-row")
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .gap(px(4.0))
                    .child(selectable_markdown_view(
                        text,
                        t,
                        base_font,
                        p.color(p.label_primary),
                        &self.message_selection,
                        index,
                    ))
                    .when(streaming && !self.cancel.load(Ordering::Relaxed), |el| {
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_caption))
                                .child("正在生成"),
                        )
                    })
                    .when(!streaming && !text.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .child(self.copy_message_button(index, text, t, cx)),
                        )
                    })
                    .into_any_element()
            }
            Row::Process(steps) => {
                let open = self.open_processes.contains(&index);
                let tools = steps
                    .iter()
                    .filter(|row| matches!(row, Row::Tool { .. }))
                    .count();
                let thinking = steps.iter().any(|row| matches!(row, Row::Reasoning { .. }));
                let keep_visible = |row: &Row| {
                    self.inline_trial_call(row).is_some()
                        || matches!(row, Row::Tool { delivered_files, .. } if !delivered_files.is_empty())
                };
                let active = steps.iter().any(|row| {
                    matches!(
                        row,
                        Row::Tool { pending: true, .. }
                            | Row::Reasoning {
                                streaming: true,
                                ..
                            }
                    )
                });
                let label = match (active, thinking, tools) {
                    (true, true, 0) => "正在思考".to_owned(),
                    (_, true, 0) => "思考过程".to_owned(),
                    (true, _, _) => format!("进行中 · {tools} 次工具调用"),
                    (_, true, _) => format!("思考过程 · {tools} 次工具调用"),
                    _ => format!("{tools} 次工具调用"),
                };
                div()
                    .id(("turn-process", index))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("process-toggle")
                            .role(Role::Button)
                            .aria_expanded(open)
                            .focusable()
                            .cursor_pointer()
                            .w_full()
                            .h(px(26.0))
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .rounded(t.radii.sm)
                            .px(px(4.0))
                            .text_size(t.type_scale.secondary)
                            .text_color(p.color(p.label_secondary))
                            .hover(|style| style.bg(p.color(p.interactive_hover)))
                            .child(label)
                            .child(
                                desktop_icon(
                                    if open { "\u{e70e}" } else { "\u{e70d}" },
                                    if open { "▴" } else { "▾" },
                                )
                                .text_size(px(10.0)),
                            )
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                if !chat.open_processes.remove(&index) {
                                    chat.open_processes.insert(index);
                                }
                                cx.notify();
                            })),
                    )
                    .when(open || steps.iter().any(keep_visible), |group| {
                        group.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .when(open, |steps| {
                                    steps
                                        .pl(px(10.0))
                                        .border_l_1()
                                        .border_color(p.color(p.border_l2))
                                })
                                .children(
                                    steps.iter().filter(|row| open || keep_visible(row)).map(
                                        |row| self.render_row(row, index, t, base_font, window, cx),
                                    ),
                                ),
                        )
                    })
                    .into_any_element()
            }
            Row::Reasoning { text, .. } => div()
                .text_size(t.type_scale.secondary)
                .line_height(t.type_scale.body_line)
                .text_color(p.color(p.label_secondary))
                .child(text.clone())
                .into_any_element(),
            Row::Tool {
                call_seq,
                name,
                origin,
                status,
                detail,
                is_error,
                pending,
                card,
                delivered_files,
            } => {
                let trial_call = self.inline_trial_call(row);
                let expanded = !self.collapsed_trial_cards.contains(call_seq);
                let dot = if *pending {
                    p.state_warn
                } else if *is_error {
                    p.state_error
                } else {
                    p.state_success
                };
                let status_color = if *is_error {
                    p.state_error
                } else {
                    p.label_caption
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(p.color(dot)))
                            .child(
                                div()
                                    .text_size(t.type_scale.secondary)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .child(if trial_call.is_some() {
                                        "试用插件".into()
                                    } else {
                                        name.clone()
                                    }),
                            )
                            .when(trial_call.is_none(), |header| {
                                header.child(
                                    div()
                                        .px(px(6.0))
                                        .py(px(1.0))
                                        .rounded(t.radii.xs)
                                        .bg(p.color(p.tag))
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_tertiary))
                                        .child(origin.clone()),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(status_color))
                                    .child(status.clone()),
                            )
                            .when_some(trial_call, |header, seq| {
                                let label = if expanded {
                                    "收起面板"
                                } else {
                                    "展开面板"
                                };
                                header.child(
                                    chrome_button(("toggle-trial-panel", seq), label, t)
                                        .aria_expanded(expanded)
                                        .focusable()
                                        .on_click(cx.listener(move |chat, _, _, cx| {
                                            if !chat.collapsed_trial_cards.remove(&seq) {
                                                chat.collapsed_trial_cards.insert(seq);
                                            }
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .when(!detail.is_empty() && trial_call.is_none(), |el| {
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(detail.clone()),
                        )
                    })
                    // 有专用视图的工具结果：diff / terminal 用原生组件渲染正文，
                    // 不再只有一行摘要（正文已按卡片上限截断）。
                    .when_some(
                        card.as_ref().filter(|card| {
                            matches!(card.kind, CardKind::Diff | CardKind::Terminal)
                                && !card.body.is_empty()
                        }),
                        |el, card| {
                            let view = match card.kind {
                                CardKind::Diff => diff_view_from_text(t, &card.body),
                                _ => terminal_view_from_text(t, &card.body),
                            };
                            el.child(view)
                        },
                    )
                    // M4 W3：工具结果 `meta.ui` 的受限 ADC 附加区（tool card extra）。
                    .when_some(
                        card.as_ref().and_then(|card| card.extra.as_ref()),
                        |el, extra| {
                            let (body, _diagnostics) = adc_panel_body(t, extra);
                            el.child(body)
                        },
                    )
                    .children(
                        delivered_files
                            .iter()
                            .enumerate()
                            .map(|(file_index, file)| {
                                let path = file.path.clone();
                                let label = file
                                    .path
                                    .file_name()
                                    .unwrap_or(file.path.as_os_str())
                                    .to_string_lossy()
                                    .into_owned();
                                let button_id: gpui::SharedString =
                                    format!("delivered-file-{call_seq}").into();
                                chrome_button((button_id, file_index), "", t)
                                    .aria_label(format!("打开 {label}"))
                                    .child(label)
                                    .focusable()
                                    .when_some(file.description.clone(), |button, description| {
                                        button.child(
                                            div()
                                                .text_color(p.color(p.label_tertiary))
                                                .child(description),
                                        )
                                    })
                                    .on_click(cx.listener(move |chat, _, _, cx| {
                                        match dsh_deliverables::checked_file(
                                            &chat.active_workspace,
                                            &path,
                                        ) {
                                            Ok((path, _)) => cx.open_with_system(&path),
                                            Err(_) => {
                                                chat.workspace_notice =
                                                    Some("交付文件已不可用".to_owned());
                                                cx.notify();
                                            }
                                        }
                                    }))
                            }),
                    )
                    .when_some(trial_call.filter(|_| expanded), |el, seq| {
                        el.child(
                            div()
                                .id(("trial-panel-content", seq))
                                .max_h(trial_panel_max_height(window))
                                .overflow_y_scroll()
                                .child(self.plugin_panels(Some(seq), t, cx)),
                        )
                    })
                    .when_some(trial_call.filter(|_| !expanded), |el, seq| {
                        let titles = self
                            .adc_panels
                            .iter()
                            .filter(|panel| panel.trial_call_seq == Some(seq))
                            .map(|panel| panel.title.as_str())
                            .collect::<Vec<_>>()
                            .join("、");
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .text_overflow(TextOverflow::Truncate("…".into()))
                                .child(titles),
                        )
                    })
                    .into_any_element()
            }
            Row::Meta(text) => div()
                .flex()
                .items_center()
                .justify_center()
                .py(px(4.0))
                .child(
                    div()
                        .px(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child(text.clone()),
                )
                .into_any_element(),
            Row::Stopped => div()
                .text_size(t.type_scale.secondary)
                .text_color(p.color(p.label_secondary))
                .child("已停止")
                .into_any_element(),
        }
    }

    pub(crate) fn transcript(
        &mut self,
        t: &Theme,
        show_turn_rail: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let base_font = window.text_style().font();
        self.message_selection.begin_frame(&self.session_id);
        let jump = (!self.activity_open)
            .then(|| self.turn_jump.take())
            .flatten();
        // 不 clone 整份 rows：10k 会话下每帧克隆 7500 个 String 是纯浪费。
        let activity = self.activity_open.then(|| {
            self.rows
                .iter()
                .flat_map(|row| match row {
                    Row::Process(steps) => steps.as_slice(),
                    row => std::slice::from_ref(row),
                })
                .filter(|row| matches!(row, Row::Tool { .. } | Row::Meta(_) | Row::Stopped))
                .collect::<Vec<_>>()
        });
        let total = activity.as_ref().map_or(self.rows.len(), Vec::len);
        let row_at = |index| match &activity {
            Some(rows) => rows[index],
            None => &self.rows[index],
        };
        let scroll = if self.activity_open {
            &self.activity_scroll
        } else {
            &self.scroll
        };
        let column_width = if self.activity_open {
            t.layout.content_width
        } else {
            t.layout.composer_max_width - px(32.0)
        };
        let row_gap = if self.activity_open { 16.0 } else { 14.0 };
        let item = || div().flex_none().w_full().max_w(column_width).mx_auto();
        let mut pane = div()
            .id(if self.activity_open {
                "activity-scroll"
            } else {
                "transcript-scroll"
            })
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px(px(24.0))
            .gap(px(row_gap))
            .on_scroll(
                cx.listener(|this, _event: &gpui::ScrollEvent, _window, cx| {
                    if !this.activity_open {
                        this.follow_transcript = false;
                        this.turn_focus = None;
                    }
                    cx.notify();
                }),
            )
            .py(px(20.0))
            .when(!self.activity_open, |pane| {
                self.message_selection.bind(pane)
            });
        if self.activity_open && total == 0 {
            pane = pane.child(
                item()
                    .py(px(28.0))
                    .text_size(t.type_scale.body)
                    .text_color(p.color(p.label_secondary))
                    .child("此会话暂无工具调用或上下文压缩记录。"),
            );
        }
        let start = total.saturating_sub(self.visible_limit);
        if start > 0 {
            let hidden = start;
            pane = pane.child(
                item()
                    .id("load-earlier")
                    .w_full()
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_secondary))
                    .child(format!("载入更早的 {hidden} 条（每次 200 条）"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.visible_limit = this.visible_limit.saturating_add(200);
                        cx.notify();
                    })),
            );
        }
        // 变高行虚拟化：只构造/排版视口附近的几十行，用上下占位块维持滚动高度。
        // 实测 400 行全量渲染时 prepaint 要 125ms（debug），滚动只有 ~6fps；虚拟化后
        // 帧成本与历史长度基本无关，这是 Gate 2 "滚动帧率不低于基线" 的实现基础。
        let visible = total - start;
        let mut cumulative = Vec::with_capacity(visible + 1);
        cumulative.push(0.0f32);
        for index in 0..visible {
            let gap = if index == 0 { 0.0 } else { row_gap };
            let row_index = start + index;
            let height = match row_at(row_index) {
                Row::Process(steps) => {
                    let open = self.open_processes.contains(&row_index);
                    26.0 + steps
                        .iter()
                        .filter_map(|step| {
                            if let Some(seq) = self.inline_trial_call(step) {
                                let panel_height = if self.collapsed_trial_cards.contains(&seq) {
                                    0.0
                                } else {
                                    let count = self
                                        .adc_panels
                                        .iter()
                                        .filter(|panel| panel.trial_call_seq == Some(seq))
                                        .count();
                                    (count as f32 * 130.0)
                                        .min(trial_panel_max_height(window).as_f32())
                                };
                                Some(52.0 + panel_height)
                            } else {
                                open.then(|| estimated_row_height(step) + 6.0)
                            }
                        })
                        .sum::<f32>()
                }
                row => estimated_row_height(row),
            };
            let next = cumulative[index] + height + gap;
            cumulative.push(next);
        }
        let total_height = cumulative[visible];
        let viewport_height = scroll.bounds().size.height.as_f32().max(240.0);
        let jump = jump
            .and_then(|row| row.checked_sub(start))
            .filter(|row| *row < visible);
        let offset = if let Some(target) = jump {
            let approx = cumulative[target].min((total_height - viewport_height).max(0.0));
            scroll.set_offset(point(px(0.0), px(-approx)));
            approx
        } else {
            (-scroll.offset().y.as_f32()).clamp(0.0, total_height)
        };
        const OVERSCAN: f32 = 240.0;
        let top_edge = (offset - OVERSCAN).max(0.0);
        let bottom_edge = (offset + viewport_height + OVERSCAN).min(total_height);
        let mut first = cumulative
            .partition_point(|edge| *edge < top_edge)
            .saturating_sub(1)
            .min(visible.saturating_sub(1));
        let mut last = cumulative.partition_point(|edge| *edge < bottom_edge);
        if last <= first {
            last = (first + 1).min(visible);
        }
        if let Some(target) = jump {
            first = first.min(target);
            last = last.max(target + 1);
        }
        let top_spacer = (cumulative[first] - row_gap).max(0.0);
        let bottom_spacer = (total_height - cumulative[last] - row_gap).max(0.0);
        if top_spacer > 0.5 {
            pane = pane.child(item().h(px(top_spacer)));
        }
        if let Some(target) = jump {
            let prefix = usize::from(start > 0) + usize::from(top_spacer > 0.5);
            scroll.scroll_to_top_of_item(prefix + target - first);
        }
        for index in first..last {
            let row_index = start + index;
            pane = pane.child(item().child(self.render_row(
                row_at(row_index),
                row_index,
                t,
                &base_font,
                window,
                cx,
            )));
        }
        if bottom_spacer > 0.5 {
            pane = pane.child(item().h(px(bottom_spacer)));
        }
        if !show_turn_rail {
            return pane.into_any_element();
        }
        let focus_row = if let Some(row) = self.turn_focus.filter(|row| *row < total) {
            row
        } else if self.follow_transcript || offset + viewport_height >= total_height - 1.0 {
            total.saturating_sub(1)
        } else {
            let focus_edge = offset + viewport_height * 0.25;
            start
                + cumulative
                    .partition_point(|edge| *edge <= focus_edge)
                    .saturating_sub(1)
        };
        div()
            .relative()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .child(pane)
            .children(self.turn_rail(focus_row, t, cx))
            .into_any_element()
    }
}

impl ChatState {
    /// 将会话运行事件转换为界面行快照，供桌面和需要行投影的诊断使用。
    pub(crate) fn stream_turn(
        &mut self,
        text: &str,
        emit: &mut dyn FnMut(Vec<Row>),
    ) -> Option<String> {
        self.stream_turn_with_attachments(text, &[], emit)
    }

    pub(crate) fn stream_turn_with_attachments(
        &mut self,
        text: &str,
        attachments: &[DraftAttachment],
        emit: &mut dyn FnMut(Vec<Row>),
    ) -> Option<String> {
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return None;
        }
        match self.run_turn(text, attachments, &mut |update| {
            if let TurnUpdate::Preview(log, streaming, reasoning) = update {
                emit(rows_with_preview(log, streaming, reasoning));
            }
        }) {
            Ok(result) => Some(result.reply),
            Err(error) => {
                let mut rows = rows_snapshot(&self.log, None);
                rows.push(Row::Meta(format!("消息未发送：{error}")));
                emit(rows);
                None
            }
        }
    }
}
