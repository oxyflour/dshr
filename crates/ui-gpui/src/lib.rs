//! GPUI 原生实现、主题、窗口、Dock、虚拟化渲染

#![forbid(unsafe_code)]

pub mod adc;
pub mod diff;
pub mod markdown;
mod message_selection;
pub mod terminal;
mod text_input;
pub mod theme;

pub use adc::{
    a11y_issues_in, adc_panel_body, adc_panel_body_from_str, adc_panel_body_with_handler, adc_view,
    adc_view_with_handler, A11yIssue, AdcActionHandler, Diagnostic, DiagnosticCode, Severity,
    A11Y_MISSING_CODE, MAX_CHART_POINTS, MAX_TEXT_BYTES,
};
pub use diff::{
    diff_view, diff_view_from_text, parse_diff, Diff, DiffFile, DiffHunk, DiffLine, DiffLineKind,
    MAX_RENDER_LINES,
};
pub use dshr_ui_contributions::{parse_tree, Action, ElementNode};
pub use markdown::{markdown_view, selectable_markdown_view, Block, Inline};
pub use message_selection::MessageSelection;
pub use terminal::{
    parse_terminal, terminal_view, terminal_view_from_text, AnsiColor, TermLine, TermRun,
    TermStyle, Terminal, MAX_LINES,
};
pub use text_input::{init, TextInput};
#[cfg(target_os = "windows")]
pub use text_input::{
    Copy as CopyInput, Cut as CutInput, Paste as PasteInput, SelectAll as SelectAllInput,
};
pub use theme::{Layout, Palette, Radii, Theme, ThemeKind, Typography};
