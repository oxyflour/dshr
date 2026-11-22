use crate::*;

impl ChatApp {
    pub(crate) fn toggle_composer_menu(&mut self, menu: ComposerMenu, cx: &mut Context<Self>) {
        if !self.busy || menu == ComposerMenu::Permission {
            self.turn_menu_open = false;
            let current = match self.composer_menu {
                Some(ComposerMenu::ModelList | ComposerMenu::Reasoning) => {
                    Some(ComposerMenu::Model)
                }
                current => current,
            };
            self.composer_menu = (current != Some(menu)).then_some(menu);
            cx.notify();
        }
    }

    pub(crate) fn dismiss_composer_menu(
        &mut self,
        _: &gpui::MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_menu = None;
        cx.notify();
    }

    pub(crate) fn choose_model(
        &mut self,
        model: Arc<DesktopModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.apply_model(model) {
            return;
        }
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn apply_model(&mut self, model: Arc<DesktopModel>) -> bool {
        if self.busy || self.model_settings.pending {
            return false;
        }
        let reasoning = self
            .preferences
            .reasoning
            .get(&self.model_settings.reasoning_key(&model))
            .and_then(|value| ReasoningEffort::parse(value))
            .filter(|effort| model.reasoning_levels().contains(effort));
        if !Arc::ptr_eq(&model, &self.settings.model) || reasoning != self.settings.reasoning {
            let Ok(mut state) = self.state.try_lock() else {
                return false;
            };
            state.set_model(model.clone(), reasoning);
            self.settings.model = model;
            self.settings.reasoning = reasoning;
            self.attachment_error = None;
        }
        true
    }

    pub(crate) fn select_reasoning(
        &mut self,
        reasoning: Option<ReasoningEffort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.model_settings.pending {
            return;
        }
        let model = self.settings.model.clone();
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if reasoning != self.settings.reasoning {
            state.set_model(model.clone(), reasoning);
            self.settings.reasoning = reasoning;
        }
        drop(state);
        let key = self.model_settings.reasoning_key(&model);
        if let Some(effort) = reasoning {
            self.preferences
                .reasoning
                .insert(key, effort.as_str().to_owned());
        } else {
            self.preferences.reasoning.remove(&key);
        }
        self.save_preferences();
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn reasoning_label(&self) -> &'static str {
        reasoning_label(
            self.settings
                .reasoning
                .or_else(|| self.settings.model.provider_reasoning()),
        )
    }

    pub(crate) fn change_permission(&mut self, permission: PermissionMode) -> bool {
        if self.permission.mode().is_none() {
            return false;
        }
        self.permission.set(permission);
        true
    }

    pub(crate) fn choose_permission(
        &mut self,
        permission: PermissionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.change_permission(permission) {
            return;
        }
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn composer_options(
        &self,
        menu: ComposerMenu,
        t: &Theme,
        max_width: gpui::Pixels,
        max_height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.composer_menu != Some(menu) {
            return None;
        }
        let p = t.palette;
        let mut options = div()
            .id("composer-options")
            .role(Role::Menu)
            .aria_label(match menu {
                ComposerMenu::Model => "模型和推理等级",
                ComposerMenu::ModelList => "选择模型",
                ComposerMenu::Reasoning => "推理等级",
                ComposerMenu::Permission => "选择工具权限",
                ComposerMenu::Preset => "选择会话模式",
                ComposerMenu::Commands => "选择指令",
                ComposerMenu::Add => "添加附件或调用指令",
            })
            .occlude()
            .w(px(276.0))
            .max_w(max_width)
            .max_h(max_height)
            .overflow_y_scroll()
            .p(px(5.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm();
        if let Some(label) = match menu {
            ComposerMenu::ModelList => Some("模型"),
            ComposerMenu::Reasoning => Some("推理等级"),
            _ => None,
        } {
            options = options.child(
                chrome_button("model-menu-back", "", t)
                    .role(Role::MenuItem)
                    .aria_label("返回模型菜单")
                    .focusable()
                    .w_full()
                    .px(px(9.0))
                    .gap(px(8.0))
                    .child(desktop_icon("\u{e76b}", "‹").text_size(px(10.0)))
                    .child(label)
                    .on_click(cx.listener(|chat, _, _, cx| {
                        chat.composer_menu = Some(ComposerMenu::Model);
                        cx.notify();
                    })),
            );
        }
        match menu {
            ComposerMenu::Model if !self.settings.model.reasoning_levels().is_empty() => {
                for (label, value, pane) in [
                    ("模型", self.settings.model.name(), ComposerMenu::ModelList),
                    ("推理等级", self.reasoning_label(), ComposerMenu::Reasoning),
                ] {
                    options = options.child(
                        chrome_button(label, label, t)
                            .role(Role::MenuItem)
                            .focusable()
                            .w_full()
                            .px(px(9.0))
                            .gap(px(8.0))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(value.to_owned()),
                            )
                            .child(desktop_icon("\u{e76c}", "›").text_size(px(10.0)))
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                chat.composer_menu = Some(pane);
                                cx.notify();
                            })),
                    );
                }
            }
            ComposerMenu::Reasoning => {
                for (index, effort) in std::iter::once(None)
                    .chain(
                        self.settings
                            .model
                            .reasoning_levels()
                            .iter()
                            .copied()
                            .map(Some),
                    )
                    .enumerate()
                {
                    options = options.child(
                        chrome_button(("reasoning-level", index), reasoning_label(effort), t)
                            .role(Role::MenuItemRadio)
                            .aria_selected(self.settings.reasoning == effort)
                            .focusable()
                            .w_full()
                            .px(px(9.0))
                            .justify_between()
                            .when(self.settings.reasoning == effort, |row| row.child("✓"))
                            .on_click(cx.listener(move |chat, _, window, cx| {
                                chat.select_reasoning(effort, window, cx);
                            })),
                    );
                }
            }
            ComposerMenu::Model | ComposerMenu::ModelList => {
                for (index, model) in self.model_options.iter().enumerate() {
                    let option = model.clone();
                    let description = match model.kind() {
                        ModelKind::Mock => "在本机生成演示回复".to_owned(),
                        kind => format!("{} · {}", kind.label(), self.model_settings.source(model)),
                    };
                    options = options.child(
                        composer_option(
                            ("model", index),
                            model.name(),
                            &description,
                            Arc::ptr_eq(model, &self.settings.model),
                            t,
                        )
                        .focusable()
                        .when(model.kind() != ModelKind::Mock, |row| {
                            row.aria_label(format!("{}，{description}", model.name()))
                                .tooltip(toolbar_tooltip(description, *t))
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.choose_model(option.clone(), window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Preset => {
                for (index, preset) in self.preset_choices().into_iter().enumerate() {
                    let (label, description) = Self::preset_label(preset.as_deref());
                    options = options.child(
                        composer_option(
                            ("session-preset", index),
                            label,
                            description,
                            self.active_preset == preset,
                            t,
                        )
                        .focusable()
                        .on_click(cx.listener(
                            move |chat, _, window, cx| {
                                chat.choose_session_preset(preset.clone(), window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Permission => {
                for (index, (mode, description)) in EDITABLE_PERMISSIONS.into_iter().enumerate() {
                    options = options.child(
                        composer_option(
                            ("permission", index),
                            permission_label(mode),
                            description,
                            self.permission.mode() == Some(mode),
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.choose_permission(mode, window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Commands | ComposerMenu::Add => {
                let typed_slash = menu == ComposerMenu::Commands;
                if !typed_slash && self.attachments.len() < MAX_ATTACHMENTS {
                    options = options.child(
                        composer_option(
                            "add-file",
                            "添加附件",
                            "图片、PDF 或 UTF-8 文本",
                            false,
                            t,
                        )
                        .role(Role::MenuItem)
                        .focusable()
                        .on_click(cx.listener(|chat, _, _, cx| {
                            chat.add_attachments(AttachmentSource::Picker, cx);
                        })),
                    );
                }
                let text = if typed_slash {
                    self.slash_text.as_deref().unwrap_or("/")
                } else {
                    "/"
                };
                for (index, command) in slash_commands(text).enumerate() {
                    options = options.child(
                        composer_option(
                            ("slash-command", index),
                            &format!("/{}", command.name()),
                            command.description(),
                            typed_slash && index == self.selected_command,
                            t,
                        )
                        .focusable()
                        .on_click(cx.listener(
                            move |chat, _, window, cx| {
                                chat.composer_menu = None;
                                if typed_slash {
                                    chat.input.update(cx, |input, cx| input.clear(window, cx));
                                }
                                chat.run_slash_command(command, window, cx);
                            },
                        )),
                    );
                }
            }
        }
        Some(options)
    }
}
