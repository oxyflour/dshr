mod approvals;
mod attachments;
mod chrome;
mod composer;
mod model_controls;
mod navigation;
mod panels;
mod sessions;
mod sidebar;
mod transcript;
mod turn;
mod window;

pub(crate) use rows::describe;
pub(crate) use sidebar::session_tree_order;
pub(crate) use widgets::toolbar_tooltip;
pub(crate) use window::launch;

mod frame_probe;
pub(crate) use frame_probe::FrameProbe;

mod state;
pub(crate) use state::{
    permission_label, reasoning_label, slash_commands, AdcEditor, AttachmentPreview, ChatApp,
    ComposerAttachment, ComposerMenu, DesktopTurnUpdate, SessionMenu, SlashCommand,
    EDITABLE_PERMISSIONS,
};

mod rows;
pub(crate) use rows::{
    estimated_row_height, is_streaming_row, rows_snapshot, rows_with_preview, streaming_text,
    trial_panel_max_height, turn_preview, turn_rail_window, Row,
};

mod widgets;
pub(crate) use widgets::{
    attachment_image_button, chrome_button, composer_option, desktop_icon, file_size_label,
    relative_time, stored_image, toolbar_icon_button,
};

pub(crate) use sidebar::{
    next_sidebar_session, ordered_sessions, top_level_session, workspace_name,
};
