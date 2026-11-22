// release 构建是 Windows GUI 子系统：双击/快捷方式启动不弹命令行窗口。
// debug 构建保留控制台子系统，方便 `cargo run` 直接看日志。
// CLI 用法（--selftest 等）在 release 下通过 AttachConsole 接回父控制台（见 win_console）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! 基本对话：输入框 + 会话流 + 工具调用 + 落盘恢复 + 真流式。
//!
//! 这是 M1 的产品面：能打字、能发送、能看到工具卡片、关掉再开会话还在。
//! 模型在无 key 时用 mock，有 key 时用 DeepSeek（SSE 流式）；会话/工具/日志/回放都是真实的。
//!
//! 线程模型：Agent 与工具在工作线程跑，UI 线程只消费"整份快照"——
//! 网络等待不会卡住界面，这正是把 `ModelProvider` 做成流式的目的。
//!
//! 四种模式：
//!   cargo run -p dshr-desktop-gpui --bin dshr                 # 交互界面
//!   cargo run ... --bin dshr -- --selftest                   # 无窗口自检
//!   cargo run ... --bin dshr -- --selftest-stream            # 流式自检
//!   cargo run ... --bin dshr -- --selftest-dsh               # 官方 DSH 插件自检
//!   cargo run ... --bin dshr -- --selftest-adc-panel         # ADC 插件面板链路自检
//!   cargo run ... --bin dshr -- --selftest-wasm              # WASM 插件链路自检（安装→装载→调用→trap→卸载）
//!   cargo run ... --bin dshr -- --selftest-defaults          # M3 默认差异接线自检
//!   cargo run ... --bin dshr -- --selftest-plugin-lifecycle  # M4：安装→卸载→密钥轮换自检
//!   cargo run ... --bin dshr -- --uninstall-plugin <name>    # 卸载插件（目录/profile/审计）
//!   cargo run ... --bin dshr -- --trust-key <action> ...     # 信任密钥库管理
//!   cargo run ... --bin dshr -- --live "一句话"               # 真实 LLM 单轮（需 API key）

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::profiler::{set_trace_enabled, FrameEvent, FrameTimingCollector};
use gpui::{
    actions, div, img, point, px, rgba, size, AnyElement, App, Bounds, ClipboardEntry,
    ClipboardItem, Context, Div, ElementId, Entity, EntityInputHandler, ExternalPaths, FocusHandle,
    Focusable, Font, FontWeight, KeyBinding, ObjectFit, PathPromptOptions, RenderImage, Role,
    ScrollHandle, Stateful, StyledImage, TextOverflow, TitlebarOptions, Window, WindowBounds,
    WindowOptions,
};
use gpui_platform::application;
use serde_json::json;

use dshr_agent_context::{
    history_for_model, history_from_log, plan_context, plan_context_with_spill, SpillPolicy,
    SpillRef, SpillStore,
};
use dshr_agent_core::{
    Agent, AgentEvent, AttachmentFormat, ChatMessage, CompletionRequest, ModelProvider, ModelReply,
    ModelToolCall, ReasoningEffort, SessionTreeRuntime, ToolCallRequest, ToolExecutor, ToolOutcome,
    ToolSpec, DEFAULT_MAX_DEPTH, SPAWN_CHILD_TOOL,
};
use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{
    parse_tool_registration, HostOptions, HostServiceRouter, SessionLogService, ShellProgram,
    SidecarHost, StorageKvService, ToolJobService, ToolProcessService, ToolTerminalService,
    WorkingDirectoryService, CAP_AGENT_TOOL_CONTEXT, CAP_BACKGROUND_JOBS, CAP_IMAGE_ATTACHMENTS,
    CAP_PROGRAM_TOOLS, CAP_SEARCH_PROCESS, CAP_SERVICE_PROXY, CAP_SHELL_PROCESS,
    CAP_TERMINAL_PROCESS, CAP_TOOL_GOALS, CAP_TOOL_SCHEDULES, CAP_TOOL_SESSION_QUERY,
    CAP_TOOL_SUBAGENTS, CAP_TOOL_TEAMS, CAP_USER_QUESTIONS, IMAGE_ATTACHMENTS_V1, PROTOCOL_MAX,
    SESSION_LOG_V1, STORAGE_KV_V1, TOOL_GOALS_V1, TOOL_JOBS_V1, TOOL_PROCESS_V1, TOOL_SCHEDULES_V1,
    TOOL_SESSION_QUERY_V1, TOOL_SUBAGENTS_V1, TOOL_TEAMS_V1, TOOL_TERMINALS_V1, USER_QUESTIONS_V1,
    WORKING_DIRECTORY_V1,
};
use dshr_conversation_model::{build_from_log, CardKind, Item, ToolCard, UserAttachment};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use dshr_llm_retry::{RetryPolicy, RetryingProvider};
use dshr_plugin_api::manifest::RuntimeKind;
use dshr_plugin_api::{HostVersion, Manifest, Plugin, PluginContext, PluginError};
use dshr_plugin_host::{ExternalPlugin, ExternalPluginSpec, PluginHost};
use dshr_profile::{
    add_trust_key, install_package, install_package_trusted, package_digest, read_installed_meta,
    remove_trust_key, revoke_trust_key, rotate_trust_key, uninstall_package, Profile,
    ProfileSource, TrustKey, TrustPolicy, TrustStore,
};
use dshr_sandbox::Workspace;
use dshr_session_export::{export_session, verify_export, ExportOptions, SpillStoreSource};
use dshr_session_store::{
    quarantine_corrupt, recover_backup, save_atomic, try_lock_session, EventSpec, SessionCreated,
    SessionIndex, SessionLog, SessionMeta, ToolState,
};
use dshr_sidecar_guard::GuardLimits;
#[cfg(test)]
use dshr_tool_runtime::RegistryError;
use dshr_tool_runtime::{
    AllowAll, ApprovalBroker, ApprovalOutcome, ApprovalPolicy, PendingApproval, PermissionMode,
    PolicyDecision, ToolDefinition, ToolOrigin, ToolPolicy,
};
use dshr_ui_contributions::parse_action;
use dshr_ui_gpui::{
    adc_panel_body, adc_panel_body_with_handler, diff_view_from_text, selectable_markdown_view,
    terminal_view_from_text, Action as AdcAction, AdcActionHandler, DiagnosticCode,
    MessageSelection, TextInput, Theme, ThemeKind,
};

const SESSION_ID: &str = "s-chat";
const MAX_ATTACHMENTS: usize = 4;
const MAX_TEXT_ATTACHMENT_BYTES: usize = 32 * 1024;
const MAX_MEDIA_ATTACHMENT_BYTES: usize = 12 * 1024 * 1024;

#[path = "chat/attachment_preview.rs"]
mod attachment_preview;
#[path = "chat/dsh_deliverables.rs"]
mod dsh_deliverables;
#[path = "chat/dsh_goals.rs"]
mod dsh_goals;
#[path = "chat/dsh_image_attachments.rs"]
mod dsh_image_attachments;
#[path = "chat/dsh_schedules.rs"]
mod dsh_schedules;
#[path = "chat/dsh_session_query.rs"]
mod dsh_session_query;
#[path = "chat/dsh_subagents.rs"]
mod dsh_subagents;
#[path = "chat/dsh_teams.rs"]
mod dsh_teams;
#[path = "chat/dsh_user_questions.rs"]
mod dsh_user_questions;
#[path = "chat/session_header.rs"]
mod session_header;
#[path = "chat/user_question_view.rs"]
mod user_question_view;
use attachment_preview::{
    decode_image, AttachmentThumbnail, PreviewSource, StoredAttachment, THUMBNAIL_EDGE,
};

#[cfg(target_os = "windows")]
#[path = "chat/window_bar.rs"]
mod window_bar;

#[path = "chat/composer_flow.rs"]
mod composer_flow;

#[path = "chat/cli.rs"]
mod cli;
#[path = "chat/composer_surface.rs"]
mod composer_surface;
#[path = "chat/creator.rs"]
mod creator;
#[path = "chat/diagnostics/mod.rs"]
mod diagnostics;
#[path = "chat/headless.rs"]
mod headless;
#[path = "chat/model_settings.rs"]
mod model_settings;
#[path = "chat/plugins_view.rs"]
mod plugins_view;
#[path = "chat/popover.rs"]
mod popover;
#[path = "chat/preset_reload.rs"]
mod preset_reload;
#[path = "chat/runtime/mod.rs"]
mod runtime;
#[path = "chat/runtime_components.rs"]
mod runtime_components;

use runtime::*;
#[path = "chat/settings_view.rs"]
mod settings_view;
#[path = "chat/tool_approvals.rs"]
mod tool_approvals;
#[path = "chat/workspace_view.rs"]
mod workspace_view;

// These actions keep their existing dshr_chat identifiers for key bindings.
actions!(
    dshr_chat,
    [
        NewSessionShortcut,
        FocusComposerShortcut,
        ToggleSidebarShortcut,
        TogglePluginsShortcut,
        ApproveFirstShortcut,
        DenyFirstShortcut,
        EscapeShortcut,
        QuitShortcut,
        PreviousSlashCommand,
        NextSlashCommand,
    ]
);

/// `DSHR_UI_TRACE=1` 时把快捷键触发写到 stderr，供真实窗口人工/自动化复验。
fn trace_shortcut(action: &str) {
    if std::env::var("DSHR_UI_TRACE").as_deref() == Ok("1") {
        eprintln!("[keyboard] {action}");
    }
}

/// 应用里展示/自检用的快捷键说明（键位字符串与 `chat_key_bindings()` 保持一致）。
const CHAT_SHORTCUTS: [(&str, &str); 8] = [
    ("cmd-n", "新建会话"),
    ("cmd-k", "聚焦输入框"),
    ("cmd-b", "展开/收起侧栏"),
    ("cmd-j", "打开/关闭插件面板"),
    ("cmd-shift-a", "允许第一个待审批工具"),
    ("cmd-shift-d", "拒绝第一个待审批工具"),
    ("escape", "关闭面板/取消重命名/回到输入框"),
    ("cmd-w", "关闭应用"),
];

fn chat_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-n", NewSessionShortcut, None),
        KeyBinding::new("cmd-k", FocusComposerShortcut, None),
        KeyBinding::new("cmd-b", ToggleSidebarShortcut, None),
        KeyBinding::new("cmd-j", TogglePluginsShortcut, None),
        KeyBinding::new("cmd-shift-a", ApproveFirstShortcut, None),
        KeyBinding::new("cmd-shift-d", DenyFirstShortcut, None),
        KeyBinding::new("escape", EscapeShortcut, None),
        KeyBinding::new("cmd-w", QuitShortcut, None),
        KeyBinding::new(
            "up",
            PreviousSlashCommand,
            Some("DshrSlashMenu > TextInput"),
        ),
        KeyBinding::new("down", NextSlashCommand, Some("DshrSlashMenu > TextInput")),
    ]
}

/// 把 keymap 语法（`cmd-shift-a`）转成 UI 文案（`Ctrl+Shift+A`）。
fn format_shortcut(key: &str, primary: &str) -> String {
    key.split('-')
        .map(|part| match part {
            "cmd" => primary.to_owned(),
            "ctrl" => String::from("Ctrl"),
            "shift" => String::from("Shift"),
            "alt" => String::from("Alt"),
            "escape" => String::from("Esc"),
            other if other.chars().count() == 1 => other.to_uppercase(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 输入框下方的一行快捷键提示（与 `CHAT_SHORTCUTS` 同源，避免文档漂移）。
fn chat_shortcut_hint() -> String {
    let primary = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    let label = |index: usize| format_shortcut(CHAT_SHORTCUTS[index].0, primary);
    format!(
        "{} 新建 · {} 插件 · {}/{} 审批",
        label(0),
        label(3),
        label(4),
        label(5)
    )
}

fn chrome_button(id: impl Into<ElementId>, label: &'static str, theme: &Theme) -> Stateful<Div> {
    let p = theme.palette;
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(theme.radii.sm)
        .cursor_pointer()
        .text_size(theme.type_scale.secondary)
        .text_color(p.color(p.label_secondary))
        .hover(|style| style.bg(p.color(p.interactive_hover)))
        .child(label)
}

fn desktop_icon(windows_glyph: &'static str, fallback: &'static str) -> Div {
    div()
        .flex_none()
        .text_size(px(14.0))
        .line_height(px(18.0))
        .when(cfg!(windows), |icon| icon.font_family("Segoe MDL2 Assets"))
        .child(if cfg!(windows) {
            windows_glyph
        } else {
            fallback
        })
}

struct ToolbarTooltip {
    label: gpui::SharedString,
    theme: Theme,
}

impl Render for ToolbarTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        let p = t.palette;
        div()
            .max_w(px(360.0))
            .rounded(t.radii.sm)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .text_color(p.color(p.label_primary))
            .text_size(t.type_scale.small)
            .px(px(8.0))
            .py(px(4.0))
            .shadow_sm()
            .child(self.label.clone())
    }
}

fn toolbar_tooltip(
    label: impl Into<gpui::SharedString>,
    theme: Theme,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let label = label.into();
    move |_, cx| {
        cx.new(|_| ToolbarTooltip {
            label: label.clone(),
            theme,
        })
        .into()
    }
}

fn toolbar_icon_button(
    id: impl Into<ElementId>,
    label: &'static str,
    windows_glyph: &'static str,
    fallback: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    chrome_button(id, "", theme)
        .aria_label(label)
        .focusable()
        .size(px(28.0))
        .px(px(0.0))
        .child(desktop_icon(windows_glyph, fallback))
        .tooltip(toolbar_tooltip(label, *theme))
}

fn attachment_image_button(
    id: impl Into<ElementId>,
    name: &str,
    image: Option<Arc<RenderImage>>,
    fallback: &'static str,
    width: f32,
    height: f32,
    t: &Theme,
) -> Stateful<Div> {
    let p = t.palette;
    div()
        .id(id)
        .role(Role::Button)
        .focusable()
        .aria_label(format!("预览图片 {name}"))
        .w(px(width))
        .h(px(height))
        .max_w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(t.radii.sm)
        .bg(p.color(p.tag))
        .cursor_pointer()
        .child(match image {
            Some(image) => img(image)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .text_size(t.type_scale.small)
                .text_color(p.color(p.label_secondary))
                .child(fallback)
                .into_any_element(),
        })
}

fn stored_image(file: &SpillRef) -> bool {
    AttachmentFormat::from_mime(&file.mime).is_some_and(|format| format != AttachmentFormat::Pdf)
}

fn file_size_label(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn composer_option(
    id: impl Into<ElementId>,
    label: &str,
    description: &str,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let p = theme.palette;
    div()
        .id(id)
        .role(Role::MenuItemRadio)
        .aria_label(label.to_owned())
        .aria_selected(selected)
        .flex()
        .flex_col()
        .px(px(9.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .cursor_pointer()
        .hover(|style| style.bg(p.color(p.interactive_hover)))
        .when(selected, |row| row.bg(p.color(p.business_tertiary)))
        .child(
            div()
                .text_size(theme.type_scale.secondary)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.color(if selected { p.link } else { p.label_primary }))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(theme.type_scale.small)
                .text_color(p.color(p.label_secondary))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(description.to_owned()),
        )
}

fn workspace_name(workspace: &str) -> &str {
    workspace
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
}

struct WorkspaceSessions<'a> {
    workspace: Option<&'a str>,
    sessions: Vec<(usize, &'a SessionMeta)>,
}

fn archive_label(archived: bool) -> &'static str {
    if archived {
        "取消归档"
    } else {
        "归档会话"
    }
}

const WORKSPACE_SESSION_PAGE: usize = 5;
const WORKSPACE_SESSION_AUTO_LIMIT: usize = 15;

fn top_level_session(
    session: &SessionMeta,
    depths: &std::collections::BTreeMap<String, u32>,
) -> bool {
    depths.get(&session.id).copied().unwrap_or(0) == 0
}

fn next_sidebar_session(
    sessions: &[SessionMeta],
    depths: &std::collections::BTreeMap<String, u32>,
) -> Option<String> {
    sessions
        .iter()
        .find(|session| !session.archived && top_level_session(session, depths))
        .map(|session| session.id.clone())
}

fn workspace_sessions<'a>(
    sessions: &'a [SessionMeta],
    depths: &std::collections::BTreeMap<String, u32>,
    archived: bool,
    query: &str,
) -> Vec<WorkspaceSessions<'a>> {
    let mut groups = Vec::new();
    let mut indices = HashMap::new();
    for (index, session) in sessions.iter().enumerate() {
        if session.archived != archived || !top_level_session(session, depths) {
            continue;
        }
        let workspace = session
            .workspace_id
            .as_deref()
            .filter(|id| !id.trim().is_empty());
        if !query.is_empty()
            && !session.title.to_lowercase().contains(query)
            && !workspace.is_some_and(|id| id.to_lowercase().contains(query))
        {
            continue;
        }
        let group = *indices.entry(workspace).or_insert_with(|| {
            let group = groups.len();
            groups.push(WorkspaceSessions {
                workspace,
                sessions: Vec::new(),
            });
            group
        });
        groups[group].sessions.push((index, session));
    }
    groups
}

fn desktop_permission_mode() -> Option<PermissionMode> {
    if std::env::var("DSHR_TOOL_POLICY").as_deref() == Ok("ask-all") {
        None
    } else {
        Some(PermissionMode::from_env())
    }
}

/// 非空环境变量 → 路径。空字符串按"未设置"处理（Windows 上 `set VAR=` 是常见写法）。
fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 只在用户没显式设置时写入默认值；自检/测试里显式设置的 env 永远优先。
fn set_env_default(key: &str, value: impl Into<std::ffi::OsString>) {
    let missing = std::env::var_os(key)
        .map(|existing| existing.is_empty())
        .unwrap_or(true);
    if missing {
        std::env::set_var(key, value.into());
    }
}

/// 目录可写探测：创建一个短命探针文件。Program Files 等位置会在这里被排除。
fn is_writable_dir(dir: &std::path::Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".dshr-write-probe-{}", std::process::id()));
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .is_ok();
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    writable
}

/// 数据根来源（诊断/单测用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataRootSource {
    /// 用户显式 `DSHR_HOME`。
    Home,
    /// 打包布局：exe 旁边有 `sidecar/` 且目录可写。
    ExeDir,
    /// 默认 `%LOCALAPPDATA%\dshr`。
    LocalAppData,
}

/// 纯选择逻辑：`DSHR_HOME` 优先；打包布局（exe 目录含 `sidecar/` 且可写）其次；
/// 否则 `%LOCALAPPDATA%\dshr`。创建/回退不在这个函数里做，便于单测。
fn pick_data_root(
    dshr_home: Option<&std::path::Path>,
    exe_dir: Option<&std::path::Path>,
    local_app_data: Option<&std::path::Path>,
    exe_dir_usable: bool,
) -> Option<(DataRootSource, PathBuf)> {
    if let Some(home) = dshr_home {
        return Some((DataRootSource::Home, home.to_path_buf()));
    }
    if exe_dir_usable {
        if let Some(dir) = exe_dir {
            return Some((DataRootSource::ExeDir, dir.to_path_buf()));
        }
    }
    local_app_data.map(|base| (DataRootSource::LocalAppData, base.join("dshr")))
}

/// 兜底数据根：exe 目录（可写）→ 当前目录（可写）→ 系统临时目录。
fn fallback_data_root(exe_dir: Option<&std::path::Path>) -> PathBuf {
    if let Some(dir) = exe_dir.filter(|dir| is_writable_dir(dir)) {
        return dir.to_path_buf();
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .filter(|dir| is_writable_dir(dir))
    {
        return cwd;
    }
    std::env::temp_dir()
}

/// 解析可写数据根：与 CWD 完全解耦。
///
/// 旧实现把状态目录建在 CWD 下（`.tmp/sessions`）。桌面快捷方式"起始位置"为空时
/// CWD 可能落到 `C:\Windows\System32`，或 exe 被放进 Program Files 等只读位置，
/// 于是建目录直接 `拒绝访问 (os error 5)` 退出——这就是"被 Windows 权限拦住"的可复现根因。
fn resolve_data_root() -> PathBuf {
    let home = non_empty_env_path("DSHR_HOME");
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    let exe_dir_usable = exe_dir
        .as_deref()
        .is_some_and(|dir| dir.join("sidecar").is_dir() && is_writable_dir(dir));
    let local_app_data = non_empty_env_path("LOCALAPPDATA");

    if let Some((source, path)) = pick_data_root(
        home.as_deref(),
        exe_dir.as_deref(),
        local_app_data.as_deref(),
        exe_dir_usable,
    ) {
        match source {
            DataRootSource::Home => {
                // 显式覆盖优先：创建失败也尊重用户选择，让后面的错误指向用户指定路径。
                if let Err(err) = std::fs::create_dir_all(&path) {
                    eprintln!(
                        "[chat] DSHR_HOME 数据目录不可用: {}（{err}）",
                        path.display()
                    );
                }
                return path;
            }
            _ => {
                // 不只是"能创建"：已存在但当前完整性级别/ACL 不可写的目录也要换。
                let unavailable = match std::fs::create_dir_all(&path) {
                    Ok(()) if is_writable_dir(&path) => return path,
                    Ok(()) => "目录存在但不可写".to_owned(),
                    Err(err) => format!("无法创建: {err}"),
                };
                eprintln!("[chat] 数据目录 {} 不可用（{unavailable}）", path.display());
            }
        }
    }

    let fallback = fallback_data_root(exe_dir.as_deref());
    eprintln!(
        "[chat] 数据根回退到 {}（不依赖当前工作目录；可用 DSHR_HOME 显式指定）",
        fallback.display()
    );
    fallback
}

/// 进程级数据根缓存：`prepare_data_env` 解析一次，其它地方（工具工作区兜底）复用。
static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 已解析的数据根；`prepare_data_env` 之前调用时现场解析（单测/非 main 入口）。
fn data_root() -> PathBuf {
    DATA_ROOT.get().cloned().unwrap_or_else(resolve_data_root)
}

/// 把默认状态目录挂到数据根上；必须在 `install_crash_hook()` 之前调用（hook 读 env）。
fn prepare_data_env() {
    let root = resolve_data_root();
    let _ = DATA_ROOT.set(root.clone());
    let debug_assets = if non_empty_env_path("DSHR_APP_ROOT").is_none() {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(std::path::Path::parent)
            .and_then(windows_debug_assets)
    } else {
        None
    };
    if let Some(assets) = &debug_assets {
        set_env_default("DSHR_RUNTIME_DIR", assets.join("runtime"));
    }
    let profile = debug_assets
        .filter(|_| non_empty_env_path("DSHR_HOME").is_none())
        .map_or_else(
            || root.join("profile"),
            |assets| assets.with_file_name("profile"),
        );
    set_env_default("DSHR_SESSIONS_DIR", root.join("sessions"));
    set_env_default("DSHR_PROFILE_DIR", profile);
    set_env_default("DSHR_SPILL_DIR", root.join("spill"));
    set_env_default("DSHR_CRASH_LOG", root.join("crash.log"));
}

/// 自检从只读 CWD（`C:\Windows\System32`、受限打包目录等）启动时，`--selftest*`
/// 使用的 `.tmp/...` 相对路径会失败。自检不依赖用户 CWD，统一切到数据根（可写、与
/// 用户数据同源）；GUI 与 `--install-plugin`/`--export-session` 等命令不切。
fn prepare_selftest_cwd(args: &[String]) {
    if !args.iter().skip(1).any(|arg| arg.starts_with("--selftest")) {
        return;
    }
    // 自检里的工具工作区必须仍是“用户当前目录/仓库根”（dsh 自检会 read README.md），
    // 只把 `.tmp/...` 这类相对路径切到数据根；两者不能一起切。
    if std::env::var_os("DSHR_WORKSPACE").is_none() {
        let workspace = std::env::current_dir()
            .ok()
            .filter(|dir| is_writable_dir(dir))
            .unwrap_or_else(app_root);
        std::env::set_var("DSHR_WORKSPACE", workspace);
    }
    let root = data_root();
    if std::fs::create_dir_all(&root).is_ok() {
        let _ = std::env::set_current_dir(&root);
    }
}

/// 判断本次是命令行用法（自检/导出/安装/显式 --console）还是纯 GUI 双击。
///
/// GUI 启动永远是零参数；任何 `--` 开关都意味着用户从终端跑：需要把输出接回控制台。
fn is_cli_invocation(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg.starts_with("--"))
}

fn windows_debug_assets(exe_dir: &std::path::Path) -> Option<PathBuf> {
    if !cfg!(all(windows, debug_assertions)) || exe_dir.join("sidecar").is_dir() {
        return None;
    }
    let assets = exe_dir.join("dshr-debug-assets");
    assets
        .join("sidecar/dsh-official/default-profile.json")
        .is_file()
        .then_some(assets)
}

/// 应用资源根：`DSHR_APP_ROOT` → 包内资源 → 暂存的 Windows 调试资源 → 编译期仓库根。
/// 打包后整目录拷走即可运行，不再依赖构建机上的源码树。
fn app_root() -> PathBuf {
    if let Some(root) = non_empty_env_path("DSHR_APP_ROOT") {
        return root;
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        if dir.join("sidecar").is_dir() {
            return dir;
        }
        if let Some(assets) = windows_debug_assets(&dir) {
            return assets;
        }
    }
    std::path::absolute(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Windows release 构建的控制台重建（GUI 子系统没有控制台，CLI 仍要能打印）。
#[cfg(windows)]
#[cfg_attr(debug_assertions, allow(dead_code))]
mod win_console {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GetConsoleWindow, GetStdHandle, SetConsoleOutputCP,
        SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// 打开当前控制台的屏幕缓冲区/输入缓冲区设备（`CONOUT$` / `CONIN$`）。
    fn open_console_device(name: &str) -> Option<HANDLE> {
        let handle = unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        // windows-sys 的 HANDLE 是 *mut c_void，INVALID_HANDLE_VALUE 就是 -1。
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
    }

    fn handle_is_valid(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    /// 挂到父进程（PowerShell/cmd）的控制台，并把标准句柄装回去。
    ///
    /// - 从终端运行：挂上父控制台 → stdout/stderr 可打印（UTF-8 代码页修中文乱码）；
    /// - 双击启动：父进程是 explorer，挂不上 → 不显示控制台、继续开窗；
    /// - 已被重定向（`Start-Process -RedirectStandardOutput`）：保留继承的管道句柄；
    /// - 启动时已经带控制台（少见）：不再 `FreeConsole`，只补装标准句柄——释放后再
    ///   `AttachConsole(ATTACH_PARENT_PROCESS)` 会被 Windows 拒绝（ERROR_ACCESS_DENIED）。
    ///
    /// 返回是否成功得到可用的控制台（失败不影响 GUI 启动）。
    pub fn attach_parent_console() -> bool {
        if unsafe { GetConsoleWindow() }.is_null() {
            unsafe {
                FreeConsole();
            }
            if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
                return false;
            }
        }

        // 关键：Rust std 每次写 stdio 都会 GetStdHandle，所以 SetStdHandle 立即生效。
        // 已重定向（继承到有效管道）的句柄保持不动。
        for (std_handle, device) in [
            (STD_OUTPUT_HANDLE, "CONOUT$"),
            (STD_ERROR_HANDLE, "CONOUT$"),
            (STD_INPUT_HANDLE, "CONIN$"),
        ] {
            let current = unsafe { GetStdHandle(std_handle) };
            if handle_is_valid(current) {
                continue;
            }
            if let Some(handle) = open_console_device(device) {
                unsafe { SetStdHandle(std_handle, handle) };
            }
        }
        // 默认代码页（中文系统是 936）会把 Rust 写出的 UTF-8 字节显示成乱码。
        unsafe { SetConsoleOutputCP(65001) };
        true
    }
}

/// release GUI 构建里，命令行用法先接回父控制台；debug/非 Windows 保持原样。
#[cfg(all(windows, not(debug_assertions)))]
fn prepare_cli_console(is_cli: bool) {
    if is_cli {
        win_console::attach_parent_console();
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn prepare_cli_console(_is_cli: bool) {}

/// 崩溃诊断：panic 时把位置/消息/工作目录追加到 `DSHR_CRASH_LOG`
/// （默认落在数据根下，见 `prepare_data_env`），再交给默认 hook（stderr 照常输出）。
/// GUI 模式下用户能看到"出过什么错"。
fn install_crash_hook() {
    use std::io::Write;
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut report = format!(
            "=== panic {} ===\nlocation: {:?}\n",
            now_ts(),
            info.location()
        );
        if let Some(message) = info.payload().downcast_ref::<&str>() {
            report.push_str(message);
            report.push('\n');
        } else if let Some(message) = info.payload().downcast_ref::<String>() {
            report.push_str(message);
            report.push('\n');
        }
        if let Ok(cwd) = std::env::current_dir() {
            report.push_str(&format!("cwd: {}\n", cwd.display()));
        }
        let path = std::env::var("DSHR_CRASH_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".tmp/crash.log"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(report.as_bytes());
        }
        eprintln!("[chat] 已记录崩溃诊断到 {}", path.display());
        default(info);
    }));
}

fn now_ts() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos}")
}

/// 上下文外部化对象目录：默认在会话目录旁的 `spill/`，可用 `DSHR_SPILL_DIR` 覆盖。
fn spill_dir_for(session_path: &std::path::Path) -> PathBuf {
    spill_dir_in(
        session_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".tmp")),
    )
}

fn spill_dir_in(sessions_dir: &std::path::Path) -> PathBuf {
    if let Ok(dir) = std::env::var("DSHR_SPILL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    sessions_dir.join("spill")
}

/// 自检重置会话夹具：主文件 + `.bak` + `.corrupt-*` 全部清掉。
///
/// 只删主文件的话，`recover_backup()` 会在下一次自检启动时把上次的备份恢复回来，
/// 造成“新会话里混入旧事件”的测试污染（P1-1 恢复机制的副作用）。
fn reset_session_file(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    if let (Some(parent), Some(name)) = (
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) {
        let prefix = format!("{name}.corrupt-");
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// 侧栏的相对时间（不引入日期库）。
fn relative_time(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(updated_at);
    if diff < 60 {
        String::from("刚刚")
    } else if diff < 3_600 {
        format!("{} 分钟前", diff / 60)
    } else if diff < 86_400 {
        format!("{} 小时前", diff / 3_600)
    } else {
        format!("{} 天前", diff / 86_400)
    }
}

const SUMMARY_PROMPT: &str = "你是对话压缩器。把下面的对话压缩成 200 字以内的中文摘要，\
保留：任务目标、已确认的事实、关键决定、未完成的步骤、工具调用的结论。只输出摘要，不要客套。";

enum DesktopTurnUpdate {
    Rows(Vec<Row>),
    Finished {
        plugin_errors: Vec<String>,
        adc_panels: Vec<AdcPanelView>,
    },
}

/// 一个最小的 Rust 原生插件：注册 `probe_native` 工具 + 一个 Panel。
struct ProbeNativePlugin;

impl Plugin for ProbeNativePlugin {
    fn id(&self) -> &'static str {
        "builtin.probe"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "probe_native",
                "把输入文本转成大写（Rust 原生插件，不经过 Node）",
                ToolOrigin::Native {
                    plugin_id: "builtin.probe".to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            }))
            .with_presentation("generic"),
        )?;
        ctx.register_panel(
            dshr_plugin_api::PanelContribution::new(
                "probe.panel",
                "Probe Panel",
                "workspace.panel",
            )
            // 插件面板的 ADC 视图名（`render_view` 的入参；与注册 id 分开）。
            .with_view("probe.panel"),
        );
        Ok(())
    }

    /// ADC-v1 面板 view：证明"原生插件 → 宿主 → adc_panel_body"的完整产品链路。
    fn render_view(&mut self, view: &str) -> Result<serde_json::Value, PluginError> {
        if view != "probe.panel" {
            return Err(PluginError::Invalid(format!("unknown view `{view}`")));
        }
        Ok(json!({
            "type": "Card",
            "props": {"title": "Probe Panel"},
            "children": [
                {"type": "Text", "props": {"value": "原生插件 ADC 面板（probe_native）"}},
                {"type": "Row", "props": {"gap": 8, "align": "center"}, "children": [
                    {"type": "Badge", "props": {"label": "native", "tone": "success"}},
                    {"type": "Text", "props": {"value": "只读渲染 · 事件留 M4", "style": "caption"}}
                ]}
            ]
        }))
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != "probe_native" {
            return Err(PluginError::UnknownTool {
                plugin: "builtin.probe".to_owned(),
                tool: tool.to_owned(),
            });
        }
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        Ok(json!({ "native_echo": text.to_uppercase() }))
    }
}

/// 原生写入工具：走沙箱 seam（路径根 + 写前快照 + 1 MiB 限制）。
///
/// 工具展示类型是 `diff`，所以默认策略会要求审批；审批通过后才写文件。
struct WorkspaceWritePlugin {
    workspace: Workspace,
    tool: &'static str,
    id: &'static str,
}

impl WorkspaceWritePlugin {
    fn new(root: &Path, tool: &'static str, id: &'static str) -> Result<Self, String> {
        if !root.is_dir() {
            return Err("工作区目录不可用".to_owned());
        }
        Workspace::new(root)
            .map(|workspace| Self {
                workspace,
                tool,
                id,
            })
            .map_err(|error| error.to_string())
    }
}

impl Plugin for WorkspaceWritePlugin {
    fn id(&self) -> &'static str {
        self.id
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                self.tool,
                "写入工作区内的 UTF-8 文件。支持新建多层父目录，例如 hello-world/src/main.rs。路径相对工作区，覆盖前留快照，单次限 1 MiB。",
                ToolOrigin::Native {
                    plugin_id: self.id.to_owned(),
                },
            )
            .with_schema(json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }))
            .with_presentation("diff"),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if tool != self.tool {
            return Err(PluginError::UnknownTool {
                plugin: self.id.to_owned(),
                tool: tool.to_owned(),
            });
        }
        let path = arguments
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing path")))?;
        let content = arguments
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or_else(|| PluginError::Invalid(String::from("missing content")))?;
        let receipt = self
            .workspace
            .write_text_creating_parents(path, content)
            .map_err(|err| PluginError::Invalid(err.to_string()))?;
        Ok(json!({
            "path": receipt.path.display().to_string(),
            "bytes": receipt.bytes,
            "snapshot": receipt.snapshot.map(|snapshot| snapshot.id),
        }))
    }
}

/// 工具/写入的工作区根：`DSHR_WORKSPACE` → **可写的**当前目录 → 数据根下的 `workspace/`。
///
/// 和状态目录一样必须与 CWD 解耦：桌面快捷方式"起始位置"为空时 CWD 可能是
/// `C:\Windows\System32`，或 exe 被放进 Program Files。旧实现把工具工作区钉在 CWD，
/// 沙箱初始化在不可写的 CWD 上直接失败（`os error 5`），用户看到的就是"启动即退出"。
fn workspace_root() -> PathBuf {
    if let Some(dir) = non_empty_env_path("DSHR_WORKSPACE") {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .filter(|dir| is_writable_dir(dir))
    {
        return cwd;
    }
    let fallback = data_root().join("workspace");
    let _ = std::fs::create_dir_all(&fallback);
    eprintln!(
        "[chat] 当前工作目录不可写，工具工作区改用 {}（可用 DSHR_WORKSPACE 指定）",
        fallback.display()
    );
    fallback
}

fn loader_inputs() -> Option<(PathBuf, Vec<PathBuf>)> {
    let bundle = PathBuf::from(std::env::var_os("DSHR_LOADER_BUNDLE")?);
    let patches = std::env::var_os("DSHR_LOADER_PATCH")
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some((bundle, patches))
}

fn read_agent_presets() -> ParsedAgentPresets {
    read_agent_presets_from(loader_inputs().as_ref())
}

fn read_agent_presets_from(inputs: Option<&(PathBuf, Vec<PathBuf>)>) -> ParsedAgentPresets {
    match inputs {
        Some((bundle, patches)) => dshr_profile::loader::AgentPresets::from_files(bundle, patches),
        None => Ok(dshr_profile::loader::AgentPresets::default()),
    }
}

fn creator_preset_id() -> &'static str {
    if std::env::var("DSHR_SIDECAR").as_deref() == Ok("dsh") {
        creator::CORDIS_PRESET
    } else {
        creator::PRESET
    }
}

fn blank_root_created(log: &SessionLog) -> Option<&SessionCreated> {
    log.created()
        .filter(|created| !created.is_child() && log.events().len() == 1)
}

fn prepare_desktop_sidecar(profile: &Profile) {
    set_env_default("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());
}

/// 启动 Sidecar；失败时原生工具仍可用。
///
/// 三种运行时（`DSHR_SIDECAR`）：
/// - `probe`：最小协议探针（仅显式诊断命令）；
/// - `cordis`：真实 Cordis 4.0.5 + 只注册 `ctx.bridge.tool` 的探针插件（仅显式诊断命令）；
/// - `dsh`：桌面默认，真实 Cordis + 官方 DSH 插件。
///
/// 启动路径统一走：
/// 1. Node 解析 `DSHR_NODE` → 捆绑 `runtime/node` → PATH（`dshr-sidecar-guard`）；
/// 2. Job Object 包裹（进程数/内存上限 + 随宿主退出，Windows）；
/// 3. 官方 DSH 侧车额外协商 `fs.write`：官方 write/edit/apply_patch 的写盘请求
///    回到 Rust 走审批 + 沙箱（见 `docs/m2-official-write.md`）。
fn spawn_sidecar(
    approvals: Option<Arc<ApprovalBroker>>,
    l2_access: Option<Arc<ChatSessionAccess>>,
) -> Option<SidecarHost> {
    spawn_sidecar_in(&workspace_root(), approvals, None, l2_access, true)
        .inspect_err(|error| eprintln!("[chat] {error}"))
        .ok()
}

fn spawn_sidecar_in(
    workspace: &Path,
    approvals: Option<Arc<ApprovalBroker>>,
    live_controls: Option<(LivePermission, Arc<AtomicBool>)>,
    l2_access: Option<Arc<ChatSessionAccess>>,
    diagnostics: bool,
) -> Result<SidecarHost, String> {
    if !workspace.is_dir() {
        return Err(format!("sidecar 工作区目录不可用：{}", workspace.display()));
    }
    // 资源根按 exe 相对解析：`dist/dshr-dev` 整目录拷走后仍能找到 sidecar/（见 `app_root`）。
    let root = app_root();
    let interactive = live_controls.is_some();
    let mode = std::env::var("DSHR_SIDECAR")
        .ok()
        .filter(|mode| !mode.is_empty())
        .unwrap_or_else(|| if diagnostics { "probe" } else { "dsh" }.to_owned());
    let (script, plugin) = match mode.as_str() {
        "cordis" if diagnostics => (
            root.join("sidecar/cordis/sidecar-cordis.mjs"),
            root.join("sidecar/cordis/bridge-plugin.mjs"),
        ),
        "dsh" => {
            let dir = root.join("sidecar/dsh-official");
            let modules = dir.join("node_modules");
            if !modules.is_dir() {
                return Err(format!("官方 DSH sidecar 缺少依赖：{}", modules.display()));
            }
            let script = dir.join("sidecar-dsh.mjs");
            // 官方侧车不需要插件路径参数，但协议签名保持两个文件参数。
            (script.clone(), script)
        }
        "probe" if diagnostics => (
            root.join("sidecar/probe-sidecar.mjs"),
            root.join("sidecar/probe-plugin.mjs"),
        ),
        "probe" | "cordis" => return Err(format!("Sidecar {mode} 仅供诊断命令使用")),
        _ => return Err(format!("不支持的 Sidecar 模式 {mode}")),
    };

    let mut options = HostOptions::default()
        .with_current_dir(workspace)
        .with_env("DSHR_WORKSPACE", workspace.to_string_lossy());
    if mode == "dsh" {
        if let Some(effective) = std::env::var("DSHR_DSH_PLUGINS")
            .ok()
            .and_then(|raw| runtime_components::sidecar_profile(&raw))
        {
            let credentials: std::collections::BTreeMap<_, _> =
                runtime_components::web_credential_refs(&effective)
                    .into_iter()
                    .filter_map(|name| std::env::var(&name).ok().map(|value| (name, value)))
                    .collect();
            if !credentials.is_empty() {
                options = options.with_env(
                    "DSHR_WEB_CREDENTIAL_VALUES",
                    serde_json::to_string(&credentials).expect("string map"),
                );
            }
            options = options.with_env("DSHR_DSH_PLUGINS", effective);
        }
    }
    match dshr_sidecar_guard::resolve_node() {
        Some(node) => {
            eprintln!(
                "[chat] sidecar node={}（{:?}）",
                node.program.display(),
                node.source
            );
            options = options.with_program(node.program);
        }
        None => eprintln!("[chat] 未找到捆绑 Node，回退到 PATH 上的 node"),
    }
    // 只有官方 DSH 侧车实现了 fs.write.request；其它侧车保持只读能力集。
    // `DSHR_FS_WRITE=0` 可显式关闭官方写工具（只读自检/受限环境），默认开启。
    let write_enabled = std::env::var("DSHR_FS_WRITE")
        .map(|value| value != "0")
        .unwrap_or(true);
    if mode == "dsh" && write_enabled {
        if let Some(approvals) = approvals {
            match FsWriteService::open(workspace, approvals) {
                Ok(service) => {
                    let service = if let Some((permission, cancel)) = live_controls {
                        service.with_live_controls(move || permission.file_write_policy(), cancel)
                    } else {
                        service
                    };
                    options = options.with_fs_write(Arc::new(service));
                }
                Err(err) => eprintln!("[chat] 官方写工具不可用（沙箱初始化失败）: {err}"),
            }
        }
    }
    // L2 v2 Host Service：只有官方 sidecar 实现了 host.service.*；默认开启协商，
    // `DSHR_HOST_SERVICES=0` 显式关闭（受限环境/回退 M2 行为）。握手协商到 1 时
    // SidecarHost 自动回退 v1，router 完全不会被使用（安全失败）。
    let l2_enabled = std::env::var("DSHR_HOST_SERVICES")
        .map(|value| value != "0")
        .unwrap_or(true);
    if mode == "dsh" && l2_enabled {
        if let Some(access) = l2_access {
            let search_name = if cfg!(windows) { "rg.exe" } else { "rg" };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                other => other,
            };
            let platform = match std::env::consts::OS {
                "windows" => "win32",
                "macos" => "darwin",
                other => other,
            };
            let packaged_search = root.join(format!(
                "sidecar/dsh-official/node_modules/@vscode/ripgrep-{platform}-{arch}/bin/{search_name}"
            ));
            let search = if packaged_search.is_file() {
                Some(packaged_search)
            } else {
                path_executable(search_name)
            };
            let shell = if interactive {
                let (name, executable) = if cfg!(windows) {
                    ("pwsh", "pwsh.exe")
                } else {
                    ("bash", "bash")
                };
                let bundled = root.join("runtime/powershell/pwsh.exe");
                let path = if cfg!(windows) && bundled.is_file() {
                    Some(bundled)
                } else {
                    path_executable(executable)
                };
                path.map(|path| ShellProgram { name, path })
            } else {
                None
            };
            let process_caps = [
                (search.is_some(), CAP_SEARCH_PROCESS),
                (shell.is_some(), CAP_SHELL_PROCESS),
            ];
            let jobs = shell.clone().and_then(|shell| {
                ToolJobService::new(workspace, data_root().join("tool-jobs"), shell)
                    .inspect_err(|error| eprintln!("[chat] 官方后台任务不可用: {error}"))
                    .ok()
            });
            let jobs_available = jobs.is_some();
            let terminals = shell.as_ref().and_then(|shell| {
                let terminals = ToolTerminalService::new(workspace, access.clone())
                    .inspect_err(|error| eprintln!("[chat] 官方终端不可用: {error}"))
                    .ok()?;
                options.env.push((
                    "DSHR_TERMINAL_SHELL".to_owned(),
                    shell.path.to_string_lossy().into_owned(),
                ));
                Some(terminals)
            });
            let terminals_available = terminals.is_some();
            let processes = if process_caps.iter().any(|(enabled, _)| *enabled) {
                match ToolProcessService::new(workspace, search, shell) {
                    Ok(service) => Some(service),
                    Err(error) => {
                        eprintln!("[chat] 官方进程工具不可用: {error}");
                        None
                    }
                }
            } else {
                None
            };
            let processes_available = processes.is_some();
            match build_l2_router_at(
                access,
                data_root().join("storage"),
                workspace.to_path_buf(),
                "dsh-official",
                processes,
                jobs,
                terminals,
                interactive,
            ) {
                Ok(router) => {
                    options = options.with_host_services(router);
                    options.extra_caps.push(CAP_IMAGE_ATTACHMENTS.to_owned());
                    options.extra_caps.push(CAP_USER_QUESTIONS.to_owned());
                    options.extra_caps.push(CAP_AGENT_TOOL_CONTEXT.to_owned());
                    options.extra_caps.push(CAP_TOOL_GOALS.to_owned());
                    options.extra_caps.push(CAP_TOOL_SESSION_QUERY.to_owned());
                    options.extra_caps.push(CAP_TOOL_SUBAGENTS.to_owned());
                    options.extra_caps.push(CAP_TOOL_TEAMS.to_owned());
                    options.extra_caps.push(CAP_TOOL_SCHEDULES.to_owned());
                    options.extra_caps.push(CAP_PROGRAM_TOOLS.to_owned());
                    if jobs_available {
                        options.extra_caps.push(CAP_BACKGROUND_JOBS.to_owned());
                    }
                    if terminals_available {
                        options.extra_caps.push(CAP_TERMINAL_PROCESS.to_owned());
                    }
                    if processes_available {
                        options.extra_caps.extend(
                            process_caps
                                .into_iter()
                                .filter(|(enabled, _)| *enabled)
                                .map(|(_, cap)| cap.to_owned()),
                        );
                    }
                }
                Err(err) => eprintln!("[chat] L2 host-service 不可用（回退 v1）: {err}"),
            }
        }
    }
    options = options.with_guard(GuardLimits::sidecar_default());
    SidecarHost::spawn_with(script, plugin, options)
        .map_err(|error| format!("sidecar 启动失败（{mode}）：{error}"))
}

fn path_executable(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// 构造 v2 Host Service Router，并在 runtime.ready 协商后更新 generation。
fn build_l2_router_at(
    access: Arc<ChatSessionAccess>,
    storage_root: PathBuf,
    workspace: PathBuf,
    plugin_id: &str,
    processes: Option<ToolProcessService>,
    jobs: Option<ToolJobService>,
    terminals: Option<ToolTerminalService>,
    interactive: bool,
) -> Result<Arc<Mutex<HostServiceRouter>>, String> {
    let mut router = HostServiceRouter::new(
        "node-pending",
        PROTOCOL_MAX,
        vec![CAP_SERVICE_PROXY.to_owned()],
    )
    .map_err(|error| format!("router: {} ({})", error.message, error.code))?;
    let storage = StorageKvService::open(&storage_root)
        .map_err(|error| format!("storage-kv: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(storage))
        .map_err(|error| format!("register storage-kv: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(WorkingDirectoryService::new(
            access.clone(),
            &workspace,
        )))
        .map_err(|error| {
            format!(
                "register working-directory: {} ({})",
                error.message, error.code
            )
        })?;
    let images = dsh_image_attachments::ImageAttachmentService::new(
        &workspace,
        access.spill_store(),
        access.clone(),
    )
    .map_err(|error| format!("image-attachments: {error}"))?;
    router.register(Box::new(images)).map_err(|error| {
        format!(
            "register image-attachments: {} ({})",
            error.message, error.code
        )
    })?;
    router
        .register(Box::new(dsh_user_questions::UserQuestionService::new(
            interactive.then(|| access.questions.clone()),
        )))
        .map_err(|error| {
            format!(
                "register user-questions: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_goals::GoalService::new(access.clone())))
        .map_err(|error| format!("register tool-goals: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(dsh_session_query::SessionQueryService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-session-query: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_subagents::SubagentService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-subagents: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(dsh_teams::TeamService::new(access.clone())))
        .map_err(|error| format!("register tool-teams: {} ({})", error.message, error.code))?;
    router
        .register(Box::new(dsh_schedules::ScheduleService::new(
            access.clone(),
        )))
        .map_err(|error| {
            format!(
                "register tool-schedules: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(SessionLogService::new(access)))
        .map_err(|error| format!("register session-log: {} ({})", error.message, error.code))?;
    let mut inject = vec![
        STORAGE_KV_V1.to_owned(),
        WORKING_DIRECTORY_V1.to_owned(),
        SESSION_LOG_V1.to_owned(),
        IMAGE_ATTACHMENTS_V1.to_owned(),
        USER_QUESTIONS_V1.to_owned(),
        TOOL_GOALS_V1.to_owned(),
        TOOL_SESSION_QUERY_V1.to_owned(),
        TOOL_SUBAGENTS_V1.to_owned(),
        TOOL_TEAMS_V1.to_owned(),
        TOOL_SCHEDULES_V1.to_owned(),
    ];
    if let Some(processes) = processes {
        router.register(Box::new(processes)).map_err(|error| {
            format!("register tool-process: {} ({})", error.message, error.code)
        })?;
        inject.push(TOOL_PROCESS_V1.to_owned());
    }
    if let Some(jobs) = jobs {
        router
            .register(Box::new(jobs))
            .map_err(|error| format!("register tool-jobs: {} ({})", error.message, error.code))?;
        inject.push(TOOL_JOBS_V1.to_owned());
    }
    if let Some(terminals) = terminals {
        router.register(Box::new(terminals)).map_err(|error| {
            format!(
                "register tool-terminals: {} ({})",
                error.message, error.code
            )
        })?;
        inject.push(TOOL_TERMINALS_V1.to_owned());
    }
    router.register_plugin_inject(plugin_id, inject);
    Ok(Arc::new(Mutex::new(router)))
}

/// 把 sidecar 注册映射进同一个 ToolRegistry。
fn register_sidecar_tools(plugins: &mut PluginHost, host: &SidecarHost, headless: bool) {
    for definition in host.tool_definitions() {
        if headless
            && (definition.presentation.as_deref() == Some("terminal")
                || definition.name.starts_with("terminal_")
                || matches!(
                    definition.name.as_str(),
                    "bash"
                        | "pwsh"
                        | "shell"
                        | "powershell"
                        | "exec"
                        | "job_list"
                        | "job_output"
                        | "job_kill"
                ))
        {
            continue;
        }
        let name = definition.name.clone();
        if let Err(err) = plugins.registry_mut().register(definition) {
            eprintln!("[chat] 注册 sidecar 工具 `{name}` 失败: {err}");
        }
    }
}

struct OpenedSessionLog {
    log: SessionLog,
    errors: Vec<String>,
    save_blocked: Option<String>,
    wrote_created: bool,
    recovered: bool,
}

fn open_session_log(path: &std::path::Path, session_id: &str) -> OpenedSessionLog {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let _writer = match std::fs::create_dir_all(parent).and_then(|_| try_lock_session(path)) {
        Ok(writer) => writer,
        Err(error) => {
            let message = format!("无法取得会话写入权：{error}");
            let log = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl(session_id, &text).ok())
                .unwrap_or_else(|| SessionLog::new(session_id));
            return OpenedSessionLog {
                log,
                errors: vec![message.clone()],
                save_blocked: Some(message),
                wrote_created: false,
                recovered: false,
            };
        }
    };
    let mut errors = Vec::new();
    let mut recovered = false;
    // 上次保存若在 Windows remove→rename 窗口崩溃，主文件会缺失而 .bak 完整：
    // 先尝试恢复，避免把可恢复的历史当成“新会话”重写。
    if !path.exists() {
        match recover_backup(path) {
            Ok(true) => {
                recovered = true;
                eprintln!("[chat] 已从备份恢复会话日志 {}", path.display());
                errors.push(format!(
                    "已恢复：会话从备份恢复（{}），可继续使用",
                    path.display()
                ));
            }
            Ok(false) => {}
            Err(err) => errors.push(format!("会话备份恢复失败：{err}")),
        }
    }
    let mut save_blocked: Option<String> = None;
    let mut log = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(text) => match SessionLog::from_jsonl(session_id, &text) {
                Ok(log) => log,
                // 损坏：隔离原文件（字节保留供导出排查），绝不覆盖。
                Err(err) => {
                    match quarantine_corrupt(path) {
                        Ok(quarantined) => {
                            let message = format!(
                                "会话日志损坏，已隔离为 {}（原文件保留，可导出排查）: {err}",
                                quarantined.display()
                            );
                            eprintln!("[chat] {message}");
                            errors.push(message);
                        }
                        Err(quarantine_err) => {
                            let message = format!(
                                "会话日志损坏且隔离失败（为避免覆盖，已停止写入）: {err}; {quarantine_err}"
                            );
                            eprintln!("[chat] {message}");
                            errors.push(message);
                            save_blocked = Some(String::from("会话日志损坏且无法隔离"));
                        }
                    }
                    SessionLog::new(session_id)
                }
            },
            Err(err) => {
                // 文件存在但读失败（锁定/权限）：绝不能当空会话重写。
                let message = format!("读取会话日志失败（为避免覆盖，已停止写入）: {err}");
                eprintln!("[chat] {message}");
                errors.push(message);
                save_blocked = Some(format!("读取会话日志失败: {err}"));
                SessionLog::new(session_id)
            }
        }
    } else {
        SessionLog::new(session_id)
    };
    // 新会话：把 `session.created` 作为第一条事件（父链/来源/工作区的权威来源）。
    let mut wrote_created = false;
    if log.is_empty() {
        let workspace = workspace_root().display().to_string();
        match log.append_created(now_ts(), &SessionCreated::root(workspace)) {
            Ok(_) => wrote_created = true,
            Err(err) => eprintln!("[chat] 写入 session.created 失败: {err}"),
        }
    }

    if wrote_created && save_blocked.is_none() {
        if let Err(error) = save_atomic(path, log.to_jsonl().as_bytes()) {
            let message = format!("保存会话失败：{error}");
            errors.push(message.clone());
            save_blocked = Some(message);
        }
    }
    OpenedSessionLog {
        log,
        errors,
        save_blocked,
        wrote_created,
        recovered,
    }
}
/// 旧日志可能包含别的 ID 格式。只恢复此处生成的消息计数。
fn last_message_seq(log: &SessionLog) -> u64 {
    log.events()
        .iter()
        .filter_map(|event| {
            let id = event.payload.get("message_id")?.as_str()?;
            let seq = id
                .strip_prefix("m-u")
                .or_else(|| id.strip_prefix("m-a"))
                .or_else(|| id.strip_prefix("m-sum"))?;
            seq.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0)
}

fn session_workspace(log: &SessionLog) -> PathBuf {
    if let Some(workspace) = log
        .created()
        .map(|created| created.workspace_id.trim())
        .filter(|workspace| !workspace.is_empty())
    {
        let path = Path::new(workspace);
        if path.is_absolute() {
            return path.to_path_buf();
        }
        let path = workspace_root().join(path);
        return std::path::absolute(&path).unwrap_or(path);
    }
    workspace_root()
}

fn register_probe_write(plugins: &mut PluginHost, workspace: &Path) {
    let result = WorkspaceWritePlugin::new(workspace, "probe_write", "builtin.probe-write")
        .and_then(|plugin| {
            plugins
                .load(Box::new(plugin))
                .map_err(|error| error.to_string())
        });
    if let Err(error) = result {
        eprintln!("[chat] 工作区写入工具不可用: {error}");
    }
}

impl ChatState {
    /// 将会话运行事件转换为界面行快照，供桌面和需要行投影的诊断使用。
    fn stream_turn(&mut self, text: &str, emit: &mut dyn FnMut(Vec<Row>)) -> Option<String> {
        self.stream_turn_with_attachments(text, &[], emit)
    }

    fn stream_turn_with_attachments(
        &mut self,
        text: &str,
        attachments: &[DraftAttachment],
        emit: &mut dyn FnMut(Vec<Row>),
    ) -> Option<String> {
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return None;
        }
        match self.run_turn(text, attachments, &mut |update| {
            if let TurnUpdate::Preview(log, streaming, reasoning) = update {
                emit(rows_with_preview(log, streaming, reasoning));
            }
        }) {
            Ok(result) => Some(result.reply),
            Err(error) => {
                let mut rows = rows_snapshot(&self.log, None);
                rows.push(Row::Meta(format!("消息未发送：{error}")));
                emit(rows);
                None
            }
        }
    }
}

/// 滚动帧探针：用 GPUI profiler 的 `FrameTiming` 记录真实帧耗时与呈现间隔。
///
/// 由 `DSHR_FRAME_PROBE=<帧数>` 开启：每帧推进滚动位置、请求下一帧，达到目标帧数后
/// 把统计写到 `DSHR_FRAME_LOG`（默认 `.tmp/frame-probe.txt`）并按 `DSHR_FRAME_QUIT`
/// （默认 1）退出。这是 Gate 2 的测量手段：时间戳来自渲染管线，不是伪造的 sleep。
struct FrameProbe {
    target: usize,
    dir: f32,
    scroll_pos: f32,
    step: f32,
    collector: FrameTimingCollector,
    draw_work_ns: Vec<u64>,
    layout_ns: Vec<u64>,
    prepaint_ns: Vec<u64>,
    paint_ns: Vec<u64>,
    present_interval_ns: Vec<u64>,
    refresh_ns: Option<u64>,
    viewport: Option<(f32, f32)>,
    scale: f32,
    scroll_max: f32,
    rows: usize,
    started: std::time::Instant,
    log_path: PathBuf,
    quit: bool,
    ticks: usize,
    debug: bool,
}

impl FrameProbe {
    fn from_env() -> Option<Self> {
        let target = std::env::var("DSHR_FRAME_PROBE")
            .ok()?
            .parse::<usize>()
            .ok()?;
        if target == 0 {
            return None;
        }
        let step = std::env::var("DSHR_FRAME_STEP")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.01);
        let log_path = std::env::var("DSHR_FRAME_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".tmp/frame-probe.txt"));
        let quit = std::env::var("DSHR_FRAME_QUIT")
            .map(|value| value != "0")
            .unwrap_or(true);
        Some(Self {
            target,
            dir: 1.0,
            scroll_pos: 0.0,
            step,
            collector: FrameTimingCollector::new(),
            draw_work_ns: Vec::new(),
            layout_ns: Vec::new(),
            prepaint_ns: Vec::new(),
            paint_ns: Vec::new(),
            present_interval_ns: Vec::new(),
            refresh_ns: None,
            viewport: None,
            scale: 1.0,
            scroll_max: 0.0,
            rows: 0,
            started: std::time::Instant::now(),
            log_path,
            quit,
            ticks: 0,
            debug: std::env::var("DSHR_FRAME_DEBUG").is_ok(),
        })
    }

    /// 收集上一帧的 profiler 事件并推进滚动；返回 true 表示达到目标帧数。
    fn tick(
        &mut self,
        scroll: &ScrollHandle,
        viewport: gpui::Size<gpui::Pixels>,
        scale: f32,
        rows: usize,
    ) -> bool {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(timing) => {
                    self.draw_work_ns.push(
                        timing
                            .draw_end
                            .saturating_duration_since(timing.draw_start)
                            .as_nanos() as u64,
                    );
                    self.layout_ns
                        .push(timing.phases.request_layout.as_nanos() as u64);
                    self.prepaint_ns
                        .push(timing.phases.prepaint.as_nanos() as u64);
                    self.paint_ns.push(timing.phases.paint.as_nanos() as u64);
                    if self.refresh_ns.is_none() {
                        self.refresh_ns = timing.refresh_interval.map(|d| d.as_nanos() as u64);
                    }
                }
                FrameEvent::Present(present) => {
                    if let Some(interval) = present.animation_interval {
                        self.present_interval_ns.push(interval.as_nanos() as u64);
                    }
                }
            }
        }
        // 滚动范围按当前窗口（逻辑像素）计算；看不到内容时仍以 1px 兜底，避免除零。
        let max = scroll.max_offset().y.as_f32().max(1.0);
        self.scroll_pos = (self.scroll_pos + self.dir * self.step).clamp(0.0, 1.0);
        if self.scroll_pos >= 1.0 {
            self.dir = -1.0;
        }
        if self.scroll_pos <= 0.0 {
            self.dir = 1.0;
        }
        scroll.set_offset(point(px(0.0), px(-self.scroll_pos * max)));
        self.viewport = Some((viewport.width.as_f32(), viewport.height.as_f32()));
        self.scale = scale;
        self.scroll_max = max;
        self.rows = rows;
        self.ticks += 1;
        if self.debug && self.ticks.is_multiple_of(60) {
            eprintln!(
                "[probe] ticks={} draws={} trace={}",
                self.ticks,
                self.draw_work_ns.len(),
                gpui::profiler::trace_enabled()
            );
        }
        // 安全阀：120s 内一帧都没完成说明内容量让首帧无法收敛（例如一次性展开上万行），
        // 记录空报告并退出，避免探针把窗口挂死。
        if self.draw_work_ns.is_empty() && self.started.elapsed() > Duration::from_secs(120) {
            return true;
        }
        self.draw_work_ns.len() >= self.target
    }

    fn report(&self) -> String {
        let draws = self.draw_work_ns.len();
        let elapsed = self.started.elapsed();
        let fps = if elapsed.as_secs_f64() > 0.0 {
            draws as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let mut out = String::new();
        out.push_str(&format!(
            "[frame-probe] draws={} elapsed_ms={} avg_fps={:.1} rows={} viewport={:?} scale={} scroll_max={:.0}\n",
            draws,
            elapsed.as_millis(),
            fps,
            self.rows,
            self.viewport,
            self.scale,
            self.scroll_max
        ));
        for (name, values) in [
            ("draw_work", &self.draw_work_ns),
            ("request_layout", &self.layout_ns),
            ("prepaint", &self.prepaint_ns),
            ("paint", &self.paint_ns),
            ("present_interval", &self.present_interval_ns),
        ] {
            if values.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "[frame-probe] {} p50_us={:.2} p95_us={:.2} max_us={:.2} samples={}\n",
                name,
                percentile_ns(values, 0.50) as f64 / 1_000.0,
                percentile_ns(values, 0.95) as f64 / 1_000.0,
                values.iter().copied().max().unwrap_or(0) as f64 / 1_000.0,
                values.len()
            ));
        }
        let budget = self.refresh_ns.unwrap_or(16_666_667);
        let missed = self
            .draw_work_ns
            .iter()
            .filter(|&&work| work > budget)
            .count();
        out.push_str(&format!(
            "[frame-probe] refresh_ns={:?} budget_us={:.2} missed_refresh={} missed_ratio={:.4}\n",
            self.refresh_ns,
            budget as f64 / 1_000.0,
            missed,
            if draws == 0 {
                0.0
            } else {
                missed as f64 / draws as f64
            }
        ));
        out
    }
}

fn percentile_ns(values: &[u64], pct: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// ADC Input/Textarea 的编辑会话（M4 W3）：用真实 `TextInput`，提交时把值注入 onChange。
struct AdcEditor {
    plugin_id: String,
    view: String,
    path: String,
    on_change: AdcAction,
    input: Entity<TextInput>,
    multiline: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ComposerMenu {
    Model,
    ModelList,
    Reasoning,
    Permission,
    Preset,
    Commands,
    Add,
}

struct SessionMenu {
    session_id: String,
    position: Option<gpui::Point<gpui::Pixels>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlashCommand {
    New,
    Creator,
    Search,
    Turns,
    Plugins,
    Theme,
}

impl SlashCommand {
    const ALL: [Self; 6] = [
        Self::New,
        Self::Creator,
        Self::Search,
        Self::Turns,
        Self::Plugins,
        Self::Theme,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Creator => "creator",
            Self::Search => "search",
            Self::Turns => "turns",
            Self::Plugins => "plugins",
            Self::Theme => "theme",
        }
    }

    fn description(self) -> &'static str {
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

fn slash_commands(text: &str) -> impl Iterator<Item = SlashCommand> + '_ {
    let prefix = text.strip_prefix('/');
    SlashCommand::ALL
        .into_iter()
        .filter(move |command| prefix.is_some_and(|prefix| command.name().starts_with(prefix)))
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "只读",
        PermissionMode::WorkspaceWrite => "工作区写入",
        PermissionMode::DangerFullAccess => "工具免审批",
    }
}

fn reasoning_label(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "默认",
        Some(ReasoningEffort::Off) => "关闭",
        Some(ReasoningEffort::Low) => "低",
        Some(ReasoningEffort::High) => "高",
        Some(ReasoningEffort::Max) => "最高",
    }
}

const EDITABLE_PERMISSIONS: [(PermissionMode, &str); 3] = [
    (PermissionMode::ReadOnly, "拒绝写入和执行类工具"),
    (PermissionMode::WorkspaceWrite, "写入和执行需要确认"),
    (
        PermissionMode::DangerFullAccess,
        "普通工具和工作区内写入直接执行",
    ),
];

struct AttachmentPreview {
    name: String,
    focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    image: Option<Result<Arc<RenderImage>, String>>,
    _load: gpui::Task<()>,
}

struct ComposerAttachment {
    draft: DraftAttachment,
    thumbnail: Option<Arc<RenderImage>>,
}

impl ComposerAttachment {
    fn new(draft: DraftAttachment) -> Self {
        let thumbnail = Self::thumbnail(&draft);
        Self { draft, thumbnail }
    }

    fn thumbnail(draft: &DraftAttachment) -> Option<Arc<RenderImage>> {
        match &draft.content {
            AttachmentContent::Media { format, bytes } => {
                decode_image(bytes, *format, THUMBNAIL_EDGE).ok()
            }
            AttachmentContent::Text(_) => None,
        }
    }
}

struct ChatApp {
    creator_mode: bool,
    active_preset: Option<String>,
    blank_root: bool,
    preset_notice: Option<String>,
    /// Agent / 工具 / 日志都在工作线程里改；UI 只读快照，所以这里必须加锁。
    state: Arc<Mutex<ChatState>>,
    /// 当前渲染用的整份快照（由工作线程推送）。
    rows: Vec<Row>,
    open_processes: std::collections::HashSet<usize>,
    copied_message: Option<(String, usize, std::time::Instant)>,
    /// 真正的文本输入组件（实现 EntityInputHandler，中文 IME 组合输入可用）。
    input: Entity<TextInput>,
    scroll: ScrollHandle,
    message_selection: MessageSelection,
    activity_open: bool,
    activity_scroll: ScrollHandle,
    settings: DesktopSettings,
    permission: LivePermission,
    model_options: Vec<Arc<DesktopModel>>,
    model_settings: model_settings::ModelSettings,
    composer_menu: Option<ComposerMenu>,
    #[cfg(target_os = "windows")]
    window_menu: Option<window_bar::WindowMenu>,
    slash_text: Option<String>,
    selected_command: usize,
    turn_menu_open: bool,
    turn_jump: Option<usize>,
    turn_focus: Option<usize>,
    follow_transcript: bool,
    attachments: Vec<ComposerAttachment>,
    attachment_error: Option<String>,
    attachment_load: Option<u64>,
    attachment_request: u64,
    attachment_preview: Option<AttachmentPreview>,
    attachment_thumbnails: RefCell<VecDeque<StoredAttachment>>,
    /// 一轮正在生成：输入框仍可编辑，但回车不会发第二条。
    busy: bool,
    /// 与 provider 共享的停止句柄（UI 不持锁就能请求中断）。
    cancel: Arc<AtomicBool>,
    /// DSH 设计 token（浅色/深色）。
    theme_kind: ThemeKind,
    preferences: settings_view::Preferences,
    settings_tab: Option<settings_view::SettingsTab>,
    settings_focus: FocusHandle,
    settings_restore_focus: Option<FocusHandle>,
    settings_notice: Option<String>,
    runtime_installing: Option<runtime_components::Component>,
    runtime_notice: Option<String>,
    /// 左侧会话栏收起状态（对齐 DSH 的折叠侧栏）。
    sidebar_collapsed: Option<bool>,
    /// 首次渲染后把历史滚到最新一条（DSH 打开会话也是停在底部）。
    initial_scroll_done: bool,
    /// 会话目录索引与列表。
    store: SessionIndex,
    sessions: Vec<SessionMeta>,
    session_search: Entity<TextInput>,
    session_search_open: bool,
    session_menu: Option<SessionMenu>,
    header_menu: Option<session_header::HeaderMenu>,
    header_notice: Option<(bool, String)>,
    right_sidebar_open: bool,
    show_archived: bool,
    workspace_session_limits: HashMap<String, usize>,
    session_id: String,
    active_workspace: PathBuf,
    folded_workspaces: std::collections::HashSet<String>,
    workspace_picker_open: bool,
    workspace_dialog_open: bool,
    workspace_notice: Option<String>,
    /// 正在重命名的会话与输入框（None = 不在重命名）。
    renaming: Option<String>,
    rename_input: Option<Entity<TextInput>>,
    /// 等待二次确认删除的会话（None = 没有待确认项）。
    pending_delete: Option<String>,
    /// 工具审批 broker（与 ChatState 共享同一实例）。
    approvals: Arc<ApprovalBroker>,
    /// 上一次渲染时的 pending 数，用于只在变化时触发重绘。
    pending_approvals: usize,
    questions: Option<Arc<dsh_user_questions::QuestionBroker>>,
    question_draft: Option<user_question_view::QuestionDraft>,
    approval_error: Option<(PendingApproval, String)>,
    approval_saving: Option<PendingApproval>,
    tool_approvals_loading: bool,
    /// 插件 profile（启停状态持久化）。
    profile: Profile,
    /// 主内容区的插件管理页。
    plugins_open: bool,
    plugin_focus: gpui::FocusHandle,
    plugin_panels_open: bool,
    plugin_detail: Option<usize>,
    plugin_manifest_path: Option<PathBuf>,
    plugin_edit_json: bool,
    plugin_search: Entity<TextInput>,
    plugin_config_input: Entity<TextInput>,
    plugin_notice: Option<String>,
    /// 等待用户确认的 `openExternal` URL（M4 W3；确认前不打开）。
    pending_external: Option<String>,
    /// ADC Input/Textarea 的编辑模态（None = 未编辑）。
    adc_editor: Option<AdcEditor>,
    /// 长会话只渲染最近 N 条（点"载入更早"每次 +200），避免每帧构造上万元素。
    visible_limit: usize,
    /// 帧探针（DSHR_FRAME_PROBE）：滚动时测量真实帧耗时与呈现间隔。
    frame_probe: Option<FrameProbe>,
    /// 会话树深度（session id → depth）；侧栏按树顺序展示并缩进子会话。
    session_depths: std::collections::BTreeMap<String, u32>,
    /// sidecar 启动/插件装载/插件面板错误：插件面板顶部提示（避免"插件失败但界面无感"）。
    plugin_errors: Vec<String>,
    sidecar_reload_pending: bool,
    /// 插件 ADC 面板快照（渲染路径只读；刷新见加载插件/打开面板处，不持锁渲染）。
    adc_panels: Vec<AdcPanelView>,
    collapsed_trial_cards: HashSet<u64>,
    reveal_trial_call: Option<u64>,
}

impl ChatApp {
    /// 请求停止生成：provider 在下一个 SSE 行边界返回已生成的部分。
    fn stop(&mut self, cx: &mut Context<Self>) {
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
    fn on_submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
    fn button_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.stop(cx);
            return;
        }
        let text = self.input.read(cx).content().to_owned();
        if self.submit_composer(&text, window, cx) {
            self.input.update(cx, |input, cx| input.clear(window, cx));
        }
    }

    fn start_turn(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
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
                        }) => {
                            finished = Some((plugin_errors, adc_panels));
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
                        if let Some((errors, panels)) = finished {
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

    fn on_composer_input_changed(&mut self, input: Entity<TextInput>, cx: &mut Context<Self>) {
        let input = input.read(cx);
        let text = input.content();
        let slash_text =
            (!self.busy && !input.is_composing() && slash_commands(text).next().is_some())
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

    fn submit_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.attachment_preview.is_some() {
            return false;
        }
        if self.composer_menu == Some(ComposerMenu::Commands) {
            let Some(command) = slash_commands(text).nth(self.selected_command) else {
                return false;
            };
            self.composer_menu = None;
            let chat = cx.weak_entity();
            // TextInput 调用此回调时仍在更新自身。先让它清空，再执行会切换焦点的指令。
            window.defer(cx, move |window, cx| {
                let _ = chat.update(cx, |chat, cx| {
                    chat.run_slash_command(command, window, cx);
                });
            });
            return true;
        }
        self.on_submit(text, window, cx)
    }

    fn move_slash_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(text) = self.slash_text.as_deref() else {
            return;
        };
        let count = slash_commands(text).count();
        if self.composer_menu == Some(ComposerMenu::Commands) && count > 0 {
            self.selected_command =
                (self.selected_command as isize + delta).rem_euclid(count as isize) as usize;
            cx.notify();
        }
    }

    fn run_slash_command(
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

    fn theme(&self) -> Theme {
        Theme::new(self.theme_kind)
    }

    fn show_empty_composer(&self) -> bool {
        self.rows.is_empty() && !self.busy && !self.activity_open
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.set_theme(self.theme_kind.toggled(), cx);
    }

    fn set_theme(&mut self, kind: ThemeKind, cx: &mut Context<Self>) {
        self.theme_kind = kind;
        self.preferences.theme = kind;
        self.save_preferences();
        cx.notify();
    }

    fn sidebar_is_collapsed(&self, window: &Window) -> bool {
        self.sidebar_collapsed
            .unwrap_or(window.bounds().size.width < px(600.0))
    }

    fn toggle_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let collapsed = self.sidebar_is_collapsed(window);
        self.session_menu = None;
        self.sidebar_collapsed = Some(!collapsed);
        cx.notify();
    }

    /// 打开/关闭插件管理页；打开时补一次 ADC 面板快照。
    fn toggle_plugins_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// ADC Input/Textarea 点击：打开真实 TextInput 模态；提交时把值注入 onChange。
    fn open_adc_editor(
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

    fn commit_adc_edit(&mut self, cx: &mut Context<Self>) {
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

    fn cancel_adc_edit(&mut self, cx: &mut Context<Self>) {
        self.adc_editor = None;
        cx.notify();
    }

    fn adc_editor_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
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
    fn request_open_external(&mut self, url: String, cx: &mut Context<Self>) {
        self.pending_external = Some(url);
        cx.notify();
    }

    fn external_confirm_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
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

    /// 键盘审批：处理当前第一个待确认调用；没有 pending 时返回 false。
    fn resolve_first_approval(&mut self, outcome: ApprovalOutcome, cx: &mut Context<Self>) -> bool {
        let Some(first) = self.approvals.pending().first().cloned() else {
            return false;
        };
        if self.approval_saving.as_ref() == Some(&first) {
            return false;
        }
        self.approvals.resolve(&first.call_id, outcome);
        self.approval_error = None;
        self.pending_approvals = self.approvals.pending().len();
        cx.notify();
        true
    }

    // ------------------------------------------------------------------
    // 键盘动作（M4 W2）：处理函数只做状态变更，键位映射见 `chat_key_bindings()`。
    // ------------------------------------------------------------------

    fn shortcut_new_session(
        &mut self,
        _: &NewSessionShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("new-session");
        self.new_session(window, cx);
    }

    fn shortcut_focus_composer(
        &mut self,
        _: &FocusComposerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("focus-composer");
        self.focus_composer(window, cx);
    }

    fn shortcut_toggle_sidebar(
        &mut self,
        _: &ToggleSidebarShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("toggle-sidebar");
        self.toggle_sidebar(window, cx);
    }

    fn shortcut_toggle_plugins(
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

    fn shortcut_approve_first(
        &mut self,
        _: &ApproveFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("approve-first");
        self.resolve_first_approval(ApprovalOutcome::AllowOnce, cx);
    }

    fn shortcut_deny_first(
        &mut self,
        _: &DenyFirstShortcut,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        trace_shortcut("deny-first");
        self.resolve_first_approval(ApprovalOutcome::Deny, cx);
    }

    fn shortcut_escape(&mut self, _: &EscapeShortcut, window: &mut Window, cx: &mut Context<Self>) {
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

    fn shortcut_quit(&mut self, _: &QuitShortcut, _window: &mut Window, cx: &mut Context<Self>) {
        trace_shortcut("quit");
        cx.quit();
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn reload_sessions(&mut self) {
        let (sessions, depths) = ordered_sessions(&self.store);
        self.sessions = sessions;
        self.session_depths = depths;
    }

    fn active_title(&self) -> String {
        self.sessions
            .iter()
            .find(|session| session.id == self.session_id)
            .map(|session| session.title.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    fn open_session_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_is_collapsed(window) {
            self.toggle_sidebar(window, cx);
        }
        self.session_menu = None;
        self.session_search_open = true;
        window.focus(&self.session_search.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    fn close_session_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_search_open = false;
        self.session_search
            .update(cx, |input, cx| input.clear(window, cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// 切到另一个会话：加载日志并刷新对话视图，保留窗口的侧车和插件。
    ///
    /// 生成中不允许切换——工作线程还在往旧 state 写，切走会让 UI 与日志分叉。
    fn open_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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

    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.new_session_workspace();
        self.new_session_with_preset(workspace, None, window, cx);
    }

    fn new_creator_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.plugins_open = false;
        self.plugin_panels_open = false;
        let workspace = self.new_session_workspace();
        self.new_session_with_preset(workspace, Some(creator_preset_id()), window, cx);
    }

    fn new_session_workspace(&self) -> PathBuf {
        if self.active_workspace.is_dir() {
            self.active_workspace.clone()
        } else {
            workspace_root()
        }
    }

    fn delete_session(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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

    fn set_session_archived(
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

    fn start_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
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

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        self.rename_input = None;
        cx.notify();
    }

    /// 把插件面板的 `call`/`command` 动作派发给插件，然后刷新面板树（M4 W3）。
    ///
    /// 动作是 fire-and-forget；插件更新自身状态后，`refresh_adc_panels` 再发
    /// `ui.render` 取新树。失败只进日志，不阻塞 UI 其它部分。
    fn dispatch_adc_action(
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

    fn inline_trial_call(&self, row: &Row) -> Option<u64> {
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

    fn plugin_panels(
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

    fn session_list(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let query = self.session_search.read(cx).content().trim().to_lowercase();
        let groups = workspace_sessions(
            &self.sessions,
            &self.session_depths,
            self.show_archived,
            &query,
        );
        let active_root = self.session_branch().first();
        let mut list = div().flex().flex_col().gap(px(8.0)).w_full();
        if groups.is_empty() {
            return list
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_secondary))
                        .child(if self.show_archived && query.is_empty() {
                            "暂无已归档会话"
                        } else {
                            "没有匹配的会话"
                        }),
                )
                .into_any_element();
        }
        for (
            group_index,
            WorkspaceSessions {
                workspace,
                sessions,
            },
        ) in groups.into_iter().enumerate()
        {
            let label = workspace.map_or("未分组会话", workspace_name);
            let key = workspace.unwrap_or("").to_owned();
            let fold_key = key.clone();
            let folded = query.is_empty() && self.folded_workspaces.contains(&key);
            let path = workspace.map(|workspace| workspace_root().join(workspace));
            let mut group = div().flex().flex_col().gap(px(2.0)).child(
                div()
                    .group("workspace-row")
                    .flex()
                    .items_center()
                    .pr(px(4.0))
                    .rounded(t.radii.sm)
                    .hover(|style| style.bg(p.color(p.interactive_hover)))
                    .child(
                        chrome_button(("workspace-fold", group_index), "", t)
                            .aria_label(label.to_owned())
                            .aria_expanded(!folded)
                            .focusable()
                            .flex_1()
                            .min_w(px(0.0))
                            .h(px(34.0))
                            .justify_start()
                            .gap(px(6.0))
                            .child(desktop_icon(
                                if folded { "\u{e8b7}" } else { "\u{e838}" },
                                "▱",
                            ))
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(label.to_owned()),
                            )
                            .tooltip(toolbar_tooltip(workspace.unwrap_or(label).to_owned(), *t))
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                if !chat.folded_workspaces.remove(&fold_key) {
                                    chat.folded_workspaces.insert(fold_key.clone());
                                    chat.workspace_session_limits.remove(&fold_key);
                                }
                                cx.notify();
                            })),
                    )
                    .when_some(path, |row, path| {
                        row.child(
                            toolbar_icon_button(
                                ("workspace-new-session", group_index),
                                "在此工作区新建会话",
                                "\u{e710}",
                                "+",
                                t,
                            )
                            .invisible()
                            .group_hover("workspace-row", |style| style.visible())
                            .on_click(cx.listener(
                                move |chat, _, window, cx| {
                                    chat.new_session_in(path.clone(), window, cx);
                                },
                            )),
                        )
                    }),
            );
            if !folded {
                let total = sessions.len();
                let selected = sessions
                    .iter()
                    .take(WORKSPACE_SESSION_AUTO_LIMIT)
                    .position(|(_, session)| active_root.is_some_and(|root| root.id == session.id));
                let base_limit = WORKSPACE_SESSION_PAGE.max(selected.map_or(0, |index| index + 1));
                let limit = if query.is_empty() {
                    base_limit.max(
                        self.workspace_session_limits
                            .get(&key)
                            .copied()
                            .unwrap_or(0),
                    )
                } else {
                    total
                };
                for (index, session) in sessions.into_iter().take(limit) {
                    group = group.child(div().pl(px(12.0)).child(self.session_row(
                        index,
                        session,
                        active_root.is_some_and(|root| root.id == session.id),
                        t,
                        cx,
                    )));
                }
                if query.is_empty() && total > base_limit {
                    let remaining = total.saturating_sub(limit);
                    let label = if remaining == 0 {
                        "收起".to_owned()
                    } else {
                        format!("展开其余 {remaining} 个会话")
                    };
                    group = group.child(
                        chrome_button(("workspace-more-sessions", group_index), "", t)
                            .aria_label(label.clone())
                            .aria_expanded(remaining == 0)
                            .focusable()
                            .justify_start()
                            .pl(px(22.0))
                            .text_color(p.color(p.label_caption))
                            .child(label)
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                chat.session_menu = None;
                                if remaining == 0 {
                                    chat.workspace_session_limits.remove(&key);
                                } else {
                                    chat.workspace_session_limits.insert(
                                        key.clone(),
                                        (limit + WORKSPACE_SESSION_PAGE).min(total),
                                    );
                                }
                                cx.notify();
                            })),
                    );
                }
            }
            list = list.child(group);
        }
        list.into_any_element()
    }

    fn session_menu_trigger(
        &self,
        index: usize,
        session: &SessionMeta,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let open = self
            .session_menu
            .as_ref()
            .filter(|menu| menu.session_id == session.id);
        let id = session.id.clone();
        let trigger = chrome_button(("session-actions", index), "", t)
            .aria_label("会话操作")
            .focusable()
            .size(px(24.0))
            .px(px(0.0))
            .child(desktop_icon("\u{e712}", "⋯"))
            .aria_expanded(open.is_some())
            .when(open.is_none(), |button| {
                button
                    .invisible()
                    .group_hover("session-row", |style| style.visible())
                    .focus_visible(|style| style.visible())
            })
            .on_click(cx.listener(move |chat, _, _, cx| {
                cx.stop_propagation();
                let same_button = chat
                    .session_menu
                    .as_ref()
                    .is_some_and(|menu| menu.session_id == id && menu.position.is_none());
                chat.session_menu = (!same_button).then(|| SessionMenu {
                    session_id: id.clone(),
                    position: None,
                });
                cx.notify();
            }));
        let Some(open) = open else {
            return trigger.into_any_element();
        };

        let rename_id = session.id.clone();
        let delete_id = session.id.clone();
        let archive_id = session.id.clone();
        let archived = session.archived;
        let menu = div()
            .id("session-menu")
            .role(Role::Menu)
            .aria_label("会话操作")
            .occlude()
            .flex()
            .flex_col()
            .w(px(152.0))
            .p(px(4.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm()
            .child(
                chrome_button("session-menu-rename", "重命名", t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.start_rename(rename_id.clone(), window, cx);
                    })),
            )
            .child(
                chrome_button("session-menu-archive", archive_label(archived), t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.set_session_archived(&archive_id, !archived, window, cx);
                    })),
            )
            .child(
                chrome_button("session-menu-delete", "删除", t)
                    .role(Role::MenuItem)
                    .justify_start()
                    .text_color(p.color(p.state_error))
                    .focusable()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.delete_session(delete_id.clone(), window, cx);
                    })),
            );
        let dismiss = cx.listener(|chat, _: &gpui::MouseDownEvent, _, cx| {
            chat.session_menu = None;
            cx.notify();
        });
        if let Some(position) = open.position {
            div()
                .child(trigger)
                .child(popover::at_point(
                    position,
                    popover::dismiss_on_outside(menu, |_| false, dismiss),
                ))
                .into_any_element()
        } else {
            popover::attach_dismissible(
                trigger,
                Some(menu),
                gpui::Anchor::BottomLeft,
                gpui::Anchor::TopLeft,
                dismiss,
            )
        }
    }

    fn session_row(
        &self,
        index: usize,
        session: &SessionMeta,
        is_active: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let is_renaming = self.renaming.as_deref() == Some(session.id.as_str());

        if is_renaming {
            let input = self.rename_input.clone();
            let row = div()
                .id(("rename-row", index))
                .w_full()
                .px(px(8.0))
                .py(px(5.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .rounded(t.radii.sm)
                .bg(p.color(p.tag))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_primary))
                        .children(input),
                )
                .child(
                    div()
                        .id(("rename-ok", index))
                        .role(Role::Button)
                        .flex_none()
                        .aria_label("保存会话名称")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_success))
                        .child("✓")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            cx.stop_propagation();
                            this.commit_rename(cx);
                        })),
                )
                .child(
                    div()
                        .id(("rename-cancel", index))
                        .role(Role::Button)
                        .flex_none()
                        .aria_label("取消重命名")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child("✕")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            cx.stop_propagation();
                            this.cancel_rename(cx);
                        })),
                );
            return row.into_any_element();
        }

        let open_id = session.id.clone();
        let menu_id = session.id.clone();
        let is_menu_open = self
            .session_menu
            .as_ref()
            .is_some_and(|menu| menu.session_id == session.id);
        let is_pending_delete = self.pending_delete.as_deref() == Some(session.id.as_str());
        let actions: AnyElement = if is_pending_delete {
            let confirm_id = session.id.clone();
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .id(("confirm-delete", index))
                        .role(Role::Button)
                        .aria_label("确认删除会话")
                        .px(px(4.0))
                        .rounded(t.radii.xs)
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child("删除")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.delete_session(confirm_id.clone(), window, cx);
                        })),
                )
                .child(
                    div()
                        .id(("cancel-delete", index))
                        .role(Role::Button)
                        .aria_label("取消删除会话")
                        .px(px(4.0))
                        .cursor_pointer()
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child("取消")
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            cx.stop_propagation();
                            this.pending_delete = None;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            let archive_id = session.id.clone();
            let archived = session.archived;
            div()
                .flex()
                .items_center()
                .child(
                    toolbar_icon_button(
                        ("archive-session", index),
                        archive_label(archived),
                        if archived { "\u{e8a7}" } else { "\u{e7b8}" },
                        if archived { "↶" } else { "▣" },
                        t,
                    )
                    .size(px(24.0))
                    .when(!is_menu_open, |button| {
                        button
                            .invisible()
                            .group_hover("session-row", |style| style.visible())
                            .focus_visible(|style| style.visible())
                    })
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        cx.stop_propagation();
                        chat.set_session_archived(&archive_id, !archived, window, cx);
                    })),
                )
                .child(self.session_menu_trigger(index, session, t, cx))
                .into_any_element()
        };
        div()
            .id(("session-row", index))
            .group("session-row")
            .aria_label(session.title.clone())
            .w_full()
            .pl(px(10.0))
            .pr(px(10.0))
            .min_h(px(32.0))
            .py(px(3.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(6.0))
            .rounded(t.radii.sm)
            .when(is_active, |row| row.bg(p.color(p.business_tertiary)))
            .hover(|style| style.bg(p.color(p.interactive_hover_solid)))
            .when(is_menu_open && !is_active, |row| {
                row.bg(p.color(p.interactive_hover_solid))
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(t.type_scale.small)
                    .line_height(px(18.0))
                    .when(is_active, |title| title.font_weight(FontWeight::MEDIUM))
                    .text_color(p.color(if session.archived {
                        p.label_secondary
                    } else {
                        p.label_primary
                    }))
                    .child(session.title.clone()),
            )
            .child(if is_pending_delete {
                actions
            } else {
                div()
                    .relative()
                    .min_w(px(48.0))
                    .flex()
                    .flex_none()
                    .justify_end()
                    .child(
                        div()
                            .id(("session-time", index))
                            .flex_none()
                            .text_size(px(11.0))
                            .line_height(px(18.0))
                            .text_color(p.color(p.label_secondary))
                            .when(is_menu_open, |time| time.invisible())
                            .group_hover("session-row", |style| style.invisible())
                            .child(relative_time(session.updated_at)),
                    )
                    .child(div().absolute().right_0().top(px(-3.0)).child(actions))
                    .into_any_element()
            })
            .when(!session.archived, |row| {
                row.role(Role::Button)
                    .cursor_pointer()
                    .on_click(cx.listener(move |chat, _, window, cx| {
                        chat.open_session(open_id.clone(), window, cx);
                    }))
            })
            .when(!is_pending_delete, |row| {
                row.on_aux_click(cx.listener(move |chat, event, _, cx| {
                    if matches!(event, gpui::ClickEvent::Mouse(mouse) if mouse.up.button == gpui::MouseButton::Right) {
                        cx.stop_propagation();
                        chat.session_menu = Some(SessionMenu {
                            session_id: menu_id.clone(),
                            position: Some(event.position()),
                        });
                        cx.notify();
                    }
                }))
            })
            .into_any_element()
    }

    fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let searching = !self.session_search.read(cx).content().is_empty();
        div()
            .w(t.layout.sidebar_width)
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(10.0))
            .bg(p.color(p.sidebar_fill))
            .child(
                div()
                    .h(px(36.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .size(px(24.0))
                            .rounded(t.radii.sm)
                            .bg(p.color(p.business_tertiary))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(t.type_scale.small)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.link))
                            .child("D"),
                    )
                    .child(
                        div()
                            .text_size(t.type_scale.body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.color(p.label_primary))
                            .child("DSHR"),
                    ),
            )
            .child(
                chrome_button("new-session", "", t)
                    .aria_label("新建会话")
                    .w_full()
                    .h(px(38.0))
                    .gap(px(6.0))
                    .bg(p.color(p.interactive_hover_solid))
                    .text_color(p.color(p.label_primary))
                    .font_weight(FontWeight::MEDIUM)
                    .child(desktop_icon("\u{e710}", "+"))
                    .child("新建会话")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_session(window, cx);
                    })),
            )
            .child(
                chrome_button("plugins-toggle", "", t)
                    .w_full()
                    .focusable()
                    .justify_start()
                    .gap(px(6.0))
                    .child(desktop_icon("\u{e71d}", "⋮"))
                    .child("插件")
                    .aria_label("打开或关闭插件面板")
                    .aria_selected(self.plugins_open)
                    .when(self.plugins_open, |button| {
                        button.bg(p.color(p.business_tertiary))
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_plugins_panel(window, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .border_t_1()
                    .border_color(p.color(p.border_l2))
                    .pt(px(10.0))
                    .child(
                        div()
                            .pl(px(8.0))
                            .pr(px(2.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(t.type_scale.small)
                            .text_color(p.color(p.label_secondary))
                            .child(div().flex_1().child("工作区"))
                            .child(self.workspace_add_button(t, cx))
                            .child(
                                toolbar_icon_button(
                                    "archived-sessions",
                                    "已归档会话",
                                    "\u{e7b8}",
                                    "▣",
                                    t,
                                )
                                .aria_selected(self.show_archived)
                                .when(self.show_archived, |button| {
                                    button.bg(p.color(p.tag)).text_color(p.color(p.link))
                                })
                                .on_click(cx.listener(
                                    |chat, _, _, cx| {
                                        chat.show_archived = !chat.show_archived;
                                        chat.workspace_session_limits.clear();
                                        chat.session_menu = None;
                                        chat.workspace_notice = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                toolbar_icon_button(
                                    "toggle-session-search",
                                    "搜索会话或工作区",
                                    "\u{e721}",
                                    "搜索",
                                    t,
                                )
                                .aria_expanded(self.session_search_open)
                                .when(self.session_search_open, |button| {
                                    button.bg(p.color(p.tag)).text_color(p.color(p.link))
                                })
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if this.session_search_open {
                                            this.close_session_search(window, cx);
                                        } else {
                                            this.open_session_search(window, cx);
                                        }
                                    },
                                )),
                            ),
                    )
                    .when(self.session_search_open, |workspace| {
                        workspace.child(
                            div()
                                .flex()
                                .items_center()
                                .px(px(6.0))
                                .py(px(4.0))
                                .rounded(t.radii.sm)
                                .border_1()
                                .border_color(p.color(p.border_l2))
                                .bg(p.color(p.input_major))
                                .text_color(p.color(p.label_primary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .child(self.session_search.clone()),
                                )
                                .when(searching, |search| {
                                    search.child(
                                        chrome_button("clear-session-search", "×", t)
                                            .aria_label("清除会话搜索")
                                            .h(px(24.0))
                                            .px(px(6.0))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.session_search.update(cx, |input, cx| {
                                                    input.clear(window, cx)
                                                });
                                                let handle =
                                                    this.session_search.read(cx).focus_handle(cx);
                                                window.focus(&handle, cx);
                                            })),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .id("session-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .on_scroll(cx.listener(|chat, _: &gpui::ScrollEvent, _, cx| {
                        if chat.session_menu.take().is_some() {
                            cx.notify();
                        }
                    }))
                    .child(self.session_list(t, cx)),
            )
            .when_some(self.workspace_notice.as_ref(), |sidebar, notice| {
                sidebar.child(
                    div()
                        .px(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child(notice.clone()),
                )
            })
            .child(self.settings_trigger(false, t, cx))
            .into_any_element()
    }

    fn sidebar_rail(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w(px(48.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .py(px(10.0))
            .child(
                toolbar_icon_button("rail-new-session", "新建会话", "\u{e710}", "+", t)
                    .on_click(cx.listener(|chat, _, window, cx| chat.new_session(window, cx))),
            )
            .child(
                toolbar_icon_button("rail-plugins", "插件", "\u{e71d}", "◇", t)
                    .aria_selected(self.plugins_open)
                    .on_click(
                        cx.listener(|chat, _, window, cx| chat.toggle_plugins_panel(window, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(self.settings_trigger(true, t, cx))
            .into_any_element()
    }

    fn jump_to_turn(&mut self, row: usize, cx: &mut Context<Self>) {
        if !matches!(self.rows.get(row), Some(Row::User { .. })) {
            return;
        }
        self.activity_open = false;
        self.turn_menu_open = false;
        self.initial_scroll_done = true;
        self.follow_transcript = false;
        self.visible_limit = self.visible_limit.max(self.rows.len() - row);
        self.turn_jump = Some(row);
        self.turn_focus = Some(row);
        cx.notify();
    }

    fn turn_rail(&self, focus_row: usize, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let anchors: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, Row::User { .. }).then_some(index))
            .collect();
        if anchors.is_empty() {
            return None;
        }
        let active = anchors
            .partition_point(|row| *row <= focus_row)
            .saturating_sub(1);
        let p = t.palette;
        Some(
            div()
                .id("turn-rail")
                .aria_label("会话轮次")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(3.0))
                .w(px(22.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .children(turn_rail_window(anchors.len(), active).map(|turn| {
                    let row = anchors[turn];
                    let selected = turn == active;
                    let label = format!("跳到第 {} 轮", turn + 1);
                    let preview: String = turn_preview(&self.rows[row])
                        .unwrap_or_default()
                        .chars()
                        .take(50)
                        .collect();
                    let tooltip = if preview.is_empty() {
                        label.clone()
                    } else {
                        format!("{label} · {preview}")
                    };
                    div()
                        .id(("turn-marker", turn))
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_selected(selected)
                        .focusable()
                        .w_full()
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .pr(px(4.0))
                        .rounded(t.radii.xs)
                        .cursor_pointer()
                        .hover(|style| style.bg(p.color(p.interactive_hover)))
                        .child(
                            div()
                                .w(px(if selected { 14.0 } else { 8.0 }))
                                .h(px(2.0))
                                .rounded_full()
                                .bg(p.color(if selected { p.link } else { p.label_secondary })),
                        )
                        .tooltip(toolbar_tooltip(tooltip, *t))
                        .on_click(cx.listener(move |chat, _, _, cx| chat.jump_to_turn(row, cx)))
                }))
                .into_any_element(),
        )
    }

    fn turn_navigator(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let anchors: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row, Row::User { .. }).then_some(index))
            .collect();
        let p = t.palette;
        let mut menu = div()
            .id("turn-navigator")
            .role(Role::ListBox)
            .aria_label("轮次导航")
            .absolute()
            .top(gpui::relative(1.0))
            .right(px(12.0))
            .w(px(312.0))
            .max_w_full()
            .occlude()
            .p(px(5.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm();
        if anchors.is_empty() {
            menu = menu.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(t.type_scale.secondary)
                    .text_color(p.color(p.label_secondary))
                    .child("此会话暂无可跳转的轮次"),
            );
        } else {
            let height = 36.0 * anchors.len().min(6) as f32;
            let theme = *t;
            menu = menu.child(
                gpui::uniform_list(
                    "turn-anchors",
                    anchors.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|turn| {
                                let row = anchors[turn];
                                let preview = turn_preview(this.rows.get(row)?)?;
                                Some(
                                    div()
                                        .id(("turn-anchor", turn))
                                        .role(Role::ListBoxOption)
                                        .aria_label(format!("跳到第 {} 轮 {preview}", turn + 1))
                                        .h(px(36.0))
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .px(px(8.0))
                                        .rounded(theme.radii.sm)
                                        .cursor_pointer()
                                        .text_size(theme.type_scale.secondary)
                                        .text_color(p.color(p.label_secondary))
                                        .hover(|style| style.bg(p.color(p.interactive_hover)))
                                        .children([
                                            div().flex_none().child(format!("第 {} 轮", turn + 1)),
                                            div()
                                                .min_w(px(0.0))
                                                .overflow_hidden()
                                                .text_color(p.color(p.label_primary))
                                                .text_overflow(TextOverflow::Truncate("…".into()))
                                                .child(preview.to_owned()),
                                        ])
                                        .on_click(cx.listener(move |chat, _, _, cx| {
                                            chat.jump_to_turn(row, cx)
                                        })),
                                )
                            })
                            .collect()
                    }),
                )
                .h(px(height))
                .w_full(),
            );
        }
        gpui::deferred(menu).with_priority(1).into_any_element()
    }

    fn header(
        &self,
        t: &Theme,
        collapsed: bool,
        compact: bool,
        show_window_bar: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let hero = self.show_empty_composer();
        let sidebar_label = if collapsed {
            "展开侧栏"
        } else {
            "收起侧栏"
        };
        div()
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .justify_end()
            .gap(px(4.0))
            .px(if compact { px(12.0) } else { px(20.0) })
            .pt(px(6.0))
            .when(!hero, |header| {
                header
                    .min_h(t.layout.header_height)
                    .border_b_1()
                    .border_color(p.color(p.border_l2))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .when(!show_window_bar, |header| {
                        header.child(
                            toolbar_icon_button(
                                "sidebar-toggle",
                                sidebar_label,
                                "\u{e700}",
                                "☰",
                                t,
                            )
                            .aria_expanded(!collapsed)
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.toggle_sidebar(window, cx);
                                },
                            )),
                        )
                    })
                    .when(!hero, |header| {
                        header.child(self.session_heading(t, compact, cx))
                    })
                    .child(div().flex_1().min_w(px(0.0)))
                    .when(self.busy && !compact, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .child("生成中…"),
                        )
                    })
                    .when(!hero, |header| header.child(self.header_actions(t, cx)))
                    .when(!self.right_sidebar_open, |header| {
                        header.child(
                            toolbar_icon_button(
                                "expand-right-sidebar",
                                "展开右侧栏",
                                "\u{e89f}",
                                "▥",
                                t,
                            )
                            .aria_expanded(false)
                            .on_click(cx.listener(|chat, _, _, cx| {
                                chat.header_menu = None;
                                chat.composer_menu = None;
                                chat.workspace_picker_open = false;
                                chat.turn_menu_open = false;
                                chat.right_sidebar_open = true;
                                cx.notify();
                            })),
                        )
                    }),
            )
            .children((!hero).then(|| {
                div().flex().items_center().gap(px(6.0)).children(
                    [
                        ("conversation-tab", "对话", false),
                        ("activity-tab", "轨迹", true),
                    ]
                    .into_iter()
                    .map(|(id, label, activity)| {
                        let selected = self.activity_open == activity;
                        chrome_button(id, label, t)
                            .role(Role::Tab)
                            .aria_selected(selected)
                            .focusable()
                            .h(px(30.0))
                            .rounded(px(0.0))
                            .border_b_2()
                            .border_color(p.color(if selected { p.link } else { 0x0000_0000 }))
                            .when(selected, |tab| {
                                tab.text_color(p.color(p.link))
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activity_open = activity;
                                cx.notify();
                            }))
                    }),
                )
            }))
            .when_some(self.header_notice.as_ref(), |header, (error, notice)| {
                header.child(
                    div()
                        .pb(px(4.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(if *error {
                            p.state_error
                        } else {
                            p.label_secondary
                        }))
                        .child(notice.clone()),
                )
            })
            .when(self.turn_menu_open, |header| {
                header.child(self.turn_navigator(t, cx))
            })
            .into_any_element()
    }

    fn copy_message_button(
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

    fn message_attachments(
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

    fn message_thumbnail(
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

    fn open_attachment_preview(
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

    fn close_attachment_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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

    fn image_preview_overlay(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
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

    fn render_row(
        &self,
        row: &Row,
        index: usize,
        t: &Theme,
        base_font: &Font,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        match row {
            Row::User { text, attachments } => div()
                .group("message-row")
                .flex()
                .flex_col()
                .items_end()
                .gap(px(2.0))
                .when(!attachments.is_empty(), |row| {
                    row.child(self.message_attachments(attachments, index, t, window, cx))
                })
                .when(!text.is_empty(), |row| {
                    let text_view = gpui::SharedString::from(text.clone());
                    row.child(
                        div()
                            .max_w(gpui::relative(0.702))
                            .px(px(14.0))
                            .py(px(8.0))
                            .rounded(t.radii.lg)
                            .bg(p.color(p.bubble))
                            .text_size(t.type_scale.body)
                            .line_height(t.type_scale.body_line)
                            .text_color(p.color(p.label_primary))
                            .child(self.message_selection.fragment(
                                index,
                                0,
                                text_view.clone(),
                                gpui::StyledText::new(text_view),
                                p.color(p.business_primary).into(),
                            )),
                    )
                    .child(self.copy_message_button(index, text, t, cx))
                })
                .into_any_element(),
            Row::Assistant(text) | Row::Streaming(text) => {
                let streaming = is_streaming_row(row);
                div()
                    .group("message-row")
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .gap(px(4.0))
                    .child(selectable_markdown_view(
                        text,
                        t,
                        base_font,
                        p.color(p.label_primary),
                        &self.message_selection,
                        index,
                    ))
                    .when(streaming && !self.cancel.load(Ordering::Relaxed), |el| {
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_caption))
                                .child("正在生成"),
                        )
                    })
                    .when(!streaming && !text.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .child(self.copy_message_button(index, text, t, cx)),
                        )
                    })
                    .into_any_element()
            }
            Row::Process(steps) => {
                let open = self.open_processes.contains(&index);
                let tools = steps
                    .iter()
                    .filter(|row| matches!(row, Row::Tool { .. }))
                    .count();
                let thinking = steps.iter().any(|row| matches!(row, Row::Reasoning { .. }));
                let keep_visible = |row: &Row| {
                    self.inline_trial_call(row).is_some()
                        || matches!(row, Row::Tool { delivered_files, .. } if !delivered_files.is_empty())
                };
                let active = steps.iter().any(|row| {
                    matches!(
                        row,
                        Row::Tool { pending: true, .. }
                            | Row::Reasoning {
                                streaming: true,
                                ..
                            }
                    )
                });
                let label = match (active, thinking, tools) {
                    (true, true, 0) => "正在思考".to_owned(),
                    (_, true, 0) => "思考过程".to_owned(),
                    (true, _, _) => format!("进行中 · {tools} 次工具调用"),
                    (_, true, _) => format!("思考过程 · {tools} 次工具调用"),
                    _ => format!("{tools} 次工具调用"),
                };
                div()
                    .id(("turn-process", index))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("process-toggle")
                            .role(Role::Button)
                            .aria_expanded(open)
                            .focusable()
                            .cursor_pointer()
                            .w_full()
                            .h(px(26.0))
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .rounded(t.radii.sm)
                            .px(px(4.0))
                            .text_size(t.type_scale.secondary)
                            .text_color(p.color(p.label_secondary))
                            .hover(|style| style.bg(p.color(p.interactive_hover)))
                            .child(label)
                            .child(
                                desktop_icon(
                                    if open { "\u{e70e}" } else { "\u{e70d}" },
                                    if open { "▴" } else { "▾" },
                                )
                                .text_size(px(10.0)),
                            )
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                if !chat.open_processes.remove(&index) {
                                    chat.open_processes.insert(index);
                                }
                                cx.notify();
                            })),
                    )
                    .when(open || steps.iter().any(keep_visible), |group| {
                        group.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .when(open, |steps| {
                                    steps
                                        .pl(px(10.0))
                                        .border_l_1()
                                        .border_color(p.color(p.border_l2))
                                })
                                .children(
                                    steps.iter().filter(|row| open || keep_visible(row)).map(
                                        |row| self.render_row(row, index, t, base_font, window, cx),
                                    ),
                                ),
                        )
                    })
                    .into_any_element()
            }
            Row::Reasoning { text, .. } => div()
                .text_size(t.type_scale.secondary)
                .line_height(t.type_scale.body_line)
                .text_color(p.color(p.label_secondary))
                .child(text.clone())
                .into_any_element(),
            Row::Tool {
                call_seq,
                name,
                origin,
                status,
                detail,
                is_error,
                pending,
                card,
                delivered_files,
            } => {
                let trial_call = self.inline_trial_call(row);
                let expanded = !self.collapsed_trial_cards.contains(call_seq);
                let dot = if *pending {
                    p.state_warn
                } else if *is_error {
                    p.state_error
                } else {
                    p.state_success
                };
                let status_color = if *is_error {
                    p.state_error
                } else {
                    p.label_caption
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(t.radii.sm)
                    .border_1()
                    .border_color(p.color(p.border_l2))
                    .bg(p.color(p.bg_layer_1))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(p.color(dot)))
                            .child(
                                div()
                                    .text_size(t.type_scale.secondary)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.color(p.label_primary))
                                    .child(if trial_call.is_some() {
                                        "试用插件".into()
                                    } else {
                                        name.clone()
                                    }),
                            )
                            .when(trial_call.is_none(), |header| {
                                header.child(
                                    div()
                                        .px(px(6.0))
                                        .py(px(1.0))
                                        .rounded(t.radii.xs)
                                        .bg(p.color(p.tag))
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_tertiary))
                                        .child(origin.clone()),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(status_color))
                                    .child(status.clone()),
                            )
                            .when_some(trial_call, |header, seq| {
                                let label = if expanded {
                                    "收起面板"
                                } else {
                                    "展开面板"
                                };
                                header.child(
                                    chrome_button(("toggle-trial-panel", seq), label, t)
                                        .aria_expanded(expanded)
                                        .focusable()
                                        .on_click(cx.listener(move |chat, _, _, cx| {
                                            if !chat.collapsed_trial_cards.remove(&seq) {
                                                chat.collapsed_trial_cards.insert(seq);
                                            }
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .when(!detail.is_empty() && trial_call.is_none(), |el| {
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(detail.clone()),
                        )
                    })
                    // 有专用视图的工具结果：diff / terminal 用原生组件渲染正文，
                    // 不再只有一行摘要（正文已按卡片上限截断）。
                    .when_some(
                        card.as_ref().filter(|card| {
                            matches!(card.kind, CardKind::Diff | CardKind::Terminal)
                                && !card.body.is_empty()
                        }),
                        |el, card| {
                            let view = match card.kind {
                                CardKind::Diff => diff_view_from_text(t, &card.body),
                                _ => terminal_view_from_text(t, &card.body),
                            };
                            el.child(view)
                        },
                    )
                    // M4 W3：工具结果 `meta.ui` 的受限 ADC 附加区（tool card extra）。
                    .when_some(
                        card.as_ref().and_then(|card| card.extra.as_ref()),
                        |el, extra| {
                            let (body, _diagnostics) = adc_panel_body(t, extra);
                            el.child(body)
                        },
                    )
                    .children(
                        delivered_files
                            .iter()
                            .enumerate()
                            .map(|(file_index, file)| {
                                let path = file.path.clone();
                                let label = file
                                    .path
                                    .file_name()
                                    .unwrap_or(file.path.as_os_str())
                                    .to_string_lossy()
                                    .into_owned();
                                let button_id: gpui::SharedString =
                                    format!("delivered-file-{call_seq}").into();
                                chrome_button((button_id, file_index), "", t)
                                    .aria_label(format!("打开 {label}"))
                                    .child(label)
                                    .focusable()
                                    .when_some(file.description.clone(), |button, description| {
                                        button.child(
                                            div()
                                                .text_color(p.color(p.label_tertiary))
                                                .child(description),
                                        )
                                    })
                                    .on_click(cx.listener(move |chat, _, _, cx| {
                                        match dsh_deliverables::checked_file(
                                            &chat.active_workspace,
                                            &path,
                                        ) {
                                            Ok((path, _)) => cx.open_with_system(&path),
                                            Err(_) => {
                                                chat.workspace_notice =
                                                    Some("交付文件已不可用".to_owned());
                                                cx.notify();
                                            }
                                        }
                                    }))
                            }),
                    )
                    .when_some(trial_call.filter(|_| expanded), |el, seq| {
                        el.child(
                            div()
                                .id(("trial-panel-content", seq))
                                .max_h(trial_panel_max_height(window))
                                .overflow_y_scroll()
                                .child(self.plugin_panels(Some(seq), t, cx)),
                        )
                    })
                    .when_some(trial_call.filter(|_| !expanded), |el, seq| {
                        let titles = self
                            .adc_panels
                            .iter()
                            .filter(|panel| panel.trial_call_seq == Some(seq))
                            .map(|panel| panel.title.as_str())
                            .collect::<Vec<_>>()
                            .join("、");
                        el.child(
                            div()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .text_overflow(TextOverflow::Truncate("…".into()))
                                .child(titles),
                        )
                    })
                    .into_any_element()
            }
            Row::Meta(text) => div()
                .flex()
                .items_center()
                .justify_center()
                .py(px(4.0))
                .child(
                    div()
                        .px(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_caption))
                        .child(text.clone()),
                )
                .into_any_element(),
            Row::Stopped => div()
                .text_size(t.type_scale.secondary)
                .text_color(p.color(p.label_secondary))
                .child("已停止")
                .into_any_element(),
        }
    }

    fn transcript(
        &mut self,
        t: &Theme,
        show_turn_rail: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let base_font = window.text_style().font();
        self.message_selection.begin_frame(&self.session_id);
        let jump = (!self.activity_open)
            .then(|| self.turn_jump.take())
            .flatten();
        // 不 clone 整份 rows：10k 会话下每帧克隆 7500 个 String 是纯浪费。
        let activity = self.activity_open.then(|| {
            self.rows
                .iter()
                .flat_map(|row| match row {
                    Row::Process(steps) => steps.as_slice(),
                    row => std::slice::from_ref(row),
                })
                .filter(|row| matches!(row, Row::Tool { .. } | Row::Meta(_) | Row::Stopped))
                .collect::<Vec<_>>()
        });
        let total = activity.as_ref().map_or(self.rows.len(), Vec::len);
        let row_at = |index| match &activity {
            Some(rows) => rows[index],
            None => &self.rows[index],
        };
        let scroll = if self.activity_open {
            &self.activity_scroll
        } else {
            &self.scroll
        };
        let column_width = if self.activity_open {
            t.layout.content_width
        } else {
            t.layout.composer_max_width - px(32.0)
        };
        let row_gap = if self.activity_open { 16.0 } else { 14.0 };
        let item = || div().flex_none().w_full().max_w(column_width).mx_auto();
        let mut pane = div()
            .id(if self.activity_open {
                "activity-scroll"
            } else {
                "transcript-scroll"
            })
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px(px(24.0))
            .gap(px(row_gap))
            .on_scroll(
                cx.listener(|this, _event: &gpui::ScrollEvent, _window, cx| {
                    if !this.activity_open {
                        this.follow_transcript = false;
                        this.turn_focus = None;
                    }
                    cx.notify();
                }),
            )
            .py(px(20.0))
            .when(!self.activity_open, |pane| {
                self.message_selection.bind(pane)
            });
        if self.activity_open && total == 0 {
            pane = pane.child(
                item()
                    .py(px(28.0))
                    .text_size(t.type_scale.body)
                    .text_color(p.color(p.label_secondary))
                    .child("此会话暂无工具调用或上下文压缩记录。"),
            );
        }
        let start = total.saturating_sub(self.visible_limit);
        if start > 0 {
            let hidden = start;
            pane = pane.child(
                item()
                    .id("load-earlier")
                    .w_full()
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(t.type_scale.small)
                    .text_color(p.color(p.label_secondary))
                    .child(format!("载入更早的 {hidden} 条（每次 200 条）"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.visible_limit = this.visible_limit.saturating_add(200);
                        cx.notify();
                    })),
            );
        }
        // 变高行虚拟化：只构造/排版视口附近的几十行，用上下占位块维持滚动高度。
        // 实测 400 行全量渲染时 prepaint 要 125ms（debug），滚动只有 ~6fps；虚拟化后
        // 帧成本与历史长度基本无关，这是 Gate 2 "滚动帧率不低于基线" 的实现基础。
        let visible = total - start;
        let mut cumulative = Vec::with_capacity(visible + 1);
        cumulative.push(0.0f32);
        for index in 0..visible {
            let gap = if index == 0 { 0.0 } else { row_gap };
            let row_index = start + index;
            let height = match row_at(row_index) {
                Row::Process(steps) => {
                    let open = self.open_processes.contains(&row_index);
                    26.0 + steps
                        .iter()
                        .filter_map(|step| {
                            if let Some(seq) = self.inline_trial_call(step) {
                                let panel_height = if self.collapsed_trial_cards.contains(&seq) {
                                    0.0
                                } else {
                                    let count = self
                                        .adc_panels
                                        .iter()
                                        .filter(|panel| panel.trial_call_seq == Some(seq))
                                        .count();
                                    (count as f32 * 130.0)
                                        .min(trial_panel_max_height(window).as_f32())
                                };
                                Some(52.0 + panel_height)
                            } else {
                                open.then(|| estimated_row_height(step) + 6.0)
                            }
                        })
                        .sum::<f32>()
                }
                row => estimated_row_height(row),
            };
            let next = cumulative[index] + height + gap;
            cumulative.push(next);
        }
        let total_height = cumulative[visible];
        let viewport_height = scroll.bounds().size.height.as_f32().max(240.0);
        let jump = jump
            .and_then(|row| row.checked_sub(start))
            .filter(|row| *row < visible);
        let offset = if let Some(target) = jump {
            let approx = cumulative[target].min((total_height - viewport_height).max(0.0));
            scroll.set_offset(point(px(0.0), px(-approx)));
            approx
        } else {
            (-scroll.offset().y.as_f32()).clamp(0.0, total_height)
        };
        const OVERSCAN: f32 = 240.0;
        let top_edge = (offset - OVERSCAN).max(0.0);
        let bottom_edge = (offset + viewport_height + OVERSCAN).min(total_height);
        let mut first = cumulative
            .partition_point(|edge| *edge < top_edge)
            .saturating_sub(1)
            .min(visible.saturating_sub(1));
        let mut last = cumulative.partition_point(|edge| *edge < bottom_edge);
        if last <= first {
            last = (first + 1).min(visible);
        }
        if let Some(target) = jump {
            first = first.min(target);
            last = last.max(target + 1);
        }
        let top_spacer = (cumulative[first] - row_gap).max(0.0);
        let bottom_spacer = (total_height - cumulative[last] - row_gap).max(0.0);
        if top_spacer > 0.5 {
            pane = pane.child(item().h(px(top_spacer)));
        }
        if let Some(target) = jump {
            let prefix = usize::from(start > 0) + usize::from(top_spacer > 0.5);
            scroll.scroll_to_top_of_item(prefix + target - first);
        }
        for index in first..last {
            let row_index = start + index;
            pane = pane.child(item().child(self.render_row(
                row_at(row_index),
                row_index,
                t,
                &base_font,
                window,
                cx,
            )));
        }
        if bottom_spacer > 0.5 {
            pane = pane.child(item().h(px(bottom_spacer)));
        }
        if !show_turn_rail {
            return pane.into_any_element();
        }
        let focus_row = if let Some(row) = self.turn_focus.filter(|row| *row < total) {
            row
        } else if self.follow_transcript || offset + viewport_height >= total_height - 1.0 {
            total.saturating_sub(1)
        } else {
            let focus_edge = offset + viewport_height * 0.25;
            start
                + cumulative
                    .partition_point(|edge| *edge <= focus_edge)
                    .saturating_sub(1)
        };
        div()
            .relative()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .child(pane)
            .children(self.turn_rail(focus_row, t, cx))
            .into_any_element()
    }

    /// 工具审批卡片：工作线程阻塞在这里，等待"允许一次 / 拒绝"。
    fn approval_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.approvals.pending();
        let first = pending.first()?;
        let p = t.palette;
        let call_id = first.call_id.clone();
        let is_trial = first.tool == creator::TRY_PLUGIN_TOOL;
        let is_build = first.tool == creator::BUILD_PLUGIN_TOOL;
        let preview = if is_trial {
            creator::approval_preview(&first.arguments)
        } else if is_build {
            format!(
                "{}\n构建入口 {}",
                first.arguments["operation"].as_str().unwrap_or_default(),
                first.arguments["entry"].as_str().unwrap_or_default(),
            )
        } else {
            let preview = serde_json::to_string_pretty(&first.arguments).unwrap_or_default();
            if preview.chars().count() > 260 {
                let mut text: String = preview.chars().take(260).collect();
                text.push('…');
                text
            } else {
                preview
            }
        };
        let call_id_allow = call_id.clone();
        let call_id_deny = call_id.clone();
        let approvals_allow = self.approvals.clone();
        let approvals_deny = self.approvals.clone();
        let can_remember = first.supports_always_allow();
        let saving = self.approval_saving.as_ref() == Some(first);
        let remember_request = first.clone();
        let approval_error = self
            .approval_error
            .as_ref()
            .filter(|(request, _)| request == first)
            .map(|(_, error)| error.clone());
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
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(t.radii.md)
                        .border_1()
                        .border_color(p.color(p.state_warn))
                        .bg(p.color(p.bg_layer_1))
                        .shadow_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded_full()
                                        .bg(p.color(p.state_warn)),
                                )
                                .child(
                                    div()
                                        .text_size(t.type_scale.secondary)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .child(if is_trial {
                                            "允许在当前会话试用插件？".to_owned()
                                        } else if is_build {
                                            "允许构建插件？".to_owned()
                                        } else {
                                            format!("工具调用需要确认：{}", first.tool)
                                        }),
                                )
                                .child(div().flex_1())
                                .when(pending.len() > 1, |row| {
                                    row.child(
                                        div()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.label_caption))
                                            .child(format!("另有 {} 个待确认", pending.len() - 1)),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .max_h(px(if is_trial { 136.0 } else { 80.0 }))
                                .overflow_hidden()
                                .text_size(t.type_scale.small)
                                .line_height(t.type_scale.small_line)
                                .text_color(p.color(p.label_tertiary))
                                .child(preview),
                        )
                        .when_some(approval_error, |card, error| {
                            card.child(div().text_size(t.type_scale.small).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.0))
                                .when(saving, |row| row.opacity(0.5))
                                .child(
                                    div()
                                        .id("approval-deny")
                                        .role(Role::Button)
                                        .focusable()
                                        .aria_label("拒绝这次工具调用")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .border_1()
                                        .border_color(p.color(p.border_l2))
                                        .bg(p.color(p.button_elevated))
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(p.label_primary))
                                        .child("拒绝")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if this.approval_saving.as_ref().is_some_and(
                                                |request| request.call_id == call_id_deny,
                                            ) {
                                                return;
                                            }
                                            approvals_deny
                                                .resolve(&call_id_deny, ApprovalOutcome::Deny);
                                            this.approval_error = None;
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .id("approval-allow")
                                        .role(Role::Button)
                                        .focusable()
                                        .aria_label("允许一次工具调用")
                                        .px(px(12.0))
                                        .py(px(5.0))
                                        .rounded(t.radii.sm)
                                        .bg(p.color(if can_remember {
                                            p.button_elevated
                                        } else {
                                            p.button_info
                                        }))
                                        .when(can_remember, |button| {
                                            button.border_1().border_color(p.color(p.border_l2))
                                        })
                                        .cursor_pointer()
                                        .text_size(t.type_scale.small)
                                        .text_color(p.color(if can_remember {
                                            p.label_primary
                                        } else {
                                            p.bg_base
                                        }))
                                        .child("允许一次")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if this.approval_saving.as_ref().is_some_and(
                                                |request| request.call_id == call_id_allow,
                                            ) {
                                                return;
                                            }
                                            approvals_allow.resolve(
                                                &call_id_allow,
                                                ApprovalOutcome::AllowOnce,
                                            );
                                            this.approval_error = None;
                                            this.pending_approvals = 0;
                                            cx.notify();
                                        })),
                                )
                                .when(can_remember, |actions| {
                                    actions.child(
                                        div()
                                            .id("approval-always")
                                            .role(Role::Button)
                                            .focusable()
                                            .aria_label("始终允许当前会话中的此工具")
                                            .px(px(12.0))
                                            .py(px(5.0))
                                            .rounded(t.radii.sm)
                                            .bg(p.color(p.button_info))
                                            .cursor_pointer()
                                            .text_size(t.type_scale.small)
                                            .text_color(p.color(p.bg_base))
                                            .child(if saving {
                                                "正在保存…"
                                            } else {
                                                "始终允许"
                                            })
                                            .tooltip(toolbar_tooltip(
                                                "仅对当前会话中的此工具生效",
                                                *t,
                                            ))
                                            .on_click(cx.listener(move |chat, _, _, cx| {
                                                chat.allow_tool_always(
                                                    remember_request.clone(),
                                                    cx,
                                                );
                                            })),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }

    fn toggle_composer_menu(&mut self, menu: ComposerMenu, cx: &mut Context<Self>) {
        if !self.busy || menu == ComposerMenu::Permission {
            self.turn_menu_open = false;
            let current = match self.composer_menu {
                Some(ComposerMenu::ModelList | ComposerMenu::Reasoning) => {
                    Some(ComposerMenu::Model)
                }
                current => current,
            };
            self.composer_menu = (current != Some(menu)).then_some(menu);
            cx.notify();
        }
    }

    fn dismiss_composer_menu(
        &mut self,
        _: &gpui::MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_menu = None;
        cx.notify();
    }

    fn choose_model(
        &mut self,
        model: Arc<DesktopModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.apply_model(model) {
            return;
        }
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn apply_model(&mut self, model: Arc<DesktopModel>) -> bool {
        if self.busy || self.model_settings.pending {
            return false;
        }
        let reasoning = self
            .preferences
            .reasoning
            .get(&self.model_settings.reasoning_key(&model))
            .and_then(|value| ReasoningEffort::parse(value))
            .filter(|effort| model.reasoning_levels().contains(effort));
        if !Arc::ptr_eq(&model, &self.settings.model) || reasoning != self.settings.reasoning {
            let Ok(mut state) = self.state.try_lock() else {
                return false;
            };
            state.set_model(model.clone(), reasoning);
            self.settings.model = model;
            self.settings.reasoning = reasoning;
            self.attachment_error = None;
        }
        true
    }

    fn select_reasoning(
        &mut self,
        reasoning: Option<ReasoningEffort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.model_settings.pending {
            return;
        }
        let model = self.settings.model.clone();
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if reasoning != self.settings.reasoning {
            state.set_model(model.clone(), reasoning);
            self.settings.reasoning = reasoning;
        }
        drop(state);
        let key = self.model_settings.reasoning_key(&model);
        if let Some(effort) = reasoning {
            self.preferences
                .reasoning
                .insert(key, effort.as_str().to_owned());
        } else {
            self.preferences.reasoning.remove(&key);
        }
        self.save_preferences();
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn reasoning_label(&self) -> &'static str {
        reasoning_label(
            self.settings
                .reasoning
                .or_else(|| self.settings.model.provider_reasoning()),
        )
    }

    fn change_permission(&mut self, permission: PermissionMode) -> bool {
        if self.permission.mode().is_none() {
            return false;
        }
        self.permission.set(permission);
        true
    }

    fn choose_permission(
        &mut self,
        permission: PermissionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.change_permission(permission) {
            return;
        }
        self.composer_menu = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn composer_options(
        &self,
        menu: ComposerMenu,
        t: &Theme,
        max_width: gpui::Pixels,
        max_height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.composer_menu != Some(menu) {
            return None;
        }
        let p = t.palette;
        let mut options = div()
            .id("composer-options")
            .role(Role::Menu)
            .aria_label(match menu {
                ComposerMenu::Model => "模型和推理等级",
                ComposerMenu::ModelList => "选择模型",
                ComposerMenu::Reasoning => "推理等级",
                ComposerMenu::Permission => "选择工具权限",
                ComposerMenu::Preset => "选择会话模式",
                ComposerMenu::Commands => "选择指令",
                ComposerMenu::Add => "添加附件或调用指令",
            })
            .occlude()
            .w(px(276.0))
            .max_w(max_width)
            .max_h(max_height)
            .overflow_y_scroll()
            .p(px(5.0))
            .rounded(t.radii.md)
            .border_1()
            .border_color(p.color(p.border_l2))
            .bg(p.color(p.bg_layer_1))
            .shadow_sm();
        if let Some(label) = match menu {
            ComposerMenu::ModelList => Some("模型"),
            ComposerMenu::Reasoning => Some("推理等级"),
            _ => None,
        } {
            options = options.child(
                chrome_button("model-menu-back", "", t)
                    .role(Role::MenuItem)
                    .aria_label("返回模型菜单")
                    .focusable()
                    .w_full()
                    .px(px(9.0))
                    .gap(px(8.0))
                    .child(desktop_icon("\u{e76b}", "‹").text_size(px(10.0)))
                    .child(label)
                    .on_click(cx.listener(|chat, _, _, cx| {
                        chat.composer_menu = Some(ComposerMenu::Model);
                        cx.notify();
                    })),
            );
        }
        match menu {
            ComposerMenu::Model if !self.settings.model.reasoning_levels().is_empty() => {
                for (label, value, pane) in [
                    ("模型", self.settings.model.name(), ComposerMenu::ModelList),
                    ("推理等级", self.reasoning_label(), ComposerMenu::Reasoning),
                ] {
                    options = options.child(
                        chrome_button(label, label, t)
                            .role(Role::MenuItem)
                            .focusable()
                            .w_full()
                            .px(px(9.0))
                            .gap(px(8.0))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(value.to_owned()),
                            )
                            .child(desktop_icon("\u{e76c}", "›").text_size(px(10.0)))
                            .on_click(cx.listener(move |chat, _, _, cx| {
                                chat.composer_menu = Some(pane);
                                cx.notify();
                            })),
                    );
                }
            }
            ComposerMenu::Reasoning => {
                for (index, effort) in std::iter::once(None)
                    .chain(
                        self.settings
                            .model
                            .reasoning_levels()
                            .iter()
                            .copied()
                            .map(Some),
                    )
                    .enumerate()
                {
                    options = options.child(
                        chrome_button(("reasoning-level", index), reasoning_label(effort), t)
                            .role(Role::MenuItemRadio)
                            .aria_selected(self.settings.reasoning == effort)
                            .focusable()
                            .w_full()
                            .px(px(9.0))
                            .justify_between()
                            .when(self.settings.reasoning == effort, |row| row.child("✓"))
                            .on_click(cx.listener(move |chat, _, window, cx| {
                                chat.select_reasoning(effort, window, cx);
                            })),
                    );
                }
            }
            ComposerMenu::Model | ComposerMenu::ModelList => {
                for (index, model) in self.model_options.iter().enumerate() {
                    let option = model.clone();
                    let description = match model.kind() {
                        ModelKind::Mock => "在本机生成演示回复".to_owned(),
                        kind => format!("{} · {}", kind.label(), self.model_settings.source(model)),
                    };
                    options = options.child(
                        composer_option(
                            ("model", index),
                            model.name(),
                            &description,
                            Arc::ptr_eq(model, &self.settings.model),
                            t,
                        )
                        .focusable()
                        .when(model.kind() != ModelKind::Mock, |row| {
                            row.aria_label(format!("{}，{description}", model.name()))
                                .tooltip(toolbar_tooltip(description, *t))
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.choose_model(option.clone(), window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Preset => {
                for (index, preset) in self.preset_choices().into_iter().enumerate() {
                    let (label, description) = Self::preset_label(preset.as_deref());
                    options = options.child(
                        composer_option(
                            ("session-preset", index),
                            label,
                            description,
                            self.active_preset == preset,
                            t,
                        )
                        .focusable()
                        .on_click(cx.listener(
                            move |chat, _, window, cx| {
                                chat.choose_session_preset(preset.clone(), window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Permission => {
                for (index, (mode, description)) in EDITABLE_PERMISSIONS.into_iter().enumerate() {
                    options = options.child(
                        composer_option(
                            ("permission", index),
                            permission_label(mode),
                            description,
                            self.permission.mode() == Some(mode),
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.choose_permission(mode, window, cx);
                            },
                        )),
                    );
                }
            }
            ComposerMenu::Commands | ComposerMenu::Add => {
                let typed_slash = menu == ComposerMenu::Commands;
                if !typed_slash && self.attachments.len() < MAX_ATTACHMENTS {
                    options = options.child(
                        composer_option(
                            "add-file",
                            "添加附件",
                            "图片、PDF 或 UTF-8 文本",
                            false,
                            t,
                        )
                        .role(Role::MenuItem)
                        .focusable()
                        .on_click(cx.listener(|chat, _, _, cx| {
                            chat.add_attachments(AttachmentSource::Picker, cx);
                        })),
                    );
                }
                let text = if typed_slash {
                    self.slash_text.as_deref().unwrap_or("/")
                } else {
                    "/"
                };
                for (index, command) in slash_commands(text).enumerate() {
                    options = options.child(
                        composer_option(
                            ("slash-command", index),
                            &format!("/{}", command.name()),
                            command.description(),
                            typed_slash && index == self.selected_command,
                            t,
                        )
                        .focusable()
                        .on_click(cx.listener(
                            move |chat, _, window, cx| {
                                chat.composer_menu = None;
                                if typed_slash {
                                    chat.input.update(cx, |input, cx| input.clear(window, cx));
                                }
                                chat.run_slash_command(command, window, cx);
                            },
                        )),
                    );
                }
            }
        }
        Some(options)
    }

    fn paste_attachments(&mut self, item: &mut ClipboardItem, cx: &mut Context<Self>) -> bool {
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

    fn add_attachments(&mut self, source: AttachmentSource, cx: &mut Context<Self>) {
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

    fn attachment_chips(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
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

    fn composer(
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

    fn empty_composer(
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

/// 会话流里的一行。字段全部是 `String`：快照跨线程从 Agent 线程送到 UI 线程。
///
/// 这是"数据"而不是"样式"：渲染时再按 DSH 的对话样式组合。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    User {
        text: String,
        attachments: Vec<UserAttachment>,
    },
    Assistant(String),
    Streaming(String),
    Process(Vec<Row>),
    Reasoning {
        text: String,
        streaming: bool,
    },
    Tool {
        call_seq: u64,
        name: String,
        origin: String,
        status: String,
        detail: String,
        is_error: bool,
        pending: bool,
        /// 结构化卡片（Diff/Terminal 的原生视图从这里取正文）。
        card: Option<ToolCard>,
        delivered_files: Vec<dsh_deliverables::DeliveredFile>,
    },
    Meta(String),
    Stopped,
}

impl Row {
    fn has_tool_call(&self, seq: u64) -> bool {
        match self {
            Self::Tool { call_seq, .. } => *call_seq == seq,
            Self::Process(steps) => steps.iter().any(|step| step.has_tool_call(seq)),
            _ => false,
        }
    }

    fn main_text(&self) -> String {
        match self {
            Self::User { text, .. } => format!("你 · {text}"),
            Self::Assistant(text) | Self::Streaming(text) => format!("助手 · {text}"),
            Self::Process(_) => "思考过程".to_owned(),
            Self::Reasoning { text, .. } => text.clone(),
            Self::Tool {
                name,
                origin,
                status,
                ..
            } => format!("工具 · {name} [{origin}] · {status}"),
            Self::Meta(text) => text.clone(),
            Self::Stopped => "已停止".to_owned(),
        }
    }

    fn sub_text(&self) -> Option<String> {
        match self {
            Self::Tool { detail, .. } if !detail.is_empty() => Some(detail.clone()),
            _ => None,
        }
    }
}

fn is_streaming_row(row: &Row) -> bool {
    matches!(row, Row::Streaming(_))
}

fn turn_preview(row: &Row) -> Option<&str> {
    let Row::User { text, attachments } = row else {
        return None;
    };
    Some(
        text.lines()
            .find(|line| !line.trim().is_empty())
            .or_else(|| attachments.first().map(|file| file.name.as_str()))
            .unwrap_or_default(),
    )
}

fn turn_rail_window(total: usize, active: usize) -> std::ops::Range<usize> {
    let count = total.min(7);
    let start = active.saturating_sub(count / 2).min(total - count);
    start..start + count
}

/// 估计一行的高度（逻辑像素）。用于虚拟化的滚动占位；估算偏差只影响滚动条比例，
/// 不影响可见行之间的真实排版。按 CJK 宽度保守估计每行字符数。
fn trial_panel_max_height(window: &Window) -> gpui::Pixels {
    (window.viewport_size().height * 0.42).clamp(px(90.0), px(360.0))
}

fn estimated_row_height(row: &Row) -> f32 {
    const LINE: f32 = 23.0;
    match row {
        Row::User { text, attachments } => {
            let chars = text.chars().count() as f32;
            let text_height = if text.is_empty() {
                0.0
            } else {
                (chars / 28.0).ceil() * LINE + 43.0
            };
            let image_height = if attachments.len() == 1 {
                144.0_f32
            } else {
                80.0
            };
            let attachment_height: f32 = attachments
                .chunks(2)
                .map(|row| {
                    if row
                        .iter()
                        .any(|attachment| attachment.file.as_ref().is_some_and(stored_image))
                    {
                        image_height + 6.0
                    } else {
                        48.0
                    }
                })
                .sum();
            text_height + attachment_height
        }
        Row::Assistant(text) | Row::Streaming(text) => {
            let chars = text.chars().count() as f32;
            let actions = if is_streaming_row(row) {
                22.0
            } else if text.is_empty() {
                0.0
            } else {
                28.0
            };
            (chars / 36.0).ceil().max(1.0) * LINE + 12.0 + actions
        }
        Row::Tool { detail, .. } => {
            let chars = detail.chars().count() as f32;
            46.0 + (chars / 60.0).ceil() * 18.0
        }
        Row::Process(_) => 26.0,
        Row::Reasoning { text, .. } => text
            .lines()
            .map(|line| (line.chars().count() as f32 / 42.0).ceil().max(1.0) * LINE)
            .sum(),
        Row::Meta(_) | Row::Stopped => 30.0,
    }
}

fn streaming_text(row: &Row) -> &str {
    match row {
        Row::Streaming(text) => text,
        _ => "",
    }
}

fn describe(item: &Item) -> Row {
    match item {
        Item::User { text, attachments } => Row::User {
            text: text.clone(),
            attachments: attachments.clone(),
        },
        Item::Assistant { text } => Row::Assistant(text.clone()),
        Item::Reasoning(text) => Row::Reasoning {
            text: text.clone(),
            streaming: false,
        },
        Item::Stopped => Row::Stopped,
        Item::Tool(tool) => {
            let (status, pending) = match &tool.state {
                ToolState::Pending => ("运行中".to_owned(), true),
                ToolState::Settled { is_error } => {
                    if *is_error {
                        ("失败".to_owned(), false)
                    } else {
                        ("完成".to_owned(), false)
                    }
                }
                ToolState::Aborted { reason } => (format!("中止 · {reason}"), false),
            };
            Row::Tool {
                call_seq: tool.call_seq,
                name: tool.tool.clone(),
                origin: tool.origin.clone(),
                status,
                detail: tool
                    .card
                    .as_ref()
                    .map(ToolCard::summary)
                    .unwrap_or_default(),
                is_error: matches!(tool.state, ToolState::Settled { is_error: true }),
                pending,
                card: tool.card.clone(),
                delivered_files: Vec::new(),
            }
        }
        Item::Compaction { from_seq, to_seq } => {
            Row::Meta(format!("上下文已压缩 · seq {from_seq}–{to_seq}"))
        }
    }
}

impl ChatApp {
    /// 启动/加载错误横幅：GUI 双击没有控制台，eprintln 用户看不到，必须画在界面上。
    /// 覆盖 sidecar 装载错误、会话日志损坏/隔离、备份恢复失败等。
    fn error_banner(&self, theme: &Theme) -> AnyElement {
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

/// 从当前日志生成整份快照；`streaming` 非空时把"正在生成的回复"追加为最后一行。
///
/// 快照而不是增量：UI 只做覆盖，不用维护事件合并逻辑，也不会因为丢一条消息就错位。
fn rows_snapshot(log: &SessionLog, streaming: Option<&str>) -> Vec<Row> {
    rows_with_preview(log, streaming, None)
}

fn rows_with_preview(
    log: &SessionLog,
    streaming: Option<&str>,
    reasoning: Option<&str>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut steps = Vec::new();
    let mut deliveries = dsh_deliverables::replay(log);
    for item in build_from_log(log) {
        let mut row = describe(&item);
        if let Item::Tool(tool) = &item {
            if tool.origin == "sidecar"
                && tool.tool == "present"
                && matches!(tool.state, ToolState::Settled { is_error: false })
            {
                if let Row::Tool {
                    delivered_files, ..
                } = &mut row
                {
                    *delivered_files = deliveries.remove(&tool.call_seq).unwrap_or_default();
                }
            }
        }
        if matches!(row, Row::Tool { .. } | Row::Reasoning { .. }) {
            steps.push(row);
        } else {
            if !steps.is_empty() {
                rows.push(Row::Process(std::mem::take(&mut steps)));
            }
            rows.push(row);
        }
    }
    if let Some(text) = reasoning.filter(|text| !text.is_empty()) {
        steps.push(Row::Reasoning {
            text: text.to_owned(),
            streaming: true,
        });
    }
    if !steps.is_empty() {
        rows.push(Row::Process(steps));
    }
    if let Some(text) = streaming.filter(|text| !text.is_empty()) {
        rows.push(Row::Streaming(text.to_owned()));
    }
    rows
}

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

/// 离线安装插件：`dshr --install-plugin <本地包目录>`。
///
/// 复制到 `sidecar/dsh-official/node_modules/<name>` 并追加 profile 条目；
/// 不解析 npm 依赖（包需要在本地已完整）。
/// 会话树前序顺序 + 深度（父在前、子紧随其后），侧栏与自检共用。
fn session_tree_order(store: &SessionIndex) -> Vec<(String, u32)> {
    match store.tree() {
        Ok(tree) => tree
            .flatten()
            .into_iter()
            .map(|(depth, node)| (node.id.clone(), depth))
            .collect(),
        Err(err) => {
            eprintln!("[chat] 会话树损坏（回退到扁平列表）: {err}");
            Vec::new()
        }
    }
}

/// 按会话树前序排好的会话列表 + 深度表（侧栏与启动初始化共用）。
fn ordered_sessions(
    store: &SessionIndex,
) -> (Vec<SessionMeta>, std::collections::BTreeMap<String, u32>) {
    let order = session_tree_order(store);
    let metas: std::collections::BTreeMap<String, SessionMeta> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .map(|meta| (meta.id.clone(), meta))
        .collect();
    if order.is_empty() {
        return (
            metas.into_values().collect(),
            std::collections::BTreeMap::new(),
        );
    }
    let mut ordered = Vec::new();
    let mut depths = std::collections::BTreeMap::new();
    for (id, depth) in &order {
        depths.insert(id.clone(), *depth);
        if let Some(meta) = metas.get(id) {
            ordered.push(meta.clone());
        }
    }
    // 树里没有的（索引竞争等极端情况）按 id 补在末尾，保证不丢会话。
    for (id, meta) in &metas {
        if !depths.contains_key(id) {
            ordered.push(meta.clone());
        }
    }
    (ordered, depths)
}

fn sessions_dir_from_env() -> PathBuf {
    std::env::var("DSHR_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/sessions"))
}

/// `dshr --export-session <id> [--export-dir <dir>]`：
/// 导出 JSONL + 外部化附件 + manifest（sha256），并立刻自校验。
fn export_session_cli(id: &str, out: Option<PathBuf>) -> i32 {
    let store = SessionIndex::new(sessions_dir_from_env());
    let path = store.path_for(id);
    if !path.exists() {
        eprintln!("[export] 会话不存在: {}", path.display());
        return 1;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("[export] 读取会话失败: {err}");
            return 1;
        }
    };
    let log = match SessionLog::from_jsonl(id, &text) {
        Ok(log) => log,
        Err(err) => {
            eprintln!("[export] 会话日志损坏: {err}");
            return 1;
        }
    };
    let spill = SpillStore::new(spill_dir_for(&path));
    let out_dir = out.unwrap_or_else(|| PathBuf::from(".tmp/exports").join(id));
    let manifest = match export_session(
        &log,
        &SpillStoreSource::new(&spill),
        &out_dir,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            eprintln!("[export] 导出失败: {err}");
            return 1;
        }
    };
    println!(
        "[export] session={} events={} attachments={} missing={} bytes={} out={}",
        manifest.session_id,
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        out_dir.display()
    );
    match verify_export(&out_dir, id) {
        Ok(report) if report.is_ok() => {
            println!(
                "[export] verify=ok jsonl_ok={} attachments_checked={}",
                report.jsonl_ok, report.attachments_checked
            );
            0
        }
        Ok(report) => {
            eprintln!("[export] 自校验失败: {:?}", report.problems);
            1
        }
        Err(err) => {
            eprintln!("[export] 自校验失败: {err}");
            1
        }
    }
}

/// `dshr --retention-plan`：只计算、不删除（每个会话一个 JSONL 段）。
fn retention_plan_cli() -> i32 {
    use dshr_session_export::{plan_retention, RetentionPolicy, SegmentUsage, SessionUsage};
    let store = SessionIndex::new(sessions_dir_from_env());
    let metas = match store.scan() {
        Ok(metas) => metas,
        Err(err) => {
            eprintln!("[retention] 扫描会话失败: {err}");
            return 1;
        }
    };
    let usages: Vec<SessionUsage> = metas
        .iter()
        .map(|meta| {
            let size = std::fs::metadata(&meta.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            SessionUsage::new(
                meta.id.clone(),
                vec![SegmentUsage {
                    segment_id: String::from("main"),
                    relative_path: meta
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size_bytes: size,
                    last_modified_secs: meta.updated_at,
                }],
                0,
            )
        })
        .collect();
    let policy = RetentionPolicy::new(2 * 1024 * 1024 * 1024, 90, 1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let plan = plan_retention(&usages, &policy, now);
    println!(
        "[retention] sessions={} bytes_before={} bytes_after={} deletions={} over_cap={} reclaimed={}",
        usages.len(),
        plan.bytes_before,
        plan.bytes_after,
        plan.deletions.len(),
        plan.over_cap.len(),
        plan.bytes_reclaimed()
    );
    for deletion in &plan.deletions {
        println!(
            "[retention] delete session={} segment={} size={} reason={:?}",
            deletion.session_id, deletion.segment_id, deletion.size_bytes, deletion.reason
        );
    }
    // 只出计划，不调用 apply_retention：删除必须由用户显式触发。
    println!("[retention] plan-only（未删除任何文件）");
    0
}

struct InstallCliOptions {
    package: PathBuf,
    /// 非交互确认（CI / 脚本）；未验证签名的包必须显式给出。
    yes_install: bool,
    allow_downgrade: bool,
}

fn install_plugin_cli(package: PathBuf) -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    install_plugin_with_options(
        &InstallCliOptions {
            package,
            yes_install: args.iter().any(|arg| arg == "--yes-install"),
            allow_downgrade: args.iter().any(|arg| arg == "--allow-downgrade"),
        },
        &sidecar_dir,
        &profile_dir,
    )
}

/// 可信安装 CLI（M3 G2）：摘要/签名校验 → 首装确认（`--yes-install`）→ 审计。
fn install_plugin_with_options(
    options: &InstallCliOptions,
    sidecar_dir: &std::path::Path,
    profile_dir: &std::path::Path,
) -> i32 {
    if let Err(err) = std::fs::create_dir_all(sidecar_dir) {
        eprintln!("[install] 创建 sidecar 目录失败: {err}");
        return 1;
    }
    let mut profile = match Profile::load(profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let store = match TrustStore::load(profile_dir) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("[install] 信任密钥库无效（{profile_dir:?}/trust-keys.json）: {err}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: options.allow_downgrade,
        require_confirm_for_unsigned: true,
        store,
        now_ms: None,
    };
    let package = options.package.clone();
    let yes_install = options.yes_install;
    let result = install_package_trusted(sidecar_dir, &package, &mut profile, &policy, |preview| {
        // 非交互 CLI：必须显式 --yes-install；打印与 spec §5 一致的预览项。
        println!(
            "[install] 待安装: {} {}（{}）",
            preview.name,
            preview.version,
            preview.signature_status.as_str()
        );
        println!(
            "[install] 来源: {}；摘要: {}",
            package.display(),
            preview.digest_sha256
        );
        if let Some(key_id) = &preview.key_id {
            println!("[install] 签名 key: {key_id}");
        }
        if let Some(installed) = &preview.installed_version {
            println!("[install] 已安装版本: {installed}");
        }
        println!("[install] 会获得的权限: {}", preview.permissions_hint);
        println!("[install] 警告: 该插件可在你的用户权限下执行任意代码");
        if !yes_install {
            eprintln!("[install] 未验证签名的包需要显式确认：加 --yes-install 重跑（本次未安装）");
        }
        yes_install
    });
    match result {
        Ok(report) => {
            println!(
                "[install] {} -> {}（profile_added={}，signature={}）",
                report.install.name,
                report.install.destination.display(),
                report.install.added_to_profile,
                report.trust.signature_status.as_str()
            );
            println!(
                "[install] profile={}；重启 DSHR 后加载",
                profile.path().display()
            );
            println!(
                "[install] 审计记录: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            if matches!(err, dshr_profile::TrustError::NotConfirmed) {
                eprintln!("[install] 已取消：未确认安装");
                return 2;
            }
            eprintln!("[install] 安装失败（{}）: {err}", err.code());
            1
        }
    }
}

/// 侧车/插件安装目录（与 `--install-plugin`、sidecar 启动同源）。
fn sidecar_dir_for_app() -> PathBuf {
    std::env::var("DSHR_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_root().join("sidecar/dsh-official"))
}

/// profile 目录（与 `--install-plugin` 同源）。
fn profile_dir_for_app() -> PathBuf {
    std::env::var("DSHR_PROFILE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".tmp/profile"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// `dshr --uninstall-plugin <name>`：目录 + profile + 审计（M4 W1）。
fn uninstall_plugin_cli(name: String) -> i32 {
    let sidecar_dir = sidecar_dir_for_app();
    let profile_dir = profile_dir_for_app();
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[uninstall] 加载 profile 失败: {err}");
            return 1;
        }
    };
    match uninstall_package(&sidecar_dir, &name, &mut profile) {
        Ok(report) => {
            println!(
                "[uninstall] {} version={} dir_removed={} profile_removed={}",
                report.name,
                report.version.as_deref().unwrap_or("-"),
                report.removed_dir,
                report.removed_profile_entry
            );
            if let Some(error) = report.audit_error {
                eprintln!("[uninstall] 警告：审计写入失败（卸载已生效）: {error}");
            }
            println!(
                "[uninstall] 审计: {}",
                profile_dir.join("install-audit.jsonl").display()
            );
            0
        }
        Err(err) => {
            eprintln!("[uninstall] 卸载失败: {err}");
            1
        }
    }
}

fn trust_key_status(key: &TrustKey, at_ms: u64) -> &'static str {
    if key.revoked {
        "revoked"
    } else if key.not_before_ms.is_some_and(|before| at_ms < before) {
        "not-yet-valid"
    } else if key.not_after_ms.is_some_and(|after| at_ms > after) {
        "expired"
    } else {
        "active"
    }
}

/// `dshr --trust-key <list|add|remove|revoke|rotate> ...`：本机信任密钥库管理。
fn trust_key_cli(args: &[String]) -> i32 {
    let profile_dir = profile_dir_for_app();
    if let Err(err) = std::fs::create_dir_all(&profile_dir) {
        eprintln!("[trust-key] 创建 profile 目录失败: {err}");
        return 1;
    }
    let action = args.first().map(String::as_str).unwrap_or("list");
    let at_ms = now_millis();
    match action {
        "list" => {
            let store = match TrustStore::load(&profile_dir) {
                Ok(store) => store,
                Err(err) => {
                    eprintln!("[trust-key] 密钥库无效（{}）: {err}", err.code());
                    return 1;
                }
            };
            if store.keys().is_empty() {
                println!("[trust-key] 密钥库为空（{}）", profile_dir.display());
            }
            for key in store.keys() {
                println!(
                    "[trust-key] key_id={} status={} not_before={} not_after={} note={}",
                    key.key_id,
                    trust_key_status(key, at_ms),
                    key.not_before_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.not_after_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| String::from("-")),
                    key.note.as_deref().unwrap_or("-")
                );
            }
            0
        }
        "add" => {
            let (Some(key_id), Some(public_key_hex)) = (args.get(1), args.get(2)) else {
                eprintln!("用法: dshr --trust-key add <key_id> <public_key_hex> [--note <text>]");
                return 2;
            };
            let note = args
                .iter()
                .position(|arg| arg == "--note")
                .and_then(|index| args.get(index + 1))
                .cloned();
            let mut key = TrustKey::new(key_id, public_key_hex);
            key.note = note;
            match add_trust_key(&profile_dir, key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] add {}（keys={}）",
                        report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 添加失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "remove" | "revoke" => {
            let Some(key_id) = args.get(1) else {
                eprintln!("用法: dshr --trust-key {action} <key_id>");
                return 2;
            };
            let result = if action == "remove" {
                remove_trust_key(&profile_dir, key_id, at_ms)
            } else {
                revoke_trust_key(&profile_dir, key_id, at_ms)
            };
            match result {
                Ok(report) => {
                    println!(
                        "[trust-key] {} {}（keys={}）",
                        report.action, report.key_id, report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] {action} 失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        "rotate" => {
            let (Some(old_key_id), Some(new_key_id), Some(public_key_hex)) =
                (args.get(1), args.get(2), args.get(3))
            else {
                eprintln!(
                    "用法: dshr --trust-key rotate <old_key_id> <new_key_id> <public_key_hex>"
                );
                return 2;
            };
            let new_key = TrustKey::new(new_key_id, public_key_hex);
            match rotate_trust_key(&profile_dir, old_key_id, new_key, at_ms) {
                Ok(report) => {
                    println!(
                        "[trust-key] rotate {} -> {}（keys={}）",
                        report.old_key_id.as_deref().unwrap_or("-"),
                        report.key_id,
                        report.key_count
                    );
                    0
                }
                Err(err) => {
                    eprintln!("[trust-key] 轮换失败（{}）: {err}", err.code());
                    1
                }
            }
        }
        other => {
            eprintln!(
                "未知 --trust-key 动作 `{other}`；可用: list / add / remove / revoke / rotate"
            );
            2
        }
    }
}

fn ed25519_keypair() -> Option<ring::signature::Ed25519KeyPair> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).ok()?;
    ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).ok()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 用给定 ed25519 钥重签包（自检专用；作者侧签名由 `tools/plugin-pack.mjs` 完成）。
fn sign_package_with_key(
    package_dir: &std::path::Path,
    name: &str,
    version: &str,
    keypair: &ring::signature::Ed25519KeyPair,
    key_id: &str,
) -> Result<String, String> {
    let digest = package_digest(package_dir).map_err(|err| err.to_string())?;
    let message = format!("dshr-trust/v1\n{name}\n{version}\n{digest}");
    let signature = keypair.sign(message.as_bytes());
    let trust = json!({
        "schema_version": 1,
        "plugin_id": name,
        "version": version,
        "package": { "digest_sha256": digest },
        "signature": {
            "scheme": "ed25519",
            "key_id": key_id,
            "value_hex": hex_lower(signature.as_ref())
        }
    });
    std::fs::write(
        package_dir.join("dshr-trust.json"),
        trust.to_string() + "\n",
    )
    .map_err(|err| format!("写 dshr-trust.json 失败: {err}"))?;
    Ok(digest)
}

/// 宿主兼容基线：用 `dshr-plugin-api` 的 semver（0.1.0），不是应用 workspace 的 0.0.0。
fn host_sdk_version() -> HostVersion {
    HostVersion::parse(dshr_plugin_api::HOST_VERSION).unwrap_or_else(|_| HostVersion::new(0, 1, 0))
}

/// 从 profile 的 enabled 条目装载已安装插件（spec/cpp-v1.md §3.3）：
/// - `external-process` → [`ExternalPluginSpec`] + [`ExternalPlugin::spawn`]；
/// - `wasm-component` → [`PluginHost::load_wasm_plugin`]（P1-1：不再静默跳过）；
/// - 其它 runtime kind（如 `rust-static`）装错位置 → 记为 plugin error（不静默）。
///
/// 单个插件失败只记诊断，不影响其它插件与内置工具。
fn load_enabled_external_plugins(
    plugins: &mut PluginHost,
    profile: &Profile,
    sidecar_dir: &std::path::Path,
) -> Vec<String> {
    let mut errors = Vec::new();
    let node_modules = sidecar_dir.join("node_modules");
    for entry in profile.entries().iter().filter(|entry| entry.enabled) {
        let package_dir = node_modules.join(&entry.name);
        let manifest_path = package_dir.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = match Manifest::from_path(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                errors.push(format!("插件 {} 的 manifest 解析失败: {err}", entry.name));
                continue;
            }
        };
        if let Err(error) = load_cpp_package(plugins, manifest, &package_dir) {
            errors.push(error);
        }
    }
    errors
}

fn load_cpp_package(
    plugins: &mut PluginHost,
    manifest: Manifest,
    package_dir: &std::path::Path,
) -> Result<(), String> {
    let plugin_id = manifest.plugin.id.clone();
    let host = host_sdk_version();
    match manifest.runtime.kind {
        RuntimeKind::ExternalProcess => {
            let spec = ExternalPluginSpec::new(manifest, package_dir, host, 1);
            let plugin = ExternalPlugin::spawn(spec)
                .map_err(|error| format!("插件 {plugin_id} 启动失败: {error}"))?;
            plugins
                .load(Box::new(plugin))
                .map_err(|error| format!("插件 {plugin_id} 装载失败: {error}"))?;
            eprintln!("[plugin] external-process 已装载: {plugin_id}");
            Ok(())
        }
        RuntimeKind::WasmComponent => {
            let entry = manifest.runtime.entry.as_deref().unwrap_or("plugin.wasm");
            let is_component = std::fs::read(package_dir.join(entry))
                .map(|bytes| bytes.get(4..8) == Some(&[0x0d, 0x00, 0x01, 0x00]))
                .unwrap_or(false);
            let loaded = if is_component {
                plugins.load_cpp_component_plugin_from_manifest(&manifest, package_dir, &host)
            } else {
                plugins.load_wasm_plugin(&manifest, package_dir, &host)
            };
            loaded.map_err(|error| format!("插件 {plugin_id} WASM 装载失败: {error}"))?;
            eprintln!("[plugin] wasm-component 已装载: {plugin_id}");
            Ok(())
        }
        other => Err(format!(
            "插件 {plugin_id} 的 runtime.kind={other:?} 不支持从安装包装载（rust-static 需内置）"
        )),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 1. 先把命令行用法的输出来路接好（release GUI 子系统没有控制台）。
    prepare_cli_console(is_cli_invocation(&args));
    if headless::selected(&args[1..]) {
        std::process::exit(headless::main(&args[1..]));
    }
    // 2. 数据根必须在崩溃 hook 之前解析：hook 通过 DSHR_CRASH_LOG 写日志。
    prepare_data_env();
    install_crash_hook();
    cli::dispatch(&args);

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
            load_enabled_external_plugins(&mut guard.plugins, &profile, &sidecar_dir_for_app());
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

#[cfg(test)]
#[path = "chat/tests/mod.rs"]
mod tests;
