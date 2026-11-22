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
