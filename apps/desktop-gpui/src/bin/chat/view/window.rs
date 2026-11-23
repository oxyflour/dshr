use crate::*;

impl Render for ChatApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[ui-trace] bounds={:?} viewport={:?} scale={} rows={} busy={} last={:?}",
                window.bounds(),
                window.viewport_size(),
                window.scale_factor(),
                self.rows.len(),
                self.busy,
                self.rows.last().map(Row::main_text)
            );
        }
        let mut theme = self.theme();
        let p = theme.palette;
        if let Some(seq) = self.reveal_trial_call.take() {
            // 先让新卡片参与布局，再定位，避免流式快照排队的滚动到底覆盖定位。
            let chat = cx.weak_entity();
            let session = self.session_id.clone();
            window.on_next_frame(move |_, cx| {
                let _ = chat.update(cx, |app, cx| {
                    if app.session_id == session
                        && app.follow_transcript
                        && !app.busy
                        && !app.activity_open
                        && !app.plugins_open
                    {
                        app.turn_jump = app.rows.iter().rposition(|row| row.has_tool_call(seq));
                        cx.notify();
                    }
                });
            });
        }
        if !self.initial_scroll_done && !self.rows.is_empty() {
            self.scroll.scroll_to_bottom();
            self.initial_scroll_done = true;
            // Re-anchor after the virtualized list has laid out the real tail rows.
            let session_id = self.session_id.clone();
            let chat = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = chat.update(cx, |chat, cx| {
                    if chat.session_id == session_id
                        && chat.follow_transcript
                        && !chat.activity_open
                    {
                        chat.scroll.scroll_to_bottom();
                        cx.notify();
                    }
                });
            });
        }
        // 帧探针：每帧推进滚动位置并请求下一帧；达到目标帧数后写报告（可选退出）。
        // 时间戳全部来自 GPUI 渲染管线，测量的是真实滚动帧率而不是估算。
        let mut probe_report = None;
        if let Some(probe) = self.frame_probe.as_mut() {
            self.initial_scroll_done = true;
            let finished = probe.tick(
                &self.scroll,
                window.viewport_size(),
                window.scale_factor(),
                self.visible_limit.min(self.rows.len()),
            );
            if finished {
                probe_report = Some(probe.report());
            } else {
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| {
                    entity.update(cx, |_this, cx| cx.notify());
                });
            }
        }
        if let Some(report) = probe_report {
            let quit = self
                .frame_probe
                .as_ref()
                .map(|probe| probe.quit)
                .unwrap_or(false);
            let log_path = self
                .frame_probe
                .as_ref()
                .map(|probe| probe.log_path.clone())
                .unwrap_or_else(|| PathBuf::from(".tmp/frame-probe.txt"));
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&log_path, &report);
            print!("{report}");
            self.frame_probe = None;
            if quit {
                cx.quit();
            }
        }
        let collapsed = self.sidebar_is_collapsed(window);
        if collapsed {
            self.session_menu = None;
        }
        let width = window.bounds().size.width;
        let sidebar_width = if collapsed {
            px(48.0)
        } else {
            theme.layout.sidebar_width
        };
        let available_width = width - sidebar_width;
        let right_sidebar_visible = self.right_sidebar_open && !self.plugins_open;
        let right_sidebar_overlay = available_width < px(744.0);
        let right_sidebar_width = px(264.0).min(available_width.max(px(0.0)));
        let body_width = if right_sidebar_visible && !right_sidebar_overlay {
            available_width - right_sidebar_width
        } else {
            available_width
        };
        let compact = body_width < px(480.0);
        theme.layout.composer_max_width =
            (body_width * 0.64).clamp(px(680.0), px(920.0)) + px(32.0);
        let hero = self.show_empty_composer();
        let show_turn_rail =
            !compact && !self.activity_open && window.viewport_size().height >= px(388.0);
        let show_window_bar = cfg!(target_os = "windows") && !window.is_fullscreen();
        let shell = div()
            .relative()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .bg(p.color(p.sidebar_fill))
            .child(if collapsed {
                self.sidebar_rail(&theme, cx)
            } else {
                self.sidebar(&theme, cx)
            });
        let body = div()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .rounded_tl(theme.radii.lg)
            .border_l_1()
            .border_color(p.color(p.border_l3))
            .bg(p.color(p.bg_base));
        let body = if self.plugins_open {
            body.child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.plugins_page(&theme, cx)),
            )
            .when_some(self.approval_bar(&theme, cx), |el, bar| el.child(bar))
            .when_some(self.user_question_bar(&theme, cx), |el, bar| el.child(bar))
            .when_some(self.external_confirm_bar(&theme, cx), |el, bar| {
                el.child(bar)
            })
            .when_some(self.adc_editor_bar(&theme, cx), |el, bar| el.child(bar))
        } else {
            let menu_top = px(0.0);
            #[cfg(target_os = "windows")]
            let menu_top = if show_window_bar {
                px(window_bar::HEIGHT)
            } else {
                menu_top
            };
            let composer = self.composer(
                &theme,
                hero,
                body_width,
                window.viewport_size().height,
                menu_top,
                cx,
            );
            body.child(self.header(&theme, collapsed, compact, show_window_bar, cx))
                // 启动/加载错误在 GUI 主界面可见（双击启动没有控制台可看 stderr）。
                .when(!self.plugin_errors.is_empty(), |el| {
                    el.child(self.error_banner(&theme))
                })
                .when(!hero, |body| {
                    body.child(self.transcript(&theme, show_turn_rail, window, cx))
                })
                .when_some(self.approval_bar(&theme, cx), |el, bar| el.child(bar))
                .when_some(self.user_question_bar(&theme, cx), |el, bar| el.child(bar))
                .when_some(self.external_confirm_bar(&theme, cx), |el, bar| {
                    el.child(bar)
                })
                .when_some(self.adc_editor_bar(&theme, cx), |el, bar| el.child(bar))
                .child(if hero {
                    self.empty_composer(composer, &theme, cx)
                } else {
                    composer
                })
        };
        let shell = shell
            .child(body)
            .when(right_sidebar_visible, |shell| {
                shell.child(self.right_sidebar(
                    right_sidebar_width,
                    right_sidebar_overlay,
                    &theme,
                    cx,
                ))
            })
            .children(self.settings_overlay(&theme, width, cx))
            .when_some(self.image_preview_overlay(&theme, cx), |el, overlay| {
                el.child(overlay)
            });
        let frame = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.color(p.sidebar_fill));
        #[cfg(target_os = "windows")]
        let frame = frame
            .when(show_window_bar, |frame| {
                frame.child(self.window_bar(&theme, collapsed, window, cx))
            })
            .when(show_window_bar, |frame| {
                frame.children(self.window_menu_dismiss_layer(cx))
            });
        frame
            .child(shell)
            .on_action(cx.listener(Self::shortcut_new_session))
            .on_action(cx.listener(Self::shortcut_focus_composer))
            .on_action(cx.listener(Self::shortcut_toggle_sidebar))
            .on_action(cx.listener(Self::shortcut_toggle_plugins))
            .on_action(cx.listener(Self::shortcut_approve_first))
            .on_action(cx.listener(Self::shortcut_deny_first))
            .on_action(cx.listener(Self::shortcut_escape))
            .on_action(cx.listener(Self::shortcut_quit))
            .on_action(cx.listener(|this, _: &PreviousSlashCommand, _, cx| {
                this.move_slash_selection(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSlashCommand, _, cx| {
                this.move_slash_selection(1, cx);
            }))
    }
}

pub(crate) fn launch() {
    // 会话目录：默认 `.tmp/sessions`，可用 DSHR_SESSIONS_DIR 覆盖（测试/打包用）。
    let dir = std::env::var("DSHR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/sessions"));
    // 帧探针：必须在第一帧之前开启 profiler trace，否则 FrameTiming 不会被记录。
    let frame_probe = FrameProbe::from_env();
    if frame_probe.is_some() {
        set_trace_enabled(true);
    }
    // 行窗口上限：默认不限制（虚拟化负责渲染成本），仅压力/对比实验用
    // DSHR_VISIBLE_LIMIT 保留"只看尾部 N 行"的旧行为。
    let visible_limit = std::env::var("DSHR_VISIBLE_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let store = SessionIndex::new(dir);
    if let Err(err) = store.ensure_dir() {
        eprintln!("[chat] 无法创建会话目录: {err}");
        std::process::exit(1);
    }

    // 迁移旧版单会话文件，避免升级后历史看起来“消失”。
    let legacy = PathBuf::from(".tmp/chat-session.jsonl");
    if store
        .scan()
        .map(|sessions| sessions.is_empty())
        .unwrap_or(false)
        && legacy.exists()
    {
        let target = store.path_for(SESSION_ID);
        if let Ok(_writer) = try_lock_session(&target) {
            if !target.exists() {
                let _ = std::fs::copy(&legacy, &target);
            }
        }
    }

    let (mut sessions, mut session_depths) = ordered_sessions(&store);
    if next_sidebar_session(&sessions, &session_depths).is_none() {
        let id = store.new_id();
        let default = read_agent_presets()
            .ok()
            .and_then(|catalog| catalog.default);
        let created =
            SessionCreated::root(workspace_root().to_string_lossy()).with_optional_preset(default);
        if let Err(err) = store.create_with_created(&id, &created, &now_ts()) {
            eprintln!("[chat] 无法创建初始会话: {err}");
            std::process::exit(1);
        }
        let rebuilt = ordered_sessions(&store);
        sessions = rebuilt.0;
        session_depths = rebuilt.1;
    }
    let active =
        next_sidebar_session(&sessions, &session_depths).unwrap_or_else(|| SESSION_ID.to_owned());
    // 插件 profile：启停状态持久化到 DSHR_PROFILE_DIR/plugins.json，并注入 sidecar。
    let profile_dir = std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"));
    // 两种来源：Loader bundle+patch（只读派生视图），或 plugins.json（可编辑）。
    let profile = if let Some((bundle, patches)) = loader_inputs() {
        Profile::from_loader(&profile_dir, &bundle, &patches).unwrap_or_else(|err| {
            eprintln!("[chat] 加载 Loader bundle 失败（使用默认值）: {err}");
            Profile::defaults(&profile_dir)
        })
    } else {
        let seed = app_root().join("sidecar/dsh-official/default-profile.json");
        Profile::load_with_seed(&profile_dir, seed.is_file().then_some(seed.as_path()))
            .unwrap_or_else(|err| {
                eprintln!("[chat] 加载插件 profile 失败（使用默认值）: {err}");
                Profile::defaults(&profile_dir)
            })
    };
    prepare_desktop_sidecar(&profile);

    let path = store.path_for(&active);
    // G2 真窗口恢复流需要知道恢复目标：`ChatState::open` 会消费 path，
    // 这里先留一份给 DSHR_RECOVERY_FLOW 的断言/轮询使用。
    let recovery_path = path.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    #[cfg(all(test, windows))]
    creator::live_test::drive_notes_panel_approval(&approvals);
    let model_options = configured_models();
    let model_settings = model_settings::ModelSettings::new(&data_root(), model_options.clone());
    let preferences = settings_view::Preferences::load(&data_root());
    let mut settings = DesktopSettings::from_env(&model_options);
    preferences.apply(&mut settings, &model_options);
    let state = Arc::new(Mutex::new(ChatState::open_with_settings(
        path,
        &active,
        &cancel,
        approvals.clone(),
        true,
        true,
        settings.clone(),
    )));
    let preset_updates = preset_reload::start(&state);
    let app_cancel = cancel.clone();
    // 已安装的 external-process 插件：按 profile 的 enabled 条目在启动时装载；
    // 单个插件失败会在窗口上报。
    {
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let external_errors =
            load_enabled_external_plugins(&mut guard.plugins, &profile, &sidecar_plugin_roots());
        guard.external_plugin_errors.extend(external_errors);
        // external-process 面板也在这里取一次树（之后渲染只读缓存）。
        guard.refresh_adc_panels();
    }
    let (
        rows,
        plugin_errors,
        adc_panels,
        active_workspace,
        creator_mode,
        active_preset,
        blank_root,
        permission,
        questions,
    ) = {
        let guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            rows_snapshot(&guard.log, None),
            guard.load_errors(),
            guard.adc_panels.clone(),
            guard.workspace.clone(),
            creator::is_creator_session(&guard.log),
            guard
                .log
                .created()
                .and_then(|created| created.preset.clone()),
            blank_root_created(&guard.log).is_some(),
            guard.permission.clone(),
            guard
                .l2_access
                .as_ref()
                .map(|access| access.questions.clone()),
        )
    };

    application().run(move |cx: &mut App| {
        // 指令菜单打开时，方向键优先选择指令，再回退到草稿光标移动。
        dshr_ui_gpui::init(cx);
        cx.bind_keys(chat_key_bindings());
        // 先用一个保守尺寸开窗，拿到真实 scale / display 后再调整。
        // 高 DPI（本机 1.75）下 GPUI 把 Bounds 当逻辑像素、平台再乘 scale 变成物理窗口，
        // 而远程会话的可见视口可能只有显示器逻辑尺寸的一半——直接开 1180x780 会让
        // 右侧与底部（包括输入框）被裁掉，看起来就像"输出文本消失了"。
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let state = state;
        let rows = rows;
        let app_cancel = app_cancel;
        let flow_approvals = approvals.clone();
        let store = store;
        let sessions = sessions;
        let active = active;
        let approvals = approvals;
        let profile = profile;
        let session_depths = session_depths;
        let plugin_errors = plugin_errors;
        let adc_panels = adc_panels;
        let recovery_path = recovery_path;
        let window_handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("DSHR".into()),
                        appears_transparent: cfg!(target_os = "windows"),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| {
                    // 按"可见视口是逻辑尺寸 / scale"来估算，保证物理窗口不超过可见区域。
                    if let Some(display) = window.display(cx) {
                        let scale = window.scale_factor();
                        let available = display.visible_bounds().size;
                        let width = (available.width.as_f32() / scale - 16.0).clamp(560.0, 1180.0);
                        let height = (available.height.as_f32() / scale - 24.0).clamp(400.0, 780.0);
                        if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
                            eprintln!(
                                "[ui-trace] display={:?} scale={scale} window={width}x{height}",
                                available
                            );
                        }
                        window.resize(size(px(width), px(height)));
                    }
                    let input =
                        cx.new(|cx| TextInput::new("输入消息，或输入 / 查看指令", cx).multiline());
                    let session_search = cx.new(|cx| TextInput::new("搜索会话或工作区", cx));
                    let plugin_search = cx.new(|cx| TextInput::new("搜索已安装插件", cx));
                    let plugin_config_input =
                        cx.new(|cx| TextInput::new("插件配置", cx).multiline());
                    let app = cx.new(|cx| {
                        cx.observe(&session_search, |_, _, cx| cx.notify()).detach();
                        cx.observe(&plugin_search, |_, _, cx| cx.notify()).detach();
                        cx.observe(&input, ChatApp::on_composer_input_changed)
                            .detach();
                        let activity_scroll = ScrollHandle::new();
                        activity_scroll.scroll_to_bottom();
                        ChatApp {
                            creator_mode,
                            active_preset,
                            blank_root,
                            preset_notice: None,
                            state,
                            rows,
                            open_processes: Default::default(),
                            copied_message: None,
                            input: input.clone(),
                            scroll: ScrollHandle::new(),
                            message_selection: MessageSelection::new(cx.focus_handle()),
                            activity_open: false,
                            activity_scroll,
                            settings,
                            permission,
                            model_options,
                            model_settings,
                            composer_menu: None,
                            #[cfg(target_os = "windows")]
                            window_menu: None,
                            slash_text: None,
                            selected_command: 0,
                            turn_menu_open: false,
                            turn_jump: None,
                            turn_focus: None,
                            follow_transcript: true,
                            attachments: Vec::new(),
                            attachment_error: None,
                            attachment_load: None,
                            attachment_request: 0,
                            attachment_preview: None,
                            attachment_thumbnails: RefCell::new(VecDeque::new()),
                            busy: false,
                            cancel: app_cancel,
                            theme_kind: preferences.theme,
                            preferences,
                            settings_tab: None,
                            runtime_installing: None,
                            runtime_notice: None,
                            settings_focus: cx.focus_handle(),
                            settings_restore_focus: None,
                            settings_notice: None,
                            sidebar_collapsed: None,
                            initial_scroll_done: false,
                            store,
                            sessions,
                            session_search,
                            session_search_open: false,
                            session_menu: None,
                            header_menu: None,
                            header_notice: None,
                            right_sidebar_open: false,
                            show_archived: false,
                            workspace_session_limits: Default::default(),
                            session_id: active,
                            active_workspace,
                            folded_workspaces: Default::default(),
                            workspace_picker_open: false,
                            workspace_dialog_open: false,
                            workspace_notice: None,
                            renaming: None,
                            rename_input: None,
                            pending_delete: None,
                            approvals,
                            pending_approvals: 0,
                            questions,
                            question_draft: None,
                            approval_error: None,
                            approval_saving: None,
                            tool_approvals_loading: false,
                            profile,
                            plugins_open: false,
                            plugin_focus: cx.focus_handle(),
                            plugin_panels_open: false,
                            plugin_detail: None,
                            plugin_manifest_path: None,
                            plugin_edit_json: false,
                            plugin_search,
                            plugin_config_input,
                            plugin_notice: None,
                            pending_external: None,
                            adc_editor: None,
                            visible_limit,
                            frame_probe,
                            session_depths,
                            plugin_errors,
                            sidecar_reload_pending: false,
                            adc_panels,
                            collapsed_trial_cards: HashSet::new(),
                            reveal_trial_call: None,
                        }
                    });
                    // 回车提交：拿到弱引用回 ChatApp；回调返回 true 时输入框自己清空。
                    let weak = app.downgrade();
                    let paste_target = weak.clone();
                    let accepted = std::rc::Rc::new(std::cell::Cell::new(false));
                    let accepted_in = accepted.clone();
                    input.update(cx, |input, _cx| {
                        input.set_on_paste(Box::new(move |item, _, cx| {
                            paste_target
                                .update(cx, |chat, cx| chat.paste_attachments(item, cx))
                                .unwrap_or(false)
                        }));
                        input.set_on_submit(Box::new(move |text, window, cx| {
                            let accepted = accepted_in.clone();
                            let _ = weak.update(cx, |chat, cx| {
                                accepted.set(chat.submit_composer(text, window, cx));
                            });
                            accepted_in.get()
                        }));
                    });
                    let focus = input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                    // 首次 IME 组合前刷新一次坐标，缓解候选窗落到屏幕角落（issue 0001）。
                    window.invalidate_character_coordinates();
                    // 调试钩子：绕过输入法/鼠标，直接发一条消息（验证审批卡片等）。
                    let autosend = std::env::var("DSHR_UI_AUTOSEND").ok();
                    app.update(cx, |chat, cx| {
                        chat.load_tool_approvals(autosend, window, cx)
                    });
                    app
                },
            )
            .expect("failed to open window");
        if let Some(updates) = preset_updates {
            cx.spawn(async move |cx| {
                let mut dirty = false;
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    match updates.try_recv() {
                        Ok(()) => dirty = true,
                        Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Disconnected) => break,
                    }
                    if !dirty {
                        continue;
                    }
                    match window_handle.update(cx, |app, _, cx| {
                        let Ok(state) = app.state.try_lock() else {
                            return false;
                        };
                        let errors = state.load_errors();
                        if errors != app.plugin_errors {
                            app.plugin_errors = errors;
                            cx.notify();
                        }
                        true
                    }) {
                        Ok(applied) => dirty = !applied,
                        Err(_) => break,
                    }
                }
            })
            .detach();
        }
        if std::env::var("DSHR_COMPOSER_FLOW").as_deref() == Ok("1") {
            composer_flow::run(window_handle, cx);
        }
        #[cfg(target_os = "windows")]
        if std::env::var("DSHR_TITLEBAR_FLOW").as_deref() == Ok("1") {
            window_bar::run_titlebar_flow(window_handle, cx);
        }
        if std::env::var("DSHR_NAVIGATION_FLOW").as_deref() == Ok("1") {
            diagnostics::run_navigation_flow(window_handle, cx);
        }
        if std::env::var("DSHR_KEYBOARD_FLOW").as_deref() == Ok("1") {
            let approval_result = std::sync::Arc::new(std::sync::Mutex::new(None));
            let approval_out = approval_result.clone();
            let approvals = flow_approvals.clone();
            std::thread::spawn(move || {
                let outcome = approvals.request(
                    "flow-approval",
                    "bash",
                    &serde_json::json!({ "command": "echo flow" }),
                    std::time::Duration::from_secs(10),
                );
                *approval_out.lock().unwrap() = Some(outcome);
            });
            cx.spawn(async move |cx| {
                for key in ["cmd-n", "cmd-k"] {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(400))
                        .await;
                    let _ = window_handle.update(cx, |_app, window, cx| {
                        window.defer(cx, move |window, cx| {
                            window.dispatch_keystroke(
                                gpui::Keystroke::parse(key).expect("flow keystroke"),
                                cx,
                            );
                        });
                    });
                }
                for ch in "hello keyboard".chars() {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(30))
                        .await;
                    let _ = window_handle.update(cx, |_app, window, cx| {
                        window.defer(cx, move |window, cx| {
                            let keystroke = gpui::Keystroke {
                                modifiers: gpui::Modifiers::default(),
                                key: ch.to_string(),
                                key_char: Some(ch.to_string()),
                            };
                            window.dispatch_keystroke(keystroke, cx);
                        });
                    });
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let _ = window_handle.update(cx, |_app, window, cx| {
                    window.defer(cx, |window, cx| {
                        window.dispatch_keystroke(
                            gpui::Keystroke::parse("enter").expect("enter"),
                            cx,
                        );
                    });
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1_500))
                    .await;
                let _ = window_handle.update(cx, |_app, window, cx| {
                    window.defer(cx, |window, cx| {
                        window.dispatch_keystroke(
                            gpui::Keystroke::parse("cmd-shift-a").expect("approve"),
                            cx,
                        );
                    });
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let approved = matches!(
                    *approval_result.lock().unwrap(),
                    Some(dshr_tool_runtime::ApprovalOutcome::AllowOnce)
                );
                if !approved {
                    eprintln!("[keyboard-flow] FAIL: approval not resolved to AllowOnce");
                    cx.update(|cx| cx.quit());
                    std::process::exit(1);
                }
                for key in ["cmd-j", "cmd-w"] {
                    let _ = window_handle.update(cx, |_app, window, cx| {
                        window.defer(cx, move |window, cx| {
                            window.dispatch_keystroke(
                                gpui::Keystroke::parse(key).expect("flow keystroke"),
                                cx,
                            );
                        });
                    });
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                }
            })
            .detach();
        }
        // G2 真窗口恢复：子进程模拟“崩溃后只剩 .bak”，应用启动时索引到孤儿备份、
        // 选中并 recover_backup，然后在真实窗口里继续输入/发送。父进程
        // `--selftest-window-recovery` 负责造崩溃现场并校验最终日志。
        if std::env::var("DSHR_RECOVERY_FLOW").as_deref() == Ok("1") {
            let recovery_path = recovery_path.clone();
            let app_weak = window_handle
                .entity(cx)
                .expect("window-recovery app entity")
                .downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(600))
                    .await;
                let banner = app_weak
                    .update(cx, |chat, _cx| {
                        chat.plugin_errors
                            .iter()
                            .any(|message| message.starts_with("已恢复："))
                    })
                    .unwrap_or(false);
                if !banner {
                    eprintln!("[recovery-flow] FAIL: 启动后没有恢复横幅");
                    cx.update(|cx| cx.quit());
                    std::process::exit(1);
                }
                // 回到输入框，模拟用户在恢复后的会话里继续操作。
                let _ = window_handle.update(cx, |_app, window, cx| {
                    window.defer(cx, |window, cx| {
                        window.dispatch_keystroke(
                            gpui::Keystroke::parse("cmd-k").expect("focus keystroke"),
                            cx,
                        );
                    });
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                for ch in "after recovery".chars() {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(30))
                        .await;
                    let _ = window_handle.update(cx, |_app, window, cx| {
                        window.defer(cx, move |window, cx| {
                            window.dispatch_keystroke(
                                gpui::Keystroke {
                                    modifiers: gpui::Modifiers::default(),
                                    key: ch.to_string(),
                                    key_char: Some(ch.to_string()),
                                },
                                cx,
                            );
                        });
                    });
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let _ = window_handle.update(cx, |_app, window, cx| {
                    window.defer(cx, |window, cx| {
                        window.dispatch_keystroke(
                            gpui::Keystroke::parse("enter").expect("enter"),
                            cx,
                        );
                    });
                });

                let mut after_ok = false;
                for _ in 0..50 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                    let Ok(text) = std::fs::read_to_string(&recovery_path) else {
                        continue;
                    };
                    if !text.contains("before crash") || !text.contains("after recovery") {
                        continue;
                    }
                    if SessionLog::from_jsonl("s-crash", &text).is_ok() {
                        after_ok = true;
                        break;
                    }
                }
                if after_ok {
                    println!("[recovery-flow] PASS banner=true before=true after=true");
                    cx.update(|cx| cx.quit());
                } else {
                    eprintln!(
                        "[recovery-flow] FAIL: 恢复后消息未落盘（{}）",
                        recovery_path.display()
                    );
                    cx.update(|cx| cx.quit());
                    std::process::exit(1);
                }
            })
            .detach();
        }
        cx.activate(true);
    });
}

impl ChatApp {
    /// 启动/加载错误横幅：GUI 双击没有控制台，eprintln 用户看不到，必须画在界面上。
    /// 覆盖 sidecar 装载错误、会话日志损坏/隔离、备份恢复失败等。
    pub(crate) fn error_banner(&self, theme: &Theme) -> AnyElement {
        let t = theme;
        let p = theme.palette;
        let message = self
            .plugin_errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let recovered_only = self
            .plugin_errors
            .iter()
            .all(|message| message.starts_with("已恢复："));
        let tone = if recovered_only {
            p.color(p.state_success)
        } else {
            p.color(p.state_error)
        };
        let title = if recovered_only {
            "恢复提示"
        } else {
            "启动/加载错误"
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(tone)
            .bg(p.color(p.bg_layer_1))
            .child(
                div()
                    .text_size(t.type_scale.small)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .child(title),
            )
            .child(
                div()
                    .text_size(t.type_scale.small)
                    .line_height(t.type_scale.small_line)
                    .text_color(p.color(p.label_tertiary))
                    .child(message),
            )
            .into_any_element()
    }
}
