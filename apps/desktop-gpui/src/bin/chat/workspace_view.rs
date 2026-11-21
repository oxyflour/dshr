use super::{
    chrome_button, desktop_icon, now_ts, popover, toolbar_icon_button, toolbar_tooltip,
    workspace_name, ChatApp, Theme,
};
use gpui::prelude::*;
use gpui::{div, px, Anchor, AnyElement, Context, PathPromptOptions, Role, Window};
use std::path::PathBuf;

impl ChatApp {
    pub(super) fn choose_workspace_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.workspace_dialog_open {
            return;
        }
        self.workspace_picker_open = false;
        self.workspace_dialog_open = true;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择工作区文件夹".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selected = picker.await;
            let _ = this.update_in(cx, |chat, window, cx| {
                chat.workspace_dialog_open = false;
                match selected {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            chat.new_session_in(path, window, cx);
                        }
                    }
                    Ok(Ok(None)) | Err(_) => {}
                    Ok(Err(error)) => {
                        chat.workspace_notice = Some(format!("无法打开文件夹：{error}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn new_session_in(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Ok(path) = std::path::absolute(path) else {
            self.workspace_notice = Some("工作区路径无效".to_owned());
            cx.notify();
            return;
        };
        if !path.is_dir() {
            self.workspace_notice = Some(format!("工作区目录不可用：{}", path.display()));
            cx.notify();
            return;
        }
        let workspace = path.to_string_lossy();
        let id = self.store.new_id();
        if let Err(error) = self.store.create_root(&id, &workspace, &now_ts()) {
            self.workspace_notice = Some(format!("无法新建会话：{error}"));
            cx.notify();
            return;
        }
        self.workspace_notice = None;
        self.workspace_picker_open = false;
        self.folded_workspaces.remove(workspace.as_ref());
        self.session_search
            .update(cx, |input, cx| input.clear(window, cx));
        self.reload_sessions();
        self.open_session(id, window, cx);
    }

    pub(super) fn workspace_picker(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let active = self.active_workspace.to_string_lossy();
        let button = chrome_button("workspace-picker", "", t)
            .aria_label(format!("当前工作区 {}", workspace_name(&active)))
            .aria_expanded(self.workspace_picker_open)
            .gap(px(6.0))
            .max_w(px(256.0))
            .child(desktop_icon("\u{e8b7}", "▱"))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(workspace_name(&active).to_owned()),
            )
            .child(desktop_icon("\u{e70d}", "▾").text_size(px(10.0)))
            .focusable()
            .on_click(cx.listener(|chat, _, _, cx| {
                chat.composer_menu = None;
                chat.turn_menu_open = false;
                chat.workspace_picker_open = !chat.workspace_picker_open;
                cx.notify();
            }));
        let menu = self.workspace_picker_open.then(|| {
            let mut seen = std::collections::HashSet::new();
            let paths = self
                .sessions
                .iter()
                .filter_map(|session| session.workspace_id.as_deref())
                .filter(|path| PathBuf::from(path).is_absolute() && seen.insert(*path));
            div()
                .id("workspace-menu")
                .role(Role::Menu)
                .occlude()
                .flex()
                .flex_col()
                .w(px(272.0))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .p(px(5.0))
                .rounded(t.radii.md)
                .border_1()
                .border_color(p.color(p.border_l2))
                .bg(p.color(p.bg_layer_1))
                .shadow_sm()
                .children(paths.enumerate().map(|(index, path)| {
                    let chosen = PathBuf::from(path);
                    let current = chosen == self.active_workspace;
                    chrome_button(("workspace-menu-row", index), "", t)
                        .aria_label(path.to_owned())
                        .role(Role::MenuItem)
                        .w_full()
                        .h(px(40.0))
                        .justify_start()
                        .gap(px(6.0))
                        .focusable()
                        .child(desktop_icon("\u{e8b7}", "▱"))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(workspace_name(path).to_owned()),
                        )
                        .when(current, |row| row.child("✓"))
                        .tooltip(toolbar_tooltip(path.to_owned(), *t))
                        .on_click(cx.listener(move |chat, _, window, cx| {
                            if chat.active_workspace == chosen {
                                chat.workspace_picker_open = false;
                                chat.focus_composer(window, cx);
                                cx.notify();
                            } else {
                                chat.new_session_in(chosen.clone(), window, cx);
                            }
                        }))
                }))
                .child(
                    chrome_button("browse-workspace", "选择文件夹…", t)
                        .role(Role::MenuItem)
                        .w_full()
                        .justify_start()
                        .focusable()
                        .on_click(cx.listener(|chat, _, window, cx| {
                            chat.choose_workspace_folder(window, cx)
                        })),
                )
                .into_any_element()
        });
        popover::attach(button, menu, Anchor::TopLeft, Anchor::BottomLeft)
    }

    pub(super) fn workspace_add_button(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        toolbar_icon_button("add-workspace", "打开工作区文件夹", "\u{e710}", "+", t)
            .when(!self.busy && !self.workspace_dialog_open, |button| {
                button.on_click(
                    cx.listener(|chat, _, window, cx| chat.choose_workspace_folder(window, cx)),
                )
            })
            .into_any_element()
    }
}
