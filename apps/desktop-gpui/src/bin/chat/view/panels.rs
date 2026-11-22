use crate::*;

impl ChatApp {
    /// ADC Input/Textarea 点击：打开真实 TextInput 模态；提交时把值注入 onChange。
    pub(crate) fn open_adc_editor(
        &mut self,
        plugin_id: String,
        view: String,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(on_change) = args
            .get("on_change")
            .and_then(|value| parse_action(value).ok())
        else {
            eprintln!("[adc] 编辑请求缺少可解析的 on_change");
            return;
        };
        let path = args["path"].as_str().unwrap_or_default().to_owned();
        let value = args["value"].as_str().unwrap_or_default().to_owned();
        let multiline = args["multiline"].as_bool().unwrap_or(false);
        let placeholder = if multiline {
            "编辑多行值"
        } else {
            "编辑值"
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx));
        input.update(cx, |input, cx| input.set_content(value, window, cx));
        let handle = input.read(cx).focus_handle(cx);
        self.adc_editor = Some(AdcEditor {
            plugin_id,
            view,
            path,
            on_change,
            input,
            multiline,
        });
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(crate) fn commit_adc_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.adc_editor.take() else {
            return;
        };
        let value = editor.input.read(cx).content().to_owned();
        match editor.on_change {
            AdcAction::Call { method, mut args } => {
                if let Some(object) = args.as_object_mut() {
                    object.insert("value".to_owned(), json!(value));
                }
                self.dispatch_adc_action(
                    &editor.plugin_id,
                    &editor.view,
                    "call",
                    json!({ "method": method, "args": args }),
                    cx,
                );
            }
            AdcAction::Command { id } => {
                self.dispatch_adc_action(
                    &editor.plugin_id,
                    &editor.view,
                    "command",
                    json!({ "id": id }),
                    cx,
                );
            }
            other => {
                eprintln!(
                    "[adc] 编辑提交只支持 call/command（path={}）: {other:?}",
                    editor.path
                );
            }
        }
        cx.notify();
    }

    pub(crate) fn cancel_adc_edit(&mut self, cx: &mut Context<Self>) {
        self.adc_editor = None;
        cx.notify();
    }

    pub(crate) fn adc_editor_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.adc_editor.as_ref()?;
        let p = t.palette;
        let input = editor.input.clone();
        let save = cx.listener(|this, _, _window, cx| this.commit_adc_edit(cx));
        let cancel = cx.listener(|this, _, _window, cx| this.cancel_adc_edit(cx));
        let label = if editor.multiline {
            format!("编辑多行值 · {}", editor.path)
        } else {
            format!("编辑值 · {}", editor.path)
        };
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
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_caption))
                                        .child(label),
                                )
                                .child(div().w_full().child(input)),
                        )
                        .child(
                            div()
                                .id("adc-edit-cancel")
                                .role(Role::Button)
                                .aria_label("取消编辑")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.button_elevated))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_primary))
                                .child("取消")
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("adc-edit-save")
                                .role(Role::Button)
                                .aria_label("保存编辑")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .bg(p.color(p.button_info))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.bg_base))
                                .child("保存")
                                .on_click(save),
                        ),
                )
                .into_any_element(),
        )
    }

    /// ADC `openExternal` 必须先确认（M4 W3）；确认前只保存 URL，不打开。
    pub(crate) fn request_open_external(&mut self, url: String, cx: &mut Context<Self>) {
        self.pending_external = Some(url);
        cx.notify();
    }

    pub(crate) fn external_confirm_bar(
        &self,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let url = self.pending_external.as_ref()?;
        let p = t.palette;
        let mut display: String = url.chars().take(160).collect();
        if url.chars().count() > 160 {
            display.push('…');
        }
        let cancel = cx.listener(|this, _, _window, cx| {
            this.pending_external = None;
            cx.notify();
        });
        let open = cx.listener(|this, _, _window, cx| {
            if let Some(url) = this.pending_external.take() {
                cx.open_url(&url);
            }
            cx.notify();
        });
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
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(t.type_scale.secondary)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .child("打开外部链接？"),
                                )
                                .child(
                                    div()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_caption))
                                        .text_overflow(TextOverflow::Truncate("…".into()))
                                        .child(display),
                                ),
                        )
                        .child(
                            div()
                                .id("external-deny")
                                .role(Role::Button)
                                .aria_label("取消打开外部链接")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.button_elevated))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_primary))
                                .child("取消")
                                .on_click(cancel),
                        )
                        .child(
                            div()
                                .id("external-open")
                                .role(Role::Button)
                                .aria_label("确认打开外部链接")
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(t.radii.sm)
                                .bg(p.color(p.button_info))
                                .cursor_pointer()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.bg_base))
                                .child("打开")
                                .on_click(open),
                        ),
                )
                .into_any_element(),
        )
    }

    /// 把插件面板的 `call`/`command` 动作派发给插件，然后刷新面板树（M4 W3）。
    ///
    /// 动作是 fire-and-forget；插件更新自身状态后，`refresh_adc_panels` 再发
    /// `ui.render` 取新树。失败只进日志，不阻塞 UI 其它部分。
    pub(crate) fn dispatch_adc_action(
        &mut self,
        plugin_id: &str,
        view: &str,
        action: &str,
        payload: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let result = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = guard.apply_adc_action(plugin_id, view, action, &payload);
            if result.is_ok() {
                self.adc_panels = guard.adc_panels.clone();
            }
            result
        };
        if let Err(error) = result {
            eprintln!("[adc] action `{action}` on {plugin_id}/{view} 失败: {error}");
        }
        cx.notify();
    }

    pub(crate) fn inline_trial_call(&self, row: &Row) -> Option<u64> {
        match row {
            Row::Tool { call_seq, name, .. }
                if !self.activity_open
                    && name == creator::TRY_PLUGIN_TOOL
                    && self
                        .adc_panels
                        .iter()
                        .any(|panel| panel.trial_call_seq == Some(*call_seq)) =>
            {
                Some(*call_seq)
            }
            _ => None,
        }
    }

    pub(crate) fn plugin_panels(
        &self,
        trial_call: Option<u64>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let mut list = div().flex().flex_col().gap(px(8.0)).w_full();
        // 绘制快照时不持有 ChatState 锁，也不向插件请求渲染。
        for (index, panel) in self
            .adc_panels
            .iter()
            .enumerate()
            .filter(|(_, panel)| trial_call.is_none() || panel.trial_call_seq == trial_call)
        {
            let mut card = div()
                .id(("adc-panel", index))
                .w_full()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(t.radii.sm)
                .border_1()
                .border_color(p.color(p.border_l2))
                .bg(p.color(p.bg_layer_1))
                .child(
                    div()
                        .w_full()
                        .text_size(t.type_scale.small)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.color(p.label_secondary))
                        .text_overflow(TextOverflow::Truncate("…".into()))
                        .child(if trial_call.is_some() {
                            panel.title.clone()
                        } else {
                            format!("{} · {}", panel.plugin_id, panel.title)
                        }),
                );
            match &panel.tree {
                Some(tree) => {
                    let weak = cx.entity().downgrade();
                    let plugin_id = panel.plugin_id.clone();
                    let view = panel.view.clone();
                    let handler: AdcActionHandler =
                        std::rc::Rc::new(move |action, window, app| match action {
                            AdcAction::Copy { text } => {
                                app.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                eprintln!("[adc] copy {} bytes", text.len());
                            }
                            AdcAction::Call { method, args } => {
                                if method == "__adc_edit" {
                                    let plugin_id = plugin_id.clone();
                                    let view = view.clone();
                                    let args = args.clone();
                                    let _ = weak.update(app, |chat, cx| {
                                        chat.open_adc_editor(plugin_id, view, args, window, cx);
                                    });
                                } else {
                                    let payload = json!({ "method": method, "args": args });
                                    let _ = weak.update(app, |chat, cx| {
                                        chat.dispatch_adc_action(
                                            &plugin_id, &view, "call", payload, cx,
                                        );
                                    });
                                }
                            }
                            AdcAction::Command { id } => {
                                let payload = json!({ "id": id });
                                let _ = weak.update(app, |chat, cx| {
                                    chat.dispatch_adc_action(
                                        &plugin_id, &view, "command", payload, cx,
                                    );
                                });
                            }
                            AdcAction::OpenExternal { url } => {
                                let url = url.clone();
                                let _ = weak.update(app, |chat, cx| {
                                    chat.request_open_external(url, cx);
                                });
                            }
                            _ => {
                                eprintln!("[adc] 未知动作（ADC-v1 新增类型），已忽略");
                            }
                        });
                    let (body, diagnostics) = if self.busy {
                        adc_panel_body(t, tree)
                    } else {
                        adc_panel_body_with_handler(t, tree, handler)
                    };
                    card = card.child(body);
                    if !diagnostics.is_empty() {
                        // 未知元素 Placeholder / 配额裁剪仍然可见，诊断只作提示。
                        let errors = diagnostics
                            .iter()
                            .filter(|diagnostic| diagnostic.is_error())
                            .count();
                        card = card.child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.state_warn))
                                .child(format!(
                                    "ADC 诊断 {} 条（error {errors}）",
                                    diagnostics.len()
                                )),
                        );
                    }
                }
                None => {
                    let message = panel
                        .error
                        .clone()
                        .unwrap_or_else(|| "（没有可渲染的 view）".to_owned());
                    card = card.child(
                        div()
                            .text_size(t.type_scale.small)
                            .line_height(t.type_scale.small_line)
                            .text_color(p.color(p.state_error))
                            .child(format!("面板渲染失败：{message}")),
                    );
                }
            }
            list = list.child(card);
        }

        list.into_any_element()
    }
}
