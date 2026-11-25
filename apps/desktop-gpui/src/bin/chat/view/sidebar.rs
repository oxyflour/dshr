use crate::*;

impl ChatApp {
    pub(crate) fn session_list(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let query = self.session_search.read(cx).content().trim().to_lowercase();
        let groups = workspace_sessions(
            &self.sessions,
            &self.session_depths,
            self.show_archived,
            &query,
        );
        let active_root = self.session_branch().first();
        let mut list = div().flex().flex_col().gap(px(8.0)).w_full();
        if groups.is_empty() {
            return list
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_secondary))
                        .child(if self.show_archived && query.is_empty() {
                            "暂无已归档会话"
                        } else {
                            "没有匹配的会话"
                        }),
                )
                .into_any_element();
        }
        for (
            group_index,
            WorkspaceSessions {
                workspace,
                sessions,
            },
        ) in groups.into_iter().enumerate()
        {
            let label = workspace.map_or("未分组会话", workspace_name);
            let key = workspace.unwrap_or("").to_owned();
            let fold_key = key.clone();
            let folded = query.is_empty() && self.folded_workspaces.contains(&key);
            let path = workspace.map(|workspace| workspace_root().join(workspace));
            let mut group = div().flex().flex_col().gap(px(2.0)).child(
                div()
                    .group("workspace-row")
                    .flex()
                    .items_center()
                    .pr(px(4.0))
                    .rounded(t.radii.sm)
                    .hover(|style| style.bg(p.color(p.interactive_hover)))
                    .child(
                        chrome_button(("workspace-fold", group_index), "", t)
                            .aria_label(label.to_owned())
                            .aria_expanded(!folded)
                            .focusable()
                            .flex_1()
                            .min_w(px(0.0))
                            .h(px(34.0))
                            .justify_start()
                            .gap(px(6.0))
                            .child(desktop_icon(
                                if folded { "\u{e8b7}" } else { "\u{e838}" },
                                "▱",
                            ))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(label.to_owned()),
                            )
                            .tooltip(toolbar_tooltip(workspace.unwrap_or(label).to_owned(), *t))
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                if !chat.folded_workspaces.remove(&fold_key) {
                                    chat.folded_workspaces.insert(fold_key.clone());
                                    chat.workspace_session_limits.remove(&fold_key);
                                }
                                cx.notify();
                            })),
                    )
                    .when_some(path, |row, path| {
                        row.child(
                            toolbar_icon_button(
                                ("workspace-new-session", group_index),
                                "在此工作区新建会话",
                                "\u{e710}",
                                "+",
                                t,
                            )
                            .invisible()
                            .group_hover("workspace-row", |style| style.visible())
                            .on_click(cx.listener(
                                move |chat, _, window, cx| {
                                    chat.new_session_in(path.clone(), window, cx);
                                },
                            )),
                        )
                    }),
            );
            if !folded {
                let total = sessions.len();
                let selected = sessions
                    .iter()
                    .take(WORKSPACE_SESSION_AUTO_LIMIT)
                    .position(|(_, session)| active_root.is_some_and(|root| root.id == session.id));
                let base_limit = WORKSPACE_SESSION_PAGE.max(selected.map_or(0, |index| index + 1));
                let limit = if query.is_empty() {
                    base_limit.max(
                        self.workspace_session_limits
                            .get(&key)
                            .copied()
                            .unwrap_or(0),
                    )
                } else {
                    total
                };
                for (index, session) in sessions.into_iter().take(limit) {
                    group = group.child(div().pl(px(12.0)).child(self.session_row(
                        index,
                        session,
                        active_root.is_some_and(|root| root.id == session.id),
                        t,
                        cx,
                    )));
                }
                if query.is_empty() && total > base_limit {
                    let remaining = total.saturating_sub(limit);
                    let label = if remaining == 0 {
                        "收起".to_owned()
                    } else {
                        format!("展开其余 {remaining} 个会话")
                    };
                    group = group.child(
                        chrome_button(("workspace-more-sessions", group_index), "", t)
                            .aria_label(label.clone())
                            .aria_expanded(remaining == 0)
                            .focusable()
                            .justify_start()
                            .pl(px(22.0))
                            .text_color(p.color(p.label_caption))
                            .child(label)
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                chat.session_menu = None;
                                if remaining == 0 {
                                    chat.workspace_session_limits.remove(&key);
                                } else {
                                    chat.workspace_session_limits.insert(
                                        key.clone(),
                                        (limit + WORKSPACE_SESSION_PAGE).min(total),
                                    );
                                }
                                cx.notify();
                            })),
                    );
                }
            }
            list = list.child(group);
        }
        list.into_any_element()
    }

    pub(crate) fn session_menu_trigger(
        &self,
        index: usize,
        session: &SessionMeta,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let open = self
            .session_menu
            .as_ref()
            .filter(|menu| menu.session_id == session.id);
        let id = session.id.clone();
        let trigger = chrome_button(("session-actions", index), "", t)
            .aria_label("会话操作")
            .focusable()
            .size(px(24.0))
            .px(px(0.0))
            .child(desktop_icon("\u{e712}", "⋯"))
            .aria_expanded(open.is_some())
            .when(open.is_none(), |button| {
                button
                    .invisible()
                    .group_hover("session-row", |style| style.visible())
                    .focus_visible(|style| style.visible())
            })
            .on_click(cx.listener(move |chat, _, _, cx| {
                cx.stop_propagation();
                let same_button = chat
                    .session_menu
                    .as_ref()
                    .is_some_and(|menu| menu.session_id == id && menu.position.is_none());
                chat.session_menu = (!same_button).then(|| SessionMenu {
                    session_id: id.clone(),
                    position: None,
                });
                cx.notify();
            }));
        let Some(open) = open else {
            return trigger.into_any_element();
        };

        let rename_id = session.id.clone();
        let delete_id = session.id.clone();
        let archive_id = session.id.clone();
        let archived = session.archived;
        let menu = div()
            .id("session-menu")
            .role(Role::Menu)
            .aria_label("会话操作")
            .occlude()
            .flex()
            .flex_col()
            .w(px(152.0))
            .p(px(4.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm()
            .child(
                chrome_button("session-menu-rename", "重命名", t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.start_rename(rename_id.clone(), window, cx);
                    })),
            )
            .child(
                chrome_button("session-menu-archive", archive_label(archived), t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.set_session_archived(&archive_id, !archived, window, cx);
                    })),
            )
            .child(
                chrome_button("session-menu-delete", "删除", t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .text_color(p.color(p.state_error))
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.delete_session(delete_id.clone(), window, cx);
                    })),
            );
        let dismiss = cx.listener(|chat, _: &gpui::MouseDownEvent, _, cx| {
            chat.session_menu = None;
            cx.notify();
        });
        if let Some(position) = open.position {
            div()
                .child(trigger)
                .child(popover::at_point(
                    position,
                    popover::dismiss_on_outside(menu, |_| false, dismiss),
                ))
                .into_any_element()
        } else {
            popover::attach_dismissible(
                trigger,
                Some(menu),
                gpui::Anchor::BottomLeft,
                gpui::Anchor::TopLeft,
                dismiss,
            )
        }
    }

    pub(crate) fn session_row(
        &self,
        index: usize,
        session: &SessionMeta,
        is_active: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let is_renaming = self.renaming.as_deref() == Some(session.id.as_str());

        if is_renaming {
            let input = self.rename_input.clone();
            let row = div()
                .id(("rename-row", index))
                .w_full()
                .px(px(8.0))
                .py(px(5.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .rounded(t.radii.sm)
                .bg(p.color(p.tag))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_primary))
                        .children(input),
                )
                .child(
                    div()
                        .id(("rename-ok", index))
                        .role(Role::Button)
                        .flex_none()
                        .aria_label("保存会话名称")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_success))
                        .child("✓")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            cx.stop_propagation();
                            this.commit_rename(cx);
                        })),
                )
                .child(
                    div()
                        .id(("rename-cancel", index))
                        .role(Role::Button)
                        .flex_none()
                        .aria_label("取消重命名")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child("✕")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            cx.stop_propagation();
                            this.cancel_rename(cx);
                        })),
                );
            return row.into_any_element();
        }

        let open_id = session.id.clone();
        let menu_id = session.id.clone();
        let is_menu_open = self
            .session_menu
            .as_ref()
            .is_some_and(|menu| menu.session_id == session.id);
        let is_pending_delete = self.pending_delete.as_deref() == Some(session.id.as_str());
        let actions: AnyElement = if is_pending_delete {
            let confirm_id = session.id.clone();
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .id(("confirm-delete", index))
                        .role(Role::Button)
                        .aria_label("确认删除会话")
                        .px(px(4.0))
                        .rounded(t.radii.xs)
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child("删除")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.delete_session(confirm_id.clone(), window, cx);
                        })),
                )
                .child(
                    div()
                        .id(("cancel-delete", index))
                        .role(Role::Button)
                        .aria_label("取消删除会话")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child("取消")
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            cx.stop_propagation();
                            this.pending_delete = None;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            let archive_id = session.id.clone();
            let archived = session.archived;
            div()
                .flex()
                .items_center()
                .child(
                    toolbar_icon_button(
                        ("archive-session", index),
                        archive_label(archived),
                        if archived { "\u{e8a7}" } else { "\u{e7b8}" },
                        if archived { "↶" } else { "▣" },
                        t,
                    )
                    .size(px(24.0))
                    .when(!is_menu_open, |button| {
                        button
                            .invisible()
                            .group_hover("session-row", |style| style.visible())
                            .focus_visible(|style| style.visible())
                    })
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.set_session_archived(&archive_id, !archived, window, cx);
                    })),
                )
                .child(self.session_menu_trigger(index, session, t, cx))
                .into_any_element()
        };
        div()
            .id(("session-row", index))
            .group("session-row")
            .aria_label(session.title.clone())
            .w_full()
            .pl(px(10.0))
            .pr(px(10.0))
            .min_h(px(32.0))
            .py(px(3.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(6.0))
            .rounded(t.radii.sm)
            .when(is_active, |row| row.bg(p.color(p.business_tertiary)))
            .hover(|style| style.bg(p.color(p.interactive_hover_solid)))
            .when(is_menu_open && !is_active, |row| {
                row.bg(p.color(p.interactive_hover_solid))
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(t.type_scale.small)
                    .line_height(px(18.0))
                    .when(is_active, |title| title.font_weight(FontWeight::MEDIUM))
                    .text_color(p.color(if session.archived {
                        p.label_secondary
                    } else {
                        p.label_primary
                    }))
                    .child(session.title.clone()),
            )
            .child(if is_pending_delete {
                actions
            } else {
                div()
                    .relative()
                    .min_w(px(48.0))
                    .flex()
                    .flex_none()
                    .justify_end()
                    .child(
                        div()
                            .id(("session-time", index))
                            .flex_none()
                            .text_size(px(11.0))
                            .line_height(px(18.0))
                            .text_color(p.color(p.label_secondary))
                            .when(is_menu_open, |time| time.invisible())
                            .group_hover("session-row", |style| style.invisible())
                            .child(relative_time(session.updated_at)),
                    )
                    .child(div().absolute().right_0().top(px(-3.0)).child(actions))
                    .into_any_element()
            })
            .when(!session.archived, |row| {
                row.role(Role::Button)
                    .cursor_pointer()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        chat.open_session(open_id.clone(), window, cx);
                    }))
            })
            .when(!is_pending_delete, |row| {
                row.on_aux_click(cx.listener(move |chat, event, _, cx| {
                    if matches!(event, gpui::ClickEvent::Mouse(mouse) if mouse.up.button == gpui::MouseButton::Right) {
                        cx.stop_propagation();
                        chat.session_menu = Some(SessionMenu {
                            session_id: menu_id.clone(),
                            position: Some(event.position()),
                        });
                        cx.notify();
                    }
                }))
            })
            .into_any_element()
    }

    pub(crate) fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let searching = !self.session_search.read(cx).content().is_empty();
        div()
            .w(t.layout.sidebar_width)
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(10.0))
            .bg(p.color(p.sidebar_fill))
            .child(
                div()
                    .h(px(36.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .size(px(24.0))
                            .rounded(t.radii.sm)
                            .bg(p.color(p.business_tertiary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(t.type_scale.small)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.link))
                            .child("D"),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.label_primary))
                            .child("DSHR"),
                    ),
            )
            .child(
                chrome_button("new-session", "", t)
                    .aria_label("新建会话")
                    .w_full()
                    .h(px(38.0))
                    .gap(px(6.0))
                    .bg(p.color(p.interactive_hover_solid))
                    .text_color(p.color(p.label_primary))
                    .font_weight(FontWeight::MEDIUM)
                    .child(desktop_icon("\u{e710}", "+"))
                    .child("新建会话")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_session(window, cx);
                    })),
            )
            .child(
                chrome_button("plugins-toggle", "", t)
                    .w_full()
                    .focusable()
                    .justify_start()
                    .gap(px(6.0))
                    .child(desktop_icon("\u{e71d}", "⋮"))
                    .child("插件")
                    .aria_label("打开或关闭插件面板")
                    .aria_selected(self.plugins_open)
                    .when(self.plugins_open, |button| {
                        button.bg(p.color(p.business_tertiary))
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_plugins_panel(window, cx)),
                    ),
            )
            .children(self.adc_workspace_links(false, t, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .border_t_1()
                    .border_color(p.color(p.border_l2))
                    .pt(px(10.0))
                    .child(
                        div()
                            .pl(px(8.0))
                            .pr(px(2.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child(div().flex_1().child("工作区"))
                            .child(self.workspace_add_button(t, cx))
                            .child(
                                toolbar_icon_button(
                                    "archived-sessions",
                                    "已归档会话",
                                    "\u{e7b8}",
                                    "▣",
                                    t,
                                )
                                .aria_selected(self.show_archived)
                                .when(self.show_archived, |button| {
                                    button.bg(p.color(p.tag)).text_color(p.color(p.link))
                                })
                                .on_click(cx.listener(
                                    |chat, _, _, cx| {
                                        chat.show_archived = !chat.show_archived;
                                        chat.workspace_session_limits.clear();
                                        chat.session_menu = None;
                                        chat.workspace_notice = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                toolbar_icon_button(
                                    "toggle-session-search",
                                    "搜索会话或工作区",
                                    "\u{e721}",
                                    "搜索",
                                    t,
                                )
                                .aria_expanded(self.session_search_open)
                                .when(self.session_search_open, |button| {
                                    button.bg(p.color(p.tag)).text_color(p.color(p.link))
                                })
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if this.session_search_open {
                                            this.close_session_search(window, cx);
                                        } else {
                                            this.open_session_search(window, cx);
                                        }
                                    },
                                )),
                            ),
                    )
                    .when(self.session_search_open, |workspace| {
                        workspace.child(
                            div()
                                .flex()
                                .items_center()
                                .px(px(6.0))
                                .py(px(4.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.input_major))
                                .text_color(p.color(p.label_primary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .child(self.session_search.clone()),
                                )
                                .when(searching, |search| {
                                    search.child(
                                        chrome_button("clear-session-search", "×", t)
                                            .aria_label("清除会话搜索")
                                            .h(px(24.0))
                                            .px(px(6.0))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.session_search.update(cx, |input, cx| {
                                                    input.clear(window, cx)
                                                });
                                                let handle =
                                                    this.session_search.read(cx).focus_handle(cx);
                                                window.focus(&handle, cx);
                                            })),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .id("session-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .on_scroll(cx.listener(|chat, _: &gpui::ScrollEvent, _, cx| {
                        if chat.session_menu.take().is_some() {
                            cx.notify();
                        }
                    }))
                    .child(self.session_list(t, cx)),
            )
            .when_some(self.workspace_notice.as_ref(), |sidebar, notice| {
                sidebar.child(
                    div()
                        .px(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child(notice.clone()),
                )
            })
            .child(self.settings_trigger(false, t, cx))
            .into_any_element()
    }

    pub(crate) fn sidebar_rail(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w(px(48.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .py(px(10.0))
            .child(
                toolbar_icon_button("rail-new-session", "新建会话", "\u{e710}", "+", t)
                    .on_click(cx.listener(|chat, _, window, cx| chat.new_session(window, cx))),
            )
            .child(
                toolbar_icon_button("rail-plugins", "插件", "\u{e71d}", "◇", t)
                    .aria_selected(self.plugins_open)
                    .on_click(
                        cx.listener(|chat, _, window, cx| chat.toggle_plugins_panel(window, cx)),
                    ),
            )
            .children(self.adc_workspace_links(true, t, cx))
            .child(div().flex_1())
            .child(self.settings_trigger(true, t, cx))
            .into_any_element()
    }
}

pub(crate) fn workspace_name(workspace: &str) -> &str {
    workspace
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
}

struct WorkspaceSessions<'a> {
    workspace: Option<&'a str>,
    sessions: Vec<(usize, &'a SessionMeta)>,
}

pub(crate) fn archive_label(archived: bool) -> &'static str {
    if archived {
        "取消归档"
    } else {
        "归档会话"
    }
}

pub(crate) const WORKSPACE_SESSION_PAGE: usize = 5;
pub(crate) const WORKSPACE_SESSION_AUTO_LIMIT: usize = 15;

pub(crate) fn top_level_session(
    session: &SessionMeta,
    depths: &std::collections::BTreeMap<String, u32>,
) -> bool {
    depths.get(&session.id).copied().unwrap_or(0) == 0
}

pub(crate) fn next_sidebar_session(
    sessions: &[SessionMeta],
    depths: &std::collections::BTreeMap<String, u32>,
) -> Option<String> {
    sessions
        .iter()
        .find(|session| !session.archived && top_level_session(session, depths))
        .map(|session| session.id.clone())
}

fn workspace_sessions<'a>(
    sessions: &'a [SessionMeta],
    depths: &std::collections::BTreeMap<String, u32>,
    archived: bool,
    query: &str,
) -> Vec<WorkspaceSessions<'a>> {
    let mut groups = Vec::new();
    let mut indices = HashMap::new();
    for (index, session) in sessions.iter().enumerate() {
        if session.archived != archived || !top_level_session(session, depths) {
            continue;
        }
        let workspace = session
            .workspace_id
            .as_deref()
            .filter(|id| !id.trim().is_empty());
        if !query.is_empty()
            && !session.title.to_lowercase().contains(query)
            && !workspace.is_some_and(|id| id.to_lowercase().contains(query))
        {
            continue;
        }
        let group = *indices.entry(workspace).or_insert_with(|| {
            let group = groups.len();
            groups.push(WorkspaceSessions {
                workspace,
                sessions: Vec::new(),
            });
            group
        });
        groups[group].sessions.push((index, session));
    }
    groups
}

/// 会话树前序顺序 + 深度（父在前、子紧随其后），侧栏与自检共用。
pub(crate) fn session_tree_order(store: &SessionIndex) -> Vec<(String, u32)> {
    match store.tree() {
        Ok(tree) => tree
            .flatten()
            .into_iter()
            .map(|(depth, node)| (node.id.clone(), depth))
            .collect(),
        Err(err) => {
            eprintln!("[chat] 会话树损坏（回退到扁平列表）: {err}");
            Vec::new()
        }
    }
}

/// 按会话树前序排好的会话列表 + 深度表（侧栏与启动初始化共用）。
pub(crate) fn ordered_sessions(
    store: &SessionIndex,
) -> (Vec<SessionMeta>, std::collections::BTreeMap<String, u32>) {
    let order = session_tree_order(store);
    let metas: std::collections::BTreeMap<String, SessionMeta> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .map(|meta| (meta.id.clone(), meta))
        .collect();
    if order.is_empty() {
        return (
            metas.into_values().collect(),
            std::collections::BTreeMap::new(),
        );
    }
    let mut ordered = Vec::new();
    let mut depths = std::collections::BTreeMap::new();
    for (id, depth) in &order {
        depths.insert(id.clone(), *depth);
        if let Some(meta) = metas.get(id) {
            ordered.push(meta.clone());
        }
    }
    // 树里没有的（索引竞争等极端情况）按 id 补在末尾，保证不丢会话。
    for (id, meta) in &metas {
        if !depths.contains_key(id) {
            ordered.push(meta.clone());
        }
    }
    (ordered, depths)
}
