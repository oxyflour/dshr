use super::{
    ChatApp, ComposerMenu, Theme, blank_root_created, chrome_button, creator, creator_preset_id,
    desktop_icon, now_ts, popover, rows_snapshot, toolbar_icon_button, toolbar_tooltip,
    workspace_name,
};
use gpui::prelude::*;
use gpui::{Anchor, AnyElement, Context, PathPromptOptions, Pixels, Role, Window, div, px};
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
        if self.blank_root && self.show_empty_composer() {
            self.create_workspace_session(path, self.active_preset.clone(), window, cx);
        } else {
            self.new_session_with_preset(path, None, window, cx);
        }
    }

    pub(super) fn new_session_with_preset(
        &mut self,
        path: PathBuf,
        preset: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let selected = preset
            .map(str::to_owned)
            .or_else(|| match super::read_agent_presets() {
                Ok(catalog) => catalog.default,
                Err(_) => self
                    .state
                    .lock()
                    .ok()
                    .and_then(|state| state.default_preset.clone()),
            });
        self.create_workspace_session(path, selected, window, cx);
    }

    fn create_workspace_session(
        &mut self,
        path: PathBuf,
        selected: Option<String>,
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
        let created = super::SessionCreated::root(workspace.as_ref().to_owned())
            .with_optional_preset(selected);
        if let Err(error) = self.store.create_with_created(&id, &created, &now_ts()) {
            self.workspace_notice = Some(format!("无法新建会话：{error}"));
            cx.notify();
            return;
        }
        self.workspace_notice = None;
        self.workspace_picker_open = false;
        self.show_archived = false;
        self.folded_workspaces.remove(workspace.as_ref());
        self.session_search
            .update(cx, |input, cx| input.clear(window, cx));
        self.reload_sessions();
        self.open_session(id, window, cx);
    }

    pub(super) fn preset_label(preset: Option<&str>) -> (&str, &'static str) {
        match preset {
            None => ("标准模式", "使用工作区的常规工具"),
            Some(creator::CORDIS_PRESET | creator::PRESET) => ("创造模式", "编写、构建和试用插件"),
            Some(preset) => (preset, "使用该预设的工具"),
        }
    }

    pub(super) fn preset_choices(&self) -> Vec<Option<String>> {
        let creator = match self.active_preset.as_deref() {
            Some(id @ (creator::CORDIS_PRESET | creator::PRESET)) => id,
            _ => creator_preset_id(),
        };
        let mut choices = vec![None, Some(creator.to_owned())];
        if let Ok(state) = self.state.try_lock() {
            if let Some(presets) = state
                .host
                .as_ref()
                .and_then(|host| host.preset_status().as_object())
            {
                choices.extend(presets.iter().filter_map(|(id, status)| {
                    (id != creator::CORDIS_PRESET
                        && id != creator::PRESET
                        && status.get("error").is_none())
                    .then(|| Some(id.clone()))
                }));
            }
        }
        choices
    }

    pub(super) fn choose_session_preset(
        &mut self,
        preset: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_menu = None;
        if self.blank_root && self.show_empty_composer() {
            let result = if let Ok(mut state) = self.state.try_lock() {
                let result = state.select_blank_preset(preset);
                self.active_preset = state
                    .log
                    .created()
                    .and_then(|created| created.preset.clone());
                self.blank_root = blank_root_created(&state.log).is_some();
                self.creator_mode = creator::is_creator_session(&state.log);
                self.plugin_errors = state.load_errors();
                self.adc_panels = state.adc_panels.clone();
                if !self.blank_root {
                    self.rows = rows_snapshot(&state.log, None);
                }
                result
            } else {
                Err("工作区正在更新，请稍后重试".to_owned())
            };
            self.preset_notice = result.err();
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(super) fn preset_picker(
        &self,
        t: &Theme,
        max_width: Pixels,
        max_height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.composer_menu == Some(ComposerMenu::Preset);
        let (label, _) = Self::preset_label(self.active_preset.as_deref());
        let p = t.palette;
        let button = chrome_button("session-preset-picker", "", t)
            .aria_label(format!("会话模式：{label}"))
            .aria_expanded(open)
            .gap(px(4.0))
            .min_w(px(0.0))
            .max_w(px(156.0))
            .focusable()
            .when(open, |button| button.bg(p.color(p.interactive_hover)))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(label.to_owned()),
            )
            .child(desktop_icon("\u{e70d}", "▾").text_size(px(12.0)))
            .on_click(cx.listener(|chat, _, _, cx| {
                chat.workspace_picker_open = false;
                chat.preset_notice = None;
                chat.toggle_composer_menu(ComposerMenu::Preset, cx);
            }));
        popover::attach_dismissible(
            button,
            self.composer_options(ComposerMenu::Preset, t, max_width, max_height, cx),
            Anchor::BottomLeft,
            Anchor::TopLeft,
            cx.listener(Self::dismiss_composer_menu),
        )
    }

    pub(super) fn workspace_picker(
        &self,
        t: &Theme,
        available_width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let active = self.active_workspace.to_string_lossy();
        let button = chrome_button("workspace-picker", "", t)
            .aria_label(format!("当前工作区 {}", workspace_name(&active)))
            .aria_expanded(self.workspace_picker_open)
            .flex_shrink_1()
            .min_w(px(0.0))
            .max_w(px(360.0))
            .gap(px(4.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(p.color(p.label_primary))
            .when(self.workspace_picker_open, |button| {
                button.bg(p.color(p.interactive_hover))
            })
            .child(desktop_icon("\u{e8b7}", "▱").text_size(px(16.0)))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(workspace_name(&active).to_owned()),
            )
            .child(
                desktop_icon("\u{e70d}", "▾")
                    .text_size(px(12.0))
                    .text_color(p.color(p.label_caption)),
            )
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
                .w(px(272.0).min(available_width))
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
        });
        popover::attach_dismissible(
            button,
            menu,
            Anchor::BottomLeft,
            Anchor::TopLeft,
            cx.listener(|chat, _, _, cx| {
                chat.workspace_picker_open = false;
                cx.notify();
            }),
        )
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
