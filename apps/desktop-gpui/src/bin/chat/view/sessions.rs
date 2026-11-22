use crate::*;

impl ChatApp {
    pub(crate) fn reload_sessions(&mut self) {
        let (sessions, depths) = ordered_sessions(&self.store);
        self.sessions = sessions;
        self.session_depths = depths;
    }

    pub(crate) fn active_title(&self) -> String {
        self.sessions
            .iter()
            .find(|session| session.id == self.session_id)
            .map(|session| session.title.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    pub(crate) fn open_session_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_is_collapsed(window) {
            self.toggle_sidebar(window, cx);
        }
        self.session_menu = None;
        self.session_search_open = true;
        window.focus(&self.session_search.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    pub(crate) fn close_session_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_search_open = false;
        self.session_search
            .update(cx, |input, cx| input.clear(window, cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// 切到另一个会话：加载日志并刷新对话视图，保留窗口的侧车和插件。
    ///
    /// 生成中不允许切换——工作线程还在往旧 state 写，切走会让 UI 与日志分叉。
    pub(crate) fn open_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_menu.take().is_some() {
            cx.notify();
        }
        if id == self.session_id {
            self.focus_composer(window, cx);
            return;
        }
        if self.busy {
            return;
        }
        if !self
            .sessions
            .iter()
            .any(|session| session.id == id && !session.archived)
        {
            return;
        }
        let path = self.store.path_for(&id);
        let index_changed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let index_changed = state.switch_session(path, &id);
            self.active_workspace = state.workspace.clone();
            self.rows = rows_snapshot(&state.log, None);
            self.plugin_errors = state.load_errors();
            self.adc_panels = state.adc_panels.clone();
            self.creator_mode = creator::is_creator_session(&state.log);
            self.active_preset = state
                .log
                .created()
                .and_then(|created| created.preset.clone());
            self.blank_root = blank_root_created(&state.log).is_some();
            index_changed
        };
        self.session_id = id;
        self.header_menu = None;
        self.header_notice = None;
        self.right_sidebar_open = false;
        self.open_processes.clear();
        self.copied_message = None;
        self.collapsed_trial_cards.clear();
        self.reveal_trial_call = None;
        self.preset_notice = None;
        self.workspace_picker_open = false;
        self.workspace_notice = None;
        self.initial_scroll_done = false;
        self.activity_open = false;
        self.composer_menu = None;
        self.turn_menu_open = false;
        self.turn_jump = None;
        self.turn_focus = None;
        self.follow_transcript = true;
        self.close_attachment_preview(window, cx);
        for attachment in self.attachments.drain(..) {
            if let Some(image) = attachment.thumbnail {
                cx.drop_image(image, Some(window));
            }
        }
        self.attachment_error = None;
        self.attachment_load = None;
        self.activity_scroll = ScrollHandle::new();
        self.activity_scroll.scroll_to_bottom();
        self.visible_limit = usize::MAX;
        self.renaming = None;
        self.rename_input = None;
        self.pending_delete = None;
        if index_changed {
            self.reload_sessions();
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.new_session_workspace();
        self.new_session_with_preset(workspace, None, window, cx);
    }

    pub(crate) fn new_creator_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.plugins_open = false;
        self.plugin_panels_open = false;
        let workspace = self.new_session_workspace();
        self.new_session_with_preset(workspace, Some(creator_preset_id()), window, cx);
    }

    pub(crate) fn new_session_workspace(&self) -> PathBuf {
        if self.active_workspace.is_dir() {
            self.active_workspace.clone()
        } else {
            workspace_root()
        }
    }

    pub(crate) fn delete_session(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session_menu = None;
        if self.pending_delete.as_deref() != Some(id.as_str()) {
            self.pending_delete = Some(id);
            cx.notify();
            return;
        }
        self.pending_delete = None;
        if self.busy && id == self.session_id {
            return;
        }
        if let Err(err) = self.store.delete(&id) {
            eprintln!("[chat] 删除会话 {id} 失败: {err}");
            return;
        }
        let state = self.state.clone();
        let deleted = id.clone();
        let drop_scope = move || {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drop_session_scope(&deleted);
        };
        if self.busy {
            std::thread::spawn(drop_scope);
        } else {
            drop_scope();
        }
        self.renaming = None;
        self.rename_input = None;
        self.reload_sessions();
        if id == self.session_id {
            match next_sidebar_session(&self.sessions, &self.session_depths) {
                Some(next) => self.open_session(next, window, cx),
                None => self.new_session(window, cx),
            }
        } else {
            cx.notify();
        }
    }

    pub(crate) fn set_session_archived(
        &mut self,
        id: &str,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session_menu = None;
        self.header_menu = None;
        let active = self
            .session_branch()
            .first()
            .is_some_and(|root| root.id == id);
        if archived && active && self.busy {
            self.workspace_notice = Some("会话正在生成，结束后可归档".to_owned());
        } else if let Err(error) = self.store.set_archived(id, archived) {
            self.workspace_notice = Some(format!("无法更新归档状态：{error}"));
        } else {
            self.workspace_notice = None;
            if self.renaming.as_deref() == Some(id) {
                self.cancel_rename(cx);
            }
            if self.pending_delete.as_deref() == Some(id) {
                self.pending_delete = None;
            }
            self.reload_sessions();
            if archived && active {
                if let Some(next) = next_sidebar_session(&self.sessions, &self.session_depths) {
                    self.open_session(next, window, cx);
                } else {
                    self.new_session_with_preset(self.new_session_workspace(), None, window, cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn start_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.session_menu = None;
        let title = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new("会话名称", cx));
        input.update(cx, |input, cx| {
            input.set_content(&title, window, cx);
            input.set_selected_text_range(0..title.encode_utf16().count(), window, cx);
        });
        let handle = input.read(cx).focus_handle(cx);
        self.renaming = Some(id);
        self.rename_input = Some(input);
        self.pending_delete = None;
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(crate) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(input)) = (self.renaming.clone(), self.rename_input.clone()) else {
            return;
        };
        let title = input.read(cx).content().to_owned();
        if let Err(err) = self.store.rename(&id, &title) {
            self.workspace_notice = Some(format!("无法重命名会话：{err}"));
            cx.notify();
            return;
        }
        self.workspace_notice = None;
        self.renaming = None;
        self.rename_input = None;
        self.reload_sessions();
        cx.notify();
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        self.rename_input = None;
        cx.notify();
    }
}
