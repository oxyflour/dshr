use crate::*;

impl ChatApp {
    pub(crate) fn copy_message_button(
        &self,
        index: usize,
        text: &str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let text = text.to_owned();
        let copied = self
            .copied_message
            .as_ref()
            .is_some_and(|(session, row, _)| session == &self.session_id && *row == index);
        let (label, glyph, fallback) = if copied {
            ("已复制", "\u{e73e}", "✓")
        } else {
            ("复制消息", "\u{e8c8}", "⧉")
        };
        toolbar_icon_button(("copy-message", index), label, glyph, fallback, t)
            .size(px(24.0))
            .opacity(if copied { 1.0 } else { 0.0 })
            .group_hover("message-row", |style| style.opacity(1.0))
            .focus_visible(|style| style.opacity(1.0))
            .on_click(cx.listener(move |chat, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                let copied = (chat.session_id.clone(), index, std::time::Instant::now());
                chat.copied_message = Some(copied.clone());
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    let _ = this.update(cx, |chat, cx| {
                        if chat.copied_message.as_ref() == Some(&copied) {
                            chat.copied_message = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }))
    }

    pub(crate) fn message_attachments(
        &self,
        attachments: &[UserAttachment],
        row: usize,
        t: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let compact = attachments.len() > 1;
        div()
            .flex()
            .flex_wrap()
            .justify_end()
            .w_full()
            .max_w(t.layout.content_width * 0.702)
            .gap(px(6.0))
            .children(attachments.iter().enumerate().map(|(index, attachment)| {
                if let Some(file) = attachment.file.as_ref().filter(|file| stored_image(file)) {
                    let source = StoredAttachment {
                        root: spill_dir_for(&self.store.path_for(&self.session_id)),
                        file: file.clone(),
                    };
                    let result = self.message_thumbnail(&source, window, cx);
                    let (width, height) = if compact {
                        (108.0, 80.0)
                    } else {
                        (196.0, 144.0)
                    };
                    let name = attachment.name.clone();
                    let retry = matches!(result, Some(Err(_)));
                    let (image, fallback) = match result {
                        Some(Ok(image)) => (Some(image), ""),
                        Some(Err(_)) => (None, "图片不可用"),
                        None => (None, "加载图片…"),
                    };
                    return attachment_image_button(
                        format!("message-image-{row}-{index}"),
                        &name,
                        image,
                        fallback,
                        width,
                        height,
                        t,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if retry {
                            cx.remove_asset::<AttachmentThumbnail>(&source);
                        }
                        this.open_attachment_preview(
                            name.clone(),
                            PreviewSource::Stored(source.clone()),
                            window,
                            cx,
                        );
                    }))
                    .into_any_element();
                }
                let (kind, details) = match &attachment.file {
                    Some(file) if file.mime == "application/pdf" => {
                        ("PDF", Some(file_size_label(file.size)))
                    }
                    _ => ("附件", None),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .max_w_full()
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_base))
                    .text_color(p.color(p.label_primary))
                    .px(px(8.0))
                    .py(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.link))
                            .child(kind),
                    )
                    .child(
                        div()
                            .min_w(px(0.0))
                            .max_w(px(174.0))
                            .text_size(t.type_scale.small)
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(attachment.name.clone()),
                            )
                            .when_some(details, |card, size| {
                                card.child(div().text_color(p.color(p.label_secondary)).child(size))
                            }),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    pub(crate) fn message_thumbnail(
        &self,
        source: &StoredAttachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<Arc<RenderImage>, String>> {
        const LIMIT: usize = 128;
        let mut sources = self.attachment_thumbnails.borrow_mut();
        if let Some(index) = sources.iter().position(|entry| entry == source) {
            sources.remove(index);
        }
        sources.push_back(source.clone());
        if sources.len() > LIMIT {
            if let Some(oldest) = sources.pop_front() {
                if let Some(Ok(image)) = cx.fetch_asset::<AttachmentThumbnail>(&oldest) {
                    cx.drop_image(image, Some(window));
                }
                cx.remove_asset::<AttachmentThumbnail>(&oldest);
            }
        }
        window.use_asset::<AttachmentThumbnail>(source, cx)
    }

    pub(crate) fn open_attachment_preview(
        &mut self,
        name: String,
        source: PreviewSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_attachment_preview(window, cx);
        self.composer_menu = None;
        self.turn_menu_open = false;
        let focus = cx.focus_handle();
        let return_focus = window.focused(cx);
        let image = cx.background_executor().spawn(async move { source.load() });
        let load = cx.spawn(async move |this, cx| {
            let image = image.await;
            let _ = this.update(cx, |chat, cx| {
                if let Some(preview) = chat.attachment_preview.as_mut() {
                    preview.image = Some(image);
                    cx.notify();
                }
            });
        });
        window.focus(&focus, cx);
        self.attachment_preview = Some(AttachmentPreview {
            name,
            focus,
            return_focus,
            image: None,
            _load: load,
        });
        cx.notify();
    }

    pub(crate) fn close_attachment_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(preview) = self.attachment_preview.take() else {
            return false;
        };
        if let Some(Ok(image)) = preview.image {
            cx.drop_image(image, Some(window));
        }
        if let Some(focus) = preview.return_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
        true
    }

    pub(crate) fn image_preview_overlay(
        &self,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.attachment_preview.as_ref()?;
        let p = t.palette;
        let overlay = div()
            .id("attachment-preview-backdrop")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .bg(rgba(0x10131acc))
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_attachment_preview(window, cx);
            }))
            .child(
                div()
                    .id("attachment-preview")
                    .role(Role::Dialog)
                    .aria_label(format!("图片预览 {}", preview.name))
                    .size_full()
                    .max_w(px(1080.0))
                    .max_h(px(780.0))
                    .flex()
                    .flex_col()
                    .rounded(t.radii.lg)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_base))
                    .text_color(p.color(p.label_primary))
                    .p(px(12.0))
                    .gap(px(8.0))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(preview.name.clone()),
                            )
                            .child(
                                chrome_button("attachment-preview-close", "关闭", t)
                                    .track_focus(&preview.focus)
                                    .aria_label("关闭图片预览")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_attachment_preview(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match &preview.image {
                                Some(Ok(image)) => img(image.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Contain)
                                    .into_any_element(),
                                result => div()
                                    .text_size(t.type_scale.secondary)
                                    .text_color(p.color(p.label_secondary))
                                    .child(match result {
                                        Some(Err(error)) => error.clone(),
                                        _ => "加载图片…".to_owned(),
                                    })
                                    .into_any_element(),
                            }),
                    ),
            );
        Some(gpui::deferred(overlay).with_priority(10).into_any_element())
    }
}
