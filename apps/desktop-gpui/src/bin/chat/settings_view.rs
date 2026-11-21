use super::{
    chrome_button, desktop_icon, permission_label, save_atomic, toolbar_icon_button, ChatApp,
    DesktopModel, DesktopSettings, PermissionMode, Theme, ThemeKind, EDITABLE_PERMISSIONS,
};
use gpui::prelude::*;
use gpui::{deferred, div, px, rgba, AnyElement, Context, FontWeight, Role, Window};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use super::runtime_components::{self, Component};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTab {
    General,
    Models,
    Plugins,
    Components,
}

impl SettingsTab {
    const TABS: [(Self, &'static str, &'static str); 4] = [
        (Self::General, "通用", "\u{e713}"),
        (Self::Models, "模型", "\u{e950}"),
        (Self::Plugins, "插件", "\u{e71d}"),
        (Self::Components, "运行组件", "\u{e8b7}"),
    ];
}

pub(super) struct Preferences {
    path: PathBuf,
    pub(super) theme: ThemeKind,
    pub(super) model: Option<String>,
    pub(super) model_connection: Option<super::model_settings::ConnectionId>,
    permission: Option<PermissionMode>,
}

impl Preferences {
    pub(super) fn load(root: &Path) -> Self {
        let path = root.join("desktop-settings.json");
        let data = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            theme: if data["theme"].as_str() == Some("dark") {
                ThemeKind::Dark
            } else {
                ThemeKind::Light
            },
            model: data["model"].as_str().map(str::to_owned),
            model_connection: serde_json::from_value(data["model_connection"].clone()).ok(),
            permission: data["permission"].as_str().and_then(PermissionMode::parse),
        }
    }

    pub(super) fn apply(&self, settings: &mut DesktopSettings, models: &[Arc<DesktopModel>]) {
        if std::env::var_os("DSHR_PROVIDER").is_none() && self.model_connection.is_none() {
            if let Some(model) = models
                .iter()
                .find(|model| Some(model.name()) == self.model.as_deref())
            {
                settings.model = model.clone();
            }
        }
        if std::env::var_os(PermissionMode::ENV).is_none() && settings.permission.is_some() {
            settings.permission = self.permission.or(settings.permission);
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let data = json!({
            "theme": if self.theme == ThemeKind::Dark { "dark" } else { "light" },
            "model": self.model,
            "model_connection": self.model_connection,
            "permission": self.permission.map(PermissionMode::as_str),
        });
        save_atomic(&self.path, data.to_string().as_bytes())
    }
}

impl ChatApp {
    pub(super) fn save_preferences(&mut self) {
        self.settings_notice = self.preferences.save().err().map(|error| {
            eprintln!("[settings] {error}");
            format!("本地设置保存失败：{error}")
        });
    }

    pub(super) fn settings_trigger(
        &self,
        compact: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        chrome_button("user-settings", "", t)
            .focusable()
            .aria_label("用户设置")
            .aria_expanded(self.settings_tab.is_some())
            .h(px(if compact { 36.0 } else { 42.0 }))
            .rounded(t.radii.md)
            .when(compact, |button| button.w(px(36.0)).px(px(0.0)))
            .when(!compact, |button| {
                button.w_full().px(px(10.0)).justify_start().gap(px(8.0))
            })
            .text_color(p.color(p.label_primary))
            .child(desktop_icon("\u{e713}", "⚙"))
            .when(!compact, |button| button.child("设置"))
            .on_click(cx.listener(|chat, _, window, cx| {
                chat.open_settings(SettingsTab::General, window, cx)
            }))
            .into_any_element()
    }

    pub(super) fn open_settings(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_restore_focus = window.focused(cx);
        self.settings_tab = Some(tab);
        self.turn_menu_open = false;
        self.composer_menu = None;
        self.session_menu = None;
        self.workspace_picker_open = false;
        #[cfg(windows)]
        self.close_window_menu(cx);
        window.focus(&self.settings_focus, cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.settings_tab.take().is_none() {
            return false;
        }
        self.model_settings.close_editor();
        if let Some(handle) = self.settings_restore_focus.take() {
            window.focus(&handle, cx);
        } else if !self.plugins_open {
            self.focus_composer(window, cx);
        }
        cx.notify();
        true
    }

    pub(super) fn settings_overlay(
        &self,
        t: &Theme,
        width: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let active = self.settings_tab?;
        let p = t.palette;
        let compact = width < px(580.0);
        let content_id = if active == SettingsTab::Models && self.model_settings.editor_is_open() {
            "model-editor-content"
        } else {
            "settings-content"
        };
        let card = div()
            .id("user-settings-dialog")
            .role(Role::Dialog)
            .aria_label("用户设置")
            .track_focus(&self.settings_focus)
            .occlude()
            .w_full()
            .h_full()
            .max_w(px(800.0))
            .max_h(px(800.0))
            .rounded(t.radii.panel)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_2))
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .child(
                div()
                    .w(px(if compact { 126.0 } else { 188.0 }))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .px(px(12.0))
                    .pt(px(16.0))
                    .gap(px(4.0))
                    .child(
                        div()
                            .px(px(12.0))
                            .h(px(42.0))
                            .flex()
                            .items_center()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.label_primary))
                            .child("设置"),
                    )
                    .children(SettingsTab::TABS.into_iter().enumerate().map(
                        |(index, (tab, label, icon))| {
                            chrome_button(("settings-tab", index), "", t)
                                .aria_label(label)
                                .role(Role::Tab)
                                .aria_selected(active == tab)
                                .focusable()
                                .h(px(40.0))
                                .w_full()
                                .rounded(t.radii.md)
                                .justify_start()
                                .gap(px(8.0))
                                .px(px(12.0))
                                .when(active == tab, |button| {
                                    button.bg(p.color(p.interactive_hover_solid))
                                })
                                .child(desktop_icon(icon, ""))
                                .child(label)
                                .on_click(cx.listener(move |chat, _, _, cx| {
                                    if tab != SettingsTab::Models {
                                        chat.model_settings.close_editor();
                                    }
                                    chat.settings_tab = Some(tab);
                                    cx.notify();
                                }))
                        },
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .justify_end()
                            .items_center()
                            .h(px(54.0))
                            .pr(px(14.0))
                            .child(
                                toolbar_icon_button(
                                    "close-user-settings",
                                    "关闭设置",
                                    "\u{e8bb}",
                                    "×",
                                    t,
                                )
                                .on_click(cx.listener(
                                    |chat, _, window, cx| {
                                        chat.close_settings(window, cx);
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .id(content_id)
                            .min_h(px(0.0))
                            .flex_1()
                            .overflow_y_scroll()
                            .px(px(if compact { 12.0 } else { 24.0 }))
                            .pb(px(24.0))
                            .children(self.settings_notice.as_ref().map(|error| {
                                div()
                                    .mb(px(10.0))
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(p.state_error))
                                    .child(error.clone())
                            }))
                            .child(self.settings_section(active, t, cx)),
                    ),
            );
        let overlay = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(px(if compact { 12.0 } else { 24.0 }))
            .child(
                div()
                    .id("settings-mask")
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x0000_003d))
                    .occlude()
                    .on_click(cx.listener(|chat, _, window, cx| {
                        chat.close_settings(window, cx);
                    })),
            )
            .child(card);
        Some(deferred(overlay).with_priority(9).into_any_element())
    }

    fn settings_section(&self, tab: SettingsTab, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let section = |name: &'static str| {
            div().flex().flex_col().gap(px(12.0)).child(
                div()
                    .text_size(t.type_scale.h2)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.color(p.label_primary))
                    .child(name),
            )
        };
        let container = || {
            div()
                .flex()
                .flex_col()
                .rounded(t.radii.lg)
                .border_1()
                .border_color(p.color(p.border_l2))
                .bg(p.color(p.bg_base))
        };
        let heading = |name: &'static str| {
            div()
                .px(px(12.0))
                .pt(px(10.0))
                .pb(px(4.0))
                .text_size(t.type_scale.secondary)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.color(p.label_primary))
                .child(name)
        };

        match tab {
            SettingsTab::General => section("通用")
                .child(
                    container().child(heading("外观")).child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.0))
                            .px(px(8.0))
                            .pb(px(8.0))
                            .children(
                                [(ThemeKind::Light, "浅色"), (ThemeKind::Dark, "深色")]
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, (kind, label))| {
                                        let selected = self.theme_kind == kind;
                                        chrome_button(("appearance", index), label, t)
                                            .focusable()
                                            .aria_selected(selected)
                                            .when(selected, |button| {
                                                button.bg(p.color(p.business_tertiary))
                                            })
                                            .on_click(cx.listener(move |chat, _, _, cx| {
                                                chat.set_theme(kind, cx);
                                            }))
                                    }),
                            ),
                    ),
                )
                .child(container().child(heading("工具权限")).children(
                    EDITABLE_PERMISSIONS.iter().enumerate().map(
                        |(index, &(permission, description))| {
                            let selected = self.settings.permission == Some(permission);
                            let editable = !self.busy && self.settings.permission.is_some();
                            chrome_button(("settings-permission", index), "", t)
                                .aria_label(permission_label(permission))
                                .aria_selected(selected)
                                .h(px(48.0))
                                .w_full()
                                .justify_between()
                                .px(px(12.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex()
                                        .flex_col()
                                        .items_start()
                                        .child(permission_label(permission))
                                        .child(
                                            div()
                                                .text_size(t.type_scale.small)
                                                .text_color(p.color(p.label_tertiary))
                                                .child(description),
                                        ),
                                )
                                .when(selected, |button| button.child("✓"))
                                .when(editable, |button| {
                                    button.focusable().on_click(cx.listener(
                                        move |chat, _, _, cx| {
                                            if let Ok(mut state) = chat.state.try_lock() {
                                                chat.sidecar_reload_pending |=
                                                    state.set_permission(permission);
                                                chat.settings.permission = Some(permission);
                                            }
                                            if chat.settings.permission == Some(permission) {
                                                chat.preferences.permission = Some(permission);
                                                chat.save_preferences();
                                                cx.notify();
                                            }
                                        },
                                    ))
                                })
                        },
                    ),
                ))
                .into_any_element(),
            SettingsTab::Models => section("模型")
                .child(self.model_settings_view(t, cx))
                .into_any_element(),
            SettingsTab::Plugins => section("插件")
                .child(
                    container().child(
                        chrome_button("settings-show-plugins", "管理已安装插件", t)
                            .focusable()
                            .h(px(42.0))
                            .w_full()
                            .justify_start()
                            .px(px(12.0))
                            .on_click(cx.listener(|chat, _, window, cx| {
                                chat.close_settings(window, cx);
                                if !chat.plugins_open {
                                    chat.toggle_plugins_panel(window, cx);
                                }
                            })),
                    ),
                )
                .into_any_element(),
            SettingsTab::Components => section("运行组件")
                .child(container().children(Component::ALL.map(|component| {
                    let asset = runtime_components::asset(component);
                    let installed = runtime_components::installed(component).is_some();
                    let installing = self.runtime_installing == Some(component);
                    let status = match &asset {
                        Ok(asset) if installed => format!("已安装 {}", asset.version),
                        Ok(_) if installing => "正在安装".to_owned(),
                        Ok(asset) => format!(
                            "{} · {:.1} MiB",
                            asset.version,
                            asset.bytes as f64 / (1024.0 * 1024.0)
                        ),
                        Err(message) => message.clone(),
                    };
                    div().flex().items_center().justify_between().gap(px(12.0)).p(px(12.0))
                        .child(div().flex().flex_col().gap(px(4.0))
                            .child(component.title())
                            .child(div().text_color(p.color(p.label_secondary)).child(component.description()))
                            .child(div().text_color(p.color(p.label_secondary)).child(status)))
                        .when(asset.is_ok() && !installed && self.runtime_installing.is_none(), |row| {
                            row.child(
                                chrome_button(component.id(), "安装", t)
                                    .focusable()
                                    .on_click(cx.listener(move |chat, _, _, cx| {
                                        chat.install_component(component, cx);
                                    })),
                            )
                        })
                })))
                .children(self.runtime_notice.clone())
                .into_any_element(),
        }
    }

    fn install_component(&mut self, component: Component, cx: &mut Context<Self>) {
        if self.runtime_installing.is_some() {
            return;
        }
        self.runtime_installing = Some(component);
        self.runtime_notice = None;
        let install = cx.background_executor().spawn(async move {
            runtime_components::install(component)
        });
        cx.spawn(async move |chat, cx| {
            let result = install.await;
            let _ = chat.update(cx, |chat, cx| {
                chat.runtime_installing = None;
                chat.runtime_notice = result.err();
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}
