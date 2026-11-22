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
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
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
use dshr_compat_dsh_host::{parse_tool_registration, HostOptions, SidecarHost};
use dshr_conversation_model::{build_from_log, CardKind, Item, ToolCard, UserAttachment};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use dshr_llm_retry::{RetryPolicy, RetryingProvider};
use dshr_plugin_api::{Manifest, Plugin, PluginContext, PluginError};
use dshr_plugin_host::PluginHost;
use dshr_profile::{
    add_trust_key, install_package, install_package_trusted, package_digest, read_installed_meta,
    remove_trust_key, revoke_trust_key, rotate_trust_key, uninstall_package, Profile,
    ProfileSource, TrustKey, TrustPolicy, TrustStore,
};
use dshr_session_export::{export_session, verify_export, ExportOptions, SpillStoreSource};
use dshr_session_store::{
    recover_backup, save_atomic, try_lock_session, EventSpec, SessionCreated, SessionIndex,
    SessionLog, SessionMeta, ToolState,
};
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

#[path = "chat/bootstrap.rs"]
mod bootstrap;
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
#[path = "chat/plugin_commands.rs"]
mod plugin_commands;
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
#[path = "chat/session_commands.rs"]
mod session_commands;
#[path = "chat/view/mod.rs"]
mod view;

use bootstrap::*;
use plugin_commands::*;
use runtime::*;
use session_commands::*;
use view::*;
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

    view::launch();
}

#[cfg(test)]
#[path = "chat/tests/mod.rs"]
mod tests;
