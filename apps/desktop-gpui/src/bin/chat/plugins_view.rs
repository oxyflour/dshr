use super::{
    chrome_button, desktop_icon, sidecar_dir_for_app, toolbar_icon_button, ChatApp, Profile, Theme,
};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Div, ElementId, FontWeight, Role, Stateful, Window};
use serde_json::Value;
use std::path::PathBuf;

fn installed_manifest(name: &str) -> Option<PathBuf> {
    let path = sidecar_dir_for_app()
        .join("node_modules")
        .join(name)
        .join("plugin.toml");
    path.is_file().then_some(path)
}

fn plugin_name(id: &str) -> &str {
    match id {
        "@deepseek-ai/dsh-tool-fs" => "文件工具",
        "@deepseek-ai/dsh-tool-workspace-dependencies" => "工作区依赖",
        name => name,
    }
}

fn switch_control(id: impl Into<ElementId>, enabled: bool, t: &Theme) -> Stateful<Div> {
    let p = t.palette;
    div()
        .id(id)
        .role(Role::Switch)
        .w(px(34.0))
        .h(px(20.0))
        .px(px(3.0))
        .flex()
        .flex_none()
        .items_center()
        .when(enabled, |toggle| toggle.justify_end())
        .rounded_full()
        .bg(p.color(if enabled { p.button_info } else { p.border_l4 }))
        .child(div().size(px(14.0)).rounded_full().bg(gpui::white()))
}

impl ChatApp {
    fn save_plugin_profile(
        &mut self,
        profile: Profile,
        reload_sidecar: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Err(error) = profile.save() {
            self.plugin_notice = Some(format!("插件配置保存失败：{error}"));
            cx.notify();
            return false;
        }
        self.profile = profile;
        std::env::set_var("DSHR_DSH_PLUGINS", self.profile.sidecar_json());
        if reload_sidecar {
            self.sidecar_reload_pending = true;
            if !self.busy {
                if let Ok(mut state) = self.state.try_lock() {
                    if state.restart_sidecar() {
                        self.sidecar_reload_pending = false;
                    }
                    self.plugin_errors = state.load_errors();
                }
            }
        }
        self.plugin_notice = None;
        cx.notify();
        true
    }

    pub(super) fn show_plugin(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.profile.entries().get(index) else {
            return;
        };
        self.plugin_manifest_path = installed_manifest(&entry.name);
        let config = serde_json::to_string_pretty(&entry.config).unwrap_or_default();
        self.plugin_config_input
            .update(cx, |input, cx| input.set_content(config, window, cx));
        self.plugin_detail = Some(index);
        self.plugin_edit_json = false;
        self.plugin_notice = None;
        window.focus(&self.plugin_focus, cx);
        cx.notify();
    }

    pub(super) fn return_to_plugins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plugin_detail = None;
        self.plugin_manifest_path = None;
        self.plugin_panels_open = false;
        self.plugin_notice = None;
        window.focus(&self.plugin_focus, cx);
        cx.notify();
    }

    pub(super) fn save_plugin_config(&mut self, cx: &mut Context<Self>) {
        if self.plugin_manifest_path.is_some() {
            return;
        }
        let Some(entry) = self
            .plugin_detail
            .and_then(|index| self.profile.entries().get(index))
        else {
            return;
        };
        let config =
            match serde_json::from_str::<Value>(self.plugin_config_input.read(cx).content()) {
                Ok(config) => config,
                Err(error) => {
                    self.plugin_notice = Some(format!("JSON 无法解析：{error}"));
                    cx.notify();
                    return;
                }
            };
        let mut next = self.profile.clone();
        if next.set_config(&entry.name, config) && self.save_plugin_profile(next, true, cx) {
            self.plugin_notice = Some("插件设置已保存".to_owned());
            cx.notify();
        }
    }

    fn toggle_plugin(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.profile.can_edit() {
            return;
        }
        let Some(entry) = self.profile.entries().get(index) else {
            return;
        };
        let reload_sidecar = installed_manifest(&entry.name).is_none();
        let mut next = self.profile.clone();
        if next.set_enabled(&entry.name, !entry.enabled)
            && self.save_plugin_profile(next, reload_sidecar, cx)
            && !reload_sidecar
        {
            self.plugin_notice = Some("插件启停将在重启应用后生效".to_owned());
            cx.notify();
        }
    }

    fn plugin_switch(&self, index: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.profile.entries()[index];
        let enabled = entry.enabled;
        switch_control(("plugin-toggle", index), enabled, t)
            .aria_label(format!(
                "{} · {}",
                plugin_name(&entry.name),
                if enabled { "已启用" } else { "已停用" }
            ))
            .when(self.profile.can_edit(), |toggle| {
                toggle
                    .focusable()
                    .cursor_pointer()
                    .on_click(cx.listener(move |chat, _, _, cx| chat.toggle_plugin(index, cx)))
            })
            .into_any_element()
    }

    pub(super) fn plugins_page(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let detail = self
            .plugin_detail
            .filter(|index| *index < self.profile.entries().len());
        let mut body = div()
            .w_full()
            .max_w(px(760.0))
            .mx_auto()
            .flex()
            .flex_col()
            .gap(px(12.0));
        if let Some(notice) = &self.plugin_notice {
            body = body.child(
                div()
                    .rounded(t.radii.sm)
                    .bg(p.color(p.interactive_hover))
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(t.type_scale.secondary)
                    .text_color(p.color(p.label_secondary))
                    .child(notice.clone()),
            );
        }
        if !self.plugin_errors.is_empty() {
            body = body.child(
                div()
                    .rounded(t.radii.sm)
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.state_error))
                    .child(
                        self.plugin_errors
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
            );
        }
        if let Some(index) = detail {
            body = body.child(self.plugin_detail_view(index, t, cx));
        } else if self.plugin_panels_open {
            body = body.child(self.plugin_panels(t, cx));
        } else {
            body = body.child(self.installed_plugins(t, cx));
        }
        div()
            .track_focus(&self.plugin_focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .max_w(px(824.0))
                    .mx_auto()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(24.0))
                    .pt(px(18.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .min_w(px(0.0))
                            .when(detail.is_some() || self.plugin_panels_open, |bar| {
                                bar.child(
                                    toolbar_icon_button(
                                        "plugin-list-back",
                                        "返回插件列表",
                                        "\u{e72b}",
                                        "‹",
                                        t,
                                    )
                                    .on_click(cx.listener(
                                        |chat, _, window, cx| {
                                            chat.return_to_plugins(window, cx);
                                        },
                                    )),
                                )
                            })
                            .child(
                                div()
                                    .text_size(t.type_scale.h1)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.label_primary))
                                    .child(if detail.is_some() {
                                        "插件设置"
                                    } else if self.plugin_panels_open {
                                        "插件面板"
                                    } else {
                                        "插件"
                                    }),
                            ),
                    )
                    .child(
                        toolbar_icon_button("plugins-close", "返回对话", "\u{e8bb}", "×", t)
                            .on_click(cx.listener(|chat, _, window, cx| {
                                chat.toggle_plugins_panel(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("plugins-page-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(24.0))
                    .pb(px(22.0))
                    .child(body),
            )
            .into_any_element()
    }

    fn installed_plugins(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let query = self.plugin_search.read(cx).content().trim().to_lowercase();
        let entries = self.profile.entries();
        let mut list = div().flex().flex_col().gap(px(6.0)).w_full();
        let mut matched = 0;
        for (index, entry) in entries.iter().enumerate() {
            let title = plugin_name(&entry.name);
            if !title.to_lowercase().contains(&query) && !entry.name.to_lowercase().contains(&query)
            {
                continue;
            }
            matched += 1;
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.md)
                    .child(
                        div()
                            .size(px(44.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(t.radii.md)
                            .bg(p.color(p.tag))
                            .text_color(p.color(p.link))
                            .child(desktop_icon("\u{e71d}", "◇").text_size(px(19.0))),
                    )
                    .child(
                        chrome_button(("plugin-detail", index), "", t)
                            .aria_label(format!("{title} 设置"))
                            .focusable()
                            .min_w(px(0.0))
                            .flex_1()
                            .h(px(44.0))
                            .justify_between()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .child(
                                        div()
                                            .text_size(t.type_scale.body)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.color(p.label_primary))
                                            .child(title.to_owned()),
                                    )
                                    .when(title != entry.name, |label| {
                                        label.child(
                                            div()
                                                .text_size(t.type_scale.small)
                                                .text_color(p.color(p.label_tertiary))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(entry.name.clone()),
                                        )
                                    }),
                            )
                            .child(desktop_icon("\u{e76c}", "›").text_size(px(10.0)))
                            .on_click(cx.listener(move |chat, _, window, cx| {
                                chat.show_plugin(index, window, cx)
                            })),
                    )
                    .child(self.plugin_switch(index, t, cx)),
            );
        }
        if matched == 0 {
            list = list.child(
                div()
                    .px(px(10.0))
                    .py(px(12.0))
                    .text_color(p.color(p.label_tertiary))
                    .child("没有匹配的插件"),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.label_primary))
                            .child("已安装"),
                    )
                    .when(!self.adc_panels.is_empty(), |row| {
                        row.child(
                            chrome_button("plugin-view-panels", "插件面板", t)
                                .focusable()
                                .on_click(cx.listener(|chat, _, window, cx| {
                                    chat.plugin_panels_open = true;
                                    window.focus(&chat.plugin_focus, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .h(px(36.0))
                    .rounded(t.radii.sm)
                    .bg(p.color(p.tag))
                    .text_color(p.color(p.label_primary))
                    .child(self.plugin_search.clone()),
            )
            .child(list)
            .into_any_element()
    }

    fn plugin_detail_view(&self, index: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let entry = &self.profile.entries()[index];
        let editable = self.profile.can_edit();
        let fs_form = self.plugin_manifest_path.is_none()
            && entry.name == "@deepseek-ai/dsh-tool-fs"
            && (entry.config.is_object() || entry.config.is_null());
        let show_json = self.plugin_manifest_path.is_none() && (!fs_form || self.plugin_edit_json);
        let input = if editable {
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .w_full()
                        .min_h(px(128.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(t.radii.sm)
                        .bg(p.color(p.tag))
                        .font_family(t.font_code)
                        .text_color(p.color(p.label_primary))
                        .child(self.plugin_config_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(
                            div()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_tertiary))
                                .child("Shift+Enter 换行"),
                        )
                        .child(
                            chrome_button("plugin-config-save", "保存设置", t)
                                .focusable()
                                .bg(p.color(p.business_tertiary))
                                .text_color(p.color(p.link))
                                .on_click(
                                    cx.listener(|chat, _, _, cx| chat.save_plugin_config(cx)),
                                ),
                        ),
                )
                .into_any_element()
        } else {
            div()
                .rounded(t.radii.sm)
                .bg(p.color(p.tag))
                .p(px(10.0))
                .font_family(t.font_code)
                .text_size(t.type_scale.small)
                .line_height(t.type_scale.small_line)
                .text_color(p.color(p.label_secondary))
                .child(serde_json::to_string_pretty(&entry.config).unwrap_or_default())
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_size(t.type_scale.h2)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.color(p.label_primary))
                                    .child(plugin_name(&entry.name).to_owned()),
                            )
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(p.label_tertiary))
                                    .child(entry.name.clone()),
                            ),
                    )
                    .child(self.plugin_switch(index, t, cx)),
            )
            .when_some(self.plugin_manifest_path.as_ref(), |body, path| {
                body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .rounded(t.radii.sm)
                        .bg(p.color(p.tag))
                        .p(px(10.0))
                        .text_size(t.type_scale.secondary)
                        .text_color(p.color(p.label_secondary))
                        .child("插件从本地清单加载，启停将在重启应用后生效。")
                        .child(
                            div()
                                .font_family(t.font_code)
                                .child(path.display().to_string()),
                        ),
                )
            })
            .when(fs_form, |body| {
                body.child(self.fs_plugin_options(index, t, cx)).child(
                    chrome_button(
                        "plugin-json-toggle",
                        if self.plugin_edit_json {
                            "收起配置 JSON"
                        } else {
                            "查看配置 JSON"
                        },
                        t,
                    )
                    .focusable()
                    .justify_start()
                    .on_click(cx.listener(|chat, _, _, cx| {
                        chat.plugin_edit_json = !chat.plugin_edit_json;
                        cx.notify();
                    })),
                )
            })
            .when(show_json, |body| {
                body.child(
                    div()
                        .text_size(t.type_scale.secondary)
                        .text_color(p.color(p.label_secondary))
                        .child(if editable {
                            "配置 JSON"
                        } else {
                            "配置 JSON · Loader 只读"
                        }),
                )
                .child(input)
            })
            .into_any_element()
    }

    fn fs_plugin_options(&self, index: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.profile.entries()[index];
        let p = t.palette;
        let fields = [
            ("read", "读取文件", true),
            ("readImage", "读取图片", false),
            ("write", "写入文件", true),
            ("edit", "编辑文件", true),
            ("applyPatch", "应用补丁", true),
        ];
        div()
            .flex()
            .flex_col()
            .rounded(t.radii.lg)
            .border_1()
            .border_color(p.color(p.border_l2))
            .children(
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(row, (key, label, default))| {
                        let value = entry
                            .config
                            .get(key)
                            .and_then(Value::as_bool)
                            .unwrap_or(default);
                        let name = entry.name.clone();
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(12.0))
                            .h(px(42.0))
                            .when(row > 0, |row| {
                                row.border_t_1().border_color(p.color(p.border_l1))
                            })
                            .child(
                                div()
                                    .text_size(t.type_scale.secondary)
                                    .text_color(p.color(p.label_primary))
                                    .child(label),
                            )
                            .child(
                                switch_control(("fs-plugin-option", row), value, t)
                                    .aria_label(format!(
                                        "{label} · {}",
                                        if value { "打开" } else { "关闭" }
                                    ))
                                    .when(self.profile.can_edit(), |button| {
                                        button.focusable().cursor_pointer().on_click(cx.listener(
                                            move |chat, _, window, cx| {
                                                let mut profile = chat.profile.clone();
                                                let Some(current) = profile
                                                    .entries()
                                                    .iter()
                                                    .find(|entry| entry.name == name)
                                                else {
                                                    return;
                                                };
                                                let mut config = current
                                                    .config
                                                    .as_object()
                                                    .cloned()
                                                    .unwrap_or_default();
                                                config.insert(key.to_owned(), Value::Bool(!value));
                                                if profile.set_config(&name, Value::Object(config))
                                                    && chat.save_plugin_profile(profile, true, cx)
                                                {
                                                    chat.show_plugin(index, window, cx);
                                                    chat.plugin_notice =
                                                        Some("插件设置已保存".to_owned());
                                                }
                                            },
                                        ))
                                    }),
                            )
                    }),
            )
            .into_any_element()
    }
}
