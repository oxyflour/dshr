use crate::*;

impl ChatApp {
    /// 请求停止生成：provider 在下一个 SSE 行边界返回已生成的部分。
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.busy || self.cancel.swap(true, Ordering::Relaxed) {
            return;
        }
        // 审批等待中的调用立即结束（否则 UI 的审批条会一直挂到 300s 超时）。
        self.approvals.cancel_pending();
        self.pending_approvals = 0;
        cx.notify();
    }

    /// 回车提交（TextInput 的 Submit action）。
    ///
    /// 返回 `true` 表示已受理，输入框随后自行清空；生成中返回 `false`，内容保留。
    pub(crate) fn on_submit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy
            || self.tool_approvals_loading
            || self.model_settings.pending
            || !self.settings.model.is_configured()
            || self.attachment_preview.is_some()
            || self.attachment_load.is_some()
            || (text.trim().is_empty() && self.attachments.is_empty())
        {
            return false;
        }
        if !self.settings.model.supports_attachments()
            && self
                .attachments
                .iter()
                .any(|file| matches!(file.draft.content, AttachmentContent::Media { .. }))
        {
            self.attachment_error = Some(format!(
                "{} 不支持图片或 PDF，请切换到 DeepSeek Flash 或兼容模型。",
                self.settings.model.name()
            ));
            cx.notify();
            return false;
        }
        self.start_turn(text.to_owned(), window, cx);
        true
    }

    /// 发送按钮：空闲时发送，生成中变成"停止"。
    pub(crate) fn button_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.stop(cx);
            return;
        }
        let text = self.input.read(cx).content().to_owned();
        if self.submit_composer(&text, window, cx) {
            self.input.update(cx, |input, cx| input.clear(window, cx));
        }
    }

    pub(crate) fn start_turn(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.activity_open = false;
        self.composer_menu = None;
        self.session_menu = None;
        self.header_menu = None;
        self.turn_menu_open = false;
        self.turn_jump = None;
        self.turn_focus = None;
        self.follow_transcript = true;
        let attachments: Vec<_> = self
            .attachments
            .drain(..)
            .map(|attachment| {
                if let Some(image) = attachment.thumbnail {
                    cx.drop_image(image, Some(window));
                }
                attachment.draft
            })
            .collect();
        self.attachment_error = None;
        self.busy = true;
        // 上一轮的停止信号不能污染这一轮（provider 与 ChatState 共享同一个 Arc）。
        self.cancel.store(false, Ordering::Relaxed);
        cx.notify();

        // 模型调用与工具执行都在工作线程：UI 线程不允许被网络阻塞。
        // 事件 → 快照 → channel → UI 合并，UI 侧没有事件重放逻辑。
        let state = self.state.clone();
        let reload_sidecar = std::mem::take(&mut self.sidecar_reload_pending);
        let (tx, rx) = mpsc::channel::<DesktopTurnUpdate>();
        std::thread::spawn(move || {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _writer = match guard.lock_and_refresh() {
                Ok(writer) => writer,
                Err(error) => {
                    let mut rows = rows_snapshot(&guard.log, None);
                    rows.push(Row::Meta(format!("消息未发送：{error}")));
                    let _ = tx.send(DesktopTurnUpdate::Rows(rows));
                    return;
                }
            };
            if reload_sidecar {
                let _ = guard.restart_sidecar();
            } else if guard.host.as_mut().is_some_and(SidecarHost::has_exited) {
                let _ = guard.restart_active_sidecar();
            }
            guard.stream_turn_with_attachments(&text, &attachments, &mut |rows| {
                // UI 已关闭时发送失败：忽略并继续，不要 panic。
                let _ = tx.send(DesktopTurnUpdate::Rows(rows));
            });
            let finished = DesktopTurnUpdate::Finished {
                plugin_errors: guard.load_errors(),
                adc_panels: guard.adc_panels.clone(),
                adc_actions: guard.adc_action_entries(),
            };
            drop(_writer);
            drop(guard);
            let _ = tx.send(finished);
        });

        cx.spawn(async move |this, cx| {
            loop {
                // 同一轮询窗口内只取最后一份快照：快照是全量的，旧的不需要画。
                let mut latest = None;
                let mut finished = None;
                let mut closed = false;
                loop {
                    match rx.try_recv() {
                        Ok(DesktopTurnUpdate::Rows(rows)) => latest = Some(rows),
                        Ok(DesktopTurnUpdate::Finished {
                            plugin_errors,
                            adc_panels,
                            adc_actions,
                        }) => {
                            finished = Some((plugin_errors, adc_panels, adc_actions));
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            closed = true;
                            break;
                        }
                    }
                }
                if let Some(rows) = latest {
                    let ok = this
                        .update(cx, |app, cx| {
                            // 行数变化 = 新消息落盘：顺手刷新侧栏标题/时间。
                            if rows.len() != app.rows.len() {
                                app.reload_sessions();
                            }
                            app.rows = rows;
                            // 生成过程中始终跟随最新一行；结束后停止强制滚动，
                            // 用户往上翻历史不会被拽回去。
                            if app.busy && finished.is_none() {
                                if app.follow_transcript {
                                    app.scroll.scroll_to_bottom();
                                }
                                app.activity_scroll.scroll_to_bottom();
                            }
                            cx.notify();
                        })
                        .is_ok();
                    if !ok {
                        return;
                    }
                }
                if closed || finished.is_some() {
                    let _ = this.update(cx, |app, cx| {
                        if let Some((errors, panels, actions)) = finished {
                            if app.follow_transcript && !app.activity_open && !app.plugins_open {
                                let new_call = panels
                                    .iter()
                                    .filter_map(|panel| panel.trial_call_seq)
                                    .filter(|seq| {
                                        !app.adc_panels
                                            .iter()
                                            .any(|panel| panel.trial_call_seq == Some(*seq))
                                    })
                                    .max();
                                app.reveal_trial_call = new_call;
                                if new_call.is_none() {
                                    app.scroll.scroll_to_bottom();
                                }
                            }
                            app.activity_scroll.scroll_to_bottom();
                            app.plugin_errors = errors;
                            app.adc_panels = panels;
                            app.adc_actions = actions;
                        }
                        app.busy = false;
                        app.reload_sessions();
                        app.sync_user_questions(cx);
                        cx.notify();
                    });
                    return;
                }
                // 审批可能出现在两个快照之间：轮询 pending，变化时重绘。
                let _ = this.update(cx, |app, cx| {
                    app.sync_user_questions(cx);
                    let pending = app.approvals.pending().len();
                    if pending != app.pending_approvals {
                        app.pending_approvals = pending;
                        cx.notify();
                    }
                });
                // 16ms 轮询：与一帧同量级，肉眼看不到分批。
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
    }

    pub(crate) fn on_composer_input_changed(
        &mut self,
        input: Entity<TextInput>,
        cx: &mut Context<Self>,
    ) {
        let input = input.read(cx);
        let text = input.content();
        let slash_text =
            (!self.busy && !input.is_composing() && self.matching_commands(text).next().is_some())
                .then_some(text);
        if slash_text != self.slash_text.as_deref() {
            self.slash_text = slash_text.map(str::to_owned);
            self.selected_command = 0;
            if slash_text.is_some() {
                self.session_menu = None;
                self.turn_menu_open = false;
                self.composer_menu = Some(ComposerMenu::Commands);
            } else if self.composer_menu == Some(ComposerMenu::Commands) {
                self.composer_menu = None;
            }
        }
        if self.follow_transcript && !self.activity_open {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    pub(crate) fn submit_composer(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.attachment_preview.is_some() {
            return false;
        }
        if self.composer_menu == Some(ComposerMenu::Commands) {
            let Some((_, _, command)) = self.matching_commands(text).nth(self.selected_command)
            else {
                return false;
            };
            self.composer_menu = None;
            let chat = cx.weak_entity();
            let session = self.session_id.clone();
            // TextInput 调用此回调时仍在更新自身。先让它清空，再执行会切换焦点的指令。
            window.defer(cx, move |window, cx| {
                let _ = chat.update(cx, |chat, cx| {
                    if chat.session_id == session {
                        chat.run_command_choice(command, window, cx);
                    }
                });
            });
            return true;
        }
        self.on_submit(text, window, cx)
    }

    pub(crate) fn move_slash_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(text) = self.slash_text.as_deref() else {
            return;
        };
        let count = self.matching_commands(text).count();
        if self.composer_menu == Some(ComposerMenu::Commands) && count > 0 {
            self.selected_command =
                (self.selected_command as isize + delta).rem_euclid(count as isize) as usize;
            cx.notify();
        }
    }

    pub(crate) fn run_slash_command(
        &mut self,
        command: SlashCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            SlashCommand::New => self.new_session(window, cx),
            SlashCommand::Creator => self.new_creator_session(window, cx),
            SlashCommand::Search => {
                self.open_session_search(window, cx);
            }
            SlashCommand::Turns => self.turn_menu_open = true,
            SlashCommand::Plugins => self.toggle_plugins_panel(window, cx),
            SlashCommand::Theme => self.toggle_theme(cx),
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // DSH Desktop 对齐的界面：侧栏 / 头部 / 对话流 / 输入卡片。
    // 尺寸与颜色都来自 `dshr_ui_gpui::theme`（从 DSH 安装包解析的 token）。
    // ------------------------------------------------------------------
}
