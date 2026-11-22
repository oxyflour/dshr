use crate::*;

pub(crate) enum DesktopTurnUpdate {
    Rows(Vec<Row>),
    Finished {
        plugin_errors: Vec<String>,
        adc_panels: Vec<AdcPanelView>,
    },
}

/// ADC Input/Textarea 的编辑会话（M4 W3）：用真实 `TextInput`，提交时把值注入 onChange。
pub(crate) struct AdcEditor {
    pub(crate) plugin_id: String,
    pub(crate) view: String,
    pub(crate) path: String,
    pub(crate) on_change: AdcAction,
    pub(crate) input: Entity<TextInput>,
    pub(crate) multiline: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerMenu {
    Model,
    ModelList,
    Reasoning,
    Permission,
    Preset,
    Commands,
    Add,
}

pub(crate) struct SessionMenu {
    pub(crate) session_id: String,
    pub(crate) position: Option<gpui::Point<gpui::Pixels>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    New,
    Creator,
    Search,
    Turns,
    Plugins,
    Theme,
}

impl SlashCommand {
    pub(crate) const ALL: [Self; 6] = [
        Self::New,
        Self::Creator,
        Self::Search,
        Self::Turns,
        Self::Plugins,
        Self::Theme,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Creator => "creator",
            Self::Search => "search",
            Self::Turns => "turns",
            Self::Plugins => "plugins",
            Self::Theme => "theme",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::New => "新建会话",
            Self::Creator => "新建创造会话",
            Self::Search => "搜索工作区和会话",
            Self::Turns => "跳到指定轮次",
            Self::Plugins => "打开或关闭插件面板",
            Self::Theme => "切换浅色或深色主题",
        }
    }
}

pub(crate) fn slash_commands(text: &str) -> impl Iterator<Item = SlashCommand> + '_ {
    let prefix = text.strip_prefix('/');
    SlashCommand::ALL
        .into_iter()
        .filter(move |command| prefix.is_some_and(|prefix| command.name().starts_with(prefix)))
}

pub(crate) fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "只读",
        PermissionMode::WorkspaceWrite => "工作区写入",
        PermissionMode::DangerFullAccess => "工具免审批",
    }
}

pub(crate) fn reasoning_label(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "默认",
        Some(ReasoningEffort::Off) => "关闭",
        Some(ReasoningEffort::Low) => "低",
        Some(ReasoningEffort::High) => "高",
        Some(ReasoningEffort::Max) => "最高",
    }
}

pub(crate) const EDITABLE_PERMISSIONS: [(PermissionMode, &str); 3] = [
    (PermissionMode::ReadOnly, "拒绝写入和执行类工具"),
    (PermissionMode::WorkspaceWrite, "写入和执行需要确认"),
    (
        PermissionMode::DangerFullAccess,
        "普通工具和工作区内写入直接执行",
    ),
];

pub(crate) struct AttachmentPreview {
    pub(crate) name: String,
    pub(crate) focus: FocusHandle,
    pub(crate) return_focus: Option<FocusHandle>,
    pub(crate) image: Option<Result<Arc<RenderImage>, String>>,
    pub(crate) _load: gpui::Task<()>,
}

pub(crate) struct ComposerAttachment {
    pub(crate) draft: DraftAttachment,
    pub(crate) thumbnail: Option<Arc<RenderImage>>,
}

impl ComposerAttachment {
    pub(crate) fn new(draft: DraftAttachment) -> Self {
        let thumbnail = Self::thumbnail(&draft);
        Self { draft, thumbnail }
    }

    pub(crate) fn thumbnail(draft: &DraftAttachment) -> Option<Arc<RenderImage>> {
        match &draft.content {
            AttachmentContent::Media { format, bytes } => {
                decode_image(bytes, *format, THUMBNAIL_EDGE).ok()
            }
            AttachmentContent::Text(_) => None,
        }
    }
}

pub(crate) struct ChatApp {
    pub(crate) creator_mode: bool,
    pub(crate) active_preset: Option<String>,
    pub(crate) blank_root: bool,
    pub(crate) preset_notice: Option<String>,
    /// Agent / 工具 / 日志都在工作线程里改；UI 只读快照，所以这里必须加锁。
    pub(crate) state: Arc<Mutex<ChatState>>,
    /// 当前渲染用的整份快照（由工作线程推送）。
    pub(crate) rows: Vec<Row>,
    pub(crate) open_processes: std::collections::HashSet<usize>,
    pub(crate) copied_message: Option<(String, usize, std::time::Instant)>,
    /// 真正的文本输入组件（实现 EntityInputHandler，中文 IME 组合输入可用）。
    pub(crate) input: Entity<TextInput>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) message_selection: MessageSelection,
    pub(crate) activity_open: bool,
    pub(crate) activity_scroll: ScrollHandle,
    pub(crate) settings: DesktopSettings,
    pub(crate) permission: LivePermission,
    pub(crate) model_options: Vec<Arc<DesktopModel>>,
    pub(crate) model_settings: model_settings::ModelSettings,
    pub(crate) composer_menu: Option<ComposerMenu>,
    #[cfg(target_os = "windows")]
    pub(crate) window_menu: Option<window_bar::WindowMenu>,
    pub(crate) slash_text: Option<String>,
    pub(crate) selected_command: usize,
    pub(crate) turn_menu_open: bool,
    pub(crate) turn_jump: Option<usize>,
    pub(crate) turn_focus: Option<usize>,
    pub(crate) follow_transcript: bool,
    pub(crate) attachments: Vec<ComposerAttachment>,
    pub(crate) attachment_error: Option<String>,
    pub(crate) attachment_load: Option<u64>,
    pub(crate) attachment_request: u64,
    pub(crate) attachment_preview: Option<AttachmentPreview>,
    pub(crate) attachment_thumbnails: RefCell<VecDeque<StoredAttachment>>,
    /// 一轮正在生成：输入框仍可编辑，但回车不会发第二条。
    pub(crate) busy: bool,
    /// 与 provider 共享的停止句柄（UI 不持锁就能请求中断）。
    pub(crate) cancel: Arc<AtomicBool>,
    /// DSH 设计 token（浅色/深色）。
    pub(crate) theme_kind: ThemeKind,
    pub(crate) preferences: settings_view::Preferences,
    pub(crate) settings_tab: Option<settings_view::SettingsTab>,
    pub(crate) settings_focus: FocusHandle,
    pub(crate) settings_restore_focus: Option<FocusHandle>,
    pub(crate) settings_notice: Option<String>,
    pub(crate) runtime_installing: Option<runtime_components::Component>,
    pub(crate) runtime_notice: Option<String>,
    /// 左侧会话栏收起状态（对齐 DSH 的折叠侧栏）。
    pub(crate) sidebar_collapsed: Option<bool>,
    /// 首次渲染后把历史滚到最新一条（DSH 打开会话也是停在底部）。
    pub(crate) initial_scroll_done: bool,
    /// 会话目录索引与列表。
    pub(crate) store: SessionIndex,
    pub(crate) sessions: Vec<SessionMeta>,
    pub(crate) session_search: Entity<TextInput>,
    pub(crate) session_search_open: bool,
    pub(crate) session_menu: Option<SessionMenu>,
    pub(crate) header_menu: Option<session_header::HeaderMenu>,
    pub(crate) header_notice: Option<(bool, String)>,
    pub(crate) right_sidebar_open: bool,
    pub(crate) show_archived: bool,
    pub(crate) workspace_session_limits: HashMap<String, usize>,
    pub(crate) session_id: String,
    pub(crate) active_workspace: PathBuf,
    pub(crate) folded_workspaces: std::collections::HashSet<String>,
    pub(crate) workspace_picker_open: bool,
    pub(crate) workspace_dialog_open: bool,
    pub(crate) workspace_notice: Option<String>,
    /// 正在重命名的会话与输入框（None = 不在重命名）。
    pub(crate) renaming: Option<String>,
    pub(crate) rename_input: Option<Entity<TextInput>>,
    /// 等待二次确认删除的会话（None = 没有待确认项）。
    pub(crate) pending_delete: Option<String>,
    /// 工具审批 broker（与 ChatState 共享同一实例）。
    pub(crate) approvals: Arc<ApprovalBroker>,
    /// 上一次渲染时的 pending 数，用于只在变化时触发重绘。
    pub(crate) pending_approvals: usize,
    pub(crate) questions: Option<Arc<dsh_user_questions::QuestionBroker>>,
    pub(crate) question_draft: Option<user_question_view::QuestionDraft>,
    pub(crate) approval_error: Option<(PendingApproval, String)>,
    pub(crate) approval_saving: Option<PendingApproval>,
    pub(crate) tool_approvals_loading: bool,
    /// 插件 profile（启停状态持久化）。
    pub(crate) profile: Profile,
    /// 主内容区的插件管理页。
    pub(crate) plugins_open: bool,
    pub(crate) plugin_focus: gpui::FocusHandle,
    pub(crate) plugin_panels_open: bool,
    pub(crate) plugin_detail: Option<usize>,
    pub(crate) plugin_manifest_path: Option<PathBuf>,
    pub(crate) plugin_edit_json: bool,
    pub(crate) plugin_search: Entity<TextInput>,
    pub(crate) plugin_config_input: Entity<TextInput>,
    pub(crate) plugin_notice: Option<String>,
    /// 等待用户确认的 `openExternal` URL（M4 W3；确认前不打开）。
    pub(crate) pending_external: Option<String>,
    /// ADC Input/Textarea 的编辑模态（None = 未编辑）。
    pub(crate) adc_editor: Option<AdcEditor>,
    /// 长会话只渲染最近 N 条（点"载入更早"每次 +200），避免每帧构造上万元素。
    pub(crate) visible_limit: usize,
    /// 帧探针（DSHR_FRAME_PROBE）：滚动时测量真实帧耗时与呈现间隔。
    pub(crate) frame_probe: Option<FrameProbe>,
    /// 会话树深度（session id → depth）；侧栏按树顺序展示并缩进子会话。
    pub(crate) session_depths: std::collections::BTreeMap<String, u32>,
    /// sidecar 启动/插件装载/插件面板错误：插件面板顶部提示（避免"插件失败但界面无感"）。
    pub(crate) plugin_errors: Vec<String>,
    pub(crate) sidecar_reload_pending: bool,
    /// 插件 ADC 面板快照（渲染路径只读；刷新见加载插件/打开面板处，不持锁渲染）。
    pub(crate) adc_panels: Vec<AdcPanelView>,
    pub(crate) collapsed_trial_cards: HashSet<u64>,
    pub(crate) reveal_trial_call: Option<u64>,
}
