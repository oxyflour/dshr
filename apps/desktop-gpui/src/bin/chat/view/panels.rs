use crate::*;
use dshr_plugin_host::SlotTarget;

impl ChatApp {
    /// ADC Input/Textarea 点击：打开真实 TextInput 模态；提交时把值注入 onChange。
    pub(crate) fn open_adc_editor(
        &mut self,
        plugin_id: String,
        view: String,
        slot: Option<SlotTarget>,
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
            slot,
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
                    editor.slot,
                    "call",
                    json!({ "method": method, "args": args }),
                    cx,
                );
            }
            AdcAction::Command { id } => {
                self.dispatch_adc_action(
                    &editor.plugin_id,
                    &editor.view,
                    editor.slot,
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
        slot: Option<SlotTarget>,
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
            let result = match slot {
                Some(target) => guard.apply_adc_slot_action(target, action, &payload),
                None => guard.apply_adc_action(plugin_id, view, action, &payload),
            };
            if result.is_ok() {
                self.adc_panels = guard.adc_panels.clone();
                self.adc_actions = guard.adc_action_entries();
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

    pub(crate) fn adc_body(
        &self,
        panel: &AdcPanelView,
        tree: &serde_json::Value,
        t: &Theme,
        cx: &Context<Self>,
    ) -> (AnyElement, Vec<dshr_ui_contributions::Diagnostic>) {
        if self.busy {
            return adc_panel_body(t, tree);
        }
        let weak = cx.entity().downgrade();
        let plugin = panel.plugin_id.clone();
        let view = panel.view.clone();
        let slot = panel.slot.as_ref().map(SlotTarget::from);
        let trial = panel.trial;
        let trial_call_seq = panel.trial_call_seq;
        let tool_call_seq = panel.tool_call_seq;
        let session = self.session_id.clone();
        let handler: AdcActionHandler = std::rc::Rc::new(move |action, window, app| {
            let _ = weak.update(app, |chat, cx| {
                if chat.busy
                    || chat.session_id != session
                    || slot.is_some_and(|target| {
                        !chat.adc_panels.iter().any(|panel| {
                            panel.tree.is_some()
                                && panel.slot.as_ref().map(SlotTarget::from) == Some(target)
                                && panel.tool_call_seq == tool_call_seq
                        })
                    })
                    || (trial
                        && slot.is_none()
                        && !chat.adc_panels.iter().any(|panel| {
                            panel.trial
                                && panel.tree.is_some()
                                && panel.plugin_id == plugin
                                && panel.view == view
                                && panel.trial_call_seq == trial_call_seq
                        }))
                {
                    return;
                }
                match action {
                    AdcAction::Call { .. } | AdcAction::Command { .. }
                        if tool_call_seq.is_some() => {}
                    AdcAction::Copy { text } => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    }
                    AdcAction::Call { method, args } if method == "__adc_edit" => {
                        chat.open_adc_editor(
                            plugin.clone(),
                            view.clone(),
                            slot,
                            args.clone(),
                            window,
                            cx,
                        );
                    }
                    AdcAction::Call { method, args } => {
                        let payload = json!({ "method": method, "args": args });
                        chat.dispatch_adc_action(&plugin, &view, slot, "call", payload, cx);
                    }
                    AdcAction::Command { id } => {
                        chat.run_adc_view_command(&plugin, trial, id, cx);
                    }
                    AdcAction::OpenExternal { url } => chat.request_open_external(url.clone(), cx),
                    _ => eprintln!("[adc] 未知动作（ADC-v1 新增类型），已忽略"),
                }
            });
        });
        adc_panel_body_with_handler(t, tree, handler)
    }

    pub(crate) fn adc_tool_card_views(
        &self,
        call_seq: u64,
        t: &Theme,
        cx: &Context<Self>,
    ) -> (Option<AnyElement>, Vec<AnyElement>) {
        let mut body = None;
        let mut extras = Vec::new();
        for view in self
            .adc_panels
            .iter()
            .filter(|view| view.tool_call_seq == Some(call_seq))
        {
            let (Some(slot), Some(tree)) = (&view.slot, &view.tree) else {
                continue;
            };
            if !dshr_plugin_host::is_tool_view_slot(&slot.slot_id) {
                continue;
            }
            let element = div()
                .id(gpui::SharedString::from(format!(
                    "adc-tool-{call_seq}-{}-{}",
                    slot.generation,
                    slot.handle.value()
                )))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .child(self.adc_body(view, tree, t, cx).0)
                .into_any_element();
            if slot.slot_id == dshr_plugin_host::TOOL_CALL_VIEW {
                body = Some(element);
            } else {
                extras.push(element);
            }
        }
        (body, extras)
    }

    pub(crate) fn adc_chat_node_view(
        &self,
        node: &dshr_conversation_model::ChatNode,
    ) -> Option<(&AdcPanelView, &serde_json::Value)> {
        self.adc_panels.iter().find_map(|view| {
            let slot = view.slot.as_ref()?;
            if view.tool_call_seq != Some(node.call_seq)
                || view.plugin_id != node.plugin_id
                || slot.slot_id != dshr_plugin_host::CHAT_NODE
                || slot.key.as_deref() != Some(&node.key)
            {
                return None;
            }
            Some((view, view.tree.as_ref()?))
        })
    }

    pub(crate) fn adc_slot_elements(
        &self,
        name: &str,
        t: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        self.adc_panels
            .iter()
            .filter_map(|panel| {
                let slot = panel.slot.as_ref().filter(|slot| slot.slot_id == name)?;
                let tree = panel.tree.as_ref()?;
                let (body, _) = self.adc_body(panel, tree, t, cx);
                Some(
                    div()
                        .id(format!(
                            "adc-{}-{}-{}",
                            slot.slot_id,
                            slot.generation,
                            slot.handle.value()
                        ))
                        .min_w(px(0.0))
                        .child(body)
                        .into_any_element(),
                )
            })
            .collect()
    }

    pub(crate) fn adc_composer_slot(
        &self,
        name: &'static str,
        t: &Theme,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let views = self.adc_slot_elements(name, t, cx);
        (!views.is_empty()).then(|| {
            div()
                .id(name)
                .flex()
                .min_w(px(0.0))
                .max_w(px(144.0))
                .max_h(px(36.0))
                .overflow_x_scroll()
                .children(views)
        })
    }

    pub(crate) fn has_workspace_panels(&self) -> bool {
        self.adc_panels.iter().any(|panel| panel.slot.is_none())
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
        for (index, panel) in self.adc_panels.iter().enumerate().filter(|(_, panel)| {
            panel.slot.is_none() && (trial_call.is_none() || panel.trial_call_seq == trial_call)
        }) {
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
                    let (body, diagnostics) = self.adc_body(panel, tree, t, cx);
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
