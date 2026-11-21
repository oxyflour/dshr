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
use std::collections::{HashMap, VecDeque};
use std::io::Read;
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
    estimate_tokens, history_for_model, history_from_log, plan_context, plan_context_with_spill,
    user_message_from_payload, ContextPlan, Retention, SpillPolicy, SpillRef, SpillStore,
};
use dshr_agent_core::{
    spawn_child_spec, Agent, AgentEvent, AttachmentFormat, ChatMessage, CompletionRequest,
    MockProvider, ModelProvider, ModelReply, ModelToolCall, ReasoningEffort, SessionTreeRuntime,
    SpawnChildArgs, SpawnContext, ToolCallRequest, ToolExecutor, ToolOutcome, ToolSpec,
    DEFAULT_MAX_DEPTH, SPAWN_CHILD_TOOL,
};
use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_host::{
    parse_tool_registration, Envelope, EventPage, HostOptions, HostServiceRouter, ServiceError,
    SessionAccess, SessionEventView, SessionHeaderView, SessionLogService, SidecarHost,
    StorageKvService, WorkingDirectoryService, CAP_SERVICE_PROXY, PROTOCOL_MAX, SESSION_LOG_V1,
    STORAGE_KV_V1, WORKING_DIRECTORY_V1,
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
use dshr_tool_runtime::{
    AllowAll, ApprovalBroker, ApprovalOutcome, ApprovalPolicy, LiveApproval, PendingApproval,
    PermissionMode, PolicyDecision, RegistryError, ToolDefinition, ToolOrigin, ToolPolicy,
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
use attachment_preview::{
    decode_image, AttachmentThumbnail, PreviewSource, StoredAttachment, THUMBNAIL_EDGE,
};

#[cfg(target_os = "windows")]
#[path = "chat/window_bar.rs"]
mod window_bar;

#[path = "chat/composer_flow.rs"]
mod composer_flow;

#[path = "chat/composer_surface.rs"]
mod composer_surface;
#[path = "chat/model_settings.rs"]
mod model_settings;
#[path = "chat/creator.rs"]
mod creator;
#[path = "chat/runtime_components.rs"]
mod runtime_components;
#[path = "chat/preset_reload.rs"]
mod preset_reload;
#[path = "chat/headless.rs"]
mod headless;
#[path = "chat/plugins_view.rs"]
mod plugins_view;
#[path = "chat/popover.rs"]
mod popover;
#[path = "chat/settings_view.rs"]
mod settings_view;
#[path = "chat/tool_approvals.rs"]
mod tool_approvals;
#[path = "chat/workspace_view.rs"]
mod workspace_view;

struct DraftAttachment {
    name: String,
    content: AttachmentContent,
    thumbnail: Option<Arc<RenderImage>>,
}

impl DraftAttachment {
    fn new(name: String, content: AttachmentContent) -> Self {
        let thumbnail = match &content {
            AttachmentContent::Media { format, bytes } => {
                decode_image(bytes, *format, THUMBNAIL_EDGE).ok()
            }
            AttachmentContent::Text(_) => None,
        };
        Self {
            name,
            content,
            thumbnail,
        }
    }
}

enum AttachmentContent {
    Text(String),
    Media {
        format: AttachmentFormat,
        bytes: Arc<[u8]>,
    },
}

enum AttachmentSource {
    Picker,
    Paths(Vec<PathBuf>),
    ClipboardImage(Vec<u8>),
}

fn attachment_format(bytes: &[u8]) -> Option<AttachmentFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(AttachmentFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(AttachmentFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(AttachmentFormat::Gif)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(AttachmentFormat::Webp)
    } else if bytes.starts_with(b"%PDF-") {
        Some(AttachmentFormat::Pdf)
    } else {
        None
    }
}

fn read_attachments(
    paths: Vec<PathBuf>,
    remaining_files: usize,
    mut remaining_text: usize,
    mut remaining_media: usize,
) -> Result<Vec<DraftAttachment>, String> {
    if paths.len() > remaining_files {
        return Err(format!("每条消息最多添加 {MAX_ATTACHMENTS} 个附件。"));
    }
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let read_error = |error: std::io::Error| format!("无法读取 {name}：{error}");
            if !path.metadata().map_err(read_error)?.is_file() {
                return Err(format!("{name} 不是普通文件。"));
            }
            let file = std::fs::File::open(&path).map_err(read_error)?;
            let max_bytes = remaining_text.max(remaining_media);
            let mut bytes = Vec::new();
            file.take(max_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(read_error)?;
            if bytes.len() > max_bytes {
                return Err(format!("{name} 超出剩余附件大小。"));
            }
            let content = if let Some(format) = attachment_format(&bytes) {
                if bytes.len() > remaining_media {
                    return Err("图片和 PDF 附件总大小不能超过 12 MiB。".to_owned());
                }
                remaining_media -= bytes.len();
                AttachmentContent::Media {
                    format,
                    bytes: bytes.into(),
                }
            } else {
                if bytes.len() > remaining_text {
                    return Err("文本附件总大小不能超过 32 KiB。".to_owned());
                }
                remaining_text -= bytes.len();
                let text = String::from_utf8(bytes)
                    .ok()
                    .filter(|text| !text.contains('\0'))
                    .ok_or_else(|| format!("{name} 不是受支持的图片、PDF 或 UTF-8 文本文件。"))?;
                AttachmentContent::Text(text)
            };
            Ok(DraftAttachment::new(name, content))
        })
        .collect()
}

fn read_clipboard_image(mut bytes: Vec<u8>, remaining: usize) -> Result<DraftAttachment, String> {
    if bytes.starts_with(b"BM") {
        use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
        use std::io::Cursor;

        let decode_error = |error: image::ImageError| format!("无法读取剪贴板图片：{error}");
        let decoder = ImageReader::with_format(Cursor::new(&bytes), ImageFormat::Bmp)
            .into_decoder()
            .map_err(decode_error)?;
        let (width, height) = decoder.dimensions();
        if u64::from(width) * u64::from(height) > 16 * 1024 * 1024 {
            return Err("剪贴板图片像素过多，请缩小图片。".to_owned());
        }
        let bitmap = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
        let mut output = Cursor::new(Vec::new());
        bitmap
            .write_to(&mut output, ImageFormat::Png)
            .map_err(decode_error)?;
        bytes = output.into_inner();
    }
    if bytes.len() > remaining {
        return Err("图片和 PDF 附件总大小不能超过 12 MiB。".to_owned());
    }
    let (format, extension) = match attachment_format(&bytes) {
        Some(AttachmentFormat::Png) => (AttachmentFormat::Png, "png"),
        Some(AttachmentFormat::Jpeg) => (AttachmentFormat::Jpeg, "jpg"),
        Some(AttachmentFormat::Gif) => (AttachmentFormat::Gif, "gif"),
        Some(AttachmentFormat::Webp) => (AttachmentFormat::Webp, "webp"),
        _ => return Err("剪贴板图片格式不受支持。".to_owned()),
    };
    Ok(DraftAttachment::new(
        format!("剪贴板图片.{extension}"),
        AttachmentContent::Media {
            format,
            bytes: bytes.into(),
        },
    ))
}

fn user_message_payload(
    id: &str,
    text: &str,
    attachments: &[DraftAttachment],
    store: &SpillStore,
) -> Result<(serde_json::Value, Vec<SpillRef>), String> {
    if attachments.is_empty() {
        return Ok((json!({ "message_id": id, "text": text }), Vec::new()));
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    let mut objects = Vec::new();
    for attachment in attachments {
        match &attachment.content {
            AttachmentContent::Text(text) => content.push(json!({
                "type": "text",
                "attachment_name": attachment.name,
                "text": format!("附件 {}：\n{text}", json!(&attachment.name)),
            })),
            AttachmentContent::Media { format, bytes } => {
                let blob = SpillRef::from_payload(bytes, format.mime(), Retention::Session);
                store
                    .put(&blob, bytes)
                    .map_err(|err| format!("无法保存附件 {}：{err}", attachment.name))?;
                content.push(json!({
                    "type": "attachment",
                    "attachment_name": attachment.name,
                    "mime": blob.mime,
                    "$attachment": blob.id,
                }));
                objects.push(blob);
            }
        }
    }
    Ok((json!({ "message_id": id, "content": content }), objects))
}

// M4 W2：键盘优先的操作。键位在 `chat_key_bindings()` 注册，语义在 ChatApp 的
// `shortcut_*` 方法里；不使用鼠标也能覆盖"新建→输入→发送→审批→面板→关闭"。
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

fn workspace_sessions<'a>(sessions: &'a [SessionMeta], query: &str) -> Vec<WorkspaceSessions<'a>> {
    let mut groups = Vec::new();
    let mut indices = HashMap::new();
    for (index, session) in sessions.iter().enumerate() {
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
    set_env_default("DSHR_SESSIONS_DIR", root.join("sessions"));
    set_env_default("DSHR_PROFILE_DIR", root.join("profile"));
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

/// 应用资源根：`DSHR_APP_ROOT` → exe 旁含 `sidecar/` 的目录 → 编译期仓库根（开发机）。
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
    if let Ok(dir) = std::env::var("DSHR_SPILL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    session_path
        .parent()
        .map(|parent| parent.join("spill"))
        .unwrap_or_else(|| PathBuf::from(".tmp/spill"))
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

/// 把被丢弃的历史压缩成一段摘要；provider 失败时退回"截取开头"，不让压缩阻塞对话。
fn summarize_dropped(provider: &mut dyn ModelProvider, dropped: &[ChatMessage]) -> String {
    let transcript = transcript_text(dropped, 8_000);
    if transcript.trim().is_empty() {
        return String::from("（没有可压缩的历史）");
    }
    let request = CompletionRequest {
        messages: vec![
            ChatMessage::System(SUMMARY_PROMPT.to_owned()),
            ChatMessage::user(transcript.clone()),
        ],
        tool_specs: Vec::new(),
    };
    let reply = provider
        .complete_streaming(&request, &mut |_| {})
        .map(|reply| reply.text)
        .unwrap_or_default();
    let summary = reply.trim();
    if summary.is_empty() {
        let head: String = transcript.chars().take(600).collect();
        format!("（摘要生成失败，保留历史开头）\n{head}")
    } else {
        summary.chars().take(1_200).collect()
    }
}

/// 把消息拼成摘要输入；总长受 `max_chars` 限制。
fn transcript_text(messages: &[ChatMessage], max_chars: usize) -> String {
    let mut out = String::new();
    for message in messages {
        let (role, text) = match message {
            ChatMessage::System(text) => ("system", text.clone()),
            ChatMessage::User(text) => ("user", text.clone()),
            ChatMessage::UserWithAttachments { text, attachments } => {
                let names = attachments
                    .iter()
                    .map(|file| file.name.as_str())
                    .collect::<Vec<_>>()
                    .join("、");
                ("user", format!("{text}\n附件：{names}"))
            }
            ChatMessage::Assistant {
                text, tool_calls, ..
            } => {
                let mut body = text.clone();
                if !tool_calls.is_empty() {
                    let names: Vec<&str> =
                        tool_calls.iter().map(|call| call.name.as_str()).collect();
                    body.push_str(&format!("（调用工具 {}）", names.join(", ")));
                }
                ("assistant", body)
            }
            ChatMessage::Tool { content, .. } => ("tool", content.clone()),
        };
        out.push_str(&format!("[{role}] {text}\n"));
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect()
}

/// 一个已取到的插件 ADC 面板（宿主侧缓存；渲染只读，避免每帧走插件 IPC）。
#[derive(Clone)]
struct AdcPanelView {
    plugin_id: String,
    title: String,
    trial: bool,
    /// 面板声明的 view 名（`ui.action` 回传时用于定位插件状态）。
    view: String,
    /// `render_panel` 成功时的元素树；失败为 `None`，错误在 `error`。
    tree: Option<serde_json::Value>,
    /// 渲染失败的可读错误（未知 view / 插件不实现 view / 进程错误）。
    error: Option<String>,
}

type ParsedAgentPresets = Result<dshr_profile::loader::AgentPresets, String>;

struct ChatState {
    log: SessionLog,
    path: PathBuf,
    workspace: PathBuf,
    host: Option<SidecarHost>,
    workspace_sidecars: HashMap<PathBuf, SidecarHost>,
    preset_diagnostics: Vec<String>,
    active_preset_error: Option<String>,
    default_preset: Option<String>,
    inherited_preset_from: Option<String>,
    preset_inputs: Option<(PathBuf, Vec<PathBuf>)>,
    published_presets: Option<ParsedAgentPresets>,
    sidecar_start_error: Option<String>,
    plugins: PluginHost,
    /// Agent 在窗口内复用，会话上下文由当前日志重建。
    agent: Agent<Box<dyn ModelProvider>>,
    settings: DesktopSettings,
    permission: LivePermission,
    seq: u64,
    /// 审批 broker：工作线程阻塞等待 UI 决定；不属于大锁保护范围。
    approvals: Arc<ApprovalBroker>,
    /// 上下文外部化存储：超阈值工具结果落盘为内容寻址对象（内容只在磁盘，模型见引用）。
    spill: SpillStore,
    /// 当前会话 id（会话树事件/子会话父链接使用）。
    session_id: String,
    /// 会话目录（子会话文件与导出都基于它）。
    sessions_dir: PathBuf,
    /// L2 Host Service 的会话读写面（None = 该会话不接 sidecar 服务）。
    l2_access: Option<Arc<ChatSessionAccess>>,
    /// 与 provider 共享的停止句柄（子会话继承，用户停止会传播）。
    cancel: Arc<AtomicBool>,
    /// 是否允许派生子会话（子会话本身为 false，保证默认单层）。
    allow_spawn: bool,
    external_plugin_errors: Vec<String>,
    session_errors: Vec<String>,
    /// 原会话文件存在但读不出来时置位：禁止 save 覆盖（否则会把读失败当成空会话写回）。
    save_blocked: Option<String>,
    /// 插件 ADC 面板缓存（`workspace.panel` 的 view → 元素树 JSON）。
    adc_panels: Vec<AdcPanelView>,
    headless: bool,
    diagnostics: bool,
    creator_trials: HashMap<String, creator::TrialScope>,
}

enum TurnUpdate<'a> {
    Preview(&'a SessionLog, Option<&'a str>, Option<&'a str>),
    Call { seq: u64, tool: &'a str },
    Result { seq: u64, status: &'static str },
}

enum DesktopTurnUpdate {
    Rows(Vec<Row>),
    Finished {
        plugin_errors: Vec<String>,
        adc_panels: Vec<AdcPanelView>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnReason {
    Completed,
    Error,
    Cancelled,
}

impl TurnReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

struct TurnResult {
    reply: String,
    reason: TurnReason,
}

enum ChatRuntime {
    Desktop {
        allow_spawn: bool,
        sidecar: bool,
        diagnostics: bool,
    },
    Headless,
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
            .map(|workspace| Self { workspace, tool, id })
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

fn read_agent_presets_from(
    inputs: Option<&(PathBuf, Vec<PathBuf>)>,
) -> ParsedAgentPresets {
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
            match build_l2_router_at(
                access,
                data_root().join("storage"),
                workspace.to_path_buf(),
                "dsh-official",
            ) {
                Ok(router) => options = options.with_host_services(router),
                Err(err) => eprintln!("[chat] L2 host-service 不可用（回退 v1）: {err}"),
            }
        }
    }
    options = options.with_guard(GuardLimits::sidecar_default());
    SidecarHost::spawn_with(script, plugin, options)
        .map_err(|error| format!("sidecar 启动失败（{mode}）：{error}"))
}

/// 构造 v2 Host Service Router，并在 runtime.ready 协商后更新 generation。
fn build_l2_router_at(
    access: Arc<ChatSessionAccess>,
    storage_root: PathBuf,
    workspace: PathBuf,
    plugin_id: &str,
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
            workspace,
        )))
        .map_err(|error| {
            format!(
                "register working-directory: {} ({})",
                error.message, error.code
            )
        })?;
    router
        .register(Box::new(SessionLogService::new(access)))
        .map_err(|error| format!("register session-log: {} ({})", error.message, error.code))?;
    router.register_plugin_inject(
        plugin_id,
        vec![
            STORAGE_KV_V1.to_owned(),
            WORKING_DIRECTORY_V1.to_owned(),
            SESSION_LOG_V1.to_owned(),
        ],
    );
    Ok(Arc::new(Mutex::new(router)))
}

/// 给没有取消概念的 provider（mock）补上取消 token。
///
/// 无 key/自检场景仍要验证“停止”：Agent 通过 `provider.cancel_token()` 检查取消，
/// MockProvider 自己不带 token，所以在这里包一层（真实 provider 使用各自的 token）。
struct CancelAwareProvider<P> {
    inner: P,
    cancel: Arc<AtomicBool>,
}

impl<P: ModelProvider> ModelProvider for CancelAwareProvider<P> {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        self.inner.complete(request)
    }

    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        Some(self.cancel.clone())
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        self.inner.complete_streaming(request, on_delta)
    }
}

fn cancel_aware(
    provider: impl ModelProvider + 'static,
    cancel: &Arc<AtomicBool>,
) -> Box<dyn ModelProvider> {
    Box::new(CancelAwareProvider {
        inner: provider,
        cancel: cancel.clone(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelKind {
    Unconfigured,
    Mock,
    DeepSeek,
    OpenAi,
}

impl ModelKind {
    fn label(self) -> &'static str {
        match self {
            Self::Unconfigured => "未配置模型",
            Self::Mock => "本地模拟",
            Self::DeepSeek => "DeepSeek",
            Self::OpenAi => "OpenAI 兼容",
        }
    }
}

enum DesktopModel {
    Unconfigured,
    Mock,
    DeepSeek(DeepSeekProvider),
    OpenAi(OpenAiProvider),
}

impl DesktopModel {
    fn kind(&self) -> ModelKind {
        match self {
            Self::Unconfigured => ModelKind::Unconfigured,
            Self::Mock => ModelKind::Mock,
            Self::DeepSeek(_) => ModelKind::DeepSeek,
            Self::OpenAi(_) => ModelKind::OpenAi,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::DeepSeek(provider) => provider.model(),
            Self::OpenAi(provider) => provider.model(),
            Self::Unconfigured | Self::Mock => self.kind().label(),
        }
    }

    fn is_configured(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    fn supports_attachments(&self) -> bool {
        match self {
            Self::Unconfigured => false,
            Self::DeepSeek(provider) => provider.supports_attachments(),
            Self::OpenAi(_) | Self::Mock => true,
        }
    }

    fn reasoning_levels(&self) -> &'static [ReasoningEffort] {
        use ReasoningEffort::{High, Low, Max, Off};
        match self {
            Self::DeepSeek(_) => &[Off, Low, High, Max],
            Self::OpenAi(_) => &[Low, High, Max],
            Self::Unconfigured | Self::Mock => &[],
        }
    }

    fn provider_reasoning(&self) -> Option<ReasoningEffort> {
        match self {
            Self::DeepSeek(provider) => provider.reasoning_effort(),
            Self::OpenAi(provider) => provider.reasoning_effort(),
            Self::Unconfigured | Self::Mock => None,
        }
    }

    fn provider(
        &self,
        cancel: &Arc<AtomicBool>,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn ModelProvider> {
        let reasoning = reasoning.or_else(|| self.provider_reasoning());
        let (name, configured): (&str, Box<dyn ModelProvider>) = match self {
            Self::Unconfigured => return Box::new(UnconfiguredProvider),
            Self::Mock => {
                eprintln!("[chat] provider=mock");
                return cancel_aware(MockProvider, cancel);
            }
            Self::DeepSeek(provider) => (
                "deepseek",
                Box::new(
                    provider
                        .clone()
                        .with_reasoning_effort(reasoning)
                        .with_cancel(cancel.clone()),
                ),
            ),
            Self::OpenAi(provider) => (
                "openai",
                Box::new(
                    provider
                        .clone()
                        .with_reasoning_effort(reasoning)
                        .with_cancel(cancel.clone()),
                ),
            ),
        };
        let retry = RetryPolicy::from_env();
        eprintln!(
            "[chat] provider={name} model={} retries={}",
            self.name(),
            retry.max_attempts
        );
        Box::new(RetryingProvider::new(configured, retry))
    }
}

struct UnconfiguredProvider;

impl ModelProvider for UnconfiguredProvider {
    fn complete(&mut self, _: &CompletionRequest) -> Result<ModelReply, String> {
        Err("请先配置可用的模型连接。".to_owned())
    }
}

fn force_mock() -> bool {
    std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1")
}

fn configured_models() -> Vec<Arc<DesktopModel>> {
    if force_mock() {
        return vec![Arc::new(DesktopModel::Mock)];
    }
    let mut models = Vec::new();
    if let Some(provider) = DeepSeekProvider::from_env() {
        if matches!(
            provider.base_url().trim_end_matches('/'),
            "https://api.deepseek.com" | "https://api.deepseek.com/v1"
        ) && !provider.supports_attachments()
        {
            models.push(Arc::new(DesktopModel::DeepSeek(
                provider.clone().with_model("deepseek-flash"),
            )));
        }
        models.insert(0, Arc::new(DesktopModel::DeepSeek(provider)));
    }
    if let Some(provider) = OpenAiProvider::from_env() {
        models.push(Arc::new(DesktopModel::OpenAi(provider)));
    }
    if std::env::var("DSHR_PROVIDER").as_deref() == Ok("mock") {
        models.push(Arc::new(DesktopModel::Mock));
    }
    models
}

#[derive(Clone)]
struct DesktopSettings {
    model: Arc<DesktopModel>,
    permission: Option<PermissionMode>,
    reasoning: Option<ReasoningEffort>,
}

impl DesktopSettings {
    fn from_env(models: &[Arc<DesktopModel>]) -> Self {
        let requested = if force_mock() {
            Some(ModelKind::Mock)
        } else {
            match std::env::var("DSHR_PROVIDER").as_deref() {
                Ok("mock") => Some(ModelKind::Mock),
                Ok("deepseek") => Some(ModelKind::DeepSeek),
                Ok("openai") => Some(ModelKind::OpenAi),
                _ => None,
            }
        };
        let model = models
            .iter()
            .find(|model| requested.is_none_or(|kind| model.kind() == kind))
            .cloned()
            .unwrap_or_else(|| Arc::new(DesktopModel::Unconfigured));
        if let Some(effort) = ReasoningEffort::from_env() {
            println!("[chat] reasoning_effort={}", effort.as_str());
        }
        Self {
            model,
            permission: desktop_permission_mode(),
            reasoning: None,
        }
    }

}

#[derive(Clone)]
struct LivePermission(Arc<Mutex<Option<PermissionMode>>>);

impl LivePermission {
    fn new(mode: Option<PermissionMode>) -> Self {
        Self(Arc::new(Mutex::new(mode)))
    }

    fn mode(&self) -> Option<PermissionMode> {
        *self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn set(&self, mode: PermissionMode) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(mode);
    }

    fn can_write(&self) -> bool {
        self.mode() != Some(PermissionMode::ReadOnly)
    }

    fn file_write_policy(&self) -> PolicyDecision {
        match self.mode() {
            Some(PermissionMode::DangerFullAccess) => PolicyDecision::Allow,
            Some(PermissionMode::ReadOnly) => {
                PolicyDecision::Deny("file write denied in read-only mode".to_owned())
            }
            Some(PermissionMode::WorkspaceWrite) | None => PolicyDecision::Ask,
        }
    }
}

impl ToolPolicy for LivePermission {
    fn check(&self, tool: &ToolDefinition, arguments: &serde_json::Value) -> PolicyDecision {
        self.mode()
            .map_or_else(ApprovalPolicy::ask_all, ApprovalPolicy::for_mode)
            .check(tool, arguments)
    }
}

fn tool_approval_key(session: &str, workspace: &Path, tool: &ToolDefinition) -> Option<String> {
    let owner = match &tool.origin {
        ToolOrigin::Native { plugin_id } | ToolOrigin::Sidecar { plugin_id, .. } => plugin_id,
        ToolOrigin::Mcp { .. } => return None,
    };
    if owner.is_empty() {
        return None;
    }
    let scope = json!([
        session,
        workspace.to_string_lossy(),
        tool.origin.kind(),
        owner,
        tool.name,
        tool.input_schema,
        tool.presentation,
    ]);
    Some(hex_lower(
        ring::digest::digest(&ring::digest::SHA256, scope.to_string().as_bytes()).as_ref(),
    ))
}

/// sidecar 工具调用超时：按工具声明（`ToolDefinition.timeout_ms`，对齐 DSH 的 `timeoutMs`）
/// 而不是写死 5s。`DSHR_TOOL_TIMEOUT_MS` 可整体覆盖（诊断/慢机器），统一 clamp。
fn sidecar_tool_timeout(definition: &ToolDefinition) -> Duration {
    let override_ms = std::env::var("DSHR_TOOL_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    clamp_tool_timeout_ms(override_ms.unwrap_or(definition.timeout_ms))
}

/// 超时夹到 1s..=600s：太小会误伤正常工具，太大（或 0）会让界面像卡死。
fn clamp_tool_timeout_ms(millis: u64) -> Duration {
    Duration::from_millis(millis.clamp(1_000, 600_000))
}

/// 把 agent 本轮触发的重复工具软提醒落成 durable `tool.repeat` 事件。
///
/// 模型侧的提醒由 `agent-core` 在每步工具执行完后以 system 消息注入（软提醒不失败）；
/// 这里只负责**宿主侧留痕**：回放/审计可见，UI 不需要专门渲染。返回写入条数。
///
/// payload 形状 `{tool, count, threshold, message}`（`message` 即注入模型的提醒文本）。
fn append_repeat_reminders<P: ModelProvider>(
    agent: &mut Agent<P>,
    log: &mut SessionLog,
    ts: &str,
) -> usize {
    let reminders = agent.take_repeat_reminders();
    let written = reminders.len();
    for reminder in reminders {
        let _ = log.append(EventSpec::durable(
            "tool.repeat",
            ts,
            json!({
                "tool": reminder.tool,
                "count": reminder.count,
                "threshold": reminder.threshold,
                "message": reminder.text,
            }),
        ));
    }
    written
}

// ---------------------------------------------------------------------------
// L2 Host Service 的会话读写面（spec/sidecar-rpc.md §13，M3 G3）
// ---------------------------------------------------------------------------

/// `chat` 对 `SessionAccess` 的实现。
///
/// 与 service 层是**同一线程**的同步调用：`ChatExecutor` 在调 sidecar 工具前
/// `publish` 当前内存 `SessionLog` 快照；`working-directory`/`session-log` 的读走快照，
/// 服务的 `append_event` 进队列，工具返回后由 `ChatExecutor` 补写回内存日志。
/// 其它会话的读面落到磁盘（`sessions_dir/<id>.jsonl`），因为插件只在当前会话里执行。
///
/// seq 约定：快照尾部 + 1 递增；publish 与 drain 之间没有其它日志写入（同步单线程），
/// 所以队列事件的 seq 与后续真实日志 seq 一致。
pub struct ChatSessionAccess {
    inner: Mutex<ChatSessionAccessInner>,
}

struct ChatSessionAccessInner {
    sessions_dir: PathBuf,
    published: Option<PublishedSession>,
    pending: Vec<PendingSessionEvent>,
}

#[derive(Clone)]
struct PublishedSession {
    header: SessionHeaderView,
    events: Vec<SessionEventView>,
    next_seq: u64,
}

struct PendingSessionEvent {
    session_id: String,
    event_type: String,
    data: serde_json::Value,
}

impl ChatSessionAccess {
    pub fn new(sessions_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ChatSessionAccessInner {
                sessions_dir,
                published: None,
                pending: Vec::new(),
            }),
        })
    }

    /// 用当前内存日志刷新服务层可见的快照（在每次 sidecar 工具调用前调用）。
    pub fn publish(&self, log: &SessionLog) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let header = chat_header_from_log(log, &inner.sessions_dir);
        let events: Vec<SessionEventView> = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        let next_seq = events.last().map(|event| event.seq + 1).unwrap_or(1);
        inner.published = Some(PublishedSession {
            header,
            events,
            next_seq,
        });
    }

    /// 取走服务层为 `session_id` 追加的事件（按序写回内存日志）。
    pub fn take_appends(&self, session_id: &str) -> Vec<(String, serde_json::Value)> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut taken = Vec::new();
        let mut keep = Vec::with_capacity(inner.pending.len());
        for pending in inner.pending.drain(..) {
            if pending.session_id == session_id {
                taken.push((pending.event_type, pending.data));
            } else {
                keep.push(pending);
            }
        }
        inner.pending = keep;
        taken
    }

    fn clear_snapshot(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.published = None;
        inner.pending.clear();
    }

    // ServiceError 携带结构化 detail（与协议层一致的大 Err），这里按 host crate 的
    // 同样取舍保留原类型，不为了 clippy 的尺寸阈值再包一层 Box。
    #[allow(clippy::result_large_err)]
    fn disk_read(
        &self,
        session_id: &str,
    ) -> Result<(SessionHeaderView, Vec<SessionEventView>), ServiceError> {
        let sessions_dir = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.sessions_dir.clone()
        };
        let log = load_session_log(&sessions_dir, session_id)?;
        let header = chat_header_from_log(&log, &sessions_dir);
        let events = log
            .events()
            .iter()
            .map(|event| SessionEventView {
                seq: event.seq,
                event_type: event.event_type.clone(),
                data: event.payload.clone(),
            })
            .collect();
        Ok((header, events))
    }
}

impl SessionAccess for ChatSessionAccess {
    fn header(&self, session_id: &str) -> Result<SessionHeaderView, ServiceError> {
        {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    return Ok(published.header.clone());
                }
            }
        }
        self.disk_read(session_id).map(|(header, _)| header)
    }

    fn read_events(
        &self,
        session_id: &str,
        from_seq: u64,
        max_events: usize,
    ) -> Result<EventPage, ServiceError> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(published) = &inner.published {
                if published.header.id == session_id {
                    Some(published.events.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id)?.1,
        };
        let start = events
            .iter()
            .position(|event| event.seq >= from_seq)
            .unwrap_or(events.len());
        let page: Vec<SessionEventView> =
            events[start..].iter().take(max_events).cloned().collect();
        let eof = start + page.len() >= events.len();
        let next_seq = page.last().map(|event| event.seq + 1).unwrap_or(from_seq);
        Ok(EventPage {
            events: page,
            next_seq,
            eof,
        })
    }

    fn append_event(
        &self,
        session_id: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<u64, ServiceError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(published) = inner.published.as_mut() else {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "no active session snapshot for host service writes",
            ));
        };
        if published.header.id != session_id {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("session `{session_id}` is not the active session"),
            ));
        }
        let seq = published.next_seq;
        published.events.push(SessionEventView {
            seq,
            event_type: event_type.to_owned(),
            data: data.clone(),
        });
        published.next_seq += 1;
        inner.pending.push(PendingSessionEvent {
            session_id: session_id.to_owned(),
            event_type: event_type.to_owned(),
            data,
        });
        Ok(seq)
    }

    fn latest_working_directory(&self, session_id: &str) -> Option<String> {
        let events = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.published.as_ref().and_then(|published| {
                (published.header.id == session_id).then(|| published.events.clone())
            })
        };
        let events = match events {
            Some(events) => events,
            None => self.disk_read(session_id).ok()?.1,
        };
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "working-directory/change")
            .and_then(|event| event.data.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}

#[allow(clippy::result_large_err)]
fn load_session_log(
    sessions_dir: &std::path::Path,
    session_id: &str,
) -> Result<SessionLog, ServiceError> {
    let path = sessions_dir.join(format!("{session_id}.jsonl"));
    if !path.exists() {
        return Err(ServiceError::new(
            "E_SERVICE_UNAVAILABLE",
            format!("unknown session `{session_id}`"),
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("read session `{session_id}`: {error}"),
        )
    })?;
    SessionLog::from_jsonl(session_id, &text).map_err(|error| {
        ServiceError::new(
            "E_INTERNAL",
            format!("decode session `{session_id}`: {error}"),
        )
    })
}

fn chat_header_from_log(log: &SessionLog, sessions_dir: &std::path::Path) -> SessionHeaderView {
    let created = log.created();
    SessionHeaderView {
        id: log.session_id().to_owned(),
        cwd: created
            .and_then(|created| created.payload().get("workspace_id").cloned())
            .and_then(|workspace| workspace.as_str().map(str::to_owned)),
        created_at: log
            .events()
            .first()
            .map(|event| event.ts.clone())
            .unwrap_or_default(),
        parent_session_id: created.and_then(|created| created.parent_session_id.clone()),
        delegation_depth: session_depth(log, sessions_dir),
    }
}

/// 从 `session.created.parent_session_id` 链推导深度（根 0）；环形/断链时停止。
fn session_depth(log: &SessionLog, sessions_dir: &std::path::Path) -> u32 {
    let mut depth = 0u32;
    let mut current = log.parent_session_id().map(str::to_owned);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(parent) = current {
        if depth >= DEFAULT_MAX_DEPTH || !seen.insert(parent.clone()) {
            break;
        }
        depth += 1;
        current = load_session_log(sessions_dir, &parent)
            .ok()
            .and_then(|parent_log| parent_log.parent_session_id().map(str::to_owned));
    }
    depth
}

/// Agent 的工具执行器：authorize 之后按来源分发给 PluginHost（native）或 Sidecar。
struct ChatExecutor<'a> {
    plugins: &'a mut PluginHost,
    host: &'a mut Option<SidecarHost>,
    approvals: &'a Arc<ApprovalBroker>,
    creator: Option<creator::Session<'a>>,
    step: ToolStep,
    headless: bool,
    diagnostics: bool,
    /// 父会话日志（与流式回调共享同一个 RefCell：`spawn_child` 要向它追加
    /// child.spawned/finished，回调同时在追加 tool.call/tool.result）。
    log: std::rc::Rc<std::cell::RefCell<SessionLog>>,
    /// 会话目录（子会话文件落在这里）。
    sessions_dir: &'a std::path::Path,
    /// 父会话 id。
    parent_id: &'a str,
    workspace: &'a Path,
    /// 父会话取消句柄：子轮次共享，用户停止会同时停掉子会话。
    cancel: &'a Arc<AtomicBool>,
    settings: &'a DesktopSettings,
    permission: &'a LivePermission,
    preset_inputs: &'a Option<(PathBuf, Vec<PathBuf>)>,
    published_presets: &'a mut Option<ParsedAgentPresets>,
    /// 是否允许派生子会话（子会话本身不再派发，默认单层）。
    allow_spawn: bool,
    /// L2 Host Service 的会话读写面（None = 该轮不接 sidecar 服务）。
    l2_access: Option<Arc<ChatSessionAccess>>,
}

#[derive(Clone, Copy)]
enum ToolLocation {
    Global,
    Trial,
    Creator,
    SpawnChild,
}

#[derive(Clone)]
struct ResolvedTool {
    definition: ToolDefinition,
    location: ToolLocation,
}

type ToolStep = std::rc::Rc<RefCell<std::collections::BTreeMap<String, ResolvedTool>>>;

impl ToolExecutor for ChatExecutor<'_> {
    fn specs(&self) -> Vec<ToolSpec> {
        self.step
            .borrow()
            .values()
            .map(|tool| ToolSpec {
                name: tool.definition.name.clone(),
                description: tool.definition.description.clone(),
                parameters: tool.definition.input_schema.clone(),
            })
            .collect()
    }

    fn prepare_step(&mut self) -> Vec<ToolSpec> {
        if let Some(host) = self.host.as_mut() {
            if let Err(error) = host.refresh_registrations() {
                eprintln!("[chat] sidecar tool registry: {error}");
            }
            self.plugins.invalidate_generation(host.generation());
            register_sidecar_tools(self.plugins, host, self.headless);
        }

        let mut tools = self.step.borrow_mut();
        tools.clear();
        let workspace_dependencies_ready = runtime_components::workspace_dependencies_ready();
        let mut insert = |definition: ToolDefinition, location| {
            if !self.diagnostics && definition.name.starts_with("probe_") {
                return;
            }
            if !workspace_dependencies_ready
                && definition.name == runtime_components::WORKSPACE_DEPS_TOOL
                && matches!(&definition.origin, ToolOrigin::Sidecar { plugin_id, .. } if plugin_id == "dsh-official")
            {
                return;
            }
            tools.insert(
                definition.name.clone(),
                ResolvedTool {
                    definition,
                    location,
                },
            );
        };
        for definition in self.plugins.registry().list() {
            insert(definition.clone(), ToolLocation::Global);
        }
        if !self.headless {
            if let Some(host) = self.host.as_ref() {
                for definition in host.preset_tool_definitions(self.parent_id) {
                    insert(definition, ToolLocation::Global);
                }
                for definition in host.tool_definitions_at_scope(Some(self.parent_id)) {
                    insert(definition, ToolLocation::Global);
                }
            }
        }
        if let Some(creator) = &self.creator {
            for definition in creator.scope.plugins.registry().list() {
                insert(definition.clone(), ToolLocation::Trial);
            }
        }
        // Preserve the core entry points if a package declares the same name.
        let mut add_builtin = |spec: ToolSpec, location| {
            insert(
                ToolDefinition::new(
                    spec.name,
                    spec.description,
                    ToolOrigin::Native {
                        plugin_id: "core".to_owned(),
                    },
                )
                .with_schema(spec.parameters),
                location,
            );
        };
        if self.creator.is_some() {
            for spec in creator::specs() {
                add_builtin(spec, ToolLocation::Creator);
            }
        }
        if self.allow_spawn {
            add_builtin(spawn_child_spec(), ToolLocation::SpawnChild);
        }
        drop(tools);
        self.specs()
    }

    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.execute_batch(std::slice::from_ref(call), 1).remove(0)
    }

    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        let mut outcomes = vec![ToolOutcome::failure("batch outcome missing"); calls.len()];
        let mut sidecar_calls = Vec::new();
        let mut sidecar_indices = Vec::new();
        let mut native_calls = [Vec::new(), Vec::new()];
        let mut native_indices = [Vec::new(), Vec::new()];
        let mut replaced_trials = std::collections::BTreeSet::new();

        for (index, call) in calls.iter().enumerate() {
            let tool = self.step.borrow().get(&call.tool).cloned();
            let Some(ResolvedTool {
                definition,
                location,
            }) = tool
            else {
                outcomes[index] = ToolOutcome::failure(format!(
                    "tool {} was not offered for this model step",
                    call.tool
                ));
                continue;
            };
            match location {
                ToolLocation::Creator => {
                    let names: Vec<_> = self
                        .step
                        .borrow()
                        .iter()
                        .filter_map(|(name, tool)| {
                            matches!(tool.location, ToolLocation::Global | ToolLocation::Trial)
                                .then(|| name.clone())
                        })
                        .collect();
                    let creator = self.creator.as_mut().expect("creator step");
                    creator.read_only = !self.permission.can_write();
                    outcomes[index] = creator.execute(call, &names, self.approvals, self.cancel);
                    if call.tool == creator::TRY_PLUGIN_TOOL
                        && outcomes[index].value["replaced"] == true
                    {
                        if let Some(id) = outcomes[index].value["plugin"].as_str() {
                            replaced_trials.insert(id.to_owned());
                        }
                    }
                    continue;
                }
                ToolLocation::SpawnChild => {
                    outcomes[index] = self.execute_spawn_child(call);
                    continue;
                }
                ToolLocation::Global | ToolLocation::Trial => {}
            }
            let grant_key = if !self.headless
                && matches!(location, ToolLocation::Global)
                && self.permission.mode().is_some()
            {
                tool_approval_key(self.parent_id, self.workspace, &definition)
            } else {
                None
            };
            if let Err(outcome) = self.authorize_call(call, &definition, grant_key.as_deref()) {
                outcomes[index] = outcome;
                continue;
            }
            match &definition.origin {
                ToolOrigin::Sidecar { .. } => {
                    sidecar_indices.push(index);
                    sidecar_calls.push(
                        dshr_compat_dsh_host::BatchInvokeCall::new(
                            call.call_id.clone(),
                            call.tool.clone(),
                            call.arguments.clone(),
                            sidecar_tool_timeout(&definition),
                        )
                        .with_session(self.parent_id)
                        .with_binding(definition.origin)
                        .with_approval_key(grant_key),
                    );
                }
                ToolOrigin::Native { .. } => {
                    let scope = usize::from(matches!(location, ToolLocation::Trial));
                    native_indices[scope].push(index);
                    native_calls[scope].push((call.tool.clone(), call.arguments.clone()));
                }
                ToolOrigin::Mcp { .. } => {
                    outcomes[index] = ToolOutcome::failure("mcp not wired");
                }
            }
        }

        if !sidecar_calls.is_empty() {
            self.publish_l2_snapshot();
            if let Some(host) = self.host.as_mut() {
                let results = host.invoke_batch(&sidecar_calls, max_parallel, Some(self.cancel));
                self.drain_l2_appends();
                for (index, result) in sidecar_indices.into_iter().zip(results) {
                    outcomes[index] = match result {
                        Ok(result) if result.terminal.kind == "tool.result" => {
                            outcome_from_terminal(&result.terminal)
                        }
                        Ok(result) => ToolOutcome::failure(result.terminal.payload.to_string()),
                        Err(error) => ToolOutcome::failure(error.to_string()),
                    };
                }
            } else {
                for index in sidecar_indices {
                    outcomes[index] = ToolOutcome::failure("sidecar unavailable");
                }
            }
        }

        for (scope, (indices, calls)) in native_indices.into_iter().zip(native_calls).enumerate() {
            let (indices, calls): (Vec<_>, Vec<_>) = indices.into_iter().zip(calls)
                .filter(|(index, (name, _))| {
                    let from_replaced_trial = scope == 1 && self.step.borrow().get(name)
                        .is_some_and(|tool| matches!(&tool.definition.origin,
                            ToolOrigin::Native { plugin_id } if replaced_trials.contains(plugin_id)));
                    if from_replaced_trial {
                        outcomes[*index] = ToolOutcome::failure(
                            "This trial was replaced during the current step. Call the newly offered tool on the next step.",
                        );
                    }
                    !from_replaced_trial
                }).unzip();
            if calls.is_empty() {
                continue;
            }
            let plugins = if scope == 0 {
                &mut *self.plugins
            } else {
                &mut self.creator.as_mut().expect("trial step").scope.plugins
            };
            let results = plugins.call_approved_batch_cancellable(
                &calls,
                max_parallel.max(1),
                Some(self.cancel),
            );
            for (index, result) in indices.into_iter().zip(results) {
                outcomes[index] = match result {
                    Ok(value) => {
                        ToolOutcome::success(json!([{ "type": "json", "value": value }]), value)
                    }
                    Err(error) => ToolOutcome::failure(error),
                };
            }
        }
        outcomes
    }
}

impl ChatExecutor<'_> {
    fn execute_spawn_child(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        let log = self.log.clone();
        let outcome = TreeCoordinator::spawn_child(&mut log.borrow_mut(), call, self);
        outcome
    }

    #[allow(clippy::result_large_err)]
    fn authorize_call(
        &self,
        call: &ToolCallRequest,
        definition: &ToolDefinition,
        grant_key: Option<&str>,
    ) -> Result<(), ToolOutcome> {
        let registry = self.plugins.registry();
        match registry.authorize_definition(definition, &call.arguments) {
            Ok(_) => Ok(()),
            Err(RegistryError::NeedsApproval { tool }) => {
                let policy = || self.permission.check(definition, &call.arguments);
                let outcome = self.approvals.request_with_policy(
                    &call.call_id,
                    &tool,
                    &call.arguments,
                    Duration::from_secs(300),
                    self.cancel.as_ref(),
                    LiveApproval {
                        decide: &policy,
                        grant_key,
                    },
                );
                if let PolicyDecision::Deny(reason) = policy() {
                    return Err(ToolOutcome::failure(reason));
                }
                match outcome {
                    ApprovalOutcome::AllowOnce | ApprovalOutcome::AllowAlways => {
                        dshr_tool_runtime::ToolRegistry::validate_definition(
                            definition,
                            &call.arguments,
                        )
                        .map_err(|error| ToolOutcome::failure(error.to_string()))
                    }
                    ApprovalOutcome::Deny => {
                        Err(ToolOutcome::failure(format!("user denied tool `{tool}`")))
                    }
                }
            }
            Err(error) => Err(ToolOutcome::failure(error.to_string())),
        }
    }

    fn publish_l2_snapshot(&self) {
        if let Some(access) = &self.l2_access {
            access.publish(&self.log.borrow());
        }
    }

    fn drain_l2_appends(&self) {
        let Some(access) = &self.l2_access else {
            return;
        };
        let appends = access.take_appends(self.parent_id);
        if appends.is_empty() {
            return;
        }
        let ts = now_ts();
        let mut log = self.log.borrow_mut();
        for (event_type, data) in appends {
            let _ = log.append(EventSpec::durable(event_type, &ts, data));
        }
    }
}

/// sidecar `tool.result` 终态 → `ToolOutcome`（content 给模型、value/meta 给 UI/回放）。
fn outcome_from_terminal(terminal: &Envelope) -> ToolOutcome {
    let payload = &terminal.payload;
    let content = payload["content"].clone();
    if content.is_null() {
        if let Some(error) = &terminal.error {
            return ToolOutcome::failure(format!("{}: {}", error.code, error.message));
        }
    }
    // value 优先取自 payload.value；旧插件只在 content 里放值，做一次兼容回退。
    let value = payload.get("value").cloned().unwrap_or_else(|| {
        content
            .get(0)
            .and_then(|block| block.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    });
    ToolOutcome {
        is_error: payload["is_error"].as_bool().unwrap_or(false),
        content,
        value,
        meta: payload.get("meta").cloned(),
    }
}

/// 会话树产品面接线：`spawn_child` = 建子会话 → 同步跑一轮 → 回写父子日志。
///
/// 运行态树由"父会话**内存**日志 + 其余会话磁盘日志"重建
/// （`SessionTreeRuntime::from_logs`）：磁盘上的父日志可能还缺本轮刚追加的
/// `tool.call`，直接用磁盘版本会导致 seq 分叉。
struct TreeCoordinator;

impl TreeCoordinator {
    fn spawn_child(
        log: &mut SessionLog,
        call: &ToolCallRequest,
        executor: &mut ChatExecutor<'_>,
    ) -> ToolOutcome {
        let parent_id = executor.parent_id;
        let cancel = executor.cancel;
        let args = match SpawnChildArgs::from_call(call) {
            Ok(args) => args,
            Err(err) => return ToolOutcome::failure(format!("spawn_child 参数非法: {err}")),
        };
        let store = SessionIndex::new(executor.sessions_dir);
        let child_id = store.new_id();
        let child_path = store.path_for(&child_id);
        let _writer = match try_lock_session(&child_path) {
            Ok(writer) => writer,
            Err(error) => return ToolOutcome::failure(format!("无法取得子会话写入权：{error}")),
        };
        if child_path.exists() || child_path.with_extension("jsonl.bak").exists() {
            return ToolOutcome::failure("子会话 ID 已被使用");
        }
        let ts = now_ts();

        let mut logs: Vec<SessionLog> = Vec::new();
        for meta in store.scan().unwrap_or_default() {
            if meta.id == parent_id {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&meta.path) {
                if let Ok(other) = SessionLog::from_jsonl(&meta.id, &text) {
                    logs.push(other);
                }
            }
        }
        let mut runtime = match SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH) {
            Ok(runtime) => runtime,
            Err(err) => return ToolOutcome::failure(format!("会话树不可用: {err}")),
        };
        let workspace = log
            .created()
            .map(|created| created.workspace_id.clone())
            .unwrap_or_else(|| workspace_root().display().to_string());
        if let Err(err) = runtime.register_root_with_log(parent_id, &workspace, log.clone()) {
            return ToolOutcome::failure(format!("注册父会话失败: {err}"));
        }
        let spawned =
            match runtime.spawn_child(&child_id, parent_id, &args, &SpawnContext::at(ts.clone())) {
                Ok(spawned) => spawned,
                Err(err) => return ToolOutcome::failure(format!("派发子会话失败: {err}")),
            };

        // 子会话独立日志先落盘（含 session.created 的父链接），再跑它的第一轮。
        if let Err(err) = store.write_log(&spawned.log) {
            return ToolOutcome::failure(format!("写入子会话失败: {err}"));
        }
        let mut settings = executor.settings.clone();
        settings.permission = executor.permission.mode();
        let mut child = ChatState::from_log(
            child_path,
            spawned.log,
            session_workspace(log),
            cancel,
            executor.approvals.clone(),
            settings,
            ChatRuntime::Desktop {
                allow_spawn: false,
                sidecar: false,
                diagnostics: executor.diagnostics,
            },
        );
        child.permission = executor.permission.clone();
        child
            .plugins
            .registry_mut()
            .set_policy(child.permission.clone());
        let uses_preset = child
            .log
            .created()
            .and_then(|created| created.preset.as_deref())
            .is_some_and(|preset| preset != creator::PRESET);
        if uses_preset {
            child.host = executor.host.take();
            child.preset_inputs = executor.preset_inputs.clone();
            child.l2_access = executor.l2_access.clone();
            if args.preset.is_none() {
                child.inherited_preset_from = Some(parent_id.to_owned());
            }
        }
        let reply = child.stream_turn(&args.prompt, &mut |_| {});
        if uses_preset {
            *executor.host = child.host.take();
            // The child may have synchronized a newer catalog on this same Sidecar.
            *executor.published_presets = None;
        }
        child.save();

        let interrupted = cancel.load(Ordering::Relaxed);
        let outcome = if interrupted {
            "interrupted"
        } else if reply.is_some() {
            "done"
        } else {
            "failed"
        };
        // 中断传播：父被停止时给仍活跃的后代写 session.interrupted_by_parent。
        //
        // 子日志以 `child.log` 为准（它含子会话自己的对话事件）；runtime 里的子日志副本
        // 只有 session.created + 中断事件，直接回写会丢掉子会话的回答。
        if interrupted {
            let _ = runtime.interrupt_tree(parent_id, "user_stop", &now_ts());
            let _ = child
                .log
                .append_interrupted_by_parent(now_ts(), parent_id, "user_stop");
        }
        child.save();
        let _ = runtime.finish_child(&child_id, outcome, &now_ts());
        let _ = runtime.sync_log(parent_id, log);
        // 父日志（含 child.spawned/finished）落盘：会话索引/树查询都从磁盘重建。
        let _ = store.write_log(log);

        let preview: String = reply
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        let value = json!({
            "child_session_id": child_id,
            "parent_session_id": parent_id,
            "depth": spawned.depth,
            "outcome": outcome,
            "reply": preview,
        });
        ToolOutcome::success(json!([{ "type": "json", "value": value.clone() }]), value)
    }
}

/// 把 sidecar 注册映射进同一个 ToolRegistry。
fn register_sidecar_tools(plugins: &mut PluginHost, host: &SidecarHost, headless: bool) {
    for definition in host.tool_definitions() {
        if headless
            && (definition.presentation.as_deref() == Some("terminal")
                || matches!(
                    definition.name.as_str(),
                    "bash" | "pwsh" | "shell" | "powershell" | "exec"
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
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let _writer = match std::fs::create_dir_all(parent).and_then(|_| try_lock_session(path)) {
        Ok(writer) => writer,
        Err(error) => {
            let message = format!("无法取得会话写入权：{error}");
            let log = std::fs::read_to_string(path).ok()
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
    fn open_diagnostics(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
    ) -> Self {
        Self::open_diagnostics_with_options(path, session_id, cancel, approvals, true, true)
    }

    /// Opens a chat for explicit diagnostics and self-tests.
    fn open_diagnostics_with_options(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        allow_spawn: bool,
        with_sidecar: bool,
    ) -> Self {
        Self::open_in(
            path,
            session_id,
            cancel,
            approvals,
            DesktopSettings::from_env(&configured_models()),
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar: with_sidecar,
                diagnostics: true,
            },
        )
    }

    fn open_with_settings(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        allow_spawn: bool,
        with_sidecar: bool,
        settings: DesktopSettings,
    ) -> Self {
        Self::open_in(
            path,
            session_id,
            cancel,
            approvals,
            settings,
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar: with_sidecar,
                diagnostics: false,
            },
        )
    }

    fn open_in(
        path: PathBuf,
        session_id: &str,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        settings: DesktopSettings,
        runtime: ChatRuntime,
    ) -> Self {
        let OpenedSessionLog {
            log,
            errors: session_errors,
            save_blocked,
            ..
        } = open_session_log(&path, session_id);
        let workspace = session_workspace(&log);
        let mut state = Self::from_log(
            path, log, workspace, cancel, approvals, settings,
            runtime,
        );
        state.session_errors = session_errors;
        state.save_blocked = save_blocked;
        state
    }

    fn from_log(
        path: PathBuf,
        log: SessionLog,
        workspace: PathBuf,
        cancel: &Arc<AtomicBool>,
        approvals: Arc<ApprovalBroker>,
        settings: DesktopSettings,
        runtime: ChatRuntime,
    ) -> Self {
        let (headless, allow_spawn, with_sidecar, diagnostics) = match runtime {
            ChatRuntime::Desktop {
                allow_spawn,
                sidecar,
                diagnostics,
            } => (false, allow_spawn, sidecar, diagnostics),
            ChatRuntime::Headless => (true, false, true, false),
        };
        let sessions_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // L2 会话读写面在 spawn 之前准备好：Router 注册时需要它（chat.rs 只负责构造/注入）。
        let l2_access = if with_sidecar {
            Some(ChatSessionAccess::new(sessions_dir.clone()))
        } else {
            None
        };
        let permission = LivePermission::new(settings.permission);
        let (host, sidecar_start_error) = if with_sidecar {
            match spawn_sidecar_in(
                &workspace,
                (!headless || permission.can_write()).then(|| approvals.clone()),
                (!headless).then(|| (permission.clone(), cancel.clone())),
                l2_access.clone(),
                diagnostics,
            ) {
                Ok(host) => (Some(host), None),
                Err(error) => {
                    eprintln!("[chat] {error}");
                    (None, Some(error))
                }
            }
        } else {
            (None, None)
        };
        if let Some(mode) = settings.permission {
            eprintln!("[chat] approval mode={}", mode.as_str());
        }
        let mut plugins = PluginHost::new(permission.clone());
        if diagnostics {
            if let Err(err) = plugins.load(Box::new(ProbeNativePlugin)) {
                eprintln!("[chat] 诊断插件加载失败: {err}");
            }
            register_probe_write(&mut plugins, &workspace);
        }
        if let Some(sidecar) = &host {
            register_sidecar_tools(&mut plugins, sidecar, headless);
        }
        let seq = last_message_seq(&log);
        let session_id = log.session_id().to_owned();
        let mut state = Self {
            log,
            path: path.clone(),
            workspace,
            host,
            workspace_sidecars: HashMap::new(),
            preset_diagnostics: Vec::new(),
            active_preset_error: None,
            default_preset: None,
            inherited_preset_from: None,
            preset_inputs: loader_inputs(),
            published_presets: None,
            sidecar_start_error,
            plugins,
            agent: Agent::new(settings.model.provider(cancel, settings.reasoning)),
            settings,
            permission,
            seq,
            approvals,
            spill: SpillStore::new(spill_dir_for(&path)),
            session_id,
            sessions_dir,
            l2_access,
            cancel: cancel.clone(),
            allow_spawn,
            external_plugin_errors: Vec::new(),
            session_errors: Vec::new(),
            save_blocked: None,
            adc_panels: Vec::new(),
            headless,
            diagnostics,
            creator_trials: HashMap::new(),
        };
        if !headless {
            state.refresh_agent_presets();
            state.refresh_adc_panels();
        }
        state
    }

    /// 在同一个会话目录内切换日志，复用窗口持有的工具运行时。
    /// 返回是否已新建或恢复日志，供侧栏刷新索引。
    fn switch_session(&mut self, path: PathBuf, session_id: &str) -> bool {
        let opened = open_session_log(&path, session_id);
        if let Some(access) = &self.l2_access {
            access.clear_snapshot();
        }
        self.seq = last_message_seq(&opened.log);
        self.log = opened.log;
        let workspace = session_workspace(&self.log);
        if workspace != self.workspace {
            let previous = std::mem::replace(&mut self.workspace, workspace);
            if self.diagnostics {
                let _ = self.plugins.unload("builtin.probe-write");
                register_probe_write(&mut self.plugins, &self.workspace);
            }
            if self.l2_access.is_some() {
                if let Some(mut host) = self.host.take() {
                    self.plugins.invalidate_generation(host.generation());
                    if !host.has_exited() {
                        self.workspace_sidecars.insert(previous, host);
                    }
                }
                let cached = self.workspace_sidecars.remove(&self.workspace);
                if let Some(mut host) = cached {
                    if !host.has_exited() {
                        self.attach_sidecar(host);
                    }
                }
                if self.host.is_none() {
                    self.start_sidecar();
                }
            }
        }
        self.spill = SpillStore::new(spill_dir_for(&path));
        self.path = path;
        self.session_id = session_id.to_owned();
        self.session_errors = opened.errors;
        self.save_blocked = opened.save_blocked;
        if !self.headless {
            self.refresh_agent_presets();
            self.refresh_adc_panels();
        }
        opened.wrote_created || opened.recovered
    }

    fn refresh_agent_presets(&mut self) {
        if self.headless {
            return;
        }
        let catalog = read_agent_presets_from(self.preset_inputs.as_ref());
        self.publish_agent_presets(catalog);
        self.bind_agent_preset();
    }

    fn publish_agent_presets(&mut self, parsed: ParsedAgentPresets) {
        self.preset_diagnostics.clear();
        let catalog = match &parsed {
            Ok(catalog) => catalog,
            Err(error) => {
                self.preset_diagnostics
                    .push(format!("预设配置读取失败：{error}"));
                self.published_presets = Some(parsed);
                return;
            }
        };
        let definitions = json!(catalog.definitions);
        let mut applied = true;
        let hosts = self
            .host
            .iter_mut()
            .chain(self.workspace_sidecars.values_mut());
        for host in hosts.filter(|host| host.supports_agent_presets()) {
            if host.has_exited() {
                continue;
            }
            match host.sync_presets(&definitions) {
                Ok(status) => {
                    self.default_preset.clone_from(&catalog.default);
                    if let Some(rows) = status.as_object() {
                        for (id, row) in rows {
                            if let Some(error) = row["error"].as_str() {
                                let message = format!("预设 {id}: {error}");
                                if !self.preset_diagnostics.contains(&message) {
                                    self.preset_diagnostics.push(message);
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    applied = false;
                    self.preset_diagnostics.push(error.to_string());
                }
            }
        }
        self.published_presets = applied.then_some(parsed);
    }

    fn bind_agent_preset(&mut self) {
        self.active_preset_error = None;
        let id = self
            .log
            .created()
            .and_then(|created| created.preset.as_deref())
            .filter(|id| *id != creator::PRESET);
        if let Some(host) = self
            .host
            .as_mut()
            .filter(|host| host.supports_agent_presets())
        {
            // A blank-session change can update the stored preset ID.
            // An unchanged ID keeps its existing revision across reloads.
            if host.bound_preset(&self.session_id) != id {
                if let Err(error) = host.release_preset(&self.session_id) {
                    self.active_preset_error = Some(format!("无法释放原预设：{error}"));
                    return;
                }
            }
            if let Some(id) = id {
                if let Err(error) =
                    host.bind_preset(&self.session_id, id, self.inherited_preset_from.as_deref())
                {
                    self.active_preset_error = Some(format!("无法启用预设 {id}：{error}"));
                }
            }
        } else if let Some(id) = id {
            self.active_preset_error = Some(format!("预设 {id} 需要支持 Agent 预设的官方 Sidecar"));
        }
    }

    fn select_blank_preset(&mut self, preset: Option<String>) -> Result<(), String> {
        let _writer = self.lock_and_refresh()?;
        let created = blank_root_created(&self.log)
            .ok_or_else(|| "只能在空白的根会话更改模式".to_owned())?;
        if created.preset == preset {
            return Ok(());
        }
        let created = created.clone().with_optional_preset(preset);
        self.publish_agent_presets(read_agent_presets_from(self.preset_inputs.as_ref()));
        if let Some(preset) = created.preset.as_deref().filter(|id| *id != creator::PRESET) {
            let host = self
                .host
                .as_mut()
                .filter(|host| host.supports_agent_presets())
                .ok_or_else(|| "此模式需要支持 Agent 预设的官方 Sidecar".to_owned())?;
            if host.has_exited() {
                return Err("官方 Sidecar 已退出，无法改选模式".to_owned());
            }
            let status = host
                .preset_status()
                .get(preset)
                .ok_or_else(|| format!("预设 {preset} 不可用"))?;
            if let Some(error) = status["error"].as_str() {
                return Err(format!("无法启用预设 {preset}：{error}"));
            }
        }
        let mut updated = SessionLog::new(&self.session_id);
        updated
            .append_created(&self.log.events()[0].ts, &created)
            .map_err(|error| error.to_string())?;
        save_atomic(&self.path, updated.to_jsonl().as_bytes())
            .map_err(|error| format!("无法保存会话模式：{error}"))?;
        self.log = updated;
        self.creator_trials.remove(&self.session_id);
        self.bind_agent_preset();
        self.refresh_adc_panels();
        match &self.active_preset_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn load_errors(&self) -> Vec<String> {
        self.host
            .iter()
            .flat_map(|host| host.boot_errors())
            .chain(&self.sidecar_start_error)
            .chain(&self.active_preset_error)
            .chain(&self.preset_diagnostics)
            .chain(&self.external_plugin_errors)
            .chain(&self.session_errors)
            .cloned()
            .collect()
    }

    /// 刷新 `workspace.panel` 的 ADC 视图缓存（装载/重载插件后调用一次）。
    ///
    /// 渲染路径（`plugins_panel`）只读缓存：插件 IPC 不进入每帧渲染，避免 UI 线程
    /// 等外部进程（`ui.render` 超时默认 10s）。未知 view / 不实现 view 的插件只留下
    /// 可读错误，不影响其它面板。
    fn refresh_adc_panels(&mut self) {
        let mut panels = std::collections::BTreeMap::new();
        if let Some(scope) = self.creator_trials.get_mut(&self.session_id) {
            Self::render_adc_panels(&mut scope.plugins, true, &mut panels);
        }
        Self::render_adc_panels(&mut self.plugins, false, &mut panels);
        self.adc_panels = panels.into_values().collect();
    }

    fn apply_adc_action(
        &mut self,
        plugin_id: &str,
        view: &str,
        action: &str,
        payload: &serde_json::Value,
    ) -> Result<(), dshr_plugin_host::HostError> {
        let plugins = match self.creator_trials.get_mut(&self.session_id) {
            Some(scope) if scope.plugins.panels().iter().any(|registration| {
                registration.plugin_id == plugin_id
                    && registration.panel.slot == "workspace.panel"
                    && registration.panel.view_name() == view
            }) => &mut scope.plugins,
            _ => &mut self.plugins,
        };
        plugins.dispatch_action(plugin_id, view, action, payload)?;
        self.refresh_adc_panels();
        Ok(())
    }

    fn render_adc_panels(
        plugins: &mut PluginHost,
        trial: bool,
        panels: &mut std::collections::BTreeMap<(String, String), AdcPanelView>,
    ) {
        let registrations: Vec<_> = plugins
            .panels()
            .into_iter()
            .filter(|registration| registration.panel.slot == "workspace.panel")
            .map(|registration| {
                (
                    registration.plugin_id.clone(),
                    registration.panel.title.clone(),
                    registration.panel.view_name().to_owned(),
                )
            })
            .collect();
        for (plugin_id, title, view) in registrations {
            panels
                .entry((plugin_id.clone(), view.clone()))
                .or_insert_with(|| {
                    let (tree, error) = match plugins.render_panel(&plugin_id, &view) {
                        Ok(tree) => (Some(tree), None),
                        Err(error) => (None, Some(format!("view `{view}`: {error}"))),
                    };
                    AdcPanelView {
                        plugin_id,
                        title,
                        trial,
                        view,
                        tree,
                        error,
                    }
                });
        }
    }

    fn drop_session_scope(&mut self, session_id: &str) {
        for host in self
            .host
            .iter_mut()
            .chain(self.workspace_sidecars.values_mut())
        {
            if let Err(error) = host.retire_session(session_id) {
                eprintln!("[chat] retire sidecar session {session_id}: {error}");
            }
        }
        if self.creator_trials.remove(session_id).is_some() && self.session_id == session_id {
            self.refresh_adc_panels();
        }
    }

    /// 组装上下文计划：先按阈值把超大工具结果外部化（写对象 + 事件留痕），再重新计划。
    /// 外部化后模型只看到"引用 + 预览"，原始内容可从内容寻址对象无损取回。
    fn plan_with_spill(&mut self) -> ContextPlan {
        let policy = SpillPolicy::from_env();
        let mut plan = plan_context_with_spill(&self.log, "", &policy);
        if plan.spilled.is_empty() {
            return plan;
        }
        let ts = now_ts();
        let mut recorded = 0usize;
        for pending in &plan.spilled {
            match self.spill.put_pending(pending) {
                Ok(_) => {
                    if dshr_agent_context::spill::record_spill(&mut self.log, pending, &ts).is_ok()
                    {
                        recorded += 1;
                    }
                }
                Err(err) => eprintln!("[chat] 外部化工具结果失败: {err}"),
            }
        }
        if recorded > 0 {
            self.save();
            plan = plan_context_with_spill(&self.log, "", &policy);
        }
        plan
    }

    /// 模拟 Sidecar 崩溃：销毁进程 + 撤销该 generation 的全部工具。
    fn kill_sidecar(&mut self) -> Option<String> {
        let host = self.host.take()?;
        let generation = host.generation().to_owned();
        drop(host);
        self.plugins.invalidate_generation(&generation);
        Some(generation)
    }

    /// 重启 Sidecar：先撤销旧 generation，再按当前 profile/环境重新装载。
    fn restart_sidecar(&mut self) -> bool {
        self.workspace_sidecars.clear();
        self.restart_active_sidecar()
    }

    fn restart_active_sidecar(&mut self) -> bool {
        if let Some(old) = self.host.take() {
            self.plugins.invalidate_generation(old.generation());
        }
        self.start_sidecar()
    }

    fn start_sidecar(&mut self) -> bool {
        let host = match spawn_sidecar_in(
            &self.workspace,
            (!self.headless || self.permission.can_write()).then(|| self.approvals.clone()),
            (!self.headless).then(|| (self.permission.clone(), self.cancel.clone())),
            self.l2_access.clone(),
            self.diagnostics,
        ) {
            Ok(host) => host,
            Err(error) => {
                eprintln!("[chat] {error}");
                self.sidecar_start_error = Some(error);
                return false;
            }
        };
        self.attach_sidecar(host);
        true
    }

    fn attach_sidecar(&mut self, host: SidecarHost) {
        register_sidecar_tools(&mut self.plugins, &host, self.headless);
        self.host = Some(host);
        self.sidecar_start_error = None;
    }

    fn set_model(&mut self, model: Arc<DesktopModel>, reasoning: Option<ReasoningEffort>) {
        *self.agent.provider_mut() = model.provider(&self.cancel, reasoning);
        self.settings.model = model;
        self.settings.reasoning = reasoning;
    }

    fn save(&self) {
        if let Err(error) = self.save_checked() {
            eprintln!("[chat] {error}");
        }
    }

    fn save_checked(&self) -> Result<(), String> {
        if let Some(reason) = &self.save_blocked {
            return Err(format!("跳过保存（{reason}）: {}", self.path.display()));
        }
        // 临时文件 + 替换 + 上一版 .bak：崩溃/断电不会留下半截文件；
        // Windows 上 remove→rename 的极短窗口由 recover_backup 兜底。
        save_atomic(&self.path, self.log.to_jsonl().as_bytes())
            .map_err(|error| format!("保存会话 {} 失败：{error}", self.path.display()))
    }

    fn lock_and_refresh(&mut self) -> Result<std::fs::File, String> {
        let writer = try_lock_session(&self.path)
            .map_err(|error| format!("无法取得会话写入权：{error}"))?;
        let text = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("读取会话日志失败：{error}"))?;
        let log = SessionLog::from_jsonl(&self.session_id, &text)
            .map_err(|error| format!("解析会话日志失败：{error}"))?;
        self.seq = last_message_seq(&log);
        self.log = log;
        self.save_blocked = None;
        Ok(writer)
    }

    /// 跑一轮对话：用户消息 → Agent（模型 + 工具）→ 助手消息落库。
    ///
    /// **每产生一个事件就推一份完整快照**给调用方（UI 在别的线程画），
    /// 所以网络等待不会阻塞界面，流式文本也是真的逐段到达。
    ///
    /// 同步场景（自检、`--live`）传一个空 sink 即可。
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

    fn run_turn(
        &mut self,
        text: &str,
        attachments: &[DraftAttachment],
        emit: &mut dyn FnMut(TurnUpdate<'_>),
    ) -> Result<TurnResult, String> {
        self.refresh_agent_presets();
        if let Some(error) = &self.active_preset_error {
            return Err(error.clone());
        }
        let ts = now_ts();
        self.seq += 1;
        let user_id = format!("m-u{}", self.seq);
        let turn_id = format!("turn-{}", self.seq);
        let (payload, blobs) = user_message_payload(&user_id, text, attachments, &self.spill)?;
        if !blobs.is_empty() {
            let mut recorded: std::collections::HashSet<_> = self
                .log
                .events()
                .iter()
                .filter(|event| {
                    event.event_type == "spill.created" && event.payload["kind"] == "message.user"
                })
                .filter_map(|event| event.payload["id"].as_str().map(str::to_owned))
                .collect();
            for blob in blobs {
                if recorded.insert(blob.id.clone()) {
                    self.log.append(EventSpec::durable(
                        "spill.created",
                        &ts,
                        blob.user_attachment_payload(),
                    )).map_err(|error| error.to_string())?;
                }
            }
        }
        let current = user_message_from_payload(&payload).expect("nonempty user message");
        self.log
            .append(EventSpec::durable("message.user", &ts, payload))
            .map_err(|error| error.to_string())?;
        self.log.append(EventSpec::durable(
            "turn.started", &ts, json!({ "turn_id": turn_id, "prompt_message_id": user_id })
        )).map_err(|error| error.to_string())?;
        self.save_checked()?;
        emit(TurnUpdate::Preview(&self.log, None, None));

        // 历史从 durable 事件重建：多轮记忆靠它，重开应用也能接上。
        // 上下文预算在这里做：超大结果先外部化、超长工具结果头尾裁剪、超预算丢最旧，且不拆配对。
        let mut plan = self.plan_with_spill();
        if let Some((from_seq, to_seq)) = plan.dropped_range {
            // 丢弃了旧轮次：先让模型生成摘要，写 context.compacted，再按压缩后的历史请求。
            let summary = {
                let provider: &mut dyn ModelProvider = self.agent.provider_mut().as_mut();
                summarize_dropped(provider, &plan.dropped)
            };
            let summary_ts = now_ts();
            self.seq += 1;
            let summary_id = format!("m-sum{}", self.seq);
            let tokens_after = plan.tokens_after + estimate_tokens(&summary);
            let _ = self.log.append(EventSpec::durable(
                "message.assistant.settled",
                &summary_ts,
                json!({ "message_id": summary_id, "text": summary }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "context.compacted",
                &summary_ts,
                json!({
                    "from_seq": from_seq,
                    "to_seq": to_seq,
                    "summary_message_id": summary_id,
                    "tokens_before": plan.tokens_before,
                    "tokens_after": tokens_after,
                }),
            ));
            self.save_checked()?;
            plan = self.plan_with_spill();
            eprintln!(
                "[chat] 已压缩 seq {from_seq}..{to_seq}：{} -> {} tokens",
                plan.tokens_before, plan.tokens_after
            );
            emit(TurnUpdate::Preview(&self.log, None, None));
        }
        let _ = self.log.append(EventSpec::durable(
            "context.measured",
            &ts,
            json!({
                "tokens": {
                    "input": plan.tokens_after,
                    "output": 0,
                    "total": plan.tokens_after,
                },
                "window": plan.window,
                "ratio": plan.ratio(),
                "pruned_tool_results": plan.pruned_tool_results,
                "dropped_messages": plan.dropped_messages,
                "spilled_tool_results": plan.spilled_tool_results(),
            }),
        ));
        self.save_checked()?;
        if plan.triggered() {
            eprintln!(
                "[chat] 上下文裁剪: {} -> {} tokens（窗口 {}，裁剪工具结果 {} 条，丢弃消息 {} 条）",
                plan.tokens_before,
                plan.tokens_after,
                plan.window,
                plan.pruned_tool_results,
                plan.dropped_messages
            );
        }
        emit(TurnUpdate::Preview(&self.log, None, None));
        let mut history = plan.messages;
        // 预算计入刚落盘的用户输入。Agent 会单独追加本轮消息，避免在模型请求中发送两份。
        if history.last() == Some(&current) {
            history.pop();
        }
        history.push(current);
        dshr_agent_context::spill::restore_user_attachments(
            &self.spill,
            &self.log,
            &mut history,
            |_| self.settings.model.supports_attachments(),
        );
        let current = history.pop().expect("current user message");

        let creator_enabled = !self.headless && creator::is_creator_session(&self.log);
        let preset = self
            .log
            .created()
            .and_then(|created| created.preset.clone());
        let step = ToolStep::default();
        let mut reply = String::new();
        let mut reasoning = String::new();
        let mut reason = TurnReason::Error;
        let mut event_error = None;
        // 非失败结束原因（用户取消）：决定 TurnEnded 后写 session.interrupted 而不是“处理失败”。
        let mut cancelled: Option<String> = None;
        let trial_revision_before_turn = self
            .creator_trials
            .get(&self.session_id)
            .map_or(0, |scope| scope.revision);
        // 父日志临时放进 Rc<RefCell>：流式回调（tool.* 事件）与 spawn_child 执行器
        // （child.spawned/finished）都要写同一份父日志，单借用无法同时满足。
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        {
            let mut executor = ChatExecutor {
                plugins: &mut self.plugins,
                host: &mut self.host,
                approvals: &self.approvals,
                creator: creator_enabled.then(|| creator::Session {
                    preset: preset.as_deref().expect("creator preset"),
                    workspace: &self.workspace,
                    sessions_dir: &self.sessions_dir,
                    scope: self
                        .creator_trials
                        .entry(self.session_id.clone())
                        .or_insert_with(|| {
                            creator::TrialScope::new(self.permission.clone(), &self.workspace)
                        }),
                    read_only: !self.permission.can_write(),
                }),
                step: step.clone(),
                headless: self.headless,
                diagnostics: self.diagnostics,
                log: log_cell.clone(),
                sessions_dir: &self.sessions_dir,
                parent_id: &self.session_id,
                workspace: &self.workspace,
                cancel: &self.cancel,
                settings: &self.settings,
                permission: &self.permission,
                preset_inputs: &self.preset_inputs,
                published_presets: &mut self.published_presets,
                allow_spawn: self.allow_spawn,
                l2_access: self.l2_access.clone(),
            };
            let log_for_events = log_cell.clone();
            let mut pending_call_seqs: HashMap<String, VecDeque<u64>> = HashMap::new();
            self.agent.run_turn_streaming_message(
                &history,
                current,
                &mut executor,
                &mut |event| match event {
                    AgentEvent::TextDelta(delta) => {
                        reply.push_str(delta);
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::ReasoningDelta(delta) => {
                        reasoning.push_str(delta);
                        if !self.headless {
                            emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                        }
                    }
                    AgentEvent::ToolCall(call) => {
                        let tools = step.borrow();
                        let definition = tools.get(&call.tool).map(|tool| &tool.definition);
                        let origin = definition.map_or("unknown", |tool| tool.origin.kind());
                        let presentation = definition.and_then(|tool| tool.presentation.as_deref())
                            .unwrap_or("generic");
                        let registration = match definition.map(|tool| &tool.origin) {
                            Some(ToolOrigin::Sidecar { registration_id, generation, plugin_id }) =>
                                Some((registration_id, generation, plugin_id)),
                            _ => None,
                        };
                        let mut data = json!({
                            "call_id": call.call_id,
                            "tool": call.tool,
                            "origin": origin,
                            "presentation": presentation,
                            "arguments": call.arguments
                        });
                        if let Some((id, generation, plugin_id)) = registration {
                            data["registration_id"] = json!(id);
                            data["generation"] = json!(generation);
                            data["plugin_id"] = json!(plugin_id);
                        }
                        drop(tools);
                        {
                            let mut log = log_for_events.borrow_mut();
                            if let Err(error) = record_reasoning(&mut log, &ts, &mut reasoning) {
                                event_error.get_or_insert_with(|| error.to_string());
                            }
                            let call_seq = log.len() as u64 + 1;
                            data["call_seq"] = json!(call_seq);
                            match log.append(EventSpec::durable("tool.call", &ts, data)) {
                                Ok(seq) => {
                                    pending_call_seqs.entry(call.call_id.clone()).or_default().push_back(seq);
                                    emit(TurnUpdate::Call { seq, tool: &call.tool });
                                }
                                Err(error) => { event_error.get_or_insert_with(|| error.to_string()); }
                            }
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::ToolResult { call_id, outcome } => {
                        let Some(call_seq) = pending_call_seqs.get_mut(call_id).and_then(VecDeque::pop_front) else {
                            return;
                        };
                        let status = {
                            let mut log = log_for_events.borrow_mut();
                            // P1-3：普通工具失败（拒绝/报错/预算耗尽）必须写 tool.result(is_error)，
                            // 只有“真正被取消”的调用才写 tool.aborted（reason 用 spec 枚举）。
                            let (event, status) = if outcome.is_error && self.cancel.load(Ordering::Relaxed) {
                                (EventSpec::durable(
                                    "tool.aborted",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "call_seq": call_seq,
                                        "reason": "cancel"
                                    }),
                                ), "aborted")
                            } else {
                                (EventSpec::durable(
                                    "tool.result",
                                    &ts,
                                    json!({
                                        "call_id": call_id,
                                        "call_seq": call_seq,
                                        "is_error": outcome.is_error,
                                        // 对齐 DSH：content 给模型，value/meta 给展示与回放。
                                        "content": outcome.content,
                                        "value": outcome.value,
                                        "meta": outcome.meta,
                                    }),
                                ), if outcome.is_error { "error" } else { "completed" })
                            };
                            match log.append(event) {
                                Ok(_) => Some(status),
                                Err(error) => {
                                    event_error.get_or_insert_with(|| error.to_string());
                                    None
                                }
                            }
                        };
                        if let Some(status) = status {
                            emit(TurnUpdate::Result { seq: call_seq, status });
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::ToolCancelled { call_id, reason } => {
                        let Some(call_seq) = pending_call_seqs.get_mut(call_id).and_then(VecDeque::pop_front) else {
                            return;
                        };
                        let aborted = {
                            let mut log = log_for_events.borrow_mut();
                            log.append(EventSpec::durable(
                                "tool.aborted",
                                &ts,
                                json!({ "call_id": call_id, "call_seq": call_seq, "reason": reason }),
                            ))
                        };
                        match aborted {
                            Ok(_) => emit(TurnUpdate::Result { seq: call_seq, status: "aborted" }),
                            Err(error) => { event_error.get_or_insert_with(|| error.to_string()); }
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), None));
                    }
                    AgentEvent::TurnCancelled { reason } => {
                        cancelled = Some(reason.clone());
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::TurnFailed(err) => {
                        if !self.headless {
                            reply.push_str(&format!("处理失败：{err}"));
                        }
                        emit(TurnUpdate::Preview(&log_for_events.borrow(), Some(&reply), Some(&reasoning)));
                    }
                    AgentEvent::TurnEnded { .. } => reason = TurnReason::Completed,
                    AgentEvent::TurnStarted { .. } | AgentEvent::StepStarted { .. } => {}
                },
            );
        }
        // 重复工具软提醒：本轮结束后一次性落 durable 事件（模型侧注入由 agent-core 完成）。
        {
            let repeat_ts = now_ts();
            let mut log = log_cell.borrow_mut();
            let written = append_repeat_reminders(&mut self.agent, &mut log, &repeat_ts);
            if written > 0 {
                eprintln!("[chat] tool.repeat: {written} 条（重复工具调用软提醒已落日志）");
            }
        }
        // 还原父日志：spawn_child 可能已往里追加 child.spawned / child.finished。
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        if let Some(error) = event_error {
            return Err(error);
        }
        // 用户中断留痕（回放/审计可见；子会话的中断由 TreeCoordinator 传播）。
        // 取消可能由 Agent 的 TurnCancelled 上报，也可能发生在工具返回后，两个来源都覆盖。
        let cancelled_by_flag = self.cancel.load(Ordering::Relaxed);
        if cancelled.is_some() || cancelled_by_flag {
            let reason = cancelled
                .clone()
                .unwrap_or_else(|| String::from("user_stop"));
            let _ = self.log.append(EventSpec::durable(
                "session.interrupted",
                now_ts(),
                json!({ "reason": reason, "by_parent": false }),
            ));
        }

        if self
            .creator_trials
            .get(&self.session_id)
            .map_or(0, |scope| scope.revision)
            != trial_revision_before_turn
            || self.adc_panels.iter().any(|panel| panel.trial)
        {
            self.refresh_adc_panels();
        }

        // 审批痕迹落盘（在工具事件之后：审批链用于审计/回放，不影响模型输入）。
        let records = self.approvals.take_records();
        for record in records {
            let approval_ts = now_ts();
            let _ = self.log.append(EventSpec::durable(
                "approval.asked",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "request": { "tool": record.tool, "arguments": record.arguments },
                    "policy": "desktop",
                }),
            ));
            let _ = self.log.append(EventSpec::durable(
                "approval.answered",
                &approval_ts,
                json!({
                    "call_id": record.call_id,
                    "outcome": record.outcome.as_str(),
                    "answerer": record.answerer,
                }),
            ));
        }

        self.seq += 1;
        let ts = now_ts();
        record_reasoning(&mut self.log, &ts, &mut reasoning).map_err(|error| error.to_string())?;
        // 取消且没有任何产出时不写空的 assistant 消息（避免 UI 出现空白气泡）。
        let cancelled_turn = cancelled_by_flag || cancelled.is_some();
        if cancelled_turn {
            reason = TurnReason::Cancelled;
        }
        if !(cancelled_turn && reply.trim().is_empty()) {
            self.log
                .append(EventSpec::durable(
                    "message.assistant.settled",
                    &ts,
                    json!({ "message_id": format!("m-a{}", self.seq), "text": reply }),
                ))
                .map_err(|error| error.to_string())?;
        }
        self.log
            .append(EventSpec::durable(
                "turn.ended",
                &ts,
                json!({ "turn_id": turn_id, "reason": reason.as_str() }),
            ))
            .map_err(|error| error.to_string())?;
        self.save_checked()?;
        emit(TurnUpdate::Preview(&self.log, None, None));
        Ok(TurnResult { reply, reason })
    }

    /// 同步版本（自检与无 UI 场景使用）：丢弃 UI 快照。
    fn submit(&mut self, text: &str) {
        let _ = self.stream_turn(text, &mut |_| {});
    }

    fn items(&self) -> Vec<Item> {
        build_from_log(&self.log)
    }

    /// 直接执行一次工具调用（自检用；走与真实轮次相同的策略/审批/沙箱路径）。
    fn execute_tool(&mut self, call: &ToolCallRequest) -> ToolOutcome {
        self.refresh_agent_presets();
        if let Some(error) = &self.active_preset_error {
            return ToolOutcome::failure(error.clone());
        }
        let creator_enabled = !self.headless && creator::is_creator_session(&self.log);
        let preset = self
            .log
            .created()
            .and_then(|created| created.preset.clone());
        let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
            &mut self.log,
            SessionLog::new(&self.session_id),
        )));
        let step = ToolStep::default();
        let mut executor = ChatExecutor {
            plugins: &mut self.plugins,
            host: &mut self.host,
            approvals: &self.approvals,
            creator: creator_enabled.then(|| creator::Session {
                preset: preset.as_deref().expect("creator preset"),
                workspace: &self.workspace,
                sessions_dir: &self.sessions_dir,
                scope: self
                    .creator_trials
                    .entry(self.session_id.clone())
                    .or_insert_with(|| {
                        creator::TrialScope::new(self.permission.clone(), &self.workspace)
                    }),
                read_only: !self.permission.can_write(),
            }),
            step: step.clone(),
            headless: self.headless,
                diagnostics: self.diagnostics,
            log: log_cell.clone(),
            sessions_dir: &self.sessions_dir,
            parent_id: &self.session_id,
            workspace: &self.workspace,
            cancel: &self.cancel,
            settings: &self.settings,
            permission: &self.permission,
            preset_inputs: &self.preset_inputs,
            published_presets: &mut self.published_presets,
            allow_spawn: self.allow_spawn,
            l2_access: self.l2_access.clone(),
        };
        executor.prepare_step();
        let outcome = executor.execute(call);
        drop(executor);
        self.log = std::rc::Rc::try_unwrap(log_cell)
            .map(|cell| cell.into_inner())
            .unwrap_or_else(|cell| cell.borrow().clone());
        outcome
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
    attachments: Vec<DraftAttachment>,
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
    trial_panels_collapsed: bool,
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
                .any(|file| matches!(file.content, AttachmentContent::Media { .. }))
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
        self.turn_menu_open = false;
        self.turn_jump = None;
        self.turn_focus = None;
        self.follow_transcript = true;
        let mut attachments = std::mem::take(&mut self.attachments);
        for attachment in &mut attachments {
            if let Some(image) = attachment.thumbnail.take() {
                cx.drop_image(image, Some(window));
            }
        }
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
                        Ok(DesktopTurnUpdate::Finished { plugin_errors, adc_panels }) => {
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
                            if app.busy {
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
                            if panels.iter().filter(|panel| panel.trial).any(|panel| {
                                !app.adc_panels.iter().any(|previous| {
                                    previous.trial
                                        && previous.plugin_id == panel.plugin_id
                                        && previous.view == panel.view
                                })
                            }) {
                                app.trial_panels_collapsed = false;
                            }
                            app.plugin_errors = errors;
                            app.adc_panels = panels;
                        }
                        app.busy = false;
                        cx.notify();
                    });
                    return;
                }
                // 审批可能出现在两个快照之间：轮询 pending，变化时重绘。
                let _ = this.update(cx, |app, cx| {
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
            self.active_preset = state.log.created().and_then(|created| created.preset.clone());
            self.blank_root = blank_root_created(&state.log).is_some();
            index_changed
        };
        self.session_id = id;
        self.open_processes.clear();
        self.copied_message = None;
        self.trial_panels_collapsed = false;
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
            match self.sessions.first().map(|session| session.id.clone()) {
                Some(next) => self.open_session(next, window, cx),
                None => self.new_session(window, cx),
            }
        } else {
            cx.notify();
        }
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
            eprintln!("[chat] 重命名会话 {id} 失败: {err}");
        }
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

    fn conversation_trial_panels(
        &self,
        t: &Theme,
        height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.adc_panels.iter().any(|panel| panel.trial) {
            return None;
        }
        let expanded = !self.trial_panels_collapsed;
        let p = t.palette;
        Some(
            div()
                .flex_none()
                .w_full()
                .max_w(t.layout.composer_max_width)
                .mx_auto()
                .px(px(16.0))
                .pb(px(6.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(t.type_scale.secondary)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.color(p.label_primary))
                                .child("试用面板"),
                        )
                        .child(
                            chrome_button(
                                "toggle-trial-panels",
                                if expanded { "收起面板" } else { "展开面板" },
                                t,
                            )
                            .aria_expanded(expanded)
                            .focusable()
                            .on_click(cx.listener(|chat, _, _, cx| {
                                chat.trial_panels_collapsed = !chat.trial_panels_collapsed;
                                cx.notify();
                            })),
                        ),
                )
                .when(expanded, |section| {
                    section.child(
                        div()
                            .id("conversation-trial-panels")
                            .max_h((height * 0.35).clamp(px(90.0), px(270.0)))
                            .overflow_y_scroll()
                            .child(self.plugin_panels(true, t, cx)),
                    )
                })
                .into_any_element(),
        )
    }

    fn plugin_panels(&self, trial_only: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = t.palette;
        let mut list = div().flex().flex_col().gap(px(8.0)).w_full();
        // 绘制快照时不持有 ChatState 锁，也不向插件请求渲染。
        for (index, panel) in self.adc_panels.iter().enumerate()
            .filter(|(_, panel)| !trial_only || panel.trial)
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
                        .child(if trial_only {
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
        let groups = workspace_sessions(&self.sessions, &query);
        let mut list = div().flex().flex_col().gap(px(8.0)).w_full();
        if groups.is_empty() {
            return list
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(8.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.label_secondary))
                        .child("没有匹配的会话"),
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
                                if !chat.folded_workspaces.remove(&key) {
                                    chat.folded_workspaces.insert(key.clone());
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
                for (index, session) in sessions {
                    group = group.child(
                        div()
                            .pl(px(12.0))
                            .child(self.session_row(index, session, t, cx)),
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
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let is_active = session.id == self.session_id;
        let is_renaming = self.renaming.as_deref() == Some(session.id.as_str());
        // 子会话按树深度缩进，父会话保持 0 级。
        let depth = self
            .session_depths
            .get(&session.id)
            .copied()
            .unwrap_or(0)
            .min(6);

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
            self.session_menu_trigger(index, session, t, cx)
        };
        div()
            .id(("session-row", index))
            .group("session-row")
            .role(Role::Button)
            .aria_label(session.title.clone())
            .w_full()
            .pl(px(10.0 + depth as f32 * 14.0))
            .pr(px(10.0))
            .min_h(px(32.0))
            .py(px(3.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(6.0))
            .rounded(t.radii.sm)
            .cursor_pointer()
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
                    .text_color(p.color(p.label_primary))
                    .child(if depth > 0 {
                        format!("↳ {}", session.title)
                    } else {
                        session.title.clone()
                    }),
            )
            .child(if is_pending_delete {
                actions
            } else {
                div()
                    .relative()
                    .min_w(px(40.0))
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
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_session(open_id.clone(), window, cx);
            }))
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

    fn toggle_turn_navigator(&mut self, cx: &mut Context<Self>) {
        self.composer_menu = None;
        self.turn_menu_open = !self.turn_menu_open;
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
                .child(
                    toolbar_icon_button(
                        "open-turn-navigator-rail",
                        "查看全部轮次",
                        "\u{e712}",
                        "⋯",
                        t,
                    )
                    .size(px(22.0))
                    .mt(px(2.0))
                    .aria_expanded(self.turn_menu_open)
                    .on_click(cx.listener(|chat, _, _, cx| chat.toggle_turn_navigator(cx))),
                )
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
        show_turn_rail: bool,
        show_window_bar: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let hero = self.show_empty_composer();
        let (theme_label, theme_glyph) = match self.theme_kind {
            ThemeKind::Light => ("切换到深色主题", "\u{e708}"),
            ThemeKind::Dark => ("切换到浅色主题", "\u{e706}"),
        };
        let sidebar_label = if collapsed {
            "展开侧栏"
        } else {
            "收起侧栏"
        };
        let panel_label = if self.plugins_open {
            "关闭插件面板"
        } else {
            "打开插件面板"
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
                    .child(
                        div().flex_1().min_w(px(0.0)).when(!hero, |title| {
                            title
                                .overflow_hidden()
                                .text_size(t.type_scale.body)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.color(p.label_primary))
                                .text_overflow(TextOverflow::Truncate("…".into()))
                                .child(self.active_title())
                        }),
                    )
                    .when(self.creator_mode && !hero, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .text_size(t.type_scale.secondary)
                                .text_color(p.color(p.label_secondary))
                                .child("创造模式"),
                        )
                    })
                    .when(self.busy && !compact, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .text_size(t.type_scale.small)
                                .text_color(p.color(p.label_secondary))
                                .child("生成中…"),
                        )
                    })
                    .child(
                        toolbar_icon_button(
                            "theme-toggle-header",
                            theme_label,
                            theme_glyph,
                            "◐",
                            t,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                    )
                    .child(
                        toolbar_icon_button(
                            "plugins-toggle-header",
                            panel_label,
                            "\u{e71d}",
                            "⋮",
                            t,
                        )
                        .aria_expanded(self.plugins_open)
                        .when(self.plugins_open, |button| {
                            button
                                .bg(p.color(p.business_tertiary))
                                .text_color(p.color(p.link))
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.toggle_plugins_panel(window, cx)
                            }),
                        ),
                    ),
            )
            .children((!hero).then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .children(
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
                    .when(
                        !show_turn_rail
                            && self.rows.iter().any(|row| matches!(row, Row::User { .. })),
                        |tabs| {
                            tabs.child(div().flex_1()).child(
                                chrome_button("open-turn-navigator", "轮次 ▾", t)
                                    .aria_label("轮次导航")
                                    .aria_expanded(self.turn_menu_open)
                                    .on_click(
                                        cx.listener(|chat, _, _, cx| {
                                            chat.toggle_turn_navigator(cx)
                                        }),
                                    ),
                            )
                        },
                    )
            }))
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
                    .when(open, |group| {
                        group.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .pl(px(10.0))
                                .border_l_1()
                                .border_color(p.color(p.border_l2))
                                .children(steps.iter().map(|row| {
                                    self.render_row(row, index, t, base_font, window, cx)
                                })),
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
                name,
                origin,
                status,
                detail,
                is_error,
                pending,
                card,
            } => {
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
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .rounded(t.radii.xs)
                                    .bg(p.color(p.tag))
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(p.label_tertiary))
                                    .child(origin.clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(t.type_scale.small)
                                    .text_color(p.color(status_color))
                                    .child(status.clone()),
                            ),
                    )
                    .when(!detail.is_empty(), |el| {
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
                Row::Process(steps) if self.open_processes.contains(&row_index) => {
                    26.0 + steps
                        .iter()
                        .map(|step| estimated_row_height(step) + 6.0)
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
                        .on_click(cx.listener(move |chat, _, window, cx| {
                            chat.choose_session_preset(preset.clone(), window, cx);
                        })),
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
            match &attachment.content {
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
                    match source? {
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
                    }
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
                let tile = match &file.content {
                    AttachmentContent::Media { format, bytes }
                        if *format != AttachmentFormat::Pdf =>
                    {
                        let (format, bytes, name) = (*format, bytes.clone(), file.name.clone());
                        attachment_image_button(
                            ("draft-image", index),
                            &file.name,
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
                        let label = match &file.content {
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
                                .child(file.name.clone()),
                        )
                    })
                    .child(
                        chrome_button(("remove-attachment", index), "×", t)
                            .h(px(24.0))
                            .aria_label(format!("移除附件 {}", file.name))
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
            .when_some(self.preset_notice.as_ref().filter(|_| hero), |composer, notice| {
                composer.child(
                    div()
                        .max_w(t.layout.composer_max_width)
                        .mt(px(6.0))
                        .text_size(t.type_scale.small)
                        .text_color(p.color(p.state_error))
                        .child(notice.clone()),
                )
            })
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

    fn empty_composer(&self, composer: AnyElement, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
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
                view.child(
                    div().flex().justify_center().mt(px(8.0)).child(
                        chrome_button("creator-runtime-settings", "安装创造模式构建组件", t)
                            .on_click(cx.listener(|chat, _, window, cx| {
                                chat.open_settings(settings_view::SettingsTab::Components, window, cx);
                            })),
                    ),
                )
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
        name: String,
        origin: String,
        status: String,
        detail: String,
        is_error: bool,
        pending: bool,
        /// 结构化卡片（Diff/Terminal 的原生视图从这里取正文）。
        card: Option<ToolCard>,
    },
    Meta(String),
    Stopped,
}

impl Row {
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
    for item in build_from_log(log) {
        let row = describe(&item);
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

fn record_reasoning(
    log: &mut SessionLog,
    timestamp: &str,
    reasoning: &mut String,
) -> Result<(), dshr_session_store::LogError> {
    if !reasoning.is_empty() {
        log.append(EventSpec::durable(
            "message.assistant.reasoning",
            timestamp,
            json!({ "text": std::mem::take(reasoning) }),
        ))?;
    }
    Ok(())
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
        let body_width = width - sidebar_width;
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
            body.child(self.header(
                &theme,
                collapsed,
                compact,
                show_turn_rail,
                show_window_bar,
                cx,
            ))
            .children(self.conversation_trial_panels(&theme, window.viewport_size().height, cx))
            // 启动/加载错误在 GUI 主界面可见（双击启动没有控制台可看 stderr）。
            .when(!self.plugin_errors.is_empty(), |el| {
                el.child(self.error_banner(&theme))
            })
            .when(!hero, |body| {
                body.child(self.transcript(&theme, show_turn_rail, window, cx))
            })
            .when_some(self.approval_bar(&theme, cx), |el, bar| el.child(bar))
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

fn selftest() -> i32 {
    // 不用系统 TEMP：本机 TEMP 对部分子进程不可写（见 docs/m0-runbook.md）。
    let path = PathBuf::from(".tmp/chat-selftest.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    state.submit("你好");
    state.submit("用工具试一下"); // sidecar

    // 注册表里应当同时有 native 与 sidecar 工具。
    let both_registered = state.plugins.registry().get("probe_native").is_some()
        && state.plugins.registry().get("probe_echo").is_some();

    // 模拟 Sidecar 崩溃：整代撤销，Native 必须仍然可用。
    let killed = state.kill_sidecar().is_some();
    let sidecar_tools_gone = state.plugins.registry().get("probe_echo").is_none()
        && state.plugins.registry().get("probe_native").is_some();
    state.submit("用原生工具");

    // 重启 Sidecar：新 generation 重新注册，sidecar 工具恢复。
    let restarted = state.restart_sidecar();
    let sidecar_tools_back = state.plugins.registry().get("probe_echo").is_some();
    state.submit("再次用工具");

    let items = state.items();
    println!("[selftest] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    // 4 轮 = 4 用户 + 4 助手 + 3 工具卡片（sidecar、native、sidecar）
    let cards_ok = matches!(&items[3], Item::Tool(t)
            if t.presentation.as_deref() == Some("generic")
                && t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic
                    && !card.detail.is_empty()))
        && matches!(&items[6], Item::Tool(t)
            if t.card.as_ref().is_some_and(|card| card.kind == CardKind::Generic));
    let ok = items.len() == 11
        && both_registered
        && killed
        && sidecar_tools_gone
        && restarted
        && sidecar_tools_back
        && cards_ok
        && matches!(&items[3], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[6], Item::Tool(t) if t.origin == "native"
            && t.state == ToolState::Settled { is_error: false })
        && matches!(&items[9], Item::Tool(t) if t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    // 重启：从磁盘重新加载，回放必须一致
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload saved session");
    let replayed = build_from_log(&reloaded);
    let same = replayed == items;
    println!(
        "[selftest] persisted={} replay_equal={same}",
        path.display()
    );

    // 切换时复用真实 Node generation；两个会话可各自调用同一个工具，并能回到旧会话继续。
    const RAW_ID: &str = "model-call-preserved";
    struct FixedToolIdProvider(bool);
    impl ModelProvider for FixedToolIdProvider {
        fn complete(&mut self, _: &CompletionRequest) -> Result<ModelReply, String> {
            self.0 = !self.0;
            if self.0 {
                Ok(ModelReply {
                    reasoning: String::new(),
                    text: String::new(),
                    tool_calls: vec![ModelToolCall {
                        id: RAW_ID.to_owned(),
                        name: "probe_echo".to_owned(),
                        arguments: json!({"text": "same model ID"}),
                    }],
                })
            } else {
                Ok(ModelReply::text_only("done"))
            }
        }
    }
    state.agent = Agent::new(Box::new(FixedToolIdProvider(false)));
    let generation = state.host.as_ref().map(|host| host.generation().to_owned());
    let timed_out = state.host.as_mut().is_some_and(|host| {
        matches!(
            host.invoke_as(RAW_ID, "probe_slow", json!({}), Duration::from_millis(50)),
            Err(dshr_compat_dsh_host::HostError::Timeout(_))
        )
    });
    let other_id = "s-selftest-other";
    let other_path = path.with_file_name(format!("{other_id}.jsonl"));
    reset_session_file(&other_path);
    let created = state.switch_session(other_path.clone(), other_id);
    state.submit("用工具试一下");
    let other_ok = matches!(state.items().as_slice(), [_, Item::Tool(t), _]
        if t.call_id == RAW_ID && t.origin == "sidecar" && t.state == ToolState::Settled { is_error: false });
    let started = std::time::Instant::now();
    let index_changed = state.switch_session(path, SESSION_ID);
    let switch_elapsed = started.elapsed();
    let restored = state.items() == items;
    state.submit("切回来再用工具");
    state.submit("下一轮复用同一个原始 ID");
    let resumed = matches!(state.items().get(items.len()..), Some([_, Item::Tool(a), _, _, Item::Tool(b), _])
        if [a, b].iter().all(|t| t.call_id == RAW_ID && t.state == ToolState::Settled { is_error: false }));
    let replayed_raw_ids = std::fs::read_to_string(&state.path).ok()
        .and_then(|text| SessionLog::from_jsonl(SESSION_ID, &text).ok())
        .is_some_and(|saved| {
            let history = history_from_log(&saved);
            history.iter().filter(|msg| matches!(msg, ChatMessage::Tool { call_id, .. } if call_id == RAW_ID)).count() == 2
                && history.iter().filter(|msg| matches!(msg, ChatMessage::Assistant { tool_calls, .. } if tool_calls.iter().any(|call| call.id == RAW_ID))).count() == 2
        });
    let reused = state
        .host
        .as_mut()
        .is_some_and(|host| !host.has_exited() && generation.as_deref() == Some(host.generation()));
    reset_session_file(&other_path);
    let workspace = tempfile::tempdir().expect("second probe workspace");
    let store = SessionIndex::new(workspace.path().join("sessions"));
    store
        .create_root(
            "s-other-root",
            &workspace.path().to_string_lossy(),
            &now_ts(),
        )
        .expect("create session in second workspace");
    let first_path = state.path.clone();
    state.switch_session(store.path_for("s-other-root"), "s-other-root");
    let used_second_runtime = state
        .host
        .as_ref()
        .is_some_and(|host| generation.as_deref() != Some(host.generation()));
    state.submit("用工具试一下");
    let second_completed = matches!(state.items().as_slice(), [_, Item::Tool(t), _]
        if t.origin == "sidecar" && t.state == ToolState::Settled { is_error: false });
    state.switch_session(first_path, SESSION_ID);
    let resumed_first_runtime = state
        .host
        .as_ref()
        .is_some_and(|host| generation.as_deref() == Some(host.generation()));
    let audited_binding = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.call" && event.payload["call_id"] == RAW_ID)
        .all(|event| {
            event.payload["generation"].as_str() == generation.as_deref()
                && event.payload["registration_id"].as_str().is_some()
        });
    let workspace_switch_ok = used_second_runtime && second_completed && resumed_first_runtime;
    println!(
        "[selftest] switch created={created} other_ok={other_ok} restored={restored} resumed={resumed} reused={reused} timed_out={timed_out} replayed_raw_ids={replayed_raw_ids} workspace_switch_ok={workspace_switch_ok} audited_binding={audited_binding} load={switch_elapsed:?}"
    );

    if ok
        && same
        && created
        && other_ok
        && !index_changed
        && restored
        && resumed
        && reused
        && timed_out
        && replayed_raw_ids
        && workspace_switch_ok
        && audited_binding
    {
        println!("[selftest] PASS");
        0
    } else {
        println!("[selftest] FAIL");
        1
    }
}

/// 用真实 Cordis 运行时跑一轮对话（`DSHR_SIDECAR=cordis`）。
fn selftest_cordis() -> i32 {
    let path = PathBuf::from(".tmp/chat-cordis.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    state.submit("用工具试一下");

    let items = state.items();
    println!("[selftest-cordis] items={}", items.len());
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let ok = items.len() == 3
        && matches!(&items[1], Item::Tool(t) if t.tool == "cordis_echo"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload cordis session"),
    );
    let same = replayed == items;
    println!("[selftest-cordis] replay_equal={same}");

    if ok && same {
        println!("[selftest-cordis] PASS");
        0
    } else {
        println!("[selftest-cordis] FAIL");
        1
    }
}

/// 验证普通桌面启动默认注册的官方 DSH 插件。
///
/// 装载的是**未修改的官方包**（`@deepseek-ai/dsh-tool-workspace-dependencies`
/// 与只读配置的 `@deepseek-ai/dsh-tool-fs`），通过 `tools` 服务的 `register`
/// 进入 Rust Registry，再按普通 sidecar 工具调用。
///
/// 依赖 `sidecar/dsh-official/node_modules`（`node tools/dsh-vendor.mjs` 从本机 DSH 抽取）。
/// 缺失时打印 SKIP 并返回 0：CI 环境不一定装了 DSH。
fn selftest_dsh() -> i32 {
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !root.join("sidecar/dsh-official/node_modules").exists() {
        println!("[selftest-dsh] SKIP: 先运行 `node tools/dsh-vendor.mjs` 准备官方插件依赖");
        return 0;
    }

    let fixture = tempfile::tempdir_in(data_root()).expect("create DSH selftest workspace");
    let path = fixture.path().join("session.jsonl");
    let contents = "dshr desktop sidecar read fixture";
    std::fs::write(fixture.path().join("read-test.txt"), contents).expect("write DSH fixture");
    std::env::set_var("DSHR_WORKSPACE", fixture.path());
    std::env::remove_var("DSHR_SIDECAR");
    prepare_desktop_sidecar(&Profile::defaults(fixture.path()));
    // 本自检只验证"只读官方插件 + 回放"；写工具由 --selftest-dsh-write 覆盖。
    std::env::set_var("DSHR_FS_WRITE", "0");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    // 官方插件经 ctx.tools.register 映射进同一个 Registry，origin 必须是 sidecar。
    let registered_workspace = state
        .plugins
        .registry()
        .get("load_workspace_dependencies")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");
    let registered_read = state
        .plugins
        .registry()
        .get("read")
        .is_some_and(|tool| tool.origin.kind() == "sidecar");

    state.submit("用工具试一下");

    let items = state.items();
    println!(
        "[selftest-dsh] items={} registered(workspace={registered_workspace}, read={registered_read})",
        items.len()
    );
    for item in &items {
        let row = describe(item);
        match row.sub_text() {
            Some(sub) => println!("  {}\n      ↳ {sub}", row.main_text()),
            None => println!("  {}", row.main_text()),
        }
    }

    let tool_ok = matches!(&items[1], Item::Tool(t)
        if t.tool == "load_workspace_dependencies"
            && t.origin == "sidecar"
            && t.state == ToolState::Settled { is_error: false });
    // 官方 tool 的真实返回值里必须有 python 路径（证明 execute 真的跑了）。
    let value_ok = matches!(&items[1], Item::Tool(t)
        if t.card.as_ref().is_some_and(|card| card.detail.contains("python")));

    // 直接调官方 read 工具：证明带依赖（fs/systemPrompt）的插件也能执行。
    let read = state.execute_tool(&ToolCallRequest {
        call_id: "selftest-read-1".to_owned(),
        tool: "read".to_owned(),
        arguments: json!({ "file_path": "read-test.txt" }),
    });
    let read_ok = !read.is_error && read.content.to_string().contains(contents);
    println!("[selftest-dsh] read_tool_ok={read_ok}");

    let replayed = build_from_log(
        &SessionLog::from_jsonl(
            SESSION_ID,
            &std::fs::read_to_string(&path).unwrap_or_default(),
        )
        .expect("reload dsh session"),
    );
    let same = replayed == items;
    println!("[selftest-dsh] replay_equal={same}");

    if registered_workspace && registered_read && tool_ok && value_ok && read_ok && same {
        println!("[selftest-dsh] PASS");
        0
    } else {
        println!("[selftest-dsh] FAIL");
        1
    }
}

/// 单轮真实调用（需要 API key）；用于人工验证 provider 与真流式，不进 CI。
fn live_turn(text: &str) -> i32 {
    let path = PathBuf::from(".tmp/chat-live.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_with_settings(
        path,
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        true,
        true,
        DesktopSettings::from_env(&configured_models()),
    );

    let started = std::time::Instant::now();
    let mut updates = 0usize;
    let mut first_text = None;
    let reply = state.stream_turn(text, &mut |rows| {
        updates += 1;
        if first_text.is_none() && rows.last().is_some_and(is_streaming_row) {
            first_text = Some(started.elapsed());
        }
    });
    let total = started.elapsed();
    let Some(reply) = reply else {
        println!("[live] empty prompt");
        return 1;
    };
    println!(
        "[live] stream_updates={updates} first_text_ms={} total_ms={}",
        first_text.map_or_else(|| "n/a".to_owned(), |d| d.as_millis().to_string()),
        total.as_millis()
    );
    println!("[live] reply={reply}");
    0
}

/// 流式自检：mock provider 把回复切成多段，断言"快照是增量推送的、落库只有一条 settled"。
fn selftest_stream() -> i32 {
    let path = PathBuf::from(".tmp/chat-stream.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let mut snapshots: Vec<Vec<Row>> = Vec::new();
    let reply = state.stream_turn("你好", &mut |rows| snapshots.push(rows));
    let items = state.items();

    let streaming: Vec<&Row> = snapshots
        .iter()
        .filter_map(|rows| rows.last())
        .filter(|row| is_streaming_row(row))
        .collect();
    let final_row = snapshots.last().and_then(|rows| rows.last());
    println!(
        "[stream] snapshots={} streaming_rows={}",
        snapshots.len(),
        streaming.len()
    );
    for row in &streaming {
        println!("  {}", row.main_text());
    }

    let reply = reply.unwrap_or_default();
    let ok = reply.contains("收到：你好")
        && snapshots.len() >= 3
        && streaming.len() >= 2
        // 流式文本单调增长：每个快照都比上一个长。
        && streaming.windows(2).all(|pair| {
            streaming_text(pair[0]).chars().count() < streaming_text(pair[1]).chars().count()
        })
        && items.len() == 2
        && matches!(&items[0], Item::User { .. })
        && matches!(&items[1], Item::Assistant { text } if text == &reply)
        && final_row.is_some_and(|row| {
            matches!(row, Row::Assistant(text) if text.contains(&reply) && !text.contains('▌'))
        });

    if ok {
        println!("[selftest-stream] PASS");
        0
    } else {
        println!("[selftest-stream] FAIL");
        1
    }
}

/// 上下文预算自检：塞一份超预算的历史，断言
/// (1) 写了 `context.measured` 事件；(2) 工具结果被裁剪或旧消息被丢弃；
/// (3) 实际发给模型的历史仍然是合法配对（不会以 Tool 开头）。
fn selftest_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    // 直接写日志模拟长会话：CJK 文本 + 大工具结果，确保超过 8000 的 80%。
    for index in 0..20 {
        let user = format!("历史问题 {index}：{}", "长内容".repeat(60));
        let _ = state.log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("h-u{index}"), "text": user }),
        ));
        let call_id = format!("h-c{index}");
        let _ = state.log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({ "call_id": call_id, "tool": "probe_echo", "arguments": {} }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": call_id, "is_error": false,
                "content": [{ "type": "text", "text": "输出".repeat(800) }]
            }),
        ));
        let _ = state.log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("h-a{index}"), "text": "收到" }),
        ));
    }
    state.save();

    let reply = state.stream_turn("继续", &mut |_| {});
    let measured = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.measured")
        .cloned();
    let Some(measured) = measured else {
        println!("[selftest-context] FAIL: 没有 context.measured 事件");
        return 1;
    };
    let ratio = measured
        .payload
        .get("ratio")
        .and_then(|value| value.as_f64())
        .unwrap_or(1.0);
    let pruned = measured
        .payload
        .get("pruned_tool_results")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let dropped = measured
        .payload
        .get("dropped_messages")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    // 压缩事件 + 摘要消息 + 重建出的 System 摘要都应存在。
    let compacted = state
        .log
        .events()
        .iter()
        .rev()
        .find(|event| event.event_type == "context.compacted")
        .cloned();
    let summary_id = compacted
        .as_ref()
        .and_then(|event| event.payload.get("summary_message_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let summary_event = state.log.events().iter().find(|event| {
        event.event_type == "message.assistant.settled"
            && event.payload.get("message_id").and_then(|v| v.as_str()) == Some(&summary_id)
    });
    let history = history_from_log(&state.log);
    let has_system_summary = history
        .iter()
        .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
    println!(
        "[context] ratio={ratio:.3} pruned={pruned} dropped={dropped} compacted={} summary_chars={:?} window={:?}",
        compacted.is_some(),
        summary_event
            .and_then(|event| event.payload.get("text"))
            .and_then(|value| value.as_str())
            .map(str::chars)
            .map(Iterator::count),
        measured.payload.get("window")
    );

    // 用同一份日志重建计划，断言配对合法且压缩后不再触发丢弃。
    let plan = dshr_agent_context::plan_context(&state.log, "");
    let legal = !matches!(
        plan.messages.first(),
        Some(dshr_agent_core::ChatMessage::Tool { .. })
    );

    let ok = reply.is_some()
        && ratio <= 0.8
        && compacted.is_some()
        && !summary_id.is_empty()
        && summary_event.is_some()
        && has_system_summary
        && legal
        && plan.dropped_messages == 0
        && plan.tokens_after <= plan.window;
    if ok {
        println!("[selftest-context] PASS");
        0
    } else {
        println!("[selftest-context] FAIL");
        1
    }
}

/// 长会话压缩自检（Gate 3）：连续多轮注入超预算历史，验证
/// (1) 每次超预算都累积一条 `context.compacted`（可叠加多次压缩）；
/// (2) 每轮压缩后 `context.measured.ratio ≤ 0.8`、不再丢消息、配对合法；
/// (3) 重新读 JSONL 的 UI 行与模型历史与在线日志逐行一致（回放一致）。
fn selftest_long_context() -> i32 {
    let path = PathBuf::from(".tmp/chat-long-context.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    std::env::set_var("DSHR_CONTEXT_WINDOW", "8000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let mut failures = 0usize;
    let mut compactions = 0usize;
    // 每轮写入的历史量是窗口的 2 倍以上，保证"每轮都触发一次压缩"。
    for round in 0..8 {
        for index in 0..20 {
            let user = format!("长会话第 {round}-{index} 轮：{}", "长内容".repeat(60));
            let _ = state.log.append(EventSpec::durable(
                "message.user",
                "t0",
                json!({ "message_id": format!("lc-u{round}-{index}"), "text": user }),
            ));
            let call_id = format!("lc-c{round}-{index}");
            let _ = state.log.append(EventSpec::durable(
                "tool.call",
                "t0",
                json!({ "call_id": call_id, "tool": "probe_echo", "arguments": {} }),
            ));
            let _ = state.log.append(EventSpec::durable(
                "tool.result",
                "t0",
                json!({
                    "call_id": call_id, "is_error": false,
                    "content": [{ "type": "text", "text": "输出".repeat(800) }]
                }),
            ));
            let _ = state.log.append(EventSpec::durable(
                "message.assistant.settled",
                "t0",
                json!({ "message_id": format!("lc-a{round}-{index}"), "text": "收到" }),
            ));
        }
        let reply = state.stream_turn("继续", &mut |_| {});
        if reply.is_none() {
            failures += 1;
        }

        let latest_compacted = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.compacted")
            .cloned();
        let Some(compacted) = latest_compacted else {
            println!("[long-context] round={round} FAIL: 没有 context.compacted");
            return 1;
        };
        compactions = state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == "context.compacted")
            .count();

        let measured_ratio = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("ratio"))
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0);
        let measured_window = state
            .log
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type == "context.measured")
            .and_then(|event| event.payload.get("window"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let plan = plan_context(&state.log, "");
        let legal = !matches!(plan.messages.first(), Some(ChatMessage::Tool { .. }));

        // 回放一致性：JSONL → 事件 → 行/历史，必须与在线状态逐项一致。
        let rebuilt =
            SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay log");
        let live_rows = rows_snapshot(&state.log, None);
        let replay_rows = rows_snapshot(&rebuilt, None);
        let rows_equal = live_rows.len() == replay_rows.len()
            && live_rows
                .iter()
                .zip(replay_rows.iter())
                .all(|(live, replay)| live.main_text() == replay.main_text());
        let live_history = history_from_log(&state.log);
        let replay_history = history_from_log(&rebuilt);
        let history_equal = live_history.len() == replay_history.len()
            && live_history
                .iter()
                .zip(replay_history.iter())
                .all(|(live, replay)| format!("{live:?}") == format!("{replay:?}"));
        let has_summary = live_history
            .iter()
            .any(|message| matches!(message, ChatMessage::System(text) if text.contains("摘要")));
        let compaction_rows = live_rows
            .iter()
            .filter(|row| row.main_text().contains("上下文已压缩"))
            .count();
        let ok = measured_ratio <= 0.8
            && measured_window > 0
            && plan.tokens_after <= plan.window
            && plan.dropped_messages == 0
            && legal
            && rows_equal
            && history_equal
            && has_summary
            // UI 只显示"最近一次压缩"标记（更早的压缩事件被后续压缩区间覆盖，属预期），
            // 一致性由 rows_equal/history_equal 保证。
            && compaction_rows >= 1
            && compacted.payload.get("tokens_after").is_some();
        if !ok {
            failures += 1;
        }
        println!(
            "[long-context] round={round} compactions={compactions} ratio={measured_ratio:.3} \
             tokens={}/{} dropped={} rows={} history={} rows_equal={rows_equal} \
             history_equal={history_equal} summary={has_summary} ok={ok}",
            plan.tokens_after,
            plan.window,
            plan.dropped_messages,
            live_rows.len(),
            live_history.len(),
        );
    }

    let ok = failures == 0 && compactions >= 5;
    if ok {
        println!("[selftest-long-context] PASS compactions={compactions}");
        0
    } else {
        println!("[selftest-long-context] FAIL failures={failures} compactions={compactions}");
        1
    }
}

/// 上下文外部化自检（PLAN §6.4）：超大工具结果 → 内容寻址对象 + 两条事件留痕；
/// 模型历史只剩引用 + 预览；对象可无损取回；重复计划不重复外部化；回放一致。
fn selftest_spill() -> i32 {
    let path = PathBuf::from(".tmp/chat-spill.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let spill_dir = PathBuf::from(".tmp/chat-spill-objects");
    let _ = std::fs::remove_dir_all(&spill_dir);
    std::env::set_var("DSHR_SPILL_DIR", &spill_dir);
    // 窗口放大，避免压缩/丢弃与外部化互相干扰；阈值用默认 16 KiB。
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let big = format!("{}结尾唯一标记XYZ", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "sp-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "sp-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();

    let plan = state.plan_with_spill();
    let created = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();
    let pruned = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "tool.result.pruned")
        .count();
    let refs = dshr_agent_context::spill::spill_refs_in_log(&state.log);
    let object = refs
        .first()
        .and_then(|spill| state.spill.get_verified(spill).ok());
    let lossless = object
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).contains("外部化内容"))
        .unwrap_or(false);
    // 模型看到的是 plan.messages（由 plan_context_with_spill 生成）；history_from_log
    // 不含外部化替换，不能用来断言"模型看到什么"。
    let history_text = format!("{:?}", plan.messages);
    let model_sees_reference = history_text.contains("工具结果已外部化");
    // 预览允许出现开头 512 字符；结尾标记只能由"完整内容泄漏"带进模型历史。
    let model_body_hidden = !history_text.contains("结尾唯一标记XYZ");

    // 重复计划：事件不重复，且第二次不再产生待外部化项。
    let plan_again = state.plan_with_spill();
    let created_again = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type == "spill.created")
        .count();

    // 回放一致：从 JSONL 重建后外部化已生效、模型历史条数一致。
    let rebuilt =
        SessionLog::from_jsonl(SESSION_ID, &state.log.to_jsonl()).expect("replay spill log");
    let policy = SpillPolicy::from_env();
    let replay_plan = plan_context_with_spill(&rebuilt, "", &policy);
    let live_plan = plan_context_with_spill(&state.log, "", &policy);
    let replay_consistent = replay_plan.spilled.is_empty()
        && replay_plan.messages.len() == live_plan.messages.len()
        && replay_plan.spilled_tool_results() == 0;

    let ok = created == 1
        && pruned == 1
        && refs.len() == 1
        && lossless
        && model_sees_reference
        && model_body_hidden
        && plan.spilled.is_empty()
        && plan_again.spilled.is_empty()
        && created_again == created
        && replay_consistent
        && plan.tokens_after < plan.tokens_before;
    println!(
        "[spill] created={created} pruned={pruned} objects={} lossless={lossless} \
         reference={model_sees_reference} hidden={model_body_hidden} \
         tokens={}->{} replay={replay_consistent} history_chars={}",
        refs.len(),
        plan.tokens_before,
        plan.tokens_after,
        history_text.chars().count(),
    );
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-spill] PASS");
        0
    } else {
        println!("[selftest-spill] FAIL");
        1
    }
}

/// Gate 4 证据（最关键分界线）：官方 `dsh-tool-fs` 的 write/edit/apply_patch 在
/// "Rust 审批 + 沙箱"约束下真正可用，官方插件源码一行未改。
///
/// 校验：能力协商后四个工具都注册 → write/edit/apply_patch 真实落盘 → 越界被拒 →
/// 审批 deny 时工具报错且文件不变 → 写前快照存在。
fn selftest_dsh_write() -> i32 {
    // 资源根按 exe 相对解析（打包后不依赖源码树）。
    let root = app_root();
    if !root.join("sidecar/dsh-official/node_modules").exists() {
        println!("[selftest-dsh-write] SKIP: 先运行 `node tools/dsh-vendor.mjs`");
        return 2;
    }
    let ws = std::path::absolute(".tmp/dsh-write-ws")
        .unwrap_or_else(|_| PathBuf::from(".tmp/dsh-write-ws"));
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::create_dir_all(&ws);
    std::env::set_var("DSHR_WORKSPACE", &ws);
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    // 用全新 profile：旧 profile 文件可能还留着 write:false 的历史默认值。
    let profile_dir = PathBuf::from(".tmp/profile-dsh-write");
    let _ = std::fs::remove_dir_all(&profile_dir);
    let profile = Profile::defaults(&profile_dir);
    std::env::set_var("DSHR_DSH_PLUGINS", profile.sidecar_json());

    let path = PathBuf::from(".tmp/chat-dsh-write.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let registered: Vec<String> = state
        .plugins
        .registry()
        .list()
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let has = |name: &str| registered.iter().any(|candidate| candidate == name);
    if !(has("write") && has("edit") && has("apply_patch") && has("read")) {
        println!("[dsh-write] registered={registered:?}");
        println!("[selftest-dsh-write] FAIL: 写工具未注册（host_caps/能力协商问题）");
        return 1;
    }

    let call = |tool: &str, arguments: serde_json::Value| ToolCallRequest {
        call_id: format!("dw-{tool}-{}", now_ts()),
        tool: tool.to_owned(),
        arguments,
    };
    let read_file = |name: &str| std::fs::read_to_string(ws.join(name)).unwrap_or_default();

    let write = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "hello dsh write" }),
    ));
    let wrote = !write.is_error && read_file("hello.txt") == "hello dsh write";

    let edit = state.execute_tool(&call(
        "edit",
        json!({ "file_path": "hello.txt", "old_string": "hello", "new_string": "hi" }),
    ));
    let edited = !edit.is_error && read_file("hello.txt") == "hi dsh write";

    let patch = state.execute_tool(&call(
        "apply_patch",
        json!({ "input": "*** Begin Patch\n*** Add File: notes.txt\n+patch line\n*** End Patch\n" }),
    ));
    let patched = !patch.is_error && read_file("notes.txt").contains("patch line");

    // 越界写：Rust 沙箱必须拒绝，且不落盘。
    let escaped = state.execute_tool(&call(
        "write",
        json!({ "file_path": "../escape.txt", "content": "nope" }),
    ));
    let escaped_rejected = escaped.is_error && !ws.join("../escape.txt").exists();

    // 审批拒绝：工具报错、文件内容不变。
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let denied = state.execute_tool(&call(
        "write",
        json!({ "file_path": "hello.txt", "content": "should not land" }),
    ));
    let denied_kept = denied.is_error && read_file("hello.txt") == "hi dsh write";

    let snapshots = std::fs::read_dir(ws.join(".dshr").join("snapshots"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    let ok = wrote && edited && patched && escaped_rejected && denied_kept && snapshots > 0;
    println!(
        "[dsh-write] registered={registered:?} write={wrote} edit={edited} patch={patched} \
         escape_rejected={escaped_rejected} deny_kept={denied_kept} snapshots={snapshots}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if ok {
        println!("[selftest-dsh-write] PASS");
        0
    } else {
        println!("[selftest-dsh-write] FAIL");
        1
    }
}

/// 审批自检：策略对全部工具 Ask，
/// (1) `DSHR_APPROVAL_MODE=deny`：工具不执行、落 approval 事件、工具结果为空错误；
/// (2) `DSHR_APPROVAL_MODE=allow`：审批通过、工具真正执行。
fn selftest_approval() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_TOOL_POLICY", "ask-all");

    let denied_path = PathBuf::from(".tmp/chat-approval-deny.jsonl");
    reset_session_file(&denied_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "deny");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut denied_state = ChatState::open_diagnostics(denied_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let denied_reply = denied_state.stream_turn("用工具试一下", &mut |_| {});

    let event_count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    // P1-3：审批 Deny 属于“工具执行失败”，必须写 tool.result(is_error=true)，
    // 而不是 tool.aborted（aborted 留给真正的取消/崩溃语义）。
    let denied_tool_result = denied_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(true)
    });
    let denied_tool_aborted = event_count(&denied_state, "tool.aborted") > 0;
    let asked = event_count(&denied_state, "approval.asked");
    let answered = event_count(&denied_state, "approval.answered");
    let deny_outcome = denied_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("deny")
    });

    let allowed_path = PathBuf::from(".tmp/chat-approval-allow.jsonl");
    reset_session_file(&allowed_path);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut allowed_state =
        ChatState::open_diagnostics(allowed_path, SESSION_ID, &cancel, ApprovalBroker::new());
    let allowed_reply = allowed_state.stream_turn("用工具试一下", &mut |_| {});
    let allowed_tool = allowed_state.log.events().iter().any(|event| {
        event.event_type == "tool.result"
            && event
                .payload
                .get("is_error")
                .and_then(|value| value.as_bool())
                == Some(false)
    });
    let allow_outcome = allowed_state.log.events().iter().any(|event| {
        event.event_type == "approval.answered"
            && event.payload.get("outcome").and_then(|v| v.as_str()) == Some("allow_once")
    });

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_TOOL_POLICY");

    println!(
        "[approval] asked={asked} answered={answered} denied_tool_result={denied_tool_result} \
         denied_tool_aborted={denied_tool_aborted} deny_outcome={deny_outcome} \
         allowed_tool={allowed_tool} allow_outcome={allow_outcome}"
    );
    let ok = denied_reply.is_some()
        && allowed_reply.is_some()
        && denied_tool_result
        && !denied_tool_aborted
        && asked == 1
        && answered == 1
        && deny_outcome
        && allowed_tool
        && allow_outcome;
    if ok {
        println!("[selftest-approval] PASS");
        0
    } else {
        println!("[selftest-approval] FAIL");
        1
    }
}

/// 损坏会话日志自检：加载失败必须**隔离原文件**并新建空会话，绝不能把原文件覆盖成
/// 1 行 `session.created`（review P1-1）。
fn selftest_corrupt_session() -> i32 {
    let root = PathBuf::from(".tmp/chat-corrupt-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);

    // 先造一份合法日志，再追加半截记录模拟中断写入。
    let valid = {
        let mut log = SessionLog::new("s-corrupt");
        let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
        log.to_jsonl()
    };
    let corrupt_bytes = format!("{valid}{{\"session_id\":\"s-corrupt\",\"seq\":2");
    let path = root.join("s-corrupt.jsonl");
    std::fs::write(&path, &corrupt_bytes).expect("write corrupt fixture");

    let cancel = Arc::new(AtomicBool::new(false));
    let state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-corrupt",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    // 原文件被隔离，字节完整保留。
    let quarantined: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|candidate| {
                    candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.contains(".corrupt-"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    let quarantined_bytes = quarantined
        .first()
        .and_then(|candidate| std::fs::read_to_string(candidate).ok())
        .unwrap_or_default();

    // 新会话文件存在且只含一条 session.created（未被损坏内容污染）。
    let fresh_text = std::fs::read_to_string(&path).unwrap_or_default();
    let fresh_ok = SessionLog::from_jsonl("s-corrupt", &fresh_text)
        .map(|log| log.events().len() == 1 && log.events()[0].event_type == "session.created")
        .unwrap_or(false);

    let reported = state
        .session_errors
        .iter()
        .any(|message| message.contains("会话日志损坏") && message.contains("隔离"));
    let preserved = quarantined_bytes == corrupt_bytes;

    println!(
        "[corrupt-session] quarantined={} preserved={preserved} fresh_ok={fresh_ok} reported={reported}",
        quarantined.len()
    );
    let ok = quarantined.len() == 1 && preserved && fresh_ok && reported;
    if ok {
        println!("[selftest-corrupt-session] PASS");
        0
    } else {
        println!("[selftest-corrupt-session] FAIL");
        1
    }
}

/// 故意在注册后失败的插件：宿主必须原子回滚，且仍能装载后续健康插件。
struct RecoveryFailingPlugin;

impl Plugin for RecoveryFailingPlugin {
    fn id(&self) -> &'static str {
        "recovery.failing"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_fail_tool",
                "should be rolled back",
                ToolOrigin::Native {
                    plugin_id: "recovery.failing".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Err(PluginError::Invalid("injected apply failure".to_owned()))
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        unreachable!("failing plugin is rolled back")
    }
}

/// 失败插件之后装载的健康插件。
struct RecoveryHealthyPlugin;

impl Plugin for RecoveryHealthyPlugin {
    fn id(&self) -> &'static str {
        "recovery.healthy"
    }

    fn apply(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_tool(
            ToolDefinition::new(
                "recovery_healthy_echo",
                "healthy plugin tool",
                ToolOrigin::Native {
                    plugin_id: "recovery.healthy".to_owned(),
                },
            )
            .with_schema(json!({ "type": "object" })),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        _tool: &str,
        _arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        Ok(json!({ "ok": true }))
    }
}

/// `--selftest-recovery`：G2 数据/插件失败恢复（无窗口）。
///
/// 1. 损坏会话：隔离原字节、生成干净 `session.created`、错误可见（复用
///    `--selftest-corrupt-session`）；
/// 2. 备份恢复：删掉主文件后 `.bak` 原子恢复，重放成功并可继续追加；
/// 3. 插件失败：`apply` 注册后失败被回滚，随后健康插件可装载/调用。
fn selftest_recovery() -> i32 {
    let corrupt_ok = selftest_corrupt_session() == 0;

    let root = PathBuf::from(".tmp/m4-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-recovery.jsonl");
    let mut log = SessionLog::new("s-recovery");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let first_len = log.events().len();
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());

    // 模拟 Windows 替换窗口：主文件缺失、`.bak` 是上一次成功版本。
    std::fs::remove_file(&path).expect("remove main log");
    let recovered = recover_backup(&path).unwrap_or(false);
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok());
    let backup_ok = recovered
        && replayed
            .as_ref()
            .map(|log| log.events().len() == first_len)
            .unwrap_or(false);
    let continue_ok = replayed.map_or(false, |mut log| {
        let _ = log.append(EventSpec::durable(
            "message.user",
            now_ts(),
            json!({ "text": "after recovery" }),
        ));
        save_atomic(&path, log.to_jsonl().as_bytes()).is_ok()
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl("s-recovery", &text).ok())
                .map(|log| log.events().len() == first_len + 1)
                .unwrap_or(false)
    });

    let mut host = PluginHost::new(AllowAll);
    let rollback_ok =
        host.load(Box::new(RecoveryFailingPlugin)).is_err() && host.registry().is_empty();
    let healthy_ok = host.load(Box::new(RecoveryHealthyPlugin)).is_ok();
    let continue_plugin_ok = host.call("recovery_healthy_echo", &json!({})).is_ok();

    let ok =
        corrupt_ok && backup_ok && continue_ok && rollback_ok && healthy_ok && continue_plugin_ok;
    println!(
        "[selftest-recovery] corrupt={corrupt_ok} backup={backup_ok} continue={continue_ok} plugin_rollback={rollback_ok} healthy={healthy_ok} healthy_call={continue_plugin_ok}"
    );
    if ok {
        println!("[selftest-recovery] PASS");
        0
    } else {
        println!("[selftest-recovery] FAIL");
        1
    }
}

/// `--selftest-crash-child <path>`：模拟进程在保存后崩溃（主文件缺失、`.bak` 完整），
/// 由父进程 `--selftest-crash-recovery` 验证恢复与继续追加。
fn selftest_crash_child(path: &str) -> i32 {
    let path = PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    let mut log = SessionLog::new("s-crash");
    let _ = log.append_created(now_ts(), &SessionCreated::root(String::from(".")));
    let _ = log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before crash" }),
    ));
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    // 第二次保存产生 `.bak`（上一版含 before crash），再删主文件模拟替换窗口崩溃。
    let _ = save_atomic(&path, log.to_jsonl().as_bytes());
    let _ = std::fs::remove_file(&path);
    std::process::abort();
}

/// `--selftest-crash-recovery`：真实子进程崩溃 → `.bak` 恢复 → 继续追加。
fn selftest_crash_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-crash-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-crash.jsonl");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            println!("[selftest-crash-recovery] FAIL: current_exe: {error}");
            return 1;
        }
    };
    let status = std::process::Command::new(exe)
        .arg("--selftest-crash-child")
        .arg(&path)
        .status();
    let crashed = status.map(|status| !status.success()).unwrap_or(false);
    let backup_exists = path.with_extension("jsonl.bak").exists();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-crash",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let before_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("before crash")
            })
        })
        .unwrap_or(false);
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after crash" }),
    ));
    state.save();
    let after_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok())
        .map(|log| {
            log.events().iter().any(|event| {
                event.event_type == "message.user"
                    && event.payload["text"].as_str() == Some("after crash")
            })
        })
        .unwrap_or(false);
    let ok = crashed && backup_exists && before_ok && after_ok;
    let notice_ok = state
        .session_errors
        .iter()
        .any(|message| message.starts_with("已恢复："));
    println!(
        "[selftest-crash-recovery] crashed={crashed} backup={backup_exists} before={before_ok} after={after_ok} notice={notice_ok}"
    );
    if ok && notice_ok {
        println!("[selftest-crash-recovery] PASS");
        0
    } else {
        println!("[selftest-crash-recovery] FAIL");
        1
    }
}

/// `--selftest-window-recovery`：G2 真窗口端到端。
///
/// 1. 用 `--selftest-crash-child` 制造真实崩溃现场：主文件缺失、`.bak` 完整；
/// 2. 启动 GUI 子进程（`DSHR_RECOVERY_FLOW=1`）：会话索引必须发现孤儿 `.bak`，
///    启动恢复后显示横幅，并在真实窗口里输入/发送 “after recovery”；
/// 3. 父进程校验子进程 exit 0，且最终日志同时含 “before crash” 与 “after recovery”。
fn selftest_window_recovery() -> i32 {
    let root = std::path::absolute(PathBuf::from(".tmp/m4-window-recovery"))
        .unwrap_or_else(|_| PathBuf::from(".tmp/m4-window-recovery"));
    let sessions = root.join("sessions");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&sessions) {
        eprintln!(
            "[selftest-window-recovery] FAIL: 创建临时目录 {} 失败: {error}",
            sessions.display()
        );
        return 1;
    }
    let path = sessions.join("s-crash.jsonl");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            println!("[selftest-window-recovery] FAIL: current_exe: {error}");
            return 1;
        }
    };

    // 真实子进程崩溃：退出码非 0，留下 `<path>.bak` 并删掉主文件。
    let crashed = std::process::Command::new(&exe)
        .arg("--selftest-crash-child")
        .arg(&path)
        .status()
        .map(|status| !status.success())
        .unwrap_or(false);
    let backup = path.with_extension("jsonl.bak");
    let backup_exists = backup.exists();

    // GUI 子进程：真实窗口 + 恢复后的继续输入/发送。stdout/stderr 继承，CI 日志可见。
    let child_status = std::process::Command::new(&exe)
        .arg("--selftest-window-recovery-child")
        .env("DSHR_HOME", &root)
        .env("DSHR_SESSIONS_DIR", &sessions)
        .env("DSHR_PROFILE_DIR", root.join("profile"))
        .env("DSHR_RECOVERY_FLOW", "1")
        .env("DSHR_FORCE_MOCK", "1")
        .status();
    let child_ok = child_status.map(|status| status.success()).unwrap_or(false);

    let log = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-crash", &text).ok());
    let before_ok = log.as_ref().is_some_and(|log| {
        log.events().iter().any(|event| {
            event.event_type == "message.user"
                && event.payload["text"].as_str() == Some("before crash")
        })
    });
    let after_ok = log.as_ref().is_some_and(|log| {
        log.events().iter().any(|event| {
            event.event_type == "message.user"
                && event.payload["text"].as_str() == Some("after recovery")
        })
    });

    println!(
        "[selftest-window-recovery] crashed={crashed} backup={backup_exists} child={child_ok} before={before_ok} after={after_ok}"
    );
    if crashed && backup_exists && child_ok && before_ok && after_ok {
        println!("[selftest-window-recovery] PASS");
        0
    } else {
        println!("[selftest-window-recovery] FAIL");
        1
    }
}

/// `--selftest-sidecar-recovery`：G2 的 sidecar 崩溃场景（headless）。
///
/// 先确认 sidecar 可调用，再 kill 子进程；断言会话仍可追加/保存、原生工具仍注册，
/// 且对已死 sidecar 的调用返回错误而不是挂起。
fn selftest_sidecar_recovery() -> i32 {
    let root = PathBuf::from(".tmp/m4-sidecar-recovery");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let path = root.join("s-sidecar.jsonl");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path.clone(),
        "s-sidecar",
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );

    let Some(mut host) = spawn_sidecar(None, None) else {
        println!("[selftest-sidecar-recovery] FAIL: sidecar 不可用（需要 node + sidecar/probe-*）");
        return 1;
    };
    let live = host
        .invoke(
            "probe_echo",
            json!({ "text": "live" }),
            Duration::from_secs(10),
        )
        .is_ok();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "before kill" }),
    ));
    state.save();
    state.host = Some(host);

    let killed = state
        .host
        .as_mut()
        .map(|host| host.kill().is_ok())
        .unwrap_or(false);
    let dead_call = state
        .host
        .as_mut()
        .map(|host| {
            host.invoke(
                "probe_echo",
                json!({ "text": "after" }),
                Duration::from_secs(2),
            )
            .is_err()
        })
        .unwrap_or(false);
    let native_ok = state.plugins.registry().get("probe_native").is_some();
    let _ = state.log.append(EventSpec::durable(
        "message.user",
        now_ts(),
        json!({ "text": "after kill" }),
    ));
    state.save();
    let replayed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-sidecar", &text).ok())
        .map(|log| log.events().len())
        .unwrap_or(0);
    let ok = live && killed && dead_call && native_ok && replayed >= 3;
    println!(
        "[selftest-sidecar-recovery] live={live} killed={killed} dead_call={dead_call} native={native_ok} replayed={replayed}"
    );
    if ok {
        println!("[selftest-sidecar-recovery] PASS");
        0
    } else {
        println!("[selftest-sidecar-recovery] FAIL");
        1
    }
}

/// 取消语义自检（P1-2）：turn 开始前置位 cancel → 必须走 TurnCancelled + session.interrupted，
/// 不出现“处理失败”，也不写空的 assistant 消息。
fn selftest_cancel() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    std::env::set_var("DSHR_FORCE_MOCK", "1");
    let path = PathBuf::from(".tmp/chat-cancel.jsonl");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path,
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    cancel.store(true, Ordering::Relaxed);
    let reply = state.stream_turn("你好", &mut |_| {});
    let interrupted = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");
    let failed = reply
        .as_deref()
        .map(|text| text.contains("处理失败"))
        .unwrap_or(false);
    let settled = state
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "message.assistant.settled");
    let tool_events = state
        .log
        .events()
        .iter()
        .filter(|event| event.event_type.starts_with("tool."))
        .count();
    println!(
        "[cancel] interrupted={interrupted} failed={failed} settled={settled} tool_events={tool_events}"
    );
    let ok = interrupted && !failed && !settled && tool_events == 0 && reply.as_deref() == Some("");
    if ok {
        println!("[selftest-cancel] PASS");
        0
    } else {
        println!("[selftest-cancel] FAIL");
        1
    }
}

/// 沙箱 seam 自检：原型写工具走"策略 Ask → 自动批准 → 沙箱写入/快照/路径拒绝"。
fn selftest_sandbox() -> i32 {
    let root =
        std::path::absolute(".tmp/sandbox-ws").unwrap_or_else(|_| PathBuf::from(".tmp/sandbox-ws"));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("DSHR_WORKSPACE", &root);
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let path = PathBuf::from(".tmp/chat-sandbox.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics(path, SESSION_ID, &cancel, ApprovalBroker::new());

    let write = |call_id: &str, relative: &str, content: &str| ToolCallRequest {
        call_id: call_id.to_owned(),
        tool: "probe_write".to_owned(),
        arguments: json!({ "path": relative, "content": content }),
    };
    let first = state.execute_tool(&write("c-w1", "demo.txt", "hello sandbox"));
    let second = state.execute_tool(&write("c-w2", "demo.txt", "v2"));
    let escaped = state.execute_tool(&write("c-w3", "../escape.txt", "nope"));

    let file = root.join("demo.txt");
    let content = std::fs::read_to_string(&file).unwrap_or_default();
    let snapshots = state
        .approvals
        .take_records()
        .into_iter()
        .filter(|record| record.tool == "probe_write")
        .count();
    let snapshot_files = std::fs::read_dir(root.join(".dshr").join("snapshots"))
        .map(|entries| entries.count())
        .unwrap_or(0);

    std::env::remove_var("DSHR_APPROVAL_MODE");
    std::env::remove_var("DSHR_WORKSPACE");

    println!(
        "[sandbox] write_ok={} overwrite_ok={} denied={} content={content:?} approval_records={snapshots} snapshot_files={snapshot_files} first_err={:?} second_err={:?}",
        !first.is_error,
        !second.is_error,
        escaped.is_error,
        first.content,
        second.content
    );
    let ok = !first.is_error
        && !second.is_error
        && escaped.is_error
        && content == "v2"
        && snapshots >= 2
        && snapshot_files >= 1;
    if ok {
        println!("[selftest-sandbox] PASS");
        0
    } else {
        println!("[selftest-sandbox] FAIL");
        1
    }
}

/// 插件 profile 自检：默认值、启停持久化、sidecar JSON、以及（有 vendored 依赖时）
/// 禁用 fs 后 read 工具消失、重新启用后回来。
fn selftest_profile() -> i32 {
    let _ = std::fs::create_dir_all(".tmp");
    let dir = PathBuf::from(".tmp/profile-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let mut profile = Profile::load(&dir).expect("load profile");
    let defaults_ok = profile.entries().len() == 2;
    let all_json = profile.sidecar_json();
    let disable_ok = profile.set_enabled("@deepseek-ai/dsh-tool-fs", false);
    if let Err(err) = profile.save() {
        println!("[selftest-profile] 保存失败: {err}");
        return 1;
    }
    let disabled_json = profile.sidecar_json();
    let reloaded = Profile::load(&dir).expect("reload profile");
    let persisted = reloaded
        .entries()
        .iter()
        .any(|entry| entry.name == "@deepseek-ai/dsh-tool-fs" && !entry.enabled);
    let json_ok = all_json.contains("dsh-tool-fs") && !disabled_json.contains("dsh-tool-fs");

    let vendored = PathBuf::from("sidecar/dsh-official/node_modules").exists();
    let mut sidecar_ok = true;
    if vendored {
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", disabled_json.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let without_fs = ChatState::open_diagnostics(
            PathBuf::from(".tmp/chat-profile-without-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_absent = without_fs.plugins.registry().get("read").is_none();
        let workspace_present = without_fs
            .plugins
            .registry()
            .get("load_workspace_dependencies")
            .is_some();

        let mut reopened = Profile::load(&dir).expect("reload");
        reopened.set_enabled("@deepseek-ai/dsh-tool-fs", true);
        let _ = reopened.save();
        std::env::set_var("DSHR_DSH_PLUGINS", reopened.sidecar_json());
        let with_fs = ChatState::open_diagnostics(
            PathBuf::from(".tmp/chat-profile-with-fs.jsonl"),
            SESSION_ID,
            &cancel,
            ApprovalBroker::new(),
        );
        let read_present = with_fs.plugins.registry().get("read").is_some();
        sidecar_ok = read_absent && workspace_present && read_present;
        println!(
            "[profile] vendored=true read_absent={read_absent} workspace_present={workspace_present} read_present={read_present}"
        );
    } else {
        println!("[profile] vendored=false（跳过 sidecar 端到端）");
    }

    let ok = defaults_ok && disable_ok && persisted && json_ok && sidecar_ok;
    println!(
        "[profile] defaults_ok={defaults_ok} disable_ok={disable_ok} persisted={persisted} json_ok={json_ok}"
    );
    if ok {
        println!("[selftest-profile] PASS");
        0
    } else {
        println!("[selftest-profile] FAIL");
        1
    }
}

/// Loader YAML 自检：bundle 条目 + patch 覆盖/插入 → 只读 profile。
fn selftest_loader() -> i32 {
    const BUNDLE: &str = r#"
- name: "@deepseek-ai/dsh-tool-workspace-dependencies"
  config:
    source: "C:/dsh/primary-runtime"
- id: fs-tools
  name: "@deepseek-ai/dsh-tool-fs"
  inject: [tools, fs, systemPrompt]
  config:
    read: true
    write: false
"#;
    const PATCH: &str = r#"
- id: fs-tools
  disabled: true
  config:
    read: true
    write: true
- insert:
    - name: "@deepseek-ai/dsh-tool-extra"
      config:
        limit: 3
"#;

    let dir = PathBuf::from(".tmp/loader-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let bundle = dir.join("cordis.yml");
    let patch = dir.join("cordis.patch.yml");
    if let Err(err) = std::fs::write(&bundle, BUNDLE) {
        println!("[selftest-loader] 写 bundle 失败: {err}");
        return 1;
    }
    if let Err(err) = std::fs::write(&patch, PATCH) {
        println!("[selftest-loader] 写 patch 失败: {err}");
        return 1;
    }

    let profile = match Profile::from_loader(&dir, &bundle, &[patch]) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-loader] 加载失败: {err}");
            return 1;
        }
    };
    let entries = profile.entries();
    let fs = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-fs");
    let extra = entries
        .iter()
        .find(|entry| entry.name == "@deepseek-ai/dsh-tool-extra");
    let json = profile.sidecar_json();

    let ok = profile.source() == ProfileSource::Loader
        && !profile.can_edit()
        && profile.save().is_err()
        && fs.is_some_and(|entry| !entry.enabled && entry.config["write"] == true)
        && extra.is_some()
        && !json.contains("dsh-tool-fs")
        && json.contains("dsh-tool-extra");
    println!(
        "[loader] entries={} fs_disabled={:?} extra={} json_has_fs={} read_only={}",
        entries.len(),
        fs.map(|entry| !entry.enabled),
        extra.is_some(),
        json.contains("dsh-tool-fs"),
        !profile.can_edit()
    );
    if ok {
        println!("[selftest-loader] PASS");
        0
    } else {
        println!("[selftest-loader] FAIL");
        1
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

/// 自检：可信安装端到端（首装确认、摘要校验、篡改拒绝、元数据/审计）。
fn selftest_trust_install() -> i32 {
    let root = PathBuf::from(".tmp/trust-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-trust] 创建目录失败");
        return 1;
    }
    let package_json = json!({ "name": "selftest.trust", "version": "0.1.0" });
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-trust] 计算摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": "selftest.trust",
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    // 1) 未确认 → 退出码 2 且未安装。
    let unconfirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let destination = sidecar.join("node_modules").join("selftest.trust");
    let rejected_unconfirmed = unconfirmed == 2 && !destination.exists();

    // 2) 确认 → 安装成功，元数据/摘要一致。
    let confirmed = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let meta = read_installed_meta(&destination);
    let installed_ok = confirmed == 0
        && meta
            .as_ref()
            .map(|meta| meta.digest_sha256 == digest)
            .unwrap_or(false);

    // 3) 篡改内容后重装 → 摘要不匹配被拒。
    let _ = std::fs::write(package.join("lib/index.js"), "malicious()\n");
    let tampered = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: true,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    );
    let tamper_rejected = tampered == 1;

    // 4) 审计：至少包含 accepted 与 reject 两种记录。
    let audit =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let accepted = audit.contains("\"decision\":\"accepted\"");
    let rejected = audit.contains("\"decision\":\"rejected\"");
    let signature_status = meta.map(|meta| meta.signature.status).unwrap_or_default();
    let ok = rejected_unconfirmed && installed_ok && tamper_rejected && accepted && rejected;
    println!(
        "[selftest-trust] rejected_unconfirmed={rejected_unconfirmed} installed_ok={installed_ok} tamper_rejected={tamper_rejected} audit_accepted={accepted} audit_rejected={rejected} signature_status={signature_status}"
    );
    if ok {
        println!("[selftest-trust] PASS");
        0
    } else {
        println!("[selftest-trust] FAIL");
        1
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

/// 自检：安装 → 卸载（目录/profile/审计）→ 密钥轮换（旧钥拒、新钥装）。
fn selftest_plugin_lifecycle() -> i32 {
    use ring::signature::KeyPair as _;

    let root = PathBuf::from(".tmp/m4-plugin-lifecycle");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(package.join("lib")).is_err() {
        eprintln!("[selftest-lifecycle] 创建目录失败");
        return 1;
    }
    let name = "m4.lifecycle";
    if std::fs::write(
        package.join("package.json"),
        json!({ "name": name, "version": "0.1.0" }).to_string(),
    )
    .is_err()
        || std::fs::write(package.join("lib/index.js"), "export const ok = true\n").is_err()
    {
        eprintln!("[selftest-lifecycle] 写包失败");
        return 1;
    }
    let Some(old_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成旧钥失败");
        return 1;
    };
    let Some(new_key) = ed25519_keypair() else {
        eprintln!("[selftest-lifecycle] 生成新钥失败");
        return 1;
    };

    // A. 首装（未签名 → --yes-install）→ 卸载。
    let digest_ok = package_digest(&package)
        .map(|digest| {
            let trust = json!({
                "schema_version": 1,
                "plugin_id": name,
                "version": "0.1.0",
                "package": { "digest_sha256": digest }
            });
            std::fs::write(package.join("dshr-trust.json"), trust.to_string()).is_ok()
        })
        .unwrap_or(false);
    let installed = digest_ok
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: true,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let destination = sidecar.join("node_modules").join(name);
    let installed_ok = installed && destination.exists();

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[selftest-lifecycle] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let uninstalled = uninstall_package(&sidecar, name, &mut profile);
    let audit_text =
        std::fs::read_to_string(profile_dir.join("install-audit.jsonl")).unwrap_or_default();
    let uninstall_ok = uninstalled
        .as_ref()
        .map(|report| report.removed_dir && report.removed_profile_entry)
        .unwrap_or(false)
        && !destination.exists()
        && !profile.entries().iter().any(|entry| entry.name == name)
        && audit_text.contains("\"action\":\"uninstall\"")
        && audit_text.contains("\"decision\":\"accepted\"");

    // B. 旧钥签名安装 → 轮换 → 旧钥被拒 → 新钥重签安装。
    let key_added = add_trust_key(
        &profile_dir,
        TrustKey::new("old", hex_lower(old_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let signed_old = sign_package_with_key(&package, name, "0.1.0", &old_key, "old").is_ok();
    let old_install = signed_old
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let rotated = rotate_trust_key(
        &profile_dir,
        "old",
        TrustKey::new("new", hex_lower(new_key.public_key().as_ref())),
        now_millis(),
    )
    .is_ok();
    let store_ok = TrustStore::load(&profile_dir)
        .map(|store| {
            store.get("old").is_some_and(|key| key.revoked)
                && store.get("new").is_some_and(|key| !key.revoked)
        })
        .unwrap_or(false);
    let old_rejected = install_plugin_with_options(
        &InstallCliOptions {
            package: package.clone(),
            yes_install: false,
            allow_downgrade: false,
        },
        &sidecar,
        &profile_dir,
    ) == 1;
    let signed_new = sign_package_with_key(&package, name, "0.1.0", &new_key, "new").is_ok();
    let new_install = signed_new
        && install_plugin_with_options(
            &InstallCliOptions {
                package: package.clone(),
                yes_install: false,
                allow_downgrade: false,
            },
            &sidecar,
            &profile_dir,
        ) == 0;
    let meta_ok = read_installed_meta(&destination)
        .map(|meta| {
            meta.signature.status == "verified" && meta.signature.key_id.as_deref() == Some("new")
        })
        .unwrap_or(false);
    let key_audit =
        std::fs::read_to_string(profile_dir.join("trust-key-audit.jsonl")).unwrap_or_default();
    let key_audit_ok = key_audit.contains("\"action\":\"add\"")
        && key_audit.contains("\"action\":\"rotate\"")
        && key_audit.contains("\"old_key_id\":\"old\"");

    let ok = installed_ok
        && uninstall_ok
        && key_added
        && old_install
        && rotated
        && store_ok
        && old_rejected
        && new_install
        && meta_ok
        && key_audit_ok;
    println!(
        "[selftest-lifecycle] installed={installed_ok} uninstalled={uninstall_ok} key_added={key_added} old_install={old_install} rotated={rotated} store_ok={store_ok} old_rejected={old_rejected} new_install={new_install} meta_ok={meta_ok} key_audit={key_audit_ok}"
    );
    if ok {
        println!("[selftest-lifecycle] PASS");
        0
    } else {
        println!("[selftest-lifecycle] FAIL");
        1
    }
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
            plugins.load(Box::new(plugin))
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

/// 自检：external-process 插件产品路径（安装 → profile 装载 → 工具调用 → 卸载 → 重载）。
/// external-process fixture 插件的一次性准备（复制 exe → 写 manifest/包摘要 → 信任安装）。
///
/// `--selftest-external-plugin` 与 `--selftest-adc-panel` 共用；失败已按 `tag` 前缀打印，
/// 调用方直接返回 1 即可。
struct FixturePluginSetup {
    profile: Profile,
    sidecar: PathBuf,
    installed: bool,
}

fn prepare_fixture_plugin(
    root: &std::path::Path,
    plugin_id: &str,
    tag: &str,
) -> Option<FixturePluginSetup> {
    let _ = std::fs::remove_dir_all(root);
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if std::fs::create_dir_all(&package).is_err() {
        eprintln!("[{tag}] 创建目录失败");
        return None;
    }
    let fixture_name = if cfg!(windows) {
        "plugin-fixture.exe"
    } else {
        "plugin-fixture"
    };
    let Some(fixture) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(fixture_name)))
    else {
        eprintln!("[{tag}] 无法定位 fixture 可执行文件");
        return None;
    };
    if !fixture.is_file() {
        eprintln!(
            "[{tag}] 缺少 {}；先运行 `cargo build -p dshr-plugin-host --bins`",
            fixture.display()
        );
        return None;
    }
    if std::fs::copy(&fixture, package.join(fixture_name)).is_err() {
        eprintln!("[{tag}] 复制 fixture 失败");
        return None;
    }
    let manifest = format!(
        r#"
[plugin]
id = "{plugin_id}"
version = "0.1.0"
api = "cordis-portable/v1"
description = "external-process selftest fixture"

[runtime]
kind = "external-process"
entry = "{fixture_name}"
max_memory_mb = 128
timeout_ms = 60000

[permissions]
workspace_read = false
workspace_write = false
network = false
"#
    );
    let package_json = json!({ "name": plugin_id, "version": "0.1.0" });
    let _ = std::fs::write(package.join("plugin.toml"), manifest);
    let _ = std::fs::write(package.join("package.json"), package_json.to_string());
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[{tag}] 计算包摘要失败");
        return None;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("[{tag}] 加载 profile 失败: {err}");
            return None;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(err) => {
            eprintln!("[{tag}] 安装失败（{}）: {err}", err.code());
            return None;
        }
    };
    let installed = install.install.destination.join("plugin.toml").is_file();
    Some(FixturePluginSetup {
        profile,
        sidecar,
        installed,
    })
}

fn selftest_external_plugin() -> i32 {
    let root = PathBuf::from(".tmp/external-plugin-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.external", "selftest-external")
    else {
        return 1;
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let approvals = ApprovalBroker::new();
    let mut state = ChatState::open_diagnostics_with_options(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        approvals,
        false,
        false,
    );
    let errors = load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-external] {error}");
        }
        println!("[selftest-external] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.external") {
        println!("[selftest-external] FAIL（插件未装载）");
        return 1;
    }
    let echo = state.plugins.call("echo", &json!({ "text": "hi" }));
    let echo_ok = echo
        .as_ref()
        .map(|value| value.to_string().contains("hi"))
        .unwrap_or(false);
    let unloaded = state.plugins.unload("selftest.external").is_ok();
    let reload_errors =
        load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    let reload_ok = reload_errors.is_empty()
        && state.plugins.is_loaded("selftest.external")
        && state
            .plugins
            .call("echo", &json!({ "text": "again" }))
            .map(|value| value.to_string().contains("again"))
            .unwrap_or(false);
    // 面板声明也随 ready 注册（ADC panel 的宿主侧元数据）。
    let panel_ok = state
        .plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == "selftest.external");
    let installed_ok = setup.installed;
    let ok = echo_ok && unloaded && reload_ok && panel_ok && installed_ok;
    println!(
        "[selftest-external] installed_ok={installed_ok} echo_ok={echo_ok} unloaded={unloaded} reload_ok={reload_ok} panel_ok={panel_ok}"
    );
    if ok {
        println!("[selftest-external] PASS");
        0
    } else {
        println!("[selftest-external] FAIL");
        1
    }
}

/// `--selftest-wasm`：WASM 插件的产品链路（review P1-1）。
///
/// 链路：临时 WASM 包（package.json + plugin.toml + plugin.wasm(WAT) + schemas +
/// `dshr-trust.json`）→ `install_package_trusted` → 从 profile 装载（走
/// `PluginHost::load_wasm_plugin`）→ 调用 `echo` → 渲染 `ui.render` panel →
/// `trap` 工具撞 fuel 看门狗 → 宿主存活（再 call echo）→ unload。
///
/// 包建在 data root 下（不依赖用户 `%TEMP%` 是否可写），断言不联网。
fn selftest_clipboard() -> i32 {
    application().run(|cx: &mut App| {
        let expected = "dshr clipboard selftest 中文";
        cx.write_to_clipboard(ClipboardItem::new_string(expected.to_owned()));
        let actual = cx.read_from_clipboard().and_then(|item| item.text());
        if actual.as_deref() != Some(expected) {
            eprintln!("[selftest-clipboard] FAIL: {actual:?}");
            cx.quit();
            std::process::exit(1);
        }
        println!("[selftest-clipboard] PASS text={expected:?}");
        cx.quit();
    });
    0
}

/// `--selftest-window`：真正打开一个 GPUI 窗口，等首帧后自动关闭。
/// 只验证窗口系统/事件循环，不依赖 sidecar 或 GPU 工具。
struct WindowProbe {
    focus: FocusHandle,
    key_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl WindowProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            key_hits: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl Render for WindowProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hits = self.key_hits.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(move |_event, _window, _cx| {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .child("window probe")
    }
}

fn selftest_window() -> i32 {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let probe = cx.new(WindowProbe::new);
                    let focus = probe.read(cx).focus.clone();
                    window.focus(&focus, cx);
                    probe
                },
            )
            .expect("open window");
        let hits = handle.read(cx).expect("probe").key_hits.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            // 不能在这个 update 里直接 dispatch（实体重入）；defer 到本帧更新之后。
            let scheduled = handle.update(cx, |_probe, window, cx| {
                window.defer(cx, |window, cx| {
                    window.dispatch_keystroke(
                        gpui::Keystroke::parse("enter").expect("enter keystroke"),
                        cx,
                    );
                });
            });
            if scheduled.is_err() {
                eprintln!("[selftest-window] FAIL: defer scheduling failed");
                cx.update(|cx| cx.quit());
                std::process::exit(1);
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            if hits.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                eprintln!("[selftest-window] FAIL: deferred keystroke not handled");
                cx.update(|cx| cx.quit());
                std::process::exit(1);
            }
            println!("[selftest-window] PASS window+keyboard");
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    0
}

/// `--selftest-cpp-component`：把真实 wasm32-wasip2 guest 写进临时包，
/// 经 manifest loader 注册 `echo` 并调用，验证产品侧 cpp-v1 装载路径。
fn selftest_cpp_component() -> i32 {
    const GUEST: &[u8] =
        include_bytes!("../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm");
    let root = data_root().join("selftests").join("cpp-component");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "[selftest-cpp-component] FAIL：创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    if let Err(error) = std::fs::write(root.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-component] FAIL：写 guest 失败: {error}");
        return 1;
    }
    let manifest_raw = serde_json::json!({
        "plugin": {
            "id": "selftest.component",
            "version": "0.1.0",
            "api": "cordis-portable/v1"
        },
        "runtime": {
            "kind": "wasm-component",
            "entry": "plugin.wasm",
            "timeout_ms": 30000
        },
        "tools": []
    })
    .to_string();
    let manifest = match Manifest::from_json_str(&manifest_raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("[selftest-cpp-component] FAIL：manifest: {error}");
            return 1;
        }
    };
    let mut plugins = PluginHost::new(AllowAll);
    if let Err(error) =
        plugins.load_cpp_component_plugin_from_manifest(&manifest, &root, &host_sdk_version())
    {
        eprintln!("[selftest-cpp-component] FAIL：装载: {error}");
        return 1;
    }
    if plugins.registry().get("echo").is_none() {
        eprintln!("[selftest-cpp-component] FAIL：guest 未注册 echo");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "hello" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-component] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"hello\"}" {
        eprintln!("[selftest-cpp-component] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-component] PASS registry=echo value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

/// `--selftest-cpp-install`：走真实 `install_package` 复制到 sidecar 目录，
/// 再从 profile 装载 cpp-v1 component，验证安装 → profile → loader 链路。
fn selftest_cpp_install() -> i32 {
    const GUEST: &[u8] =
        include_bytes!("../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm");
    let root = data_root().join("selftests").join("cpp-install");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(root.join("pkg")) {
        eprintln!("[selftest-cpp-install] FAIL：创建目录: {error}");
        return 1;
    }
    let package = root.join("pkg");
    if let Err(error) = std::fs::write(package.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-install] FAIL：写 guest: {error}");
        return 1;
    }
    let manifest_raw = r#"
[plugin]
id = "selftest.installed"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp component selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
"#;
    if let Err(error) = std::fs::write(package.join("plugin.toml"), manifest_raw) {
        eprintln!("[selftest-cpp-install] FAIL：写 plugin.toml: {error}");
        return 1;
    }
    let package_json = serde_json::json!({
        "name": "selftest.installed",
        "version": "0.1.0",
        "private": true
    })
    .to_string();
    if let Err(error) = std::fs::write(package.join("package.json"), package_json) {
        eprintln!("[selftest-cpp-install] FAIL：写 package.json: {error}");
        return 1;
    }
    let profile_dir = root.join("profile");
    let sidecar_dir = root.join("sidecar");
    if let Err(error) = std::fs::create_dir_all(&sidecar_dir) {
        eprintln!("[selftest-cpp-install] FAIL：创建 sidecar: {error}");
        return 1;
    }
    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-cpp-install] FAIL：profile: {error}");
            return 1;
        }
    };
    if let Err(error) = install_package(&sidecar_dir, &package, &mut profile) {
        eprintln!("[selftest-cpp-install] FAIL：安装: {error}");
        return 1;
    }
    let mut plugins = PluginHost::new(AllowAll);
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &sidecar_dir);
    if !errors.is_empty() {
        eprintln!("[selftest-cpp-install] FAIL：装载: {errors:?}");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "installed" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-install] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"installed\"}" {
        eprintln!("[selftest-cpp-install] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-install] PASS value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

/// `--selftest-cpp-cli-install`：走真实 `install_plugin_with_options`（信任策略 +
/// 预览确认 + profile 写入），再从 profile 装载并调用 component。
fn selftest_cpp_cli_install() -> i32 {
    const GUEST: &[u8] =
        include_bytes!("../../../../crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm");
    let root = data_root().join("selftests").join("cpp-cli-install");
    let _ = std::fs::remove_dir_all(&root);
    let package = root.join("pkg");
    if let Err(error) = std::fs::create_dir_all(&package) {
        eprintln!("[selftest-cpp-cli-install] FAIL：创建目录: {error}");
        return 1;
    }
    if let Err(error) = std::fs::write(package.join("plugin.wasm"), GUEST) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 guest: {error}");
        return 1;
    }
    let manifest_raw = r#"
[plugin]
id = "selftest.cli.installed"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp component CLI install selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
"#;
    if let Err(error) = std::fs::write(package.join("plugin.toml"), manifest_raw) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 plugin.toml: {error}");
        return 1;
    }
    let package_json = serde_json::json!({
        "name": "selftest.cli.installed",
        "version": "0.1.0",
        "private": true
    })
    .to_string();
    if let Err(error) = std::fs::write(package.join("package.json"), package_json) {
        eprintln!("[selftest-cpp-cli-install] FAIL：写 package.json: {error}");
        return 1;
    }
    let sidecar_dir = root.join("sidecar");
    let profile_dir = root.join("profile");
    let options = InstallCliOptions {
        package: package.clone(),
        yes_install: true,
        allow_downgrade: false,
    };
    if install_plugin_with_options(&options, &sidecar_dir, &profile_dir) != 0 {
        eprintln!("[selftest-cpp-cli-install] FAIL：install_plugin_with_options");
        return 1;
    }
    let profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-cpp-cli-install] FAIL：profile: {error}");
            return 1;
        }
    };
    let mut plugins = PluginHost::new(AllowAll);
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &sidecar_dir);
    if !errors.is_empty() {
        eprintln!("[selftest-cpp-cli-install] FAIL：装载: {errors:?}");
        return 1;
    }
    let value = match plugins.call_approved("echo", &serde_json::json!({ "text": "cli" })) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[selftest-cpp-cli-install] FAIL：调用: {error}");
            return 1;
        }
    };
    if value["content"][0]["text"] != "{\"text\":\"cli\"}" {
        eprintln!("[selftest-cpp-cli-install] FAIL：结果: {value}");
        return 1;
    }
    println!("[selftest-cpp-cli-install] PASS value={value}");
    let _ = std::fs::remove_dir_all(&root);
    0
}

fn selftest_wasm() -> i32 {
    let root = data_root().join("selftests").join("wasm-host");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "[selftest-wasm] FAIL：创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let package = root.join("pkg");
    let sidecar = root.join("sidecar");
    let profile_dir = root.join("profile");
    if let Err(error) = std::fs::create_dir_all(package.join("schemas")) {
        eprintln!("[selftest-wasm] FAIL：创建包目录失败: {error}");
        return 1;
    }
    let _ = std::fs::create_dir_all(&sidecar);

    // 1) WAT fixture：'e'=echo 回显；'u'=ui.render 返回 data 段元素树；其它（trap）=死循环。
    let wat = r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 1024) "{\"type\":\"Text\",\"props\":{\"text\":\"wasm\"}}")
  (global $heap (mut i32) (i32.const 4096))
  (func (export "dshr_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (local.get $ptr))
  (func (export "dshr_invoke") (param $ptr i32) (param $len i32) (result i64)
    (local $tag i32)
    (local.set $tag (i32.load8_u offset=9 (local.get $ptr)))
    (if (i32.eq (local.get $tag) (i32.const 117))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (i32.const 1024)) (i64.const 32))
        (i64.const 39)))))
    (if (i32.eq (local.get $tag) (i32.const 101))
      (then (return (i64.or
        (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
        (i64.extend_i32_u (local.get $len))))))
    (loop $spin (br $spin))
    (i64.const -1))
)
"#;
    let plugin_id = "selftest.wasm";
    let manifest = r#"
[plugin]
id = "selftest.wasm"
version = "0.1.0"
api = "cordis-portable/v1"
description = "wasm-component selftest fixture"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
fuel = 2000000
max_memory_mb = 32
timeout_ms = 30000

[[tools]]
name = "echo"
description = "echo the request JSON"
input_schema = "schemas/echo.input.json"
output_schema = "schemas/echo.output.json"

[[tools]]
name = "trap"
description = "spin until the fuel watchdog kills the invocation"
input_schema = "schemas/trap.input.json"
output_schema = "schemas/trap.output.json"

[[ui]]
slot = "workspace.panel"
id = "selftest.wasm.panel"
label = "WASM Panel"
view = "selftest.wasm.view"
location = "right"
"#;
    let writes = [
        (package.join("plugin.wasm"), wat.as_bytes().to_vec()),
        (package.join("plugin.toml"), manifest.as_bytes().to_vec()),
        (
            package.join("package.json"),
            json!({ "name": plugin_id, "version": "0.1.0" })
                .to_string()
                .into_bytes(),
        ),
    ];
    for (path, bytes) in writes {
        if let Err(error) = std::fs::write(&path, bytes) {
            eprintln!("[selftest-wasm] FAIL：写 {} 失败: {error}", path.display());
            return 1;
        }
    }
    for name in ["echo", "trap"] {
        for direction in ["input", "output"] {
            let schema = json!({ "type": "object" });
            if let Err(error) = std::fs::write(
                package.join(format!("schemas/{name}.{direction}.json")),
                schema.to_string(),
            ) {
                eprintln!("[selftest-wasm] FAIL：写 schema 失败: {error}");
                return 1;
            }
        }
    }

    // 2) 信任安装（与产品安装路径一致）。
    let Ok(digest) = package_digest(&package) else {
        eprintln!("[selftest-wasm] FAIL：计算包摘要失败");
        return 1;
    };
    let trust = json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "version": "0.1.0",
        "package": { "digest_sha256": digest }
    });
    let _ = std::fs::write(package.join("dshr-trust.json"), trust.to_string());

    let mut profile = match Profile::load(&profile_dir) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("[selftest-wasm] FAIL：加载 profile 失败: {error}");
            return 1;
        }
    };
    let policy = TrustPolicy {
        allow_downgrade: false,
        require_confirm_for_unsigned: true,
        store: TrustStore::default(),
        now_ms: None,
    };
    let install = match install_package_trusted(&sidecar, &package, &mut profile, &policy, |_| true)
    {
        Ok(report) => report,
        Err(error) => {
            eprintln!(
                "[selftest-wasm] FAIL：安装失败（{}）: {error}",
                error.code()
            );
            return 1;
        }
    };
    let installed_ok = install.install.destination.join("plugin.toml").is_file();

    // 3) 从 profile 装载（产品路径：loader 内部分发到 WASM 分支）。
    let mut plugins = PluginHost::new(ApprovalPolicy::desktop());
    let errors = load_enabled_external_plugins(&mut plugins, &profile, &sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-wasm] {error}");
        }
        println!("[selftest-wasm] FAIL（装载失败）");
        return 1;
    }
    let loaded = plugins.is_loaded(plugin_id);
    let echo_ok = plugins
        .call("echo", &json!({ "text": "wasm" }))
        .ok()
        .and_then(|value| {
            value
                .get("arguments")
                .and_then(|arguments| arguments.get("text"))
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some("wasm");
    let panel_ok = plugins
        .panels()
        .iter()
        .any(|panel| panel.plugin_id == plugin_id && panel.panel.id == "selftest.wasm.panel")
        && plugins
            .panels()
            .iter()
            .any(|panel| panel.panel.view_name() == "selftest.wasm.view");
    let render_ok = plugins
        .render_panel(plugin_id, "selftest.wasm.view")
        .map(|tree| tree["type"] == json!("Text"))
        .unwrap_or(false);

    // 4) trap：fuel 看门狗杀本次调用，宿主/插件存活（再 call echo 成功）。
    let trap = plugins.call("trap", &json!({}));
    let trap_isolated = trap
        .as_ref()
        .err()
        .map(|error| {
            let text = error.to_string();
            text.contains("E_QUOTA") || text.contains("E_TIMEOUT")
        })
        .unwrap_or(false);
    let survived = plugins
        .call("echo", &json!({ "text": "alive" }))
        .map(|value| value.to_string().contains("alive"))
        .unwrap_or(false);
    let unloaded = plugins.unload(plugin_id).is_ok() && !plugins.is_loaded(plugin_id);
    let ok = installed_ok
        && loaded
        && echo_ok
        && panel_ok
        && render_ok
        && trap_isolated
        && survived
        && unloaded;
    println!(
        "[selftest-wasm] installed_ok={installed_ok} loaded={loaded} echo_ok={echo_ok} panel_ok={panel_ok} render_ok={render_ok} trap_isolated={trap_isolated} survived={survived} unloaded={unloaded}"
    );
    let _ = std::fs::remove_dir_all(&root);
    if ok {
        println!("[selftest-wasm] PASS");
        0
    } else {
        println!("[selftest-wasm] FAIL");
        1
    }
}

/// `--selftest-adc-panel`：ADC panel 产品链路（无窗口，验证到 `AnyElement` 构造层）。
///
/// 链路：external fixture 装载 → `PluginHost::render_panel` → `adc_panel_body` →
/// GPUI 元素。断言：合法树 0 诊断、未知元素 → Placeholder + 诊断、未知 view /
/// 未知插件 / 不实现 view 都是可读错误、面板缓存可全部构造且无 panic。
fn selftest_adc_panel() -> i32 {
    let root = PathBuf::from(".tmp/adc-panel-selftest");
    let Some(setup) = prepare_fixture_plugin(&root, "selftest.adc", "selftest-adc-panel") else {
        return 1;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        root.join("session.jsonl"),
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        false,
        false,
    );
    let errors = load_enabled_external_plugins(&mut state.plugins, &setup.profile, &setup.sidecar);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("[selftest-adc-panel] {error}");
        }
        println!("[selftest-adc-panel] FAIL（装载失败）");
        return 1;
    }
    if !state.plugins.is_loaded("selftest.adc") {
        println!("[selftest-adc-panel] FAIL（插件未装载）");
        return 1;
    }
    // 装载后刷新面板缓存——产品面（右侧 panel）渲染读的就是这份缓存。
    state.refresh_adc_panels();
    let theme = Theme::light();
    let mut ok = true;

    // 1) 合法树：render_panel → adc_panel_body，0 诊断、元素构造成功。
    let legal_tree = match state.plugins.render_panel("selftest.adc", "fixture.adc") {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] render_panel 失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (legal_element, legal_diagnostics) = adc_panel_body(&theme, &legal_tree);
    drop(legal_element);
    let legal_ok = legal_diagnostics.is_empty();
    ok &= legal_ok;

    // 2) 未知元素（插件返回 Sparkline）：Placeholder + 诊断，不 panic。
    let unknown_tree = match state
        .plugins
        .render_panel("selftest.adc", "fixture.unknown")
    {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("[selftest-adc-panel] 未知元素 view 渲染失败: {error}");
            println!("[selftest-adc-panel] FAIL");
            return 1;
        }
    };
    let (unknown_element, unknown_diagnostics) = adc_panel_body(&theme, &unknown_tree);
    drop(unknown_element);
    let unknown_ok = unknown_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownElement);
    ok &= unknown_ok;

    // 3) 未知 view / 未知插件 / 不实现 view：可读错误，不 panic。
    let unknown_view_ok = state.plugins.render_panel("selftest.adc", "nope").is_err();
    let unknown_plugin_ok = state
        .plugins
        .render_panel("missing.plugin", "fixture.adc")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("missing.plugin"));
    let default_view_ok = state
        .plugins
        .render_panel("builtin.probe-write", "anything")
        .as_ref()
        .is_err_and(|error| error.to_string().contains("does not render views"));
    ok &= unknown_view_ok && unknown_plugin_ok && default_view_ok;

    // 4) 面板缓存：至少 2 条 workspace.panel（fixture 声明了 right/bottom 两条），
    //    每条都能构造元素；失败走错误占位（这里不应失败）。
    let panels = state.adc_panels.len();
    let mut rendered = 0usize;
    let mut panel_errors = 0usize;
    let mut diag_total = 0usize;
    for panel in &state.adc_panels {
        match &panel.tree {
            Some(tree) => {
                let (element, diagnostics) = adc_panel_body(&theme, tree);
                drop(element);
                diag_total += diagnostics.len();
                rendered += 1;
            }
            None => panel_errors += 1,
        }
    }
    let panels_ok = panels >= 2 && rendered == panels && panel_errors == 0;
    ok &= panels_ok;

    println!(
        "[selftest-adc-panel] panels={panels} rendered={rendered} panel_errors={panel_errors} legal_diags={} unknown_diag={unknown_ok} diag_total={diag_total} unknown_view={unknown_view_ok} unknown_plugin={unknown_plugin_ok} default_view={default_view_ok}",
        legal_diagnostics.len()
    );
    if ok {
        println!("[selftest-adc-panel] PASS");
        0
    } else {
        println!("[selftest-adc-panel] FAIL");
        1
    }
}

/// 自检用的"重复同一工具调用" provider：前 3 步都发同一个工具+参数，第 4 步收尾。
///
/// 只用于 `--selftest-defaults` 验证 `agent-core` 的重复工具软提醒阈值（3/5/8）。
struct RepeatProbeProvider;

impl ModelProvider for RepeatProbeProvider {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        let executed = request
            .messages
            .iter()
            .filter(|message| matches!(message, ChatMessage::Tool { .. }))
            .count();
        if executed >= 3 {
            return Ok(ModelReply::text_only("done"));
        }
        Ok(ModelReply {
            reasoning: String::new(),
            text: String::new(),
            tool_calls: vec![ModelToolCall {
                id: format!("repeat-{executed}"),
                name: "probe_repeat".to_owned(),
                arguments: json!({ "text": "same" }),
            }],
        })
    }
}

/// 自检用的执行器：无论调用什么都返回成功。
struct ProbeRepeatExecutor;

impl ToolExecutor for ProbeRepeatExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "probe_repeat".to_owned(),
            description: "repeat probe".to_owned(),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        ToolOutcome::success(
            json!([{ "type": "json", "value": { "ok": true } }]),
            json!({ "ok": true }),
        )
    }
}

/// `--selftest-defaults`：M3 默认差异接线的无网络自检。
///
/// 覆盖：approval 三种权限模式（`DSHR_PERMISSION_MODE`）、重复工具软提醒在真实 agent
/// 序列里触发并经 `tool.repeat` 落 durable 日志、sidecar per-tool timeout 往返、
/// retainRatio 与 reasoning effort 的 env 解析。全部断言走纯函数/内存日志，不联网。
fn selftest_defaults() -> i32 {
    let mut ok = true;

    // 1) approval 模式：env → ApprovalPolicy（缺省 workspace-write = 原 desktop 行为）。
    let write_tool = ToolDefinition::new(
        "write",
        "写文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("diff");
    let read_tool = ToolDefinition::new(
        "read",
        "读文件（自检探针）",
        ToolOrigin::Native {
            plugin_id: "selftest.defaults".to_owned(),
        },
    )
    .with_presentation("read");

    std::env::remove_var(PermissionMode::ENV);
    let default_mode_ok = PermissionMode::from_env() == PermissionMode::WorkspaceWrite;
    let default_policy_ok = matches!(
        ApprovalPolicy::from_env().check(&write_tool, &json!({})),
        PolicyDecision::Ask
    ) && matches!(
        ApprovalPolicy::from_env().check(&read_tool, &json!({})),
        PolicyDecision::Allow
    );

    std::env::set_var(PermissionMode::ENV, "read-only");
    let read_only_mode = PermissionMode::from_env();
    let read_only_policy = ApprovalPolicy::from_env();
    let read_only_ok = read_only_mode == PermissionMode::ReadOnly
        && matches!(
            read_only_policy.check(&write_tool, &json!({})),
            PolicyDecision::Deny(_)
        )
        && matches!(
            read_only_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );

    std::env::set_var(PermissionMode::ENV, "danger-full-access");
    let danger_mode = PermissionMode::from_env();
    let danger_policy = ApprovalPolicy::from_env();
    let danger_ok = danger_mode == PermissionMode::DangerFullAccess
        && matches!(
            danger_policy.check(&write_tool, &json!({})),
            PolicyDecision::Allow
        )
        && matches!(
            danger_policy.check(&read_tool, &json!({})),
            PolicyDecision::Allow
        );
    std::env::remove_var(PermissionMode::ENV);
    ok &= default_mode_ok && default_policy_ok && read_only_ok && danger_ok;

    // 2) 重复工具软提醒：默认阈值 3 在真实 agent 序列里触发，并落成 `tool.repeat`。
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_CONFIG_ENV);
    std::env::remove_var(dshr_agent_core::REPEAT_TOOL_REMINDER_THRESHOLDS_ENV);
    let mut agent = Agent::new(RepeatProbeProvider);
    let default_thresholds_ok = agent.repeat_reminder_config().enabled
        && agent.repeat_reminder_config().thresholds == vec![3, 5, 8];
    let mut executor = ProbeRepeatExecutor;
    let events = agent.run_turn("repeat", &mut executor);
    let tool_calls = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall(_)))
        .count();
    let mut log = SessionLog::new("s-selftest-defaults");
    let ts = now_ts();
    let _ = log.append(EventSpec::durable(
        "session.created",
        &ts,
        SessionCreated::root(".").payload(),
    ));
    let written = append_repeat_reminders(&mut agent, &mut log, &ts);
    let reminder = log
        .events()
        .iter()
        .find(|event| event.event_type == "tool.repeat");
    let repeat_event_ok = reminder.is_some_and(|event| {
        event.payload["tool"] == json!("probe_repeat")
            && event.payload["count"] == json!(3)
            && event.payload["threshold"] == json!(3)
            && event.payload["message"]
                .as_str()
                .is_some_and(|text| text.contains("重复工具调用提醒"))
    });
    let repeat_ok = default_thresholds_ok && tool_calls == 3 && written == 1 && repeat_event_ok;
    ok &= repeat_ok;

    // 3) sidecar per-tool timeout：`tool.register.timeout_ms` 往返 + 归一化。
    let registration = parse_tool_registration(
        &json!({"name": "bash", "description": "run", "timeout_ms": 5_000}),
        "dsh-official",
        "reg-selftest-1",
    );
    let definition = registration.to_tool_definition("gen-selftest");
    let timeout_ok = registration.timeout_ms == Some(5_000)
        && definition.timeout_ms == 5_000
        && definition.effective_timeout_ms() == 5_000
        && definition.origin.generation() == Some("gen-selftest")
        && parse_tool_registration(
            &json!({"name": "slow", "timeout_ms": 50_000_000u64}),
            "p",
            "r",
        )
        .timeout_ms
            == Some(600_000)
        && parse_tool_registration(&json!({"name": "read"}), "p", "r")
            .timeout_ms
            .is_none();
    ok &= timeout_ok;

    // 4) retainRatio：默认 DSH 0.16，env 可回退旧行为 0.6。
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    let retain_default = dshr_agent_context::retain_ratio_from_env();
    let retain_default_ok = (retain_default - dshr_agent_context::DSH_RETAIN_RATIO).abs() < 1e-6;
    std::env::set_var(dshr_agent_context::RETAIN_RATIO_ENV, "0.6");
    let retain_override = dshr_agent_context::retain_ratio_from_env();
    let retain_override_ok = (retain_override - 0.6).abs() < 1e-6;
    std::env::remove_var(dshr_agent_context::RETAIN_RATIO_ENV);
    ok &= retain_default_ok && retain_override_ok;

    // 5) reasoning effort：provider 构造时读 env，chat 不覆盖。
    std::env::remove_var(ReasoningEffort::ENV);
    let reasoning_unset_ok = DeepSeekProvider::new("selftest-key")
        .reasoning_effort()
        .is_none();
    std::env::set_var(ReasoningEffort::ENV, "high");
    let reasoning_high_ok = DeepSeekProvider::new("selftest-key").reasoning_effort()
        == Some(ReasoningEffort::High)
        && OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::High);
    std::env::set_var(ReasoningEffort::ENV, "max");
    let reasoning_max_ok =
        OpenAiProvider::new("selftest-key").reasoning_effort() == Some(ReasoningEffort::Max);
    std::env::remove_var(ReasoningEffort::ENV);
    ok &= reasoning_unset_ok && reasoning_high_ok && reasoning_max_ok;

    println!(
        "[selftest-defaults] approval_default={default_policy_ok} approval_read_only={read_only_ok} \
         approval_danger={danger_ok} repeat_reminders={tool_calls} tool_repeat_events={written} \
         repeat_event_ok={repeat_event_ok} timeout_roundtrip={timeout_ok} \
         retain_default={retain_default:.2} retain_env={retain_override:.2} \
         reasoning_env={reasoning_high_ok}"
    );
    if ok {
        println!("[selftest-defaults] PASS");
        0
    } else {
        println!("[selftest-defaults] FAIL");
        1
    }
}

/// 会话树产品面自检：spawn_child 真执行（子会话独立日志 + 父链接 + 父子事件）、
/// 默认单层（子会话不能再派发）、中断传播（interrupted_by_parent）、
/// 侧栏树顺序/深度、重启后 from_logs 重建。
fn selftest_tree() -> i32 {
    let dir = PathBuf::from(".tmp/tree-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let store = SessionIndex::new(&dir);
    let approvals = ApprovalBroker::new();
    let load = |id: &str| -> Option<SessionLog> {
        let meta = store.scan().ok()?.into_iter().find(|meta| meta.id == id)?;
        let text = std::fs::read_to_string(&meta.path).ok()?;
        SessionLog::from_jsonl(&meta.id, &text).ok()
    };

    // 1) 父会话派发一次子会话；子会话同步跑完一轮并回写父子日志。
    let cancel = Arc::new(AtomicBool::new(false));
    let mut parent = ChatState::open_diagnostics(
        dir.join("s-parent.jsonl"),
        "s-parent",
        &cancel,
        approvals.clone(),
    );
    let spawn = parent.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-1"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "你好，子会话只做一句话回答" }),
    });
    let child_id = spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let depth = spawn
        .value
        .get("depth")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let parent_created = parent.log.created().is_some();
    let parent_spawned = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.spawned");
    let parent_finished = parent
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "child.finished");
    let child_log = load(&child_id);
    let child_parent_link = child_log
        .as_ref()
        .and_then(|log| log.created())
        .and_then(|created| created.parent_session_id.clone())
        .as_deref()
        == Some("s-parent");
    let child_has_reply = child_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "message.assistant.settled")
    });
    let order = session_tree_order(&store);
    let parent_index = order.iter().position(|(id, _)| id == "s-parent");
    let child_index = order.iter().position(|(id, _)| id == &child_id);
    let tree_ok = depth == 1
        && child_parent_link
        && child_has_reply
        && parent_spawned
        && parent_finished
        && parent_created
        && order
            .iter()
            .any(|(id, level)| id == &child_id && *level == 1)
        && parent_index.is_some()
        && child_index.is_some()
        && parent_index < child_index;

    // 2) 子会话不再派发（默认单层）。
    let mut child_state = ChatState::open_diagnostics_with_options(
        store.path_for(&child_id),
        &child_id,
        &cancel,
        approvals.clone(),
        false,
        false,
    );
    let nested = child_state.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-nested"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "孙会话" }),
    });
    let nested_rejected = nested.is_error;

    // 2b) 产品链路：mock 模型选择 spawn_child → 执行器派发 → 子会话回写 → 模型拿到结果。
    let cancel3 = Arc::new(AtomicBool::new(false));
    let mut parent3 = ChatState::open_diagnostics(
        dir.join("s-parent3.jsonl"),
        "s-parent3",
        &cancel3,
        approvals.clone(),
    );
    let loop_reply = parent3.stream_turn("请派发一个子会话，让它回答：子会话你好", &mut |_| {});
    let has_event =
        |log: &SessionLog, kind: &str| log.events().iter().any(|event| event.event_type == kind);
    let loop_ok = loop_reply
        .as_deref()
        .is_some_and(|reply| !reply.is_empty() && !reply.starts_with("处理失败"))
        && parent3.log.events().iter().any(|event| {
            event.event_type == "tool.call"
                && event.payload.get("tool").and_then(|value| value.as_str())
                    == Some(SPAWN_CHILD_TOOL)
        })
        && has_event(&parent3.log, "child.spawned")
        && has_event(&parent3.log, "tool.result")
        && has_event(&parent3.log, "child.finished");

    // 3) 中断传播：父取消后派发 → 父 session.interrupted + 子 interrupted_by_parent。
    let cancel2 = Arc::new(AtomicBool::new(false));
    let mut parent2 = ChatState::open_diagnostics(
        dir.join("s-parent2.jsonl"),
        "s-parent2",
        &cancel2,
        approvals.clone(),
    );
    cancel2.store(true, Ordering::Relaxed);
    let interrupted_spawn = parent2.execute_tool(&ToolCallRequest {
        call_id: String::from("tree-spawn-interrupted"),
        tool: SPAWN_CHILD_TOOL.to_owned(),
        arguments: json!({ "prompt": "中断测试" }),
    });
    let child2_id = interrupted_spawn
        .value
        .get("child_session_id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let outcome_interrupted = interrupted_spawn
        .value
        .get("outcome")
        .and_then(|value| value.as_str())
        == Some("interrupted");
    let child2_log = load(&child2_id);
    let child_interrupted = child2_log.as_ref().is_some_and(|log| {
        log.events()
            .iter()
            .any(|event| event.event_type == "session.interrupted_by_parent")
    });
    let parent_interrupted = parent2
        .log
        .events()
        .iter()
        .any(|event| event.event_type == "session.interrupted");

    // 4) 重启恢复：从磁盘日志重建运行态树，深度与中断状态一致。
    let logs: Vec<SessionLog> = store
        .scan()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|meta| {
            std::fs::read_to_string(&meta.path)
                .ok()
                .and_then(|text| SessionLog::from_jsonl(&meta.id, &text).ok())
        })
        .collect();
    let rebuilt = SessionTreeRuntime::from_logs(logs, DEFAULT_MAX_DEPTH);
    let (rebuilt_depth, rebuilt_interrupted, replay_equal) = match &rebuilt {
        Ok(runtime) => (
            runtime.depth(&child_id),
            runtime
                .session(&child2_id)
                .map(|session| session.status.is_interrupted())
                .unwrap_or(false),
            // 父日志回放：新事件不影响对话模型构建（未知事件忽略）。
            child_log.as_ref().is_some_and(|log| {
                let jsonl = log.to_jsonl();
                SessionLog::from_jsonl(log.session_id(), &jsonl)
                    .map(|reloaded| reloaded.len() == log.len())
                    .unwrap_or(false)
            }),
        ),
        Err(_) => (None, false, false),
    };
    let rebuilt_ok = rebuilt_depth == Some(1) && rebuilt_interrupted && replay_equal;

    let ok = !spawn.is_error
        && !child_id.is_empty()
        && tree_ok
        && nested_rejected
        && loop_ok
        && outcome_interrupted
        && child_interrupted
        && parent_interrupted
        && rebuilt_ok;
    println!(
        "[tree] child={child_id} depth={depth} tree_ok={tree_ok} nested_rejected={nested_rejected} \
         loop_ok={loop_ok} interrupted={outcome_interrupted}/{child_interrupted}/{parent_interrupted} \
         rebuilt_ok={rebuilt_ok} parent_created={parent_created} spawned={parent_spawned} \
         finished={parent_finished} link={child_parent_link} reply={child_has_reply} \
         order={:?}",
        order
            .iter()
            .map(|(id, level)| format!("{id}:{level}"))
            .collect::<Vec<_>>()
    );
    if ok {
        println!("[selftest-tree] PASS");
        0
    } else {
        println!("[selftest-tree] FAIL");
        1
    }
}

/// 导出/保留自检：JSONL + 外部化附件 + manifest sha256，导出后可自校验；
/// CLI 入口 `--export-session` 也走同一路径。
fn selftest_export() -> i32 {
    let root = PathBuf::from(".tmp/export-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let sessions = root.join("sessions");
    let _ = std::fs::create_dir_all(&sessions);
    std::env::set_var("DSHR_SESSIONS_DIR", &sessions);
    std::env::set_var("DSHR_SPILL_DIR", root.join("spill"));
    std::env::set_var("DSHR_CONTEXT_WINDOW", "200000");

    let cancel = Arc::new(AtomicBool::new(false));
    let path = sessions.join("s-export.jsonl");
    let mut state = ChatState::open_diagnostics(path.clone(), "s-export", &cancel, ApprovalBroker::new());
    let big = format!("{}导出附件标记", "外部化内容".repeat(6_000));
    let _ = state.log.append(EventSpec::durable(
        "tool.call",
        "t0",
        json!({ "call_id": "ex-c1", "tool": "probe_echo", "arguments": {} }),
    ));
    let _ = state.log.append(EventSpec::durable(
        "tool.result",
        "t0",
        json!({
            "call_id": "ex-c1", "is_error": false,
            "content": [{ "type": "text", "text": big }]
        }),
    ));
    state.save();
    let _ = state.plan_with_spill();

    let out = root.join("out");
    let manifest = match export_session(
        &state.log,
        &SpillStoreSource::new(&state.spill),
        &out,
        &ExportOptions::new(now_ts()),
    ) {
        Ok(manifest) => manifest,
        Err(err) => {
            println!("[selftest-export] FAIL: 导出失败 {err}");
            return 1;
        }
    };
    let verified = match verify_export(&out, "s-export") {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-export] FAIL: 自校验失败 {err}");
            return 1;
        }
    };
    let exported_jsonl = out.join(&manifest.jsonl.path);
    let reloaded = std::fs::read_to_string(&exported_jsonl)
        .ok()
        .and_then(|text| SessionLog::from_jsonl("s-export", &text).ok());
    let reload_equal = reloaded
        .as_ref()
        .map(|log| log.len() == state.log.len())
        .unwrap_or(false);
    // CLI 入口同样可复现（导出 + 自校验）。
    let cli_out = root.join("out-cli");
    let cli_ok = export_session_cli("s-export", Some(cli_out)) == 0;

    let ok = manifest.event_count == state.log.len()
        && manifest.attachments.len() == 1
        && manifest.missing_blobs.is_empty()
        && verified.is_ok()
        && verified.attachments_checked == 1
        && reload_equal
        && cli_ok
        && manifest.jsonl.size > 0
        && !manifest.jsonl.sha256.is_empty();
    println!(
        "[export] events={} attachments={} missing={} bytes={} verify={} reload_equal={reload_equal} cli={cli_ok}",
        manifest.event_count,
        manifest.attachments.len(),
        manifest.missing_blobs.len(),
        manifest.total_bytes,
        verified.is_ok()
    );
    // 保留计划：目录里只有本会话、未超限 → 不需要删除。
    let plan_code = retention_plan_cli();
    let ok = ok && plan_code == 0;
    std::env::remove_var("DSHR_SPILL_DIR");
    std::env::remove_var("DSHR_CONTEXT_WINDOW");
    if ok {
        println!("[selftest-export] PASS");
        0
    } else {
        println!("[selftest-export] FAIL");
        1
    }
}

/// 安装流程自检：本地包复制 + profile 追加 + 重启后可读。
fn selftest_install() -> i32 {
    let root = PathBuf::from(".tmp/install-selftest");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(root.join("sidecar"));
    let package = root.join("pkg");
    let _ = std::fs::create_dir_all(package.join("lib"));
    let _ = std::fs::write(
        package.join("package.json"),
        json!({ "name": "@deepseek-ai/dsh-tool-selftest", "version": "1.0.0" }).to_string(),
    );
    let _ = std::fs::write(package.join("lib/index.js"), "export const ok = true\n");

    let mut profile = match Profile::load(root.join("profile")) {
        Ok(profile) => profile,
        Err(err) => {
            println!("[selftest-install] 加载 profile 失败: {err}");
            return 1;
        }
    };
    let report = match install_package(&root.join("sidecar"), &package, &mut profile) {
        Ok(report) => report,
        Err(err) => {
            println!("[selftest-install] 安装失败: {err}");
            return 1;
        }
    };
    let copied = report.destination.join("lib/index.js").exists();
    let reloaded = Profile::load(root.join("profile")).ok();
    let in_profile = reloaded.is_some_and(|profile| {
        profile
            .entries()
            .iter()
            .any(|entry| entry.name == "@deepseek-ai/dsh-tool-selftest" && entry.enabled)
    });

    println!(
        "[install] name={} copied={copied} profile_added={} in_profile={in_profile}",
        report.name, report.added_to_profile
    );
    let ok = copied && report.added_to_profile && in_profile;
    if ok {
        println!("[selftest-install] PASS");
        0
    } else {
        println!("[selftest-install] FAIL");
        1
    }
}
/// 重试/故障注入自检：每第 3 次请求返回 503，20 轮工具调用全部应成功。
struct FlakyProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail_every: usize,
}

impl ModelProvider for FlakyProvider {
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<dshr_agent_core::ModelReply, String> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if self.fail_every > 0 && call.is_multiple_of(self.fail_every) {
            return Err(String::from("HTTP 503 temporarily unavailable"));
        }
        // 第二轮（已看到工具结果）返回文本收尾；第一轮返回一次工具调用。
        let saw_tool_result = matches!(request.messages.last(), Some(ChatMessage::Tool { .. }));
        if saw_tool_result {
            Ok(dshr_agent_core::ModelReply {
                reasoning: String::new(),
                text: format!("ok-{call}"),
                tool_calls: Vec::new(),
            })
        } else {
            Ok(dshr_agent_core::ModelReply {
                reasoning: String::new(),
                text: String::new(),
                tool_calls: vec![dshr_agent_core::ModelToolCall {
                    id: format!("call-{call}"),
                    name: String::from("probe_native"),
                    arguments: json!({ "text": "ping" }),
                }],
            })
        }
    }
}

struct CountingExecutor {
    executed: usize,
}

impl ToolExecutor for CountingExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: String::from("probe_native"),
            description: String::from("自检用"),
            parameters: json!({ "type": "object" }),
        }]
    }

    fn execute(&mut self, _call: &ToolCallRequest) -> ToolOutcome {
        self.executed += 1;
        ToolOutcome::success(
            json!([{ "type": "text", "text": "ok" }]),
            json!({ "ok": true }),
        )
    }
}

fn selftest_retry() -> i32 {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flaky = FlakyProvider {
        calls: calls.clone(),
        fail_every: 3,
    };
    let mut agent = Agent::new(RetryingProvider::new(
        Box::new(flaky),
        RetryPolicy::immediate(4),
    ));
    let mut executor = CountingExecutor { executed: 0 };
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for _ in 0..20 {
        let events = agent.run_turn("继续", &mut executor);
        if events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed(_)))
        {
            failed += 1;
        } else {
            succeeded += 1;
        }
    }
    let total_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "[retry] turns=20 succeeded={succeeded} failed={failed} tool_executions={} \
         provider_calls={total_calls} (fault every 3rd call, max attempts 4)",
        executor.executed
    );
    let ok = failed == 0 && succeeded == 20 && executor.executed == 20 && total_calls > 40;
    if ok {
        println!("[selftest-retry] PASS");
        0
    } else {
        println!("[selftest-retry] FAIL");
        1
    }
}

/// 10k 事件性能基线：冷加载 + 回放 + 快照 + 历史重建（M2 gate 2 的数据层）。
/// 生成 10k 事件会话（2500 轮 user/assistant/tool.call/tool.result），供性能自检与帧探针复用。
fn build_perf_log(session_id: &str) -> SessionLog {
    let mut log = SessionLog::new(session_id);
    for index in 0..2_500 {
        let user = format!("第 {index} 轮问题：帮我检查一下这个方案的风险点与回滚方式。");
        let assistant = format!("第 {index} 轮回答：已完成检查，主要风险在并发写入与持久化顺序。");
        log.append(EventSpec::durable(
            "message.user",
            "t0",
            json!({ "message_id": format!("u{index}"), "text": user }),
        ))
        .expect("append user");
        log.append(EventSpec::durable(
            "message.assistant.settled",
            "t0",
            json!({ "message_id": format!("a{index}"), "text": assistant }),
        ))
        .expect("append assistant");
        log.append(EventSpec::durable(
            "tool.call",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "tool": "read",
                "arguments": { "path": "README.md" }
            }),
        ))
        .expect("append tool.call");
        log.append(EventSpec::durable(
            "tool.result",
            "t0",
            json!({
                "call_id": format!("c{index}"),
                "is_error": false,
                "content": [{ "type": "text", "text": "ok" }]
            }),
        ))
        .expect("append tool.result");
    }
    log
}

/// 把 10k 事件会话写进会话目录，返回会话 id（供窗口帧探针/截图复用）。
fn seed_perf_session(dir: &std::path::Path) -> i32 {
    let store = SessionIndex::new(dir);
    if let Err(err) = store.ensure_dir() {
        eprintln!("[seed] 无法创建会话目录: {err}");
        return 1;
    }
    let id = store.new_id();
    if let Err(err) = store.create(&id) {
        eprintln!("[seed] 无法创建会话: {err}");
        return 1;
    }
    let log = build_perf_log(&id);
    let jsonl = log.to_jsonl();
    let path = store.path_for(&id);
    if let Err(err) = std::fs::write(&path, jsonl.as_bytes()) {
        eprintln!("[seed] 写入会话失败: {err}");
        return 1;
    }
    println!(
        "[seed] id={id} events={} bytes={} path={}",
        log.len(),
        jsonl.len(),
        path.display()
    );
    0
}

fn selftest_perf() -> i32 {
    let log = build_perf_log("s-perf");
    assert_eq!(log.len(), 10_000);

    let jsonl = log.to_jsonl();
    let bytes = jsonl.len();
    let started = std::time::Instant::now();
    let reloaded = SessionLog::from_jsonl("s-perf", &jsonl).expect("reload");
    let load_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let items = build_from_log(&reloaded);
    let build_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let rows = rows_snapshot(&reloaded, None);
    let snapshot_ms = started.elapsed().as_millis();

    let started = std::time::Instant::now();
    let history = history_for_model(&reloaded);
    let history_ms = started.elapsed().as_millis();

    let cold_ms = load_ms + build_ms;
    println!(
        "[perf] events=10000 jsonl_bytes={bytes} load_ms={load_ms} build_ms={build_ms} \
         snapshot_ms={snapshot_ms} history_ms={history_ms} cold_ms={cold_ms} \
         items={} rows={} history_messages={}",
        items.len(),
        rows.len(),
        history.len()
    );
    let ok = reloaded.len() == 10_000
        && items.len() == 7_500
        && rows.len() == items.len()
        && history.len() <= 60
        && cold_ms < 1_500;
    if ok {
        println!("[selftest-perf] PASS");
        0
    } else {
        println!("[selftest-perf] FAIL");
        1
    }
}

/// 多轮真实对话（需要 API key）：验证"多轮记忆"而不是单轮应答。
/// 用法：`dshr --live-chat "第一句" "第二句" ...`（每个参数是一轮用户消息）
fn live_chat(script: &[String]) -> i32 {
    let path = PathBuf::from(".tmp/chat-live-chat.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_with_settings(
        path.clone(),
        SESSION_ID,
        &cancel,
        ApprovalBroker::new(),
        true,
        true,
        DesktopSettings::from_env(&configured_models()),
    );

    let mut failed = false;
    for (index, text) in script.iter().enumerate() {
        let started = std::time::Instant::now();
        let mut updates = 0usize;
        let reply = state
            .stream_turn(text, &mut |_| updates += 1)
            .unwrap_or_default();
        if reply.starts_with("处理失败") {
            failed = true;
        }
        println!(
            "[live-chat] turn={} ms={} stream_updates={updates} reply={reply}",
            index + 1,
            started.elapsed().as_millis()
        );
    }

    // 落盘回放：同一份日志重建出的历史必须包含所有轮次。
    let reloaded = SessionLog::from_jsonl(
        SESSION_ID,
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .expect("reload live chat log");
    let history = history_for_model(&reloaded);
    println!(
        "[live-chat] persisted_turns={} history_messages={}",
        script.len(),
        history.len()
    );
    if failed {
        1
    } else {
        0
    }
}

/// Gate 1 真实网络自检：真实 provider 连跑 20 轮，每轮要求模型调用一次本地工具。
///
/// 统计口径：以"每轮一次模型请求 + 至少一次真实工具调用"为一步；一步成功 =
/// 有回复、无失败标记、`tool.call`/`tool.result` 配对且无错误/中止。
/// 需要 `DEEPSEEK_API_KEY` 或 `OPENAI_API_KEY`；两者都没有时返回 2（SKIP，不伪装通过）。
fn selftest_live_20() -> i32 {
    if DeepSeekProvider::from_env().is_none() && OpenAiProvider::from_env().is_none() {
        println!("[selftest-live-20] SKIP: 没有 DEEPSEEK_API_KEY / OPENAI_API_KEY");
        return 2;
    }
    let path = PathBuf::from(".tmp/chat-live-20.jsonl");
    let _ = std::fs::create_dir_all(".tmp");
    reset_session_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    // 这一步只统计"网络 + 模型 + 工具执行"链路；审批链由 --selftest-approval 覆盖。
    std::env::set_var("DSHR_APPROVAL_MODE", "allow");
    let mut state = ChatState::open_diagnostics(path.clone(), SESSION_ID, &cancel, ApprovalBroker::new());

    let count = |state: &ChatState, kind: &str| {
        state
            .log
            .events()
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    let count_errors = |state: &ChatState| {
        state
            .log
            .events()
            .iter()
            .filter(|event| {
                event.event_type == "tool.result"
                    && event
                        .payload
                        .get("is_error")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
            })
            .count()
    };

    let steps = 20usize;
    let mut ok_steps = 0usize;
    let mut failed_steps = 0usize;
    let mut total_calls = 0usize;
    let mut total_results = 0usize;
    let mut total_errors = 0usize;
    let mut latencies = Vec::new();

    for step in 0..steps {
        let prompt = format!(
            "第 {step} 步：只调用一次 probe_native 工具，参数 text 为 \"step-{step}\"，\
             不要调用其它工具；拿到工具结果后用一句中文确认。"
        );
        let before_calls = count(&state, "tool.call");
        let before_results = count(&state, "tool.result");
        let before_errors = count_errors(&state);
        let started = std::time::Instant::now();
        let reply = state.stream_turn(&prompt, &mut |_| {});
        let elapsed_ms = started.elapsed().as_millis() as u64;
        latencies.push(elapsed_ms);

        let calls = count(&state, "tool.call") - before_calls;
        let results = count(&state, "tool.result") - before_results;
        let errors = count_errors(&state) - before_errors;
        let aborted = count(&state, "tool.aborted");
        total_calls += calls;
        total_results += results;
        total_errors += errors;

        let reply_ok = reply
            .as_deref()
            .is_some_and(|text| !text.is_empty() && !text.starts_with("处理失败"));
        let step_ok = reply_ok && calls >= 1 && results >= 1 && errors == 0 && aborted == 0;
        if step_ok {
            ok_steps += 1;
        } else {
            failed_steps += 1;
        }
        println!(
            "[live-20] step={step} ok={step_ok} calls={calls} results={results} \
             errors={errors} ms={elapsed_ms} reply={}",
            reply.unwrap_or_default()
        );
    }

    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let rate = ok_steps as f64 / steps as f64;
    let provider = std::env::var("DSHR_PROVIDER").unwrap_or_else(|_| String::from("auto"));
    println!(
        "[live-20] provider={provider} steps={steps} ok={ok_steps} failed={failed_steps} \
         rate={rate:.4} tool_calls={total_calls} tool_results={total_results} \
         tool_errors={total_errors} p50_ms={p50} p95_ms={p95}"
    );
    std::env::remove_var("DSHR_APPROVAL_MODE");
    if rate >= 0.99 {
        println!("[selftest-live-20] PASS");
        0
    } else {
        println!("[selftest-live-20] FAIL");
        1
    }
}

/// `--selftest-official`：打包后的官方 sidecar 自检（不依赖源码树 fixture）。
///
/// 从 `app_root()/sidecar/dsh-official` 启动 vendored sidecar，断言 v2 握手、
/// host services active、白名单 3 张表；用于验证 zip/tar 包里的 node_modules 与
/// `runtime/node` 布局。失败即 exit 1，不做 SKIP。
fn selftest_official_sidecar() -> i32 {
    let dsh_dir = app_root().join("sidecar/dsh-official");
    if !dsh_dir.join("sidecar-dsh.mjs").exists() || !dsh_dir.join("node_modules").exists() {
        eprintln!(
            "[selftest-official] FAIL 缺少官方 sidecar: {}",
            dsh_dir.display()
        );
        return 1;
    }
    let l2 = ChatSessionAccess::new(sessions_dir_from_env());
    let env_keys = [
        "DSHR_SIDECAR",
        "DSHR_DSH_PLUGINS",
        "DSHR_HOST_SERVICES",
        "DSHR_FS_WRITE",
    ];
    let saved: Vec<Option<std::ffi::OsString>> = env_keys.iter().map(std::env::var_os).collect();
    std::env::set_var("DSHR_SIDECAR", "dsh");
    std::env::set_var("DSHR_DSH_PLUGINS", "[]");
    std::env::set_var("DSHR_HOST_SERVICES", "1");
    std::env::set_var("DSHR_FS_WRITE", "0");
    let host = spawn_sidecar(None, Some(l2));
    for (key, value) in env_keys.iter().zip(saved) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    match host {
        None => {
            eprintln!("[selftest-official] FAIL 官方 sidecar 启动失败");
            1
        }
        Some(host) => {
            let protocol = host.negotiated_protocol();
            let active = host.host_services_active();
            let tables = host
                .host_services_payload()
                .and_then(|payload| payload.as_array().map(Vec::len))
                .unwrap_or(0);
            drop(host);
            if protocol == 2 && active && tables == 3 {
                println!(
                    "[selftest-official] PASS protocol={protocol} active={active} host_services={tables}"
                );
                0
            } else {
                eprintln!(
                    "[selftest-official] FAIL protocol={protocol} active={active} host_services={tables}"
                );
                1
            }
        }
    }
}

/// `--selftest-l2`：v2 Host Service 的 CLI 层 smoke（不依赖真实 DSH 插件执行）。
///
/// Phase A（可选，需要 vendored 官方 sidecar）：真握手到 v2，断言
/// `negotiated_protocol == 2`、`host_services_active`、白名单 3 张表。
/// Phase B（始终）：用 compat-dsh-host 的 fixture sidecar 跑
/// `storage-kv/v1` open→putRecord→loadAll 的真实 `host.service.request` 往返。
fn selftest_l2() -> i32 {
    let mut pass = true;

    // Phase A：真实官方 sidecar（无 vendored 依赖时 SKIP，不算失败）。
    let dsh_dir = app_root().join("sidecar/dsh-official");
    if dsh_dir.join("sidecar-dsh.mjs").exists() && dsh_dir.join("node_modules").exists() {
        let l2 = ChatSessionAccess::new(sessions_dir_from_env());
        let env_keys = [
            "DSHR_SIDECAR",
            "DSHR_DSH_PLUGINS",
            "DSHR_HOST_SERVICES",
            "DSHR_FS_WRITE",
        ];
        let saved: Vec<Option<std::ffi::OsString>> =
            env_keys.iter().map(std::env::var_os).collect();
        std::env::set_var("DSHR_SIDECAR", "dsh");
        std::env::set_var("DSHR_DSH_PLUGINS", "[]");
        std::env::set_var("DSHR_HOST_SERVICES", "1");
        std::env::set_var("DSHR_FS_WRITE", "0");
        let host = spawn_sidecar(None, Some(l2));
        for (key, value) in env_keys.iter().zip(saved) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        match host {
            None => println!("[selftest-l2] SKIP phase A: 官方 sidecar 启动失败（环境受限）"),
            Some(host) => {
                let protocol = host.negotiated_protocol();
                let active = host.host_services_active();
                let tables = host
                    .host_services_payload()
                    .and_then(|payload| payload.as_array().map(Vec::len))
                    .unwrap_or(0);
                drop(host);
                if protocol == 2 && active && tables == 3 {
                    println!(
                        "[selftest-l2] phase A PASS protocol={protocol} active={active} host_services={tables}"
                    );
                } else {
                    eprintln!(
                        "[selftest-l2] phase A FAIL protocol={protocol} active={active} host_services={tables}"
                    );
                    pass = false;
                }
            }
        }
    } else {
        println!(
            "[selftest-l2] SKIP phase A: 未找到 vendored 官方 sidecar（先运行 node tools/dsh-vendor.mjs）"
        );
    }

    // Phase B：fixture sidecar 的真实 host.service.request 往返（storage-kv）。
    let fixture = std::path::absolute(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"),
    )
    .unwrap_or_else(|_| PathBuf::from("crates/compat-dsh-host/tests/fixtures/l2-sidecar.mjs"));
    if !fixture.exists() {
        eprintln!(
            "[selftest-l2] FAIL phase B: fixture 不存在: {}",
            fixture.display()
        );
        return 1;
    }
    let root = data_root()
        .join("selftests")
        .join(format!("l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        // review P2-4：不再用用户 `%TEMP%`，失败要打印真实路径与原因。
        eprintln!(
            "[selftest-l2] FAIL phase B: 创建临时目录 {} 失败: {error}",
            root.display()
        );
        return 1;
    }
    let access = ChatSessionAccess::new(root.join("sessions"));
    let router = match build_l2_router_at(
        access,
        root.join("storage"),
        root.join("workspace"),
        "l2.fixture",
    ) {
        Ok(router) => router,
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: {err}");
            return 1;
        }
    };
    let options = HostOptions::default()
        .with_env("L2_SCENARIO", "storage")
        .with_registration_window(Duration::from_secs(3))
        .with_host_services(router);
    match SidecarHost::spawn_with(&fixture, &fixture, options) {
        Err(err) => {
            eprintln!("[selftest-l2] FAIL phase B: spawn fixture: {err}");
            pass = false;
        }
        Ok(mut host) => {
            let protocol = host.negotiated_protocol();
            let active = host.host_services_active();
            match host.invoke("l2_probe", json!({}), Duration::from_secs(15)) {
                Ok(outcome) => {
                    let steps = &outcome.terminal.payload["value"]["steps"];
                    let open_ok = steps[0]["ok"].as_bool().unwrap_or(false);
                    let put_ok = steps[1]["ok"].as_bool().unwrap_or(false);
                    let load_n = steps[2]["result"]["tables"]["items"]["a"]["n"].as_i64();
                    let unit_id = steps[0]["result"]["unit_id"]
                        .as_str()
                        .unwrap_or("<none>")
                        .to_owned();
                    if protocol == 2 && active && open_ok && put_ok && load_n == Some(1) {
                        println!(
                            "[selftest-l2] phase B PASS protocol={protocol} active={active} unit_id={unit_id} roundtrip=ok"
                        );
                    } else {
                        eprintln!(
                            "[selftest-l2] phase B FAIL protocol={protocol} active={active} payload={}",
                            outcome.terminal.payload
                        );
                        pass = false;
                    }
                }
                Err(err) => {
                    eprintln!("[selftest-l2] FAIL phase B: invoke: {err}");
                    pass = false;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    if pass {
        println!("[selftest-l2] PASS");
        0
    } else {
        println!("[selftest-l2] FAIL");
        1
    }
}

/// `--selftest-parallel-tools`：M3 并行工具的 CLI 层证据（串行 vs 并行墙钟 + 顺序）。
///
/// 用 probe sidecar 的 `probe_slow`（每个 ≈2s）跑 3 个：串行 ≈6s、并行 ≈2s；
/// 另用 `probe_echo` 验证 `execute_batch` 的结果按原顺序返回。取消/超时/迟到语义
/// 由 `compat-dsh-host` 的 `M3-PARALLEL-101..107` 覆盖。
fn selftest_parallel_tools() -> i32 {
    let path = PathBuf::from(".tmp/chat-parallel-tools.jsonl");
    let _ = std::fs::remove_file(&path);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = ChatState::open_diagnostics_with_options(
        path,
        "s-parallel",
        &cancel,
        ApprovalBroker::new(),
        false,
        true,
    );
    if state.host.is_none() {
        println!(
            "[selftest-parallel-tools] SKIP: sidecar unavailable（找不到 node/probe sidecar）"
        );
        return 0;
    }
    let log_cell = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
        &mut state.log,
        SessionLog::new("s-parallel"),
    )));
    let mut executor = ChatExecutor {
        plugins: &mut state.plugins,
        host: &mut state.host,
        approvals: &state.approvals,
        creator: None,
        step: ToolStep::default(),
        headless: state.headless,
        diagnostics: state.diagnostics,
        log: log_cell.clone(),
        sessions_dir: &state.sessions_dir,
        parent_id: "s-parallel",
        workspace: &state.workspace,
        cancel: &cancel,
        settings: &state.settings,
        permission: &state.permission,
        preset_inputs: &state.preset_inputs,
        published_presets: &mut state.published_presets,
        allow_spawn: false,
        l2_access: state.l2_access.clone(),
    };

    executor.prepare_step();

    // 1) 顺序证据：3 个 probe_echo（fast），并行批返回顺序必须等于调用顺序。
    let order_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-order-{index}"),
            tool: "probe_echo".to_owned(),
            arguments: json!({ "i": index }),
        })
        .collect();
    let order_results = executor.execute_batch(&order_calls, 3);
    let order_ok = order_results.iter().enumerate().all(|(index, outcome)| {
        !outcome.is_error && outcome.value["echo"]["i"].as_u64() == Some(index as u64)
    });

    // 2) 墙钟证据：3 个 probe_slow（每个 ≈2s）先串行、再并行。
    let slow_calls: Vec<ToolCallRequest> = (0..3)
        .map(|index| ToolCallRequest {
            call_id: format!("parallel-slow-{index}"),
            tool: "probe_slow".to_owned(),
            arguments: json!({}),
        })
        .collect();
    let serial_started = std::time::Instant::now();
    let serial_results: Vec<ToolOutcome> = slow_calls
        .iter()
        .map(|call| executor.execute(call))
        .collect();
    let serial_elapsed = serial_started.elapsed();
    let parallel_started = std::time::Instant::now();
    let parallel_results = executor.execute_batch(&slow_calls, 3);
    let parallel_elapsed = parallel_started.elapsed();

    drop(executor);
    state.log = std::rc::Rc::try_unwrap(log_cell)
        .map(|cell| cell.into_inner())
        .unwrap_or_else(|cell| cell.borrow().clone());
    state.save();

    let all_ok = serial_results
        .iter()
        .chain(parallel_results.iter())
        .all(|outcome| !outcome.is_error && outcome.value["done"] == true);
    let ratio = if serial_elapsed.is_zero() {
        f64::INFINITY
    } else {
        parallel_elapsed.as_secs_f64() / serial_elapsed.as_secs_f64()
    };
    println!(
        "[selftest-parallel-tools] order_ok={order_ok} all_ok={all_ok} serial={serial_elapsed:?} parallel={parallel_elapsed:?} ratio={ratio:.2}"
    );
    if order_ok && all_ok && ratio < 0.6 {
        println!("[selftest-parallel-tools] PASS");
        0
    } else {
        println!("[selftest-parallel-tools] FAIL（并行加速比应 < 0.6）");
        1
    }
}

fn run_navigation_flow(window: gpui::WindowHandle<ChatApp>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let delay = Duration::from_millis(250);
        cx.background_executor().timer(delay).await;
        let initial = window.update(cx, |chat, _, _| {
            chat.visible_limit < chat.rows.len() && chat.scroll.offset().y < px(-100.0)
        }).unwrap_or(false);

        for key in ["/", "down", "down", "enter"] {
            let _ = window.update(cx, |_, window, cx| {
                window.defer(cx, move |window, cx| {
                    let mut keystroke = gpui::Keystroke::parse(key).expect("navigation flow key");
                    if key == "/" {
                        keystroke.key_char = Some("/".into());
                    }
                    window.dispatch_keystroke(keystroke, cx);
                });
            });
            cx.background_executor().timer(delay).await;
        }
        let invoked = window.update(cx, |chat, window, cx| {
            let invoked = chat.turn_menu_open && chat.composer_menu.is_none()
                && chat.input.read(cx).content().is_empty() && !chat.busy;
            if invoked {
                let position = point(
                    window.viewport_size().width - px(150.0),
                    chat.scroll.bounds().top() + px(20.0),
                );
                window.defer(cx, move |window, cx| {
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }), cx);
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                    }), cx);
                });
            }
            invoked
        }).unwrap_or(false);
        cx.background_executor().timer(delay).await;
        let anchored = window.update(cx, |chat, _, _| {
            let scroll = &chat.scroll;
            scroll.bounds_for_item(0).is_some_and(|bounds| {
                let top = bounds.top() + scroll.offset().y;
                top >= scroll.bounds().top() - px(1.0)
                    && top <= scroll.bounds().top() + px(24.0)
            }) && chat.visible_limit >= chat.rows.len() && !chat.turn_menu_open
        }).unwrap_or(false);
        println!("[navigation-flow] initial_tail={initial} slash_invoked={invoked} first_turn_visible={anchored}");
        if !(initial && invoked && anchored) {
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    }).detach();
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
    if let Some(index) = args.iter().position(|arg| arg == "--install-runtime-component") {
        let result = args.get(index + 1)
            .and_then(|name| runtime_components::Component::parse(name))
            .ok_or_else(|| "用法：--install-runtime-component <python|creator-build>".to_owned())
            .and_then(runtime_components::install);
        match result {
            Ok(path) => {
                println!("{}", path.display());
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    // 3. 自检的 `.tmp` 相对路径与用户 CWD 解耦（只读 CWD 下也能跑）。
    prepare_selftest_cwd(&args);
    if let Some(index) = args.iter().position(|arg| arg == "--live-chat") {
        let script: Vec<String> = args.iter().skip(index + 1).cloned().collect();
        if script.is_empty() {
            println!("[live-chat] 需要至少一条消息");
            std::process::exit(1);
        }
        std::process::exit(live_chat(&script));
    }
    if let Some(index) = args.iter().position(|arg| arg == "--live") {
        let text = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "用一句话介绍你自己".to_owned());
        std::process::exit(live_turn(&text));
    }

    if std::env::args().any(|arg| arg == "--selftest-live-20") {
        std::process::exit(selftest_live_20());
    }
    if std::env::args().any(|arg| arg == "--selftest-stream") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_stream());
    }
    if std::env::args().any(|arg| arg == "--selftest-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-long-context") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_long_context());
    }
    if std::env::args().any(|arg| arg == "--selftest-spill") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_spill());
    }
    if std::env::args().any(|arg| arg == "--selftest-approval") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_approval());
    }
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--selftest-crash-child") {
        let path = args.get(index + 1).cloned().unwrap_or_default();
        std::process::exit(selftest_crash_child(&path));
    }
    if args.iter().any(|arg| arg == "--selftest-crash-recovery") {
        std::process::exit(selftest_crash_recovery());
    }
    if args.iter().any(|arg| arg == "--selftest-window-recovery") {
        std::process::exit(selftest_window_recovery());
    }
    if args.iter().any(|arg| arg == "--selftest-corrupt-session") {
        std::process::exit(selftest_corrupt_session());
    }
    if std::env::args().any(|arg| arg == "--selftest-recovery") {
        std::process::exit(selftest_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-sidecar-recovery") {
        std::process::exit(selftest_sidecar_recovery());
    }
    if std::env::args().any(|arg| arg == "--selftest-cancel") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_cancel());
    }
    if std::env::args().any(|arg| arg == "--selftest-sandbox") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_sandbox());
    }
    if std::env::args().any(|arg| arg == "--selftest-profile") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_profile());
    }
    if std::env::args().any(|arg| arg == "--selftest-loader") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_loader());
    }
    if std::env::args().any(|arg| arg == "--selftest-install") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-retry") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_retry());
    }
    if std::env::args().any(|arg| arg == "--selftest-perf") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_perf());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--seed-perf-session") {
        let dir = std::env::args()
            .nth(index + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".tmp/sessions"));
        std::process::exit(seed_perf_session(&dir));
    }
    if std::env::args().any(|arg| arg == "--selftest-trust-install") {
        std::process::exit(selftest_trust_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-plugin-lifecycle") {
        std::process::exit(selftest_plugin_lifecycle());
    }
    if std::env::args().any(|arg| arg == "--selftest-external-plugin") {
        std::process::exit(selftest_external_plugin());
    }
    if std::env::args().any(|arg| arg == "--selftest-clipboard") {
        std::process::exit(selftest_clipboard());
    }
    if std::env::args().any(|arg| arg == "--selftest-window") {
        std::process::exit(selftest_window());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-cli-install") {
        std::process::exit(selftest_cpp_cli_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-install") {
        std::process::exit(selftest_cpp_install());
    }
    if std::env::args().any(|arg| arg == "--selftest-cpp-component") {
        std::process::exit(selftest_cpp_component());
    }
    if std::env::args().any(|arg| arg == "--selftest-wasm") {
        std::process::exit(selftest_wasm());
    }
    if std::env::args().any(|arg| arg == "--selftest-adc-panel") {
        std::process::exit(selftest_adc_panel());
    }
    if std::env::args().any(|arg| arg == "--selftest-defaults") {
        std::process::exit(selftest_defaults());
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--install-plugin") {
        let Some(path) = std::env::args().nth(index + 1) else {
            eprintln!(
                "用法: dshr --install-plugin <本地包目录> [--yes-install] [--allow-downgrade]"
            );
            std::process::exit(2);
        };
        std::process::exit(install_plugin_cli(PathBuf::from(path)));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--uninstall-plugin") {
        let Some(name) = std::env::args().nth(index + 1) else {
            eprintln!("用法: dshr --uninstall-plugin <插件名>");
            std::process::exit(2);
        };
        std::process::exit(uninstall_plugin_cli(name));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--trust-key") {
        let rest: Vec<String> = std::env::args().skip(index + 1).collect();
        std::process::exit(trust_key_cli(&rest));
    }
    if let Some(index) = std::env::args().position(|arg| arg == "--export-session") {
        let Some(id) = std::env::args().nth(index + 1) else {
            eprintln!("用法: dshr --export-session <会话 id> [--export-dir <目录>]");
            std::process::exit(2);
        };
        let out = std::env::args()
            .position(|arg| arg == "--export-dir")
            .and_then(|dir_index| std::env::args().nth(dir_index + 1))
            .map(PathBuf::from);
        std::process::exit(export_session_cli(&id, out));
    }
    if std::env::args().any(|arg| arg == "--retention-plan") {
        std::process::exit(retention_plan_cli());
    }
    if std::env::args().any(|arg| arg == "--selftest-tree") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_tree());
    }
    if std::env::args().any(|arg| arg == "--selftest-export") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_export());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh-write") {
        std::process::exit(selftest_dsh_write());
    }
    if std::env::args().any(|arg| arg == "--selftest-dsh") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_dsh());
    }
    if std::env::args().any(|arg| arg == "--selftest-official") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_official_sidecar());
    }
    if std::env::args().any(|arg| arg == "--selftest-l2") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_l2());
    }
    if std::env::args().any(|arg| arg == "--selftest-parallel-tools") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_parallel_tools());
    }
    if std::env::args().any(|arg| arg == "--selftest-cordis") {
        std::env::set_var("DSHR_SIDECAR", "cordis");
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest_cordis());
    }
    if std::env::args().any(|arg| arg == "--selftest") {
        std::env::set_var("DSHR_FORCE_MOCK", "1");
        std::process::exit(selftest());
    }

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
    if sessions.is_empty() {
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
    let active = sessions
        .first()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| SESSION_ID.to_owned());
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
        Profile::load(&profile_dir).unwrap_or_else(|err| {
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
            guard.log.created().and_then(|created| created.preset.clone()),
            blank_root_created(&guard.log).is_some(),
            guard.permission.clone(),
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
                            trial_panels_collapsed: false,
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
            run_navigation_flow(window_handle, cx);
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
mod tests {
    use super::*;

    #[test]
    fn switching_sessions_rebinds_native_writes_to_the_session_workspace() {
        let root = std::env::temp_dir().join(format!(
            "dshr-workspace-switch-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let store = SessionIndex::new(root.join("sessions"));
        store
            .create_root("first", &first.to_string_lossy(), &now_ts())
            .unwrap();
        store
            .create_root("second", &second.to_string_lossy(), &now_ts())
            .unwrap();

        let mut state = ChatState::open_in(
            store.path_for("first"),
            "first",
            &Arc::new(AtomicBool::new(false)),
            ApprovalBroker::new(),
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: Some(PermissionMode::DangerFullAccess),
                reasoning: None,
            },
            ChatRuntime::Desktop { allow_spawn: false, sidecar: false, diagnostics: true },
        );
        assert_eq!(state.workspace, first);
        state
            .plugins
            .call(
                "probe_write",
                &json!({"path": "note.txt", "content": "first"}),
            )
            .unwrap();
        state.switch_session(store.path_for("second"), "second");
        assert_eq!(state.workspace, second);
        state
            .plugins
            .call(
                "probe_write",
                &json!({"path": "note.txt", "content": "second"}),
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(first.join("note.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(second.join("note.txt")).unwrap(),
            "second"
        );
        drop(state);
        std::fs::remove_dir_all(&root).unwrap();
    }
    use std::path::Path;

    #[test]
    fn turn_rail_keeps_the_active_turn_visible_with_bounded_markers() {
        assert_eq!(turn_rail_window(0, 0), 0..0);
        assert_eq!(turn_rail_window(3, 2), 0..3);
        assert_eq!(turn_rail_window(24, 0), 0..7);
        assert_eq!(turn_rail_window(24, 10), 7..14);
        assert_eq!(turn_rail_window(24, 23), 17..24);
    }

    #[test]
    fn command_menu_matches_only_slash_prefixes() {
        assert_eq!(slash_commands("/").collect::<Vec<_>>(), SlashCommand::ALL);
        assert_eq!(
            slash_commands("/se").collect::<Vec<_>>(),
            [SlashCommand::Search]
        );
        assert_eq!(slash_commands("/theme").next(), Some(SlashCommand::Theme));
        for ordinary_message in ["/src/main.rs", "/new.txt", "/new later", "see /search"] {
            assert_eq!(slash_commands(ordinary_message).next(), None);
        }
    }

    #[test]
    fn text_attachment_survives_replay_and_is_sent_once_per_request() {
        struct RecordingModel(Arc<Mutex<Vec<Vec<String>>>>);

        impl ModelProvider for RecordingModel {
            fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
                self.0.lock().unwrap().push(
                    request
                        .messages
                        .iter()
                        .filter_map(|message| match message {
                            ChatMessage::User(text) => Some(text.clone()),
                            _ => None,
                        })
                        .collect(),
                );
                Ok(ModelReply::text_only("已收到"))
            }
        }

        let root = std::env::temp_dir().join(format!(
            "dshr-attachment-test-{}",
            now_ts().replace(':', "-")
        ));
        let log = root.join("test.jsonl");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = ChatState::open_with_settings(
            log.clone(),
            "test",
            &cancel,
            ApprovalBroker::new(),
            false,
            false,
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: Some(PermissionMode::ReadOnly),
                reasoning: None,
            },
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        *state.agent.provider_mut() = Box::new(RecordingModel(requests.clone()));
        assert!(state
            .stream_turn_with_attachments(
                "",
                &[DraftAttachment::new(
                    "notes.txt".into(),
                    AttachmentContent::Text("fixture body".into()),
                )],
                &mut |_| {},
            )
            .is_some());
        let first = "附件 \"notes.txt\"：\nfixture body".to_owned();

        let replayed =
            SessionLog::from_jsonl("test", &std::fs::read_to_string(&log).unwrap()).unwrap();
        assert!(matches!(
            &build_from_log(&replayed)[0],
            Item::User { text, attachments }
                if text.is_empty() && attachments.len() == 1
                    && attachments[0].name == "notes.txt" && attachments[0].file.is_none()
        ));
        let index = SessionIndex::new(&root);
        assert_eq!(index.scan().unwrap()[0].title, "notes.txt");
        assert_eq!(
            history_for_model(&replayed)[0],
            ChatMessage::User(first.clone())
        );
        assert!(state.stream_turn("继续", &mut |_| {}).is_some());
        assert_eq!(
            *requests.lock().unwrap(),
            vec![vec![first.clone()], vec![first, "继续".to_owned()]]
        );
        drop(state);
        index.delete("test").unwrap();
        std::fs::remove_file(root.join("test.jsonl.lock")).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn media_attachments_survive_restart_and_export() {
        struct RecordingModel(Arc<Mutex<Vec<Vec<ChatMessage>>>>);

        impl ModelProvider for RecordingModel {
            fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
                self.0.lock().unwrap().push(
                    request
                        .messages
                        .iter()
                        .filter(|message| message.user_text().is_some())
                        .cloned()
                        .collect(),
                );
                Ok(ModelReply::text_only("已收到"))
            }
        }

        let root = std::env::temp_dir().join(format!(
            "dshr-media-test-{}-{}",
            std::process::id(),
            now_ts().replace(':', "-")
        ));
        std::fs::create_dir(&root).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(300, 150, image::Rgb([32, 96, 224]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let fixtures = [
            ("样图.png", AttachmentFormat::Png, png.get_ref().as_slice()),
            (
                "notes.pdf",
                AttachmentFormat::Pdf,
                b"%PDF-1.4\nfixture\n%%EOF".as_slice(),
            ),
        ];
        let paths = fixtures
            .iter()
            .map(|(name, _, bytes)| {
                let path = root.join(name);
                std::fs::write(&path, bytes).unwrap();
                path
            })
            .collect::<Vec<_>>();
        assert!(read_attachments(paths.clone(), 1, 32 * 1024, 1024).is_err());
        assert!(read_attachments(paths.clone(), 4, 32 * 1024, 8).is_err());
        let attachments = read_attachments(paths, 4, 32 * 1024, 4096).unwrap();
        let thumbnail = attachments[0].thumbnail.as_ref().unwrap();
        assert_eq!(thumbnail.size(0), size(200u32.into(), 100u32.into()));
        assert_eq!(&thumbnail.as_bytes(0).unwrap()[..4], &[224, 96, 32, 255]);
        assert!(attachments[1].thumbnail.is_none());

        let cancel = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let open = || {
            let mut state = ChatState::open_with_settings(
                root.join("test.jsonl"),
                "test",
                &cancel,
                ApprovalBroker::new(),
                false,
                false,
                DesktopSettings {
                    model: Arc::new(DesktopModel::Mock),
                    permission: Some(PermissionMode::ReadOnly),
                    reasoning: None,
                },
            );
            state.spill = SpillStore::new(root.join("blobs"));
            *state.agent.provider_mut() = Box::new(RecordingModel(requests.clone()));
            state
        };
        let mut state = open();
        assert!(state
            .stream_turn_with_attachments("", &attachments, &mut |_| {})
            .is_some());
        drop(state);
        let mut state = open();
        let projection = build_from_log(&state.log);
        let Item::User { text, attachments } = &projection[0] else {
            panic!("missing user attachments after reopening");
        };
        assert!(text.is_empty());
        assert_eq!(attachments.len(), 2);
        for (attachment, (name, format, bytes)) in attachments.iter().zip(fixtures) {
            assert_eq!(attachment.name, name);
            let file = attachment.file.as_ref().unwrap();
            assert_eq!(file.size, bytes.len() as u64);
            assert_eq!(file.mime, format.mime());
        }
        let stored = StoredAttachment {
            root: state.spill.root().to_owned(),
            file: attachments[0].file.as_ref().unwrap().clone(),
        };
        let preview = PreviewSource::Stored(stored.clone()).load().unwrap();
        assert_eq!(preview.size(0), size(300u32.into(), 150u32.into()));
        assert_eq!(
            SessionIndex::new(&root).scan().unwrap()[0].title,
            "样图.png"
        );
        assert!(state.stream_turn("继续", &mut |_| {}).is_some());

        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].len(), 1);
        assert_eq!(recorded[1].len(), 2);
        assert_eq!(recorded[1][0], recorded[0][0]);
        assert_eq!(recorded[1][1], ChatMessage::user("继续"));
        let ChatMessage::UserWithAttachments { text, attachments } = &recorded[0][0] else {
            panic!("missing media user message");
        };
        assert!(text.is_empty());
        assert_eq!(attachments.len(), fixtures.len());
        for (attachment, (name, format, bytes)) in attachments.iter().zip(fixtures) {
            assert_eq!(attachment.name, name);
            assert_eq!(attachment.format, format);
            assert_eq!(attachment.bytes.as_deref(), Some(bytes));
        }

        let out = root.join("export");
        let manifest = export_session(
            &state.log,
            &SpillStoreSource::new(&state.spill),
            &out,
            &ExportOptions::new(now_ts()).fail_on_missing_blob(true),
        )
        .unwrap();
        assert!(verify_export(&out, "test").unwrap().is_ok());
        assert_eq!(manifest.attachments.len(), fixtures.len());
        for (_, format, bytes) in fixtures {
            let entry = manifest
                .attachments
                .iter()
                .find(|entry| entry.mime == format.mime())
                .unwrap();
            assert_eq!(std::fs::read(out.join(&entry.path)).unwrap(), bytes);
        }
        assert!(!std::fs::read(out.join(&manifest.jsonl.path))
            .unwrap()
            .windows(b"%PDF-1.4".len())
            .any(|window| window == b"%PDF-1.4"));
        let mut corrupted = fixtures[0].2.to_vec();
        corrupted[0] ^= 0xff;
        std::fs::write(state.spill.object_path(&stored.file), corrupted).unwrap();
        assert!(PreviewSource::Stored(stored).load().is_err());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clipboard_bitmap_is_converted_to_png_with_media_budget() {
        use image::{ImageFormat, Rgb, RgbImage};
        use std::io::Cursor;

        let mut bitmap = Cursor::new(Vec::new());
        RgbImage::from_pixel(2, 1, Rgb([10, 20, 30]))
            .write_to(&mut bitmap, ImageFormat::Bmp)
            .unwrap();
        let bitmap = bitmap.into_inner();
        assert!(read_clipboard_image(bitmap.clone(), 8).is_err());
        let attachment = read_clipboard_image(bitmap, 1024).unwrap();
        assert_eq!(attachment.name, "剪贴板图片.png");
        let AttachmentContent::Media { format, bytes } = attachment.content else {
            panic!("expected an image attachment");
        };
        assert_eq!(format, AttachmentFormat::Png);
        let png = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
            .unwrap()
            .into_rgb8();
        assert_eq!(png.dimensions(), (2, 1));
        assert_eq!(png.get_pixel(0, 0), &Rgb([10, 20, 30]));
    }

    #[test]
    fn permission_changes_are_visible_without_the_chat_state_lock() {
        let root = std::env::temp_dir().join(format!(
            "dshr-permission-test-{}",
            now_ts().replace(':', "-")
        ));
        let approvals = ApprovalBroker::new();
        let state = Mutex::new(ChatState::open_in(
            root.join("test.jsonl"),
            "test",
            &Arc::new(AtomicBool::new(false)),
            approvals.clone(),
            DesktopSettings {
                model: Arc::new(DesktopModel::Mock),
                permission: Some(PermissionMode::WorkspaceWrite),
                reasoning: None,
            },
            ChatRuntime::Desktop { allow_spawn: false, sidecar: false, diagnostics: true },
        ));
        let permission = state.lock().unwrap().permission.clone();
        let state = state.lock().unwrap();
        let args = json!({ "path": "probe.txt", "content": "fixture" });
        assert!(matches!(
            state.plugins.registry().authorize("probe_write", &args),
            Err(RegistryError::NeedsApproval { .. })
        ));
        permission.set(PermissionMode::ReadOnly);
        assert!(matches!(
            state.plugins.registry().authorize("probe_write", &args),
            Err(RegistryError::Denied { .. })
        ));
        assert!(!permission.can_write());
        permission.set(PermissionMode::DangerFullAccess);
        assert!(state
            .plugins
            .registry()
            .authorize("probe_write", &args)
            .is_ok());
        assert!(permission.can_write());
        drop(state);
        SessionIndex::new(&root).delete("test").unwrap();
        std::fs::remove_file(root.join("test.jsonl.lock")).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn always_allow_keys_scope_the_tool_to_its_workspace_and_session() {
        let workspace = Path::new("workspace");
        let tool = ToolDefinition::new(
            "write",
            "test",
            ToolOrigin::Native {
                plugin_id: "fixture".to_owned(),
            },
        );
        let key = tool_approval_key("session-a", workspace, &tool);
        assert!(key.is_some());
        assert_ne!(key, tool_approval_key("session-b", workspace, &tool));
        assert_ne!(
            key,
            tool_approval_key("session-a", Path::new("other"), &tool)
        );
        let new_schema = tool.clone().with_schema(json!({ "type": "object" }));
        assert_ne!(key, tool_approval_key("session-a", workspace, &new_schema));
        let other_provider = ToolDefinition::new(
            "write",
            "test",
            ToolOrigin::Native {
                plugin_id: "other".to_owned(),
            },
        );
        assert_ne!(
            key,
            tool_approval_key("session-a", workspace, &other_provider)
        );
    }

    #[test]
    fn reasoning_and_consecutive_tool_calls_replay_as_one_process() {
        let mut log = SessionLog::new("process");
        let ts = now_ts();
        for (kind, payload) in [
            (
                "message.user",
                json!({ "message_id": "u", "text": "question" }),
            ),
            (
                "message.assistant.reasoning",
                json!({ "text": "check both" }),
            ),
            ("tool.call", json!({ "call_id": "a", "tool": "read" })),
            ("tool.call", json!({ "call_id": "b", "tool": "read" })),
            ("tool.result", json!({ "call_id": "a", "content": [] })),
            ("tool.result", json!({ "call_id": "b", "content": [] })),
            (
                "message.assistant.reasoning",
                json!({ "text": "summarize" }),
            ),
            (
                "message.assistant.settled",
                json!({ "message_id": "a", "text": "answer" }),
            ),
        ] {
            log.append(EventSpec::durable(kind, &ts, payload)).unwrap();
        }
        let rows = rows_snapshot(&log, None);
        assert!(matches!(
            rows.as_slice(),
            [Row::User { .. }, Row::Process(steps), Row::Assistant(text)]
                if text == "answer" && matches!(steps.as_slice(), [
                    Row::Reasoning { text: first, streaming: false },
                    Row::Tool { pending: false, .. }, Row::Tool { pending: false, .. },
                    Row::Reasoning { text: last, streaming: false },
                ] if first == "check both" && last == "summarize")
        ));
        let history = history_from_log(&log);
        assert!(matches!(
            history.as_slice(),
            [
                ChatMessage::User(_),
                ChatMessage::Assistant { reasoning: first, tool_calls, .. },
                ChatMessage::Tool { .. }, ChatMessage::Tool { .. },
                ChatMessage::Assistant { reasoning: last, text, .. },
            ] if first == "check both" && tool_calls.len() == 2 && last == "summarize" && text == "answer"
        ));
    }

    fn args(values: &[&str]) -> Vec<String> {
        // 第 0 个元素模拟 exe 路径，与 `std::env::args()` 一致。
        std::iter::once("dshr.exe")
            .chain(values.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn gui_launch_is_not_cli() {
        assert!(!is_cli_invocation(&args(&[])));
    }

    #[test]
    fn cli_switches_attach_console() {
        for flag in [
            "--selftest",
            "--selftest-stream",
            "--selftest-dsh",
            "--selftest-l2",
            "--selftest-official",
            "--selftest-parallel-tools",
            "--selftest-dsh-write",
            "--selftest-approval",
            "--selftest-corrupt-session",
            "--selftest-crash-recovery",
            "--selftest-window-recovery",
            "--selftest-crash-child",
            "--selftest-recovery",
            "--selftest-sidecar-recovery",
            "--selftest-cancel",
            "--live",
            "--live-chat",
            "--export-session",
            "--retention-plan",
            "--install-plugin",
            "--uninstall-plugin",
            "--trust-key",
            "--selftest-plugin-lifecycle",
            "--seed-perf-session",
            "--console",
        ] {
            assert!(
                is_cli_invocation(&args(&[flag])),
                "{flag} 应识别为 CLI 用法"
            );
        }
    }

    // M4-A11Y-001：快捷键表与绑定数量一致、覆盖 G3 主流程键位、提示文案含平台主修饰键。
    #[test]
    fn keyboard_shortcut_table_is_unique_and_covers_primary_flow() {
        use std::collections::HashSet;

        let keys: HashSet<&str> = CHAT_SHORTCUTS.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys.len(), CHAT_SHORTCUTS.len(), "快捷键不能重复");
        for required in [
            "cmd-n",
            "cmd-k",
            "cmd-b",
            "cmd-j",
            "cmd-shift-a",
            "cmd-shift-d",
            "escape",
            "cmd-w",
        ] {
            assert!(keys.contains(required), "缺少快捷键 {required}");
        }
        assert_eq!(
            chat_key_bindings().len(),
            CHAT_SHORTCUTS.len() + 2,
            "绑定数量必须与说明表一致"
        );
        let hint = chat_shortcut_hint();
        assert!(hint.contains("Ctrl") || hint.contains("Cmd"), "{hint}");
        assert_eq!(format_shortcut("cmd-shift-a", "Ctrl"), "Ctrl+Shift+A");
        assert_eq!(format_shortcut("escape", "Ctrl"), "Esc");
    }

    #[test]
    fn dshr_home_wins_over_packaged_layout() {
        let home = Path::new("D:\\dshr-home");
        let picked = pick_data_root(
            Some(home),
            Some(Path::new("C:\\app")),
            Some(Path::new("C:\\Users\\me\\AppData\\Local")),
            true,
        );
        assert_eq!(picked, Some((DataRootSource::Home, home.to_path_buf())));
    }

    #[test]
    fn packaged_layout_prefers_exe_dir() {
        let exe = Path::new("C:\\app");
        let picked = pick_data_root(
            None,
            Some(exe),
            Some(Path::new("C:\\Users\\me\\AppData\\Local")),
            true,
        );
        assert_eq!(picked, Some((DataRootSource::ExeDir, exe.to_path_buf())));
    }

    #[test]
    fn dev_layout_uses_local_app_data() {
        // 从 target/release 跑（exe 旁没有 sidecar/）→ 数据落 %LOCALAPPDATA%\dshr。
        let local = Path::new("C:\\Users\\me\\AppData\\Local");
        let picked = pick_data_root(
            None,
            Some(Path::new("C:\\repo\\target\\release")),
            Some(local),
            false,
        );
        assert_eq!(
            picked,
            Some((DataRootSource::LocalAppData, local.join("dshr")))
        );
    }

    #[test]
    fn without_any_candidate_there_is_no_pick() {
        assert_eq!(pick_data_root(None, None, None, false), None);
        // 打包布局标记不可用（不可写）时也不能选 exe 目录。
        assert_eq!(
            pick_data_root(None, Some(Path::new("C:\\app")), None, false),
            None
        );
    }

    #[test]
    fn tool_timeout_is_clamped_to_sane_range() {
        assert_eq!(clamp_tool_timeout_ms(0), Duration::from_secs(1));
        assert_eq!(clamp_tool_timeout_ms(999), Duration::from_secs(1));
        assert_eq!(clamp_tool_timeout_ms(120_000), Duration::from_secs(120));
        assert_eq!(clamp_tool_timeout_ms(601_000), Duration::from_secs(600));
        assert_eq!(clamp_tool_timeout_ms(u64::MAX), Duration::from_secs(600));
    }

    #[test]
    fn sidecar_tool_timeout_follows_declaration_without_override() {
        if std::env::var_os("DSHR_TOOL_TIMEOUT_MS").is_some() {
            // 显式覆盖存在时以覆盖为准（clamp 后）。
            let definition = ToolDefinition::new(
                "x",
                "x",
                ToolOrigin::Native {
                    plugin_id: "p".into(),
                },
            );
            assert_eq!(
                sidecar_tool_timeout(&definition),
                clamp_tool_timeout_ms(
                    std::env::var("DSHR_TOOL_TIMEOUT_MS")
                        .expect("present")
                        .parse()
                        .expect("number")
                )
            );
            return;
        }
        let mut definition = ToolDefinition::new(
            "bash",
            "run shell",
            ToolOrigin::Native {
                plugin_id: "p".into(),
            },
        );
        definition.timeout_ms = 30_000;
        assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(30));
        definition.timeout_ms = 120_000;
        assert_eq!(sidecar_tool_timeout(&definition), Duration::from_secs(120));
    }
}
