use crate::*;

impl ChatApp {
    /// 键盘审批：处理当前第一个待确认调用；没有 pending 时返回 false。
    pub(crate) fn resolve_first_approval(
        &mut self,
        outcome: ApprovalOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(first) = self.approvals.pending().first().cloned() else {
            return false;
        };
        if self.approval_saving.as_ref() == Some(&first) {
            return false;
        }
        self.approvals.resolve(&first.call_id, outcome);
        self.approval_error = None;
        self.pending_approvals = self.approvals.pending().len();
        cx.notify();
        true
    }

    // ------------------------------------------------------------------
    // 键盘动作（M4 W2）：处理函数只做状态变更，键位映射见 `chat_key_bindings()`。
    // ------------------------------------------------------------------

    /// 工具审批卡片：工作线程阻塞在这里，等待"允许一次 / 拒绝"。
    pub(crate) fn approval_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.approvals.pending();
        let first = pending.first()?;
        let p = t.palette;
        let call_id = first.call_id.clone();
        let is_trial = first.tool == creator::TRY_PLUGIN_TOOL;
        let is_build = first.tool == creator::BUILD_PLUGIN_TOOL;
        let preview = if is_trial {
            creator::approval_preview(&first.arguments)
        } else if is_build {
            format!(
                "{}\n构建入口 {}",
                first.arguments["operation"].as_str().unwrap_or_default(),
                first.arguments["entry"].as_str().unwrap_or_default(),
            )
        } else {
            let preview = serde_json::to_string_pretty(&first.arguments).unwrap_or_default();
            if preview.chars().count() > 260 {
                let mut text: String = preview.chars().take(260).collect();
                text.push('…');
                text
            } else {
                preview
            }
        };
        let call_id_allow = call_id.clone();
        let call_id_deny = call_id.clone();
        let approvals_allow = self.approvals.clone();
        let approvals_deny = self.approvals.clone();
        let can_remember = first.supports_always_allow();
        let saving = self.approval_saving.as_ref() == Some(first);
        let remember_request = first.clone();
        let approval_error = self
            .approval_error
            .as_ref()
            .filter(|(request, _)| request == first)
            .map(|(_, error)| error.clone());
        Some(
            div()
                .flex()
                .flex_col()
                .items_center()
                .px(px(16.0))
                .pb(px(6.0))
                .child(
                    div()
                        .w_full()
                        .max_w(t.layout.composer_max_width)
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded_full()
                                        .bg(p.color(p.state_warn)),
                                )
                                .child(
                                    div()
                                        .text_size(t.type_scale.secondary)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .child(if is_trial {
                                            "允许在当前会话试用插件？".to_owned()
                                        } else if is_build {
                                            "允许构建插件？".to_owned()
                                        } else {
                                            format!("工具调用需要确认：{}", first.tool)
                                        }),
                                )
                                .child(div().flex_1())
                                .when(pending.len() > 1, |row| {
                                    row.child(
                                        div()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_caption))
                                            .child(format!("另有 {} 个待确认", pending.len() - 1)),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .max_h(px(if is_trial { 136.0 } else { 80.0 }))
                                .overflow_hidden()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(preview),
                        )
                        .when_some(approval_error, |card, error| {
                            card.child(div().text_size(t.type_scale.small).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .when(saving, |row| row.opacity(0.5))
                                .child(
                                    div()
                                        .id("approval-deny")
                                        .role(Role::Button)
                                        .focusable()
                                        .aria_label("拒绝这次工具调用")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .border_1()
                                        .border_color(p.color(p.border_l2))
                                        .bg(p.color(p.button_elevated))
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_primary))
                                        .child("拒绝")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if this.approval_saving.as_ref().is_some_and(
                                                |request| request.call_id == call_id_deny,
                                            ) {
                                                return;
                                            }
                                            approvals_deny
                                                .resolve(&call_id_deny, ApprovalOutcome::Deny);
                                            this.approval_error = None;
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .id("approval-allow")
                                        .role(Role::Button)
                                        .focusable()
                                        .aria_label("允许一次工具调用")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .bg(p.color(if can_remember {
                                            p.button_elevated
                                        } else {
                                            p.button_info
                                        }))
                                        .when(can_remember, |button| {
                                            button.border_1().border_color(p.color(p.border_l2))
                                        })
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(if can_remember {
                                            p.label_primary
                                        } else {
                                            p.bg_base
                                        }))
                                        .child("允许一次")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if this.approval_saving.as_ref().is_some_and(
                                                |request| request.call_id == call_id_allow,
                                            ) {
                                                return;
                                            }
                                            approvals_allow.resolve(
                                                &call_id_allow,
                                                ApprovalOutcome::AllowOnce,
                                            );
                                            this.approval_error = None;
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                )
                                .when(can_remember, |actions| {
                                    actions.child(
                                        div()
                                            .id("approval-always")
                                            .role(Role::Button)
                                            .focusable()
                                            .aria_label("始终允许当前会话中的此工具")
                                            .px(px(12.0))
                                            .py(px(5.0))
                                            .rounded(t.radii.sm)
                                            .bg(p.color(p.button_info))
                                            .cursor_pointer()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.bg_base))
                                            .child(if saving {
                                                "正在保存…"
                                            } else {
                                                "始终允许"
                                            })
                                            .tooltip(toolbar_tooltip(
                                                "仅对当前会话中的此工具生效",
                                                *t,
                                            ))
                                            .on_click(cx.listener(move |chat, _, _, cx| {
                                                chat.allow_tool_always(
                                                    remember_request.clone(),
                                                    cx,
                                                );
                                            })),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}
