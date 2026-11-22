use super::{
    chrome_button, desktop_icon, now_ts, popover, spill_dir_for, toolbar_icon_button,
    top_level_session, ChatApp, SessionLog, SessionMeta, SpillStore, Theme,
};
use dshr_session_export::{export_session, verify_export, ExportOptions, SpillStoreSource};
use gpui::prelude::*;
use gpui::{
    div, px, Anchor, AnyElement, Context, Div, FontWeight, PathPromptOptions, Pixels, Role,
    Stateful, TextOverflow, Window,
};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderMenu {
    Subagents,
    Session,
}

impl ChatApp {
    pub(super) fn session_branch(&self) -> &[SessionMeta] {
        let Some(current) = self
            .sessions
            .iter()
            .position(|session| session.id == self.session_id)
        else {
            return &[];
        };
        let is_root = |session: &SessionMeta| top_level_session(session, &self.session_depths);
        let start = self.sessions[..=current]
            .iter()
            .rposition(is_root)
            .unwrap_or(current);
        let end = self.sessions[current + 1..]
            .iter()
            .position(is_root)
            .map_or(self.sessions.len(), |offset| current + 1 + offset);
        &self.sessions[start..end]
    }

    pub(super) fn session_heading(
        &self,
        t: &Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let branch = self.session_branch();
        let child = branch.first().filter(|root| root.id != self.session_id);
        let mut heading = div()
            .min_w(px(0.0))
            .flex_shrink_1()
            .flex()
            .items_center()
            .gap(px(6.0));
        if let Some(root) = child {
            if !compact {
                let root_id = root.id.clone();
                heading = heading
                    .child(
                        chrome_button("parent-session", "", t)
                            .aria_label(format!("返回主会话 {}", root.title))
                            .focusable()
                            .max_w(px(148.0))
                            .min_w(px(0.0))
                            .flex_shrink_1()
                            .child(truncated_title(root.title.clone()))
                            .on_click(cx.listener(move |chat, _, window, cx| {
                                chat.open_session(root_id.clone(), window, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(p.color(p.label_caption))
                            .child("/"),
                    );
            }
            heading =
                heading.child(self.subagent_switcher(branch, Some(self.active_title()), t, cx));
        } else {
            heading = heading.child(
                truncated_title(self.active_title())
                    .text_size(t.type_scale.body)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.color(p.label_primary)),
            );
        }
        if let Some(preset) = self.active_preset.as_deref() {
            let (label, description) = Self::preset_label(Some(preset));
            heading = heading.child(
                div()
                    .id("active-session-preset")
                    .flex()
                    .flex_shrink_1()
                    .min_w(px(0.0))
                    .max_w(px(132.0))
                    .items_center()
                    .gap(px(4.0))
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_secondary))
                    .child(desktop_icon("\u{e77b}", "◇"))
                    .child(truncated_title(label.to_owned()))
                    .tooltip(super::toolbar_tooltip(description, *t)),
            );
        }
        if child.is_none() && branch.iter().skip(1).any(|session| !session.archived) {
            heading = heading.child(self.subagent_switcher(branch, None, t, cx));
        }
        heading.into_any_element()
    }

    fn subagent_switcher(
        &self,
        branch: &[SessionMeta],
        child_title: Option<String>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.header_menu == Some(HeaderMenu::Subagents);
        let count = branch
            .iter()
            .skip(1)
            .filter(|session| !session.archived)
            .count();
        let button = chrome_button("subagent-switcher", "", t)
            .aria_label("切换子智能体")
            .aria_expanded(open)
            .focusable()
            .gap(px(4.0));
        let button = if let Some(title) = child_title {
            button
                .min_w(px(0.0))
                .flex_shrink_1()
                .child(truncated_title(title).font_weight(FontWeight::SEMIBOLD))
                .child(desktop_icon("\u{e70d}", "▾"))
        } else {
            button
                .child(desktop_icon("\u{e716}", "♟"))
                .child(count.to_string())
                .when(!open, |button| {
                    button.tooltip(super::toolbar_tooltip(
                        format!("切换子智能体 · {count} 个"),
                        *t,
                    ))
                })
        };
        let p = t.palette;
        let menu = open.then(|| {
            header_popup("subagent-menu", "切换子智能体", t)
                .max_h(px(240.0))
                .overflow_y_scroll()
                .children(
                    branch
                        .iter()
                        .enumerate()
                        .filter(|(_, session)| !session.archived)
                        .map(|(index, session)| {
                            let id = session.id.clone();
                            let current = id == self.session_id;
                            let depth = self.session_depths.get(&id).copied().unwrap_or(0).min(6);
                            chrome_button(("subagent-session", index), "", t)
                                .role(Role::MenuItem)
                                .focusable()
                                .justify_start()
                                .pl(px(8.0 + depth as f32 * 12.0))
                                .aria_selected(current)
                                .when(current, |row| row.bg(p.color(p.business_tertiary)))
                                .when(self.busy && !current, |row| row.opacity(0.5))
                                .child(truncated_title(if index == 0 {
                                    format!("主会话 · {}", session.title)
                                } else {
                                    session.title.clone()
                                }))
                                .on_click(cx.listener(move |chat, _, window, cx| {
                                    if !chat.busy || current {
                                        chat.header_menu = None;
                                        chat.open_session(id.clone(), window, cx);
                                        cx.notify();
                                    }
                                }))
                        }),
                )
        });
        self.attach_header_menu(
            button,
            menu,
            HeaderMenu::Subagents,
            Anchor::BottomLeft,
            Anchor::TopLeft,
            cx,
        )
    }

    pub(super) fn header_actions(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.header_menu == Some(HeaderMenu::Session);
        let button = chrome_button("header-session-actions", "", t)
            .aria_label("更多会话操作")
            .aria_expanded(open)
            .focusable()
            .size(px(28.0))
            .px(px(0.0))
            .child(desktop_icon("\u{e712}", "⋯"))
            .when(!open, |button| {
                button.tooltip(super::toolbar_tooltip("更多会话操作", *t))
            });
        let menu = open.then(|| {
            header_popup("header-session-menu", "更多会话操作", t)
                .w(px(160.0))
                .child(
                    chrome_button("export-header-session", "导出会话", t)
                        .role(Role::MenuItem)
                        .focusable()
                        .justify_start()
                        .on_click(cx.listener(|chat, _, window, cx| {
                            chat.export_header_session(window, cx)
                        })),
                )
                .when(
                    self.session_branch()
                        .first()
                        .is_some_and(|root| root.id == self.session_id),
                    |menu| {
                        menu.child(
                            chrome_button("archive-header-session", "归档会话", t)
                                .role(Role::MenuItem)
                                .focusable()
                                .justify_start()
                                .on_click(cx.listener(|chat, _, window, cx| {
                                    chat.set_session_archived(
                                        &chat.session_id.clone(),
                                        true,
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    },
                )
        });
        self.attach_header_menu(
            button,
            menu,
            HeaderMenu::Session,
            Anchor::BottomRight,
            Anchor::TopRight,
            cx,
        )
    }

    fn attach_header_menu(
        &self,
        button: Stateful<Div>,
        menu: Option<Stateful<Div>>,
        kind: HeaderMenu,
        target: Anchor,
        origin: Anchor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        popover::attach_dismissible(
            button.on_click(cx.listener(move |chat, _, _, cx| {
                chat.session_menu = None;
                chat.composer_menu = None;
                chat.turn_menu_open = false;
                chat.header_menu = (chat.header_menu != Some(kind)).then_some(kind);
                cx.notify();
            })),
            menu,
            target,
            origin,
            cx.listener(|chat, _, _, cx| {
                chat.header_menu = None;
                cx.notify();
            }),
        )
    }

    fn export_header_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.header_menu = None;
        self.header_notice = None;
        let id = self.session_id.clone();
        let path = self.store.path_for(&id);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择会话导出目录".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match picker.await {
                Ok(Ok(Some(folders))) => {
                    let Some(folder) = folders.into_iter().next() else {
                        return;
                    };
                    let export_id = id.clone();
                    cx.background_executor()
                        .spawn(async move { export_to_folder(&path, &export_id, &folder) })
                        .await
                }
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |chat, _, cx| {
                if chat.session_id == id {
                    chat.header_notice = Some(match result {
                        Ok(path) => (false, format!("会话已导出至 {}", path.display())),
                        Err(error) => (true, format!("无法导出会话：{error}")),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn right_sidebar(
        &self,
        width: Pixels,
        overlay: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        div()
            .id("right-sidebar")
            .aria_label("右侧栏")
            .w(width)
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .occlude()
            .border_l_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .when(overlay, |panel| {
                panel
                    .absolute()
                    .right_0()
                    .top_0()
                    .shadow_sm()
                    .on_mouse_down_out(cx.listener(|chat, _, _, cx| {
                        chat.right_sidebar_open = false;
                        cx.notify();
                    }))
            })
            .child(
                div()
                    .h(px(42.0))
                    .px(px(8.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(t.type_scale.small).child("插件视图"))
                    .child(
                        toolbar_icon_button(
                            "collapse-right-sidebar",
                            "收起右侧栏",
                            "\u{e8a0}",
                            "▥",
                            t,
                        )
                        .on_click(cx.listener(|chat, _, window, cx| {
                            chat.right_sidebar_open = false;
                            chat.focus_composer(window, cx);
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .id("right-sidebar-content")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .child(if self.adc_panels.is_empty() {
                        div()
                            .px(px(4.0))
                            .py(px(8.0))
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child("暂无可用的插件视图")
                            .into_any_element()
                    } else {
                        self.plugin_panels(None, t, cx)
                    }),
            )
            .into_any_element()
    }
}

fn header_popup(id: &'static str, label: &'static str, t: &Theme) -> Stateful<Div> {
    let p = t.palette;
    div()
        .id(id)
        .role(Role::Menu)
        .aria_label(label)
        .occlude()
        .flex()
        .flex_col()
        .w(px(260.0))
        .p(px(4.0))
        .rounded(t.radii.md)
        .border_1()
        .border_color(p.color(p.border_l2))
        .bg(p.color(p.bg_layer_1))
        .shadow_sm()
}

fn truncated_title(title: String) -> Div {
    div()
        .min_w(px(0.0))
        .overflow_hidden()
        .text_overflow(TextOverflow::Truncate("…".into()))
        .child(title)
}

fn export_to_folder(path: &Path, id: &str, folder: &Path) -> Result<std::path::PathBuf, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let log = SessionLog::from_jsonl(id, &text).map_err(|error| error.to_string())?;
    let spill = SpillStore::new(spill_dir_for(path));
    let target = tempfile::Builder::new()
        .prefix("dshr-session-")
        .tempdir_in(folder)
        .map_err(|error| error.to_string())?;
    export_session(
        &log,
        &SpillStoreSource::new(&spill),
        target.path(),
        &ExportOptions::new(now_ts()),
    )
    .map_err(|error| error.to_string())?;
    let report = verify_export(target.path(), id).map_err(|error| error.to_string())?;
    if !report.is_ok() {
        return Err(format!("导出校验失败：{:?}", report.problems));
    }
    Ok(target.keep())
}
