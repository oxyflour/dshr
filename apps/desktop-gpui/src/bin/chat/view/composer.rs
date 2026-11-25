use crate::*;

impl ChatApp {
    pub(crate) fn paste_attachments(
        &mut self,
        item: &mut ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy || self.attachment_load.is_some() {
            return false;
        }
        let source = item
            .entries
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
                    Some(AttachmentSource::Paths(paths.paths().to_vec()))
                }
                _ => None,
            })
            .or_else(|| {
                item.entries.iter_mut().find_map(|entry| match entry {
                    ClipboardEntry::Image(image) => Some(AttachmentSource::ClipboardImage(
                        std::mem::take(&mut image.bytes),
                    )),
                    _ => None,
                })
            });
        if let Some(source) = source {
            self.add_attachments(source, cx);
            return true;
        }
        false
    }

    pub(crate) fn add_attachments(&mut self, source: AttachmentSource, cx: &mut Context<Self>) {
        if self.busy || self.attachment_load.is_some() {
            return;
        }
        if self.attachments.len() == MAX_ATTACHMENTS {
            self.attachment_error = Some(format!("每条消息最多添加 {MAX_ATTACHMENTS} 个附件。"));
            cx.notify();
            return;
        }
        let remaining_files = MAX_ATTACHMENTS - self.attachments.len();
        let (mut used_text, mut used_media) = (0, 0);
        for attachment in &self.attachments {
            match &attachment.draft.content {
                AttachmentContent::Text(text) => used_text += text.len(),
                AttachmentContent::Media { bytes, .. } => used_media += bytes.len(),
            }
        }
        self.composer_menu = None;
        self.attachment_error = None;
        self.attachment_request = self.attachment_request.wrapping_add(1);
        let request = self.attachment_request;
        self.attachment_load = Some(request);
        let picker = matches!(source, AttachmentSource::Picker).then(|| {
            cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("添加图片、PDF 或 UTF-8 文本".into()),
            })
        });
        cx.spawn(async move |this, cx| {
            let source = match picker {
                Some(picker) => match picker.await {
                    Ok(Ok(Some(paths))) => Ok(AttachmentSource::Paths(paths)),
                    Ok(Ok(None)) | Err(_) => Ok(AttachmentSource::Paths(Vec::new())),
                    Ok(Err(err)) => Err(format!("无法打开文件选择器：{err}")),
                },
                None => Ok(source),
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let remaining_media = MAX_MEDIA_ATTACHMENT_BYTES - used_media;
                    let drafts = match source? {
                        AttachmentSource::Paths(paths) => read_attachments(
                            paths,
                            remaining_files,
                            MAX_TEXT_ATTACHMENT_BYTES - used_text,
                            remaining_media,
                        ),
                        AttachmentSource::ClipboardImage(bytes) => {
                            read_clipboard_image(bytes, remaining_media).map(|image| vec![image])
                        }
                        AttachmentSource::Picker => {
                            unreachable!("picker is resolved before loading")
                        }
                    }?;
                    Ok::<Vec<_>, String>(drafts.into_iter().map(ComposerAttachment::new).collect())
                })
                .await;
            let _ = this.update(cx, |chat, cx| {
                if chat.attachment_load != Some(request) {
                    return;
                }
                chat.attachment_load = None;
                match result {
                    Ok(files) => chat.attachments.extend(files),
                    Err(err) => chat.attachment_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn attachment_chips(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(10.0))
            .children(self.attachments.iter().enumerate().map(|(index, file)| {
                let draft = &file.draft;
                let tile = match &draft.content {
                    AttachmentContent::Media { format, bytes }
                        if *format != AttachmentFormat::Pdf =>
                    {
                        let (format, bytes, name) = (*format, bytes.clone(), draft.name.clone());
                        attachment_image_button(
                            ("draft-image", index),
                            &draft.name,
                            file.thumbnail.clone(),
                            "图片",
                            66.0,
                            46.0,
                            t,
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_attachment_preview(
                                name.clone(),
                                PreviewSource::Draft {
                                    format,
                                    bytes: bytes.clone(),
                                },
                                window,
                                cx,
                            );
                        }))
                        .into_any_element()
                    }
                    _ => {
                        let label = match &draft.content {
                            AttachmentContent::Media { bytes, .. } => {
                                format!("PDF · {}", file_size_label(bytes.len() as u64))
                            }
                            AttachmentContent::Text(_) => "文本".to_owned(),
                        };
                        div()
                            .flex_none()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.link))
                            .child(label)
                            .into_any_element()
                    }
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .max_w_full()
                    .rounded(t.radii.sm)
                    .bg(p.color(p.tag))
                    .pl(px(8.0))
                    .child(tile)
                    .when(file.thumbnail.is_none(), |chip| {
                        chip.child(
                            div()
                                .min_w(px(0.0))
                                .max_w(px(150.0))
                                .overflow_hidden()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .text_overflow(TextOverflow::Truncate("…".into()))
                                .child(draft.name.clone()),
                        )
                    })
                    .child(
                        chrome_button(("remove-attachment", index), "×", t)
                            .h(px(24.0))
                            .aria_label(format!("移除附件 {}", draft.name))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if index < this.attachments.len() {
                                    if let Some(image) = this.attachments.remove(index).thumbnail {
                                        cx.drop_image(image, Some(window));
                                    }
                                    this.attachment_error = None;
                                    cx.notify();
                                }
                            })),
                    )
            }))
            .into_any_element()
    }

    pub(crate) fn composer(
        &self,
        t: &Theme,
        hero: bool,
        body_width: gpui::Pixels,
        viewport_height: gpui::Pixels,
        menu_top: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let row_width = (body_width - t.layout.composer_clearance * 2)
            .min(t.layout.composer_max_width)
            - px(16.0);
        let compact_toolbar = row_width <= px(560.0);
        let model_icon_only = row_width < px(300.0);
        let choosing_command = self.composer_menu == Some(ComposerMenu::Commands);
        let current_permission = self.permission.mode();
        let can_choose_permission = current_permission.is_some();
        let permission = current_permission.map_or("所有工具需审批", permission_label);
        let has_models = !self.model_options.is_empty();
        let model = if has_models {
            self.settings.model.name()
        } else {
            "配置模型"
        };
        let effort =
            (!self.settings.model.reasoning_levels().is_empty()).then(|| self.reasoning_label());
        let model_label = if let Some(effort) = effort {
            format!("选择模型 · {model} · 推理等级 {effort}")
        } else if has_models {
            format!("选择模型 · {model}")
        } else {
            "配置模型".to_owned()
        };
        let can_add = !self.busy && self.attachment_load.is_none();
        let model_ready = self.settings.model.is_configured();
        let can_submit = choosing_command
            || (model_ready && !self.model_settings.pending && !self.tool_approvals_loading);
        let stopping = self.busy && self.cancel.load(Ordering::Relaxed);
        let can_send = !stopping
            && (self.busy
                || (can_submit
                    && self.attachment_load.is_none()
                    && (!self.attachments.is_empty()
                        || !self.input.read(cx).content().trim().is_empty())));
        let (button_label, button_glyph, button_fallback) = if stopping {
            ("正在停止…", "", "")
        } else if self.busy {
            ("停止生成", "", "")
        } else if choosing_command {
            ("运行所选指令", "\u{e768}", "▶")
        } else if !model_ready {
            ("配置模型后发送", "\u{e898}", "↑")
        } else {
            ("发送消息", "\u{e898}", "↑")
        };
        let placeholder = if !self.attachments.is_empty() {
            ""
        } else if hero {
            "描述你想做的事，或输入 / 查看指令"
        } else {
            "发消息，或输入 / 查看指令"
        };
        // 矮窗给消息区和按钮留空间，桌面大窗最多显示 14 行草稿。
        let draft_max_height = (viewport_height * 0.45).clamp(px(48.0), px(336.0));
        let menu_max_height = (viewport_height * 0.48).clamp(px(108.0), px(320.0));
        let menu_max_width = (body_width - t.layout.composer_clearance * 2).max(px(0.0));
        self.input.update(cx, |input, cx| {
            input.style_draft(
                placeholder,
                draft_max_height,
                p.color(p.label_caption).into(),
                p.color(p.business_primary).into(),
                cx,
            );
        });

        let chevron = |open| {
            desktop_icon(
                if open { "\u{e70e}" } else { "\u{e70d}" },
                if open { "▴" } else { "▾" },
            )
            .text_color(p.color(p.label_caption))
            .text_size(px(10.0))
        };
        let permission_open = self.composer_menu == Some(ComposerMenu::Permission);
        let permission_picker = div()
            .id("permission-picker")
            .flex()
            .items_center()
            .min_w(px(0.0))
            .gap(px(4.0))
            .h(px(28.0))
            .pl(px(8.0))
            .pr(px(4.0))
            .rounded(t.radii.sm)
            .text_size(t.type_scale.secondary)
            .line_height(px(20.0))
            .text_color(p.color(p.label_secondary))
            .font_weight(FontWeight::MEDIUM)
            .child(desktop_icon("\u{ea18}", "◇"))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(permission),
            )
            .when(can_choose_permission, |button| {
                button
                    .child(chevron(permission_open))
                    .role(Role::Button)
                    .aria_label(format!("选择工具权限，当前 {permission}"))
                    .aria_expanded(permission_open)
                    .focusable()
                    .cursor_pointer()
                    .hover(|style| style.bg(p.color(p.interactive_hover)))
                    .on_click(cx.listener(|chat, _, _, cx| {
                        chat.toggle_composer_menu(ComposerMenu::Permission, cx);
                    }))
            });
        let model_menu = self.composer_menu.filter(|menu| {
            matches!(
                menu,
                ComposerMenu::Model | ComposerMenu::ModelList | ComposerMenu::Reasoning
            )
        });
        let model_open = model_menu.is_some();
        let model_picker = div()
            .id("model-picker")
            .flex()
            .items_center()
            .min_w(px(0.0))
            .max_w(px(360.0))
            .gap(px(4.0))
            .h(px(28.0))
            .pl(px(8.0))
            .pr(px(4.0))
            .rounded(t.radii.sm)
            .text_size(t.type_scale.secondary)
            .line_height(px(20.0))
            .text_color(p.color(p.label_secondary))
            .when(model_icon_only, |button| {
                button.child(desktop_icon("\u{e950}", "◇"))
            })
            .when(!model_icon_only, |button| {
                button
                    .child(
                        div()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(model.to_owned()),
                    )
                    .when_some(effort, |button, effort| {
                        button.child(
                            div()
                                .flex_none()
                                .text_color(p.color(p.label_caption))
                                .child(effort),
                        )
                    })
            })
            .when(!self.busy && !self.model_settings.pending, |button| {
                button
                    .when(has_models, |button| {
                        button.child(chevron(model_open)).aria_expanded(model_open)
                    })
                    .role(Role::Button)
                    .aria_label(model_label.clone())
                    .focusable()
                    .cursor_pointer()
                    .hover(|style| style.bg(p.color(p.interactive_hover)))
                    .when(!model_open && (has_models || model_icon_only), |button| {
                        button.tooltip(toolbar_tooltip(model_label, *t))
                    })
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        if has_models {
                            chat.toggle_composer_menu(ComposerMenu::Model, cx);
                        } else {
                            chat.open_settings(settings_view::SettingsTab::Models, window, cx);
                        }
                    }))
            });
        let add_open = self.composer_menu == Some(ComposerMenu::Add);
        let menu = |kind, cx: &mut Context<Self>| {
            self.composer_options(kind, t, menu_max_width, menu_max_height, cx)
        };
        let add_button =
            toolbar_icon_button("composer-add", "添加附件或调用指令", "\u{e710}", "+", t)
                .rounded_full()
                .bg(p.color(if add_open {
                    p.interactive_hover_solid
                } else {
                    p.tag
                }))
                .aria_expanded(add_open)
                .when(can_add, |button| {
                    button.on_click(cx.listener(|chat, _, _, cx| {
                        chat.toggle_composer_menu(ComposerMenu::Add, cx);
                    }))
                })
                .when(!can_add, |button| button.opacity(0.5).cursor_default());
        // 「+」菜单定位在输入卡顶部，但再次点击工具栏里的按钮仍应关闭它。
        let add_bounds = add_open.then(|| std::rc::Rc::new(std::cell::Cell::new(None)));
        let add_button = if let Some(bounds) = add_bounds.clone() {
            div()
                .flex_none()
                .child(add_button)
                .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
                .into_any_element()
        } else {
            add_button.into_any_element()
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(if compact_toolbar { 8.0 } else { 12.0 }))
            .pt(px(2.0))
            .px(px(8.0))
            .pb(px(6.0))
            .child(add_button)
            .child(popover::attach_dismissible(
                permission_picker,
                menu(ComposerMenu::Permission, cx),
                gpui::Anchor::TopLeft,
                gpui::Anchor::BottomLeft,
                cx.listener(Self::dismiss_composer_menu),
            ))
            .children(self.adc_composer_slot("conversation.input.left", t, cx))
            .child(div().flex_1().min_w(px(0.0)))
            .child(popover::attach_dismissible(
                model_picker,
                model_menu.and_then(|kind| menu(kind, cx)),
                gpui::Anchor::TopRight,
                gpui::Anchor::BottomRight,
                cx.listener(Self::dismiss_composer_menu),
            ))
            .when(stopping, |toolbar| {
                toolbar.child(
                    div()
                        .flex_none()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_secondary))
                        .child("正在停止…"),
                )
            })
            .children(self.adc_composer_slot("conversation.input.right", t, cx))
            .child(
                div()
                    .id("send-button")
                    .role(Role::Button)
                    .aria_label(button_label)
                    .size(px(34.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(p.color(p.button_info))
                    .text_color(gpui::white())
                    .child(if self.busy {
                        div().size(px(10.0)).rounded(px(3.0)).bg(gpui::white())
                    } else {
                        desktop_icon(button_glyph, button_fallback).text_size(px(16.0))
                    })
                    .when(!can_send, |button| button.opacity(0.4))
                    .when(can_send, |button| {
                        button
                            .focusable()
                            .cursor_pointer()
                            .hover(|style| style.bg(p.color(p.button_info_hover)))
                            .tooltip(toolbar_tooltip(button_label, *t))
                            .on_click(
                                cx.listener(|chat, _, window, cx| chat.button_click(window, cx)),
                            )
                    }),
            );
        let card = div()
            .id("composer-card")
            .w_full()
            .max_w(t.layout.composer_max_width)
            .relative()
            .flex()
            .flex_col()
            .pt(px(7.0))
            .gap(px(12.0))
            .rounded(t.radii.panel)
            .border_1()
            .border_color(gpui::transparent_black())
            .shadow(vec![
                gpui::BoxShadow::new(px(0.0), px(4.0), gpui::rgba(0x0000_0008).into())
                    .blur_radius(px(16.0)),
                gpui::BoxShadow::new(px(0.0), px(0.0), gpui::rgba(0x0000_0008).into())
                    .blur_radius(px(24.0)),
            ])
            .child(composer_surface::surface(t, can_add))
            .when(can_add, |composer| {
                composer
                    .drag_over::<ExternalPaths>(|style, _, _, _| style)
                    .on_drop(cx.listener(|chat, paths: &ExternalPaths, _, cx| {
                        chat.add_attachments(AttachmentSource::Paths(paths.paths().to_vec()), cx);
                    }))
            })
            .when(!self.attachments.is_empty(), |composer| {
                composer.child(self.attachment_chips(t, cx))
            })
            .child(
                div()
                    .id("composer-editing-area")
                    .pl(px(13.0))
                    .pr(px(11.0))
                    .pt(px(4.0))
                    .min_h(px(if hero { 52.0 } else { 36.0 }))
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .when(choosing_command, |input| input.key_context("DshrSlashMenu"))
                    .text_color(p.color(p.label_primary))
                    .child(self.input.clone())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|chat, _, window, cx| {
                            if !chat.input.read(cx).focus_handle(cx).is_focused(window) {
                                chat.focus_composer(window, cx);
                            }
                        }),
                    ),
            )
            .child(toolbar)
            .when(add_open || choosing_command, |card| {
                let kind = if add_open {
                    ComposerMenu::Add
                } else {
                    ComposerMenu::Commands
                };
                let dismiss = cx.listener(Self::dismiss_composer_menu);
                card.child(div().absolute().top_0().left_0().child(popover::attach_to(
                    div().size(px(0.0)),
                    menu(kind, cx).map(|menu| {
                        move |anchor: Bounds<gpui::Pixels>| {
                            // 为标题栏和输入卡各留 8 px，避免菜单覆盖窗口控制按钮或输入框。
                            let room_above = (anchor.top() - menu_top - px(16.0)).max(px(0.0));
                            popover::dismiss_on_outside(
                                menu.max_h(menu_max_height.min(room_above)),
                                move |position| {
                                    add_bounds
                                        .as_ref()
                                        .and_then(|bounds| bounds.get())
                                        .is_some_and(|bounds| bounds.contains(&position))
                                },
                                dismiss,
                            )
                        }
                    }),
                    gpui::Anchor::TopLeft,
                    gpui::Anchor::BottomLeft,
                )))
            });

        div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .px(t.layout.composer_clearance)
            .pb(px(if hero { 0.0 } else { 4.0 }))
            .when(hero, |composer| {
                composer.child(
                    div()
                        .flex()
                        .w_full()
                        .max_w(t.layout.composer_max_width)
                        .pl(px(20.0))
                        .pr(px(16.0))
                        .mb(px(8.0))
                        .items_center()
                        .gap(px(8.0))
                        .child(self.workspace_picker(t, menu_max_width, cx))
                        .when(self.blank_root, |row| {
                            row.child(self.preset_picker(t, menu_max_width, menu_max_height, cx))
                        }),
                )
            })
            .child(card)
            .when_some(
                self.preset_notice.as_ref().filter(|_| hero),
                |composer, notice| {
                    composer.child(
                        div()
                            .max_w(t.layout.composer_max_width)
                            .mt(px(6.0))
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.state_error))
                            .child(notice.clone()),
                    )
                },
            )
            .when_some(self.attachment_error.as_ref(), |composer, error| {
                composer.child(
                    div()
                        .max_w(t.layout.composer_max_width)
                        .mt(px(6.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child(error.clone()),
                )
            })
            .when(self.attachment_load.is_some(), |composer| {
                composer.child(
                    div()
                        .mt(px(6.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_secondary))
                        .child("正在添加附件…"),
                )
            })
            .when(!hero || choosing_command, |composer| {
                composer.child(
                    div()
                        .id("composer-key-hint")
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .h(px(26.0))
                        .pt(px(4.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_tertiary))
                        .child(if choosing_command {
                            "↑↓ 选择 · 回车运行 · Esc 关闭"
                        } else {
                            "回车发送"
                        })
                        .when(!choosing_command, |hint| {
                            let shortcuts = format!("Shift+Enter 换行 · {}", chat_shortcut_hint());
                            hint.aria_description(shortcuts.clone())
                                .child(
                                    desktop_icon("\u{e92e}", "⌨")
                                        .text_size(px(11.0))
                                        .line_height(px(14.0)),
                                )
                                .tooltip(toolbar_tooltip(shortcuts, *t))
                        }),
                )
            })
            .into_any_element()
    }

    pub(crate) fn empty_composer(
        &self,
        composer: AnyElement,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let offer_build = self.active_preset.as_deref() == Some(creator::CORDIS_PRESET)
            && creator::build_available().is_err()
            && runtime_components::asset(runtime_components::Component::CreatorBuild).is_ok();
        div()
            .id("empty-composer")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .pb(px(32.0))
            .child(div().flex_1().min_h(px(12.0)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .mb(px(12.0))
                    .child(
                        div()
                            .size(px(34.0))
                            .flex_none()
                            .rounded(t.radii.md)
                            .bg(p.color(p.business_tertiary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.link))
                            .child("D"),
                    )
                    .child(
                        div()
                            .text_size(px(24.0))
                            .line_height(px(32.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.label_primary))
                            .child("开始对话"),
                    ),
            )
            .child(composer)
            .when(offer_build, |view| {
                view.child(div().flex().justify_center().mt(px(8.0)).child(
                    chrome_button("creator-runtime-settings", "安装创造模式构建组件", t).on_click(
                        cx.listener(|chat, _, window, cx| {
                            chat.open_settings(settings_view::SettingsTab::Components, window, cx);
                        }),
                    ),
                ))
            })
            .child(div().flex_1().min_h(px(12.0)))
            .into_any_element()
    }
}
