//! GPUI 原生实现、主题、窗口、Dock、虚拟化渲染

#![forbid(unsafe_code)]

pub mod diff;
pub mod markdown;
pub mod terminal;
mod text_input;
pub mod theme;

pub use diff::{
    diff_view, diff_view_from_text, parse_diff, Diff, DiffFile, DiffHunk, DiffLine, DiffLineKind,
    MAX_RENDER_LINES,
};
pub use markdown::{markdown_view, Block, Inline};
pub use terminal::{
    parse_terminal, terminal_view, terminal_view_from_text, AnsiColor, TermLine, TermRun,
    TermStyle, Terminal, MAX_LINES,
};
pub use text_input::{init, TextInput};
pub use theme::{Layout, Palette, Radii, Theme, ThemeKind, Typography};
