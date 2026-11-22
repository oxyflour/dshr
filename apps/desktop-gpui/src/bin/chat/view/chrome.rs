use crate::*;

impl ChatApp {
    pub(crate) fn theme(&self) -> Theme {
        Theme::new(self.theme_kind)
    }

    pub(crate) fn show_empty_composer(&self) -> bool {
        self.rows.is_empty() && !self.busy && !self.activity_open
    }

    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.set_theme(self.theme_kind.toggled(), cx);
    }

    pub(crate) fn set_theme(&mut self, kind: ThemeKind, cx: &mut Context<Self>) {
        self.theme_kind = kind;
        self.preferences.theme = kind;
        self.save_preferences();
        cx.notify();
    }

    pub(crate) fn sidebar_is_collapsed(&self, window: &Window) -> bool {
        self.sidebar_collapsed
            .unwrap_or(window.bounds().size.width < px(600.0))
    }

    pub(crate) fn toggle_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let collapsed = self.sidebar_is_collapsed(window);
        self.session_menu = None;
        self.sidebar_collapsed = Some(!collapsed);
        cx.notify();
    }

    /// 打开/关闭插件管理页；打开时补一次 ADC 面板快照。
    pub(crate) fn toggle_plugins_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plugins_open = !self.plugins_open;
        self.plugin_detail = None;
        self.plugin_manifest_path = None;
        self.plugin_panels_open = false;
        self.plugin_notice = None;
        self.workspace_picker_open = false;
        self.composer_menu = None;
        self.session_menu = None;
        self.turn_menu_open = false;
        if self.plugins_open {
            // 与点击入口一致：用 try_lock，工作线程持锁时不阻塞 UI 渲染。
            if let Ok(guard) = self.state.try_lock() {
                self.adc_panels = guard.adc_panels.clone();
            }
            window.focus(&self.plugin_focus, cx);
        } else {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn shortcut_new_session(
        &mut self,
        _: &NewSessionShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("new-session");
        self.new_session(window, cx);
    }

    pub(crate) fn shortcut_focus_composer(
        &mut self,
        _: &FocusComposerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("focus-composer");
        self.focus_composer(window, cx);
    }

    pub(crate) fn shortcut_toggle_sidebar(
        &mut self,
        _: &ToggleSidebarShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("toggle-sidebar");
        self.toggle_sidebar(window, cx);
    }

    pub(crate) fn shortcut_toggle_plugins(
        &mut self,
        _: &TogglePluginsShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("toggle-plugins");
        if self.settings_tab.is_none() {
            self.toggle_plugins_panel(window, cx);
        }
    }

    pub(crate) fn shortcut_approve_first(
        &mut self,
        _: &ApproveFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("approve-first");
        self.resolve_first_approval(ApprovalOutcome::AllowOnce, cx);
    }

    pub(crate) fn shortcut_deny_first(
        &mut self,
        _: &DenyFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("deny-first");
        self.resolve_first_approval(ApprovalOutcome::Deny, cx);
    }

    pub(crate) fn shortcut_escape(
        &mut self,
        _: &EscapeShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("escape");
        #[cfg(target_os = "windows")]
        if self.close_window_menu(cx) {
            return;
        }
        if self.close_attachment_preview(window, cx) {
            return;
        }
        if self.close_settings(window, cx) {
            return;
        }
        if self.session_menu.take().is_some() {
            cx.notify();
            return;
        }
        if self.header_menu.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
            return;
        }
        if self.workspace_picker_open {
            self.workspace_picker_open = false;
            self.focus_composer(window, cx);
            cx.notify();
            return;
        }
        if self.adc_editor.is_some() {
            self.cancel_adc_edit(cx);
        } else if self.pending_external.take().is_some() {
            cx.notify();
        } else if matches!(
            self.composer_menu,
            Some(ComposerMenu::ModelList | ComposerMenu::Reasoning)
        ) {
            self.composer_menu = Some(ComposerMenu::Model);
            cx.notify();
        } else if self.composer_menu.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        } else if self.turn_menu_open {
            self.turn_menu_open = false;
            self.focus_composer(window, cx);
            cx.notify();
        } else if self.right_sidebar_open && !self.plugins_open {
            self.right_sidebar_open = false;
            self.focus_composer(window, cx);
            cx.notify();
        } else if self.plugins_open {
            if self.plugin_detail.is_some() || self.plugin_panels_open {
                self.return_to_plugins(window, cx);
            } else {
                self.toggle_plugins_panel(window, cx);
            }
        } else if self.renaming.is_some() {
            self.cancel_rename(cx);
        } else if self.pending_delete.take().is_some() {
            cx.notify();
        } else if self.session_search_open
            && self
                .session_search
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            self.close_session_search(window, cx);
        } else {
            self.focus_composer(window, cx);
        }
    }

    pub(crate) fn shortcut_quit(
        &mut self,
        _: &QuitShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("quit");
        cx.quit();
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins_open && self.settings_tab.is_none() {
            self.toggle_plugins_panel(window, cx);
            return;
        }
        if self.message_selection.clear() {
            cx.notify();
        }
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    // ------------------------------------------------------------------
    // 多会话：索引/切换/新建/重命名/删除
    // ------------------------------------------------------------------
}
