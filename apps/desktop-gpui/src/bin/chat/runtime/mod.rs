mod session_access;
mod tools;

pub(crate) use session_access::{chat_header_from_log, load_session_log, ChatSessionAccess};
#[cfg(test)]
pub(crate) use tools::{clamp_tool_timeout_ms, sidecar_tool_timeout, tool_approval_key};
pub(crate) use tools::{ChatExecutor, LivePermission, ToolStep};

mod attachments;
pub(crate) use attachments::{
    read_attachments, read_clipboard_image, AttachmentContent, AttachmentSource, DraftAttachment,
};
mod providers;
#[cfg(test)]
pub(crate) use providers::cancel_aware;
pub(crate) use providers::{
    configured_models, force_mock, DesktopModel, DesktopSettings, ModelKind,
};
mod state;
pub(crate) use state::{
    AdcPanelView, ChatRuntime, ChatState, ParsedAgentPresets, TurnReason, TurnResult, TurnUpdate,
};
mod turn;
pub(crate) use turn::append_repeat_reminders;

mod session_files;
pub(crate) use session_files::{
    last_message_seq, now_ts, open_session_log, reset_session_file, session_workspace,
    spill_dir_for, spill_dir_in, OpenedSessionLog,
};

mod sidecar;
pub(crate) use sidecar::{
    blank_root_created, build_l2_router_at, creator_preset_id, loader_inputs,
    prepare_desktop_sidecar, profile_dir_for_app, read_agent_presets, read_agent_presets_from,
    register_sidecar_tools, sidecar_dir_for_app, spawn_sidecar, spawn_sidecar_in, workspace_root,
};

mod plugins;
pub(crate) use plugins::{
    host_sdk_version, load_cpp_package, load_enabled_external_plugins, register_probe_write,
    ProbeNativePlugin, WorkspaceWritePlugin,
};
