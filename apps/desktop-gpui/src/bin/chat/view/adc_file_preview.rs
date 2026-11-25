use crate::*;
use dshr_plugin_host::{SlotTarget, VIEWER_TAB};
use serde_json::Value;

pub(crate) struct AdcFilePreview {
    pub(super) selected: bool,
    path: PathBuf,
    name: String,
    target: SlotTarget,
    request: Arc<()>,
    body: Option<Result<Value, String>>,
}

impl ChatApp {
    pub(crate) fn adc_file_target(&self, path: &std::path::Path) -> Option<SlotTarget> {
        let kind = dsh_deliverables::file_type(path)?;
        self.adc_panels
            .iter()
            .filter_map(|panel| panel.slot.as_ref())
            .find(|slot| slot.slot_id == VIEWER_TAB && slot.key.as_deref() == Some(&kind))
            .map(SlotTarget::from)
    }

    pub(crate) fn open_adc_file_preview(
        &mut self,
        call_seq: u64,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(target) = self.adc_file_target(&path) else {
            return;
        };
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let request = Arc::new(());
        self.adc_navigation.file = Some(AdcFilePreview {
            selected: true,
            path: path.clone(),
            name,
            target,
            request: request.clone(),
            body: None,
        });
        self.right_sidebar_open = true;
        let session = self.session_id.clone();
        let state = self.state.clone();
        let result = cx
            .background_executor()
            .spawn(async move { render_delivered_file(&state, &session, call_seq, &path, target) });
        cx.spawn(async move |this, cx| {
            let result = result.await;
            let _ = this.update(cx, |chat, cx| {
                if let Some(preview) = chat.adc_navigation.file.as_mut() {
                    if Arc::ptr_eq(&preview.request, &request) {
                        preview.body = Some(result);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn retain_adc_file_preview(&mut self) {
        if self
            .adc_navigation
            .file
            .as_ref()
            .is_some_and(|preview| self.adc_file_target(&preview.path) != Some(preview.target))
        {
            self.adc_navigation.file = None;
        }
    }

    pub(crate) fn close_adc_file_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adc_navigation.file = None;
        self.adc_navigation.select_panels();
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn adc_file_tab(&self, t: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let preview = self.adc_navigation.file.as_ref()?;
        Some(
            chrome_button("adc-file-tab", "", t)
                .role(Role::Tab)
                .aria_label(format!("预览 {}", preview.name))
                .aria_selected(preview.selected)
                .focusable()
                .max_w(px(140.0))
                .when(preview.selected, |tab| {
                    tab.bg(t.palette.color(t.palette.business_tertiary))
                })
                .child(
                    div()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(preview.name.clone()),
                )
                .on_click(cx.listener(|chat, _, _, cx| {
                    if let Some(preview) = &mut chat.adc_navigation.file {
                        preview.selected = true;
                        cx.notify();
                    }
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn adc_file_preview_body(
        &self,
        t: &Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self
            .adc_navigation
            .file
            .as_ref()
            .filter(|preview| preview.selected)?;
        let p = t.palette;
        let body = match &preview.body {
            Some(Ok(tree)) => {
                let weak = cx.entity().downgrade();
                let request = preview.request.clone();
                let handler: AdcActionHandler = std::rc::Rc::new(move |action, _, app| {
                    let _ = weak.update(app, |chat, cx| {
                        let active = chat.adc_navigation.file.as_ref().is_some_and(|preview| {
                            preview.selected
                                && Arc::ptr_eq(&preview.request, &request)
                                && chat.adc_file_target(&preview.path) == Some(preview.target)
                        });
                        if !active || chat.busy {
                            return;
                        }
                        match action {
                            AdcAction::Copy { text } => {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                            }
                            AdcAction::OpenExternal { url } => {
                                chat.request_open_external(url.clone(), cx)
                            }
                            _ => {}
                        }
                    });
                });
                adc_panel_body_with_handler(t, tree, handler).0
            }
            Some(Err(error)) => div()
                .text_color(p.color(p.state_error))
                .child(error.clone())
                .into_any_element(),
            None => div()
                .text_color(p.color(p.label_secondary))
                .child("正在读取文件预览")
                .into_any_element(),
        };
        let path = preview.path.clone();
        let request = preview.request.clone();
        Some(
            div()
                .id("adc-file-preview-body")
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    chrome_button("adc-file-system-open", "系统打开", t)
                        .focusable()
                        .on_click(cx.listener(move |chat, _, _, cx| {
                            if chat.adc_navigation.file.as_ref().is_some_and(|preview| {
                                preview.selected && Arc::ptr_eq(&preview.request, &request)
                            }) {
                                chat.open_delivered_in_system(&path, cx);
                            }
                        })),
                )
                .child(body)
                .into_any_element(),
        )
    }

    pub(crate) fn open_delivered_in_system(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        match dsh_deliverables::checked_file(&self.active_workspace, path) {
            Ok((path, _)) => cx.open_with_system(&path),
            Err(_) => {
                self.workspace_notice = Some("交付文件已不可用".to_owned());
                cx.notify();
            }
        }
    }
}

pub(crate) fn run_file_preview_flow(handle: gpui::WindowHandle<ChatApp>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let delay = std::time::Duration::from_millis(100);
        cx.background_executor().timer(delay).await;
        let opened = handle
            .update(cx, |chat, _, cx| {
                let delivered = chat
                    .rows
                    .iter()
                    .filter_map(|row| {
                        if let Row::Process(steps) = row {
                            Some(steps)
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .find_map(|step| {
                        if let Row::Tool {
                            call_seq,
                            delivered_files,
                            ..
                        } = step
                        {
                            delivered_files
                                .iter()
                                .find(|file| file.path == std::path::Path::new("report.TXT"))
                                .map(|file| (*call_seq, file.path.clone()))
                        } else {
                            None
                        }
                    });
                if let Some((seq, path)) = delivered {
                    chat.open_adc_file_preview(seq, path, cx);
                }
                chat.right_sidebar_open && chat.adc_navigation.file.is_some()
            })
            .unwrap_or(false);
        let mut rendered = false;
        for _ in 0..25 {
            if !opened {
                break;
            }
            cx.background_executor().timer(delay).await;
            let status = handle
                .update(cx, |chat, _, _| {
                    let preview = chat.adc_navigation.file.as_ref()?;
                    let body = preview.body.as_ref()?;
                    Some(
                        preview.selected
                            && chat.right_sidebar_open
                            && body.as_ref().is_ok_and(|tree| {
                                tree["props"]["value"] == "report.TXT:txt:ADC preview fixture"
                            }),
                    )
                })
                .unwrap_or_default();
            if let Some(status) = status {
                rendered = status;
                break;
            }
        }
        let closed = handle
            .update(cx, |chat, window, cx| {
                chat.close_adc_file_preview(window, cx);
                chat.adc_navigation.file.is_none()
                    && chat.adc_right_tab_body(&chat.theme(), cx).is_none()
            })
            .unwrap_or(false);
        println!("[adc-file-flow] opened={opened} rendered={rendered} closed={closed}");
        if !(opened && rendered && closed) {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}
