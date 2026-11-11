//! 最小 ANSI 终端输出解析与原生 GPUI 渲染。
//!
//! 目标不是完整 VT100 仿真，而是把工具/命令输出（`stdout`/`stderr`，常见是
//! `\r\n`、进度条 `\r` 覆盖、少量颜色）安全地渲染成等宽、可滚动的原生视图。
//!
//! 支持范围：
//! - 文本控制：CR（回到行首）、LF（换行并回到行首，模拟 tty 的 ONLCR）、BS、TAB（8 列制表位）；
//! - SGR：`0/1/2/3/4/7/22/23/24/27/30-37/39/40-47/49/90-97/100-107`，
//!   以及可选的 `38;5;n` / `48;5;n`（256 色）与 `38;2;r;g;b` / `48;2;r;g;b`（truecolor）；
//! - CSI 最小子集：光标上下左右 `A/B/C/D`、`E/F`、列 `G`/`` ` ``、定位 `H`/`f`、行 `d`、
//!   `J`/`K` 清除、`s`/`u` 保存恢复；
//! - 安全降级：OSC/DCS 等字符串序列整段丢弃；未知 CSI/未知 SGR 只计数，不进入文本。
//!
//! 限制（见 `docs/m2-ui-views.md`）：
//! - 不做折行、滚动区域（DECSTBM）、备用屏、光标形状；CJK/emoji 按 1 列处理；
//! - 颜色近似：ANSI 16 色由 DSH 语义 token 推导（DSH 没有独立的终端 ANSI token），
//!   256 色的 16–255 用标准 xterm 公式，truecolor 原样使用；
//! - 最多保留最近 [`MAX_LINES`] 行，超出后丢弃最早的行并置 `truncated`。

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::iter::Peekable;
use std::str::Chars;

use gpui::{
    div, prelude::*, px, rgba, AnyElement, ElementId, Font, FontStyle, FontWeight, Rgba,
    SharedString, StyledText, TextRun, UnderlineStyle,
};

use crate::theme::Theme;

/// 最多保留的终端行数（超出丢弃最早的行，避免长输出撑爆内存/元素树）。
pub const MAX_LINES: usize = 5000;
/// TAB 制表位宽度。
const TAB_WIDTH: usize = 8;
/// 单行最大列数（防止畸形光标移动制造巨大空白行）。
const MAX_COLUMN: usize = 4096;

/// ANSI 颜色：索引色或 truecolor。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    /// 0–255 索引色；0–15 走主题色表，16–255 按 xterm 公式映射。
    Indexed(u8),
    /// 24 位真彩色。
    Rgb { r: u8, g: u8, b: u8 },
}

/// SGR 解析出的字符样式。未设置的字段表示使用默认前景/背景。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TermStyle {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// 一段同样式文本。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TermRun {
    pub text: String,
    pub style: TermStyle,
}

/// 终端里的一行（若干 run）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TermLine {
    pub runs: Vec<TermRun>,
}

impl TermLine {
    /// 纯文本内容（去掉样式）。
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// 没有可见字符。
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Terminal {
    pub lines: Vec<TermLine>,
    /// 输出超过 [`MAX_LINES`]，最早的若干行已被丢弃。
    pub truncated: bool,
    /// 遇到但不支持的/畸形的控制序列个数（诊断用，不影响文本）。
    pub unsupported: usize,
}

impl Terminal {
    /// 纯文本（行之间用 `\n`），方便测试与非 UI 展示。
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(TermLine::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// 解析 ANSI 终端输出。空输入返回空 `Terminal`。
pub fn parse_terminal(input: &str) -> Terminal {
    let screen = Screen::default().feed(input);
    let mut lines: Vec<TermLine> = screen
        .lines
        .iter()
        .map(|cells| line_from_cells(cells))
        .collect();
    while lines.last().is_some_and(TermLine::is_empty) {
        lines.pop();
    }
    Terminal {
        lines,
        truncated: screen.truncated,
        unsupported: screen.unsupported,
    }
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    ch: char,
    style: TermStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: TermStyle::default(),
        }
    }
}

/// 可变的"屏幕缓冲"：光标 + 稀疏行，足够覆盖日志/进度条场景。
#[derive(Default)]
struct Screen {
    lines: VecDeque<Vec<Cell>>,
    row: usize,
    col: usize,
    style: TermStyle,
    saved: Option<(usize, usize)>,
    unsupported: usize,
    truncated: bool,
}

impl Screen {
    fn feed(mut self, input: &str) -> Self {
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\x1b' => self.escape(&mut chars),
                '\r' => self.col = 0,
                '\n' => self.new_line(),
                '\x08' => self.col = self.col.saturating_sub(1),
                '\t' => self.tab(),
                '\x07' | '\x00' => {} // BEL / NUL
                ch if ch.is_control() => self.unsupported += 1,
                ch => self.put(ch),
            }
        }
        self
    }

    fn ensure_line(&mut self) {
        while self.lines.len() <= self.row {
            if self.lines.len() >= MAX_LINES {
                self.lines.pop_front();
                self.row = self.row.saturating_sub(1);
                self.truncated = true;
            } else {
                self.lines.push_back(Vec::new());
            }
        }
    }

    fn put(&mut self, ch: char) {
        if self.col >= MAX_COLUMN {
            // 超宽输出丢弃多余列（没有折行支持），仍然安全。
            self.unsupported += 1;
            return;
        }
        self.ensure_line();
        let line = &mut self.lines[self.row];
        while line.len() <= self.col {
            line.push(Cell::default());
        }
        line[self.col] = Cell {
            ch,
            style: self.style,
        };
        self.col += 1;
    }

    fn new_line(&mut self) {
        self.row += 1;
        self.col = 0;
        self.ensure_line();
    }

    fn tab(&mut self) {
        let next = (self.col / TAB_WIDTH + 1) * TAB_WIDTH;
        while self.col < next && self.col < MAX_COLUMN {
            self.put(' ');
        }
    }

    fn cursor_up(&mut self, count: usize) {
        self.row = self.row.saturating_sub(count);
    }

    fn cursor_down(&mut self, count: usize) {
        self.row = (self.row + count).min(MAX_LINES - 1);
        self.ensure_line();
    }

    fn cursor_forward(&mut self, count: usize) {
        self.col = (self.col + count).min(MAX_COLUMN - 1);
    }

    fn cursor_back(&mut self, count: usize) {
        self.col = self.col.saturating_sub(count);
    }

    fn cursor_column(&mut self, column: usize) {
        self.col = column.saturating_sub(1).min(MAX_COLUMN - 1);
    }

    fn cursor_row(&mut self, row: usize) {
        self.row = row.saturating_sub(1).min(MAX_LINES - 1);
        self.ensure_line();
    }

    fn cursor_position(&mut self, row: usize, column: usize) {
        self.cursor_row(row);
        self.cursor_column(column);
    }

    fn erase_line(&mut self, mode: u32) {
        self.ensure_line();
        let line = &mut self.lines[self.row];
        match mode {
            // 光标到行尾
            0 => {
                if self.col < line.len() {
                    line.truncate(self.col);
                }
            }
            // 行首到光标（含光标格）
            1 => {
                let end = (self.col + 1).min(line.len());
                for cell in &mut line[..end] {
                    *cell = Cell::default();
                }
            }
            // 2 或未知：整行清空（安全降级）
            _ => line.clear(),
        }
    }

    fn erase_display(&mut self, mode: u32) {
        match mode {
            // 光标到屏幕末尾
            0 => {
                self.ensure_line();
                if self.col < self.lines[self.row].len() {
                    self.lines[self.row].truncate(self.col);
                }
                self.lines.truncate(self.row + 1);
            }
            // 屏幕开头到光标
            1 => {
                for index in 0..self.row {
                    if let Some(line) = self.lines.get_mut(index) {
                        line.clear();
                    }
                }
                self.erase_line(1);
            }
            // 2 或未知：整屏清空，光标归零（日志视图取向，见模块文档）
            _ => {
                self.lines.clear();
                self.row = 0;
                self.col = 0;
            }
        }
    }

    /// 处理 `ESC` 之后的序列。
    fn escape(&mut self, chars: &mut Peekable<Chars<'_>>) {
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                while let Some(&next) = chars.peek() {
                    if is_csi_final(next) {
                        chars.next();
                        if next == 'm' {
                            self.sgr(&params);
                        } else {
                            self.csi(&params, next);
                        }
                        return;
                    }
                    params.push(next);
                    chars.next();
                }
                // 序列没写完（输入被截断）：丢弃已消费部分。
                self.unsupported += 1;
            }
            // OSC / DCS / SOS / PM / APC：整段丢弃（标题、超链接、查询响应等）。
            Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                skip_string_sequence(chars);
            }
            Some('7') => self.saved = Some((self.row, self.col)),
            Some('8') => {
                if let Some((row, col)) = self.saved {
                    self.row = row;
                    self.col = col;
                }
            }
            // 两字节 ESC 序列（字符集选择等）：吞掉参数后安全忽略。
            Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                chars.next();
            }
            Some(_) | None => {}
        }
    }

    fn csi(&mut self, params: &str, final_ch: char) {
        // 私有模式（`?25` 光标可见性、`?1049` 备用屏等）不影响文本，直接忽略。
        if params.starts_with('?') || params.starts_with('>') || params.starts_with('!') {
            return;
        }
        if !params
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == ';' || ch == ':')
        {
            self.unsupported += 1;
            return;
        }
        if params.contains(':') {
            // SGR 冒号子参数（如 38:5:196）v1 不支持。
            self.unsupported += 1;
            return;
        }
        let args: Vec<u32> = params
            .split(';')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect();
        // CSI 里 0 或缺省都表示 1。
        let count =
            |index: usize| args.get(index).copied().filter(|v| *v > 0).unwrap_or(1) as usize;

        match final_ch {
            'A' => self.cursor_up(count(0)),
            'B' => self.cursor_down(count(0)),
            'C' => self.cursor_forward(count(0)),
            'D' => self.cursor_back(count(0)),
            'E' => {
                self.cursor_down(count(0));
                self.col = 0;
            }
            'F' => {
                self.cursor_up(count(0));
                self.col = 0;
            }
            'G' | '`' => self.cursor_column(count(0)),
            'H' | 'f' => {
                let row = count(0);
                let column = count(1);
                self.cursor_position(row, column);
            }
            'd' => {
                let column = self.col + 1;
                self.cursor_position(count(0), column);
            }
            'J' => self.erase_display(args.first().copied().unwrap_or(0)),
            'K' => self.erase_line(args.first().copied().unwrap_or(0)),
            's' => self.saved = Some((self.row, self.col)),
            'u' => {
                if let Some((row, col)) = self.saved {
                    self.row = row;
                    self.col = col;
                }
            }
            // 模式设置/重置（含私有模式已被上面拦截）：不改变已渲染文本。
            'h' | 'l' => {}
            _ => self.unsupported += 1,
        }
    }

    fn sgr(&mut self, params: &str) {
        if params.is_empty() {
            self.style = TermStyle::default();
            return;
        }
        let codes: Vec<&str> = params.split(';').collect();
        let mut index = 0usize;
        while index < codes.len() {
            let code = if codes[index].is_empty() {
                0
            } else {
                match codes[index].parse::<u32>() {
                    Ok(code) => code,
                    Err(_) => {
                        self.unsupported += 1;
                        index += 1;
                        continue;
                    }
                }
            };
            match code {
                0 => self.style = TermStyle::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                30..=37 => self.style.fg = Some(AnsiColor::Indexed((code - 30) as u8)),
                39 => self.style.fg = None,
                40..=47 => self.style.bg = Some(AnsiColor::Indexed((code - 40) as u8)),
                49 => self.style.bg = None,
                90..=97 => self.style.fg = Some(AnsiColor::Indexed((code - 90 + 8) as u8)),
                100..=107 => self.style.bg = Some(AnsiColor::Indexed((code - 100 + 8) as u8)),
                38 | 48 => {
                    let is_foreground = code == 38;
                    let color = match codes.get(index + 1).copied() {
                        Some("5") => codes
                            .get(index + 2)
                            .and_then(|value| value.parse::<u8>().ok())
                            .map(AnsiColor::Indexed),
                        Some("2") => match (
                            codes.get(index + 2).and_then(|v| v.parse::<u8>().ok()),
                            codes.get(index + 3).and_then(|v| v.parse::<u8>().ok()),
                            codes.get(index + 4).and_then(|v| v.parse::<u8>().ok()),
                        ) {
                            (Some(r), Some(g), Some(b)) => Some(AnsiColor::Rgb { r, g, b }),
                            _ => None,
                        },
                        _ => None,
                    };
                    match color {
                        Some(color) => {
                            if is_foreground {
                                self.style.fg = Some(color);
                            } else {
                                self.style.bg = Some(color);
                            }
                            index += match codes.get(index + 1) {
                                Some(&"2") => 4,
                                _ => 2,
                            };
                        }
                        None => {
                            // 畸形的扩展色：停止解析本序列，保留已生效的样式。
                            self.unsupported += 1;
                            break;
                        }
                    }
                }
                _ => self.unsupported += 1,
            }
            index += 1;
        }
    }
}

fn is_csi_final(ch: char) -> bool {
    ch.is_ascii() && (0x40..=0x7e).contains(&(ch as u8))
}

/// 丢弃 OSC/DCS 一类字符串序列，直到 BEL 或 ST（`ESC \`）。
fn skip_string_sequence(chars: &mut Peekable<Chars<'_>>) {
    while let Some(ch) = chars.next() {
        match ch {
            '\x07' => return,
            '\x1b' if chars.peek() == Some(&'\\') => {
                chars.next();
                return;
            }
            _ => {}
        }
    }
}

fn line_from_cells(cells: &[Cell]) -> TermLine {
    // 去掉行尾默认样式的空格：终端里的尾随空白不可见，留着只会制造横向滚动。
    let mut end = cells.len();
    while end > 0 && cells[end - 1].ch == ' ' && cells[end - 1].style == TermStyle::default() {
        end -= 1;
    }
    let mut runs: Vec<TermRun> = Vec::new();
    for cell in &cells[..end] {
        match runs.last_mut() {
            Some(run) if run.style == cell.style => run.text.push(cell.ch),
            _ => runs.push(TermRun {
                text: cell.ch.to_string(),
                style: cell.style,
            }),
        }
    }
    TermLine { runs }
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 把解析后的终端行渲染成可滚动的原生 GPUI 视图。
///
/// 背景、默认前景、滚动条都用主题 token；ANSI 16 色由语义 token 推导
/// （红/绿/黄/蓝分别对应 `state_error`/`state_success`/`state_warn`/`link`，
/// 洋红=红+蓝、青=绿+蓝的混合，亮色=同色相向默认前景混合 25%）。
pub fn terminal_view(theme: &Theme, lines: &[TermLine]) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    let palette = TermPalette::from_theme(theme);
    // 滚动容器必须带 id；用内容指纹做后缀，避免同屏多个终端卡片共享滚动状态。
    let scroll_id =
        ElementId::NamedInteger(SharedString::from("terminal-scroll"), fingerprint(lines));
    let container = div()
        .id(scroll_id)
        .flex()
        .flex_col()
        .w_full()
        .max_h(px(420.0))
        .rounded(theme.radii.md)
        .border_1()
        .border_color(p.color(p.border_l2))
        .bg(p.color(p.code_block))
        .overflow_x_scroll()
        .overflow_y_scroll();

    if lines.is_empty() {
        return container
            .child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_size(t.small)
                    .line_height(t.small_line)
                    .text_color(p.color(p.label_tertiary))
                    .child("（无终端输出）"),
            )
            .into_any_element();
    }

    let mut content = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(estimated_width(theme, lines)))
        .py(px(6.0));
    for line in lines {
        content = content.child(line_view(theme, &palette, line));
    }
    container.child(content).into_any_element()
}

/// 解析并渲染 ANSI 终端输出（`chat` 接线用的便捷入口）。
pub fn terminal_view_from_text(theme: &Theme, text: &str) -> AnyElement {
    terminal_view(theme, &parse_terminal(text).lines)
}

/// ANSI 色表：全部由主题 token 推导，避免在组件里写死颜色。
struct TermPalette {
    fg: Rgba,
    bg: Rgba,
    ansi: [Rgba; 16],
}

impl TermPalette {
    fn from_theme(theme: &Theme) -> Self {
        let p = theme.palette;
        let fg = p.color(p.label_primary);
        let red = p.color(p.state_error);
        let green = p.color(p.state_success);
        let yellow = p.color(p.state_warn);
        let blue = p.color(p.link);
        let magenta = mix(red, blue, 0.5);
        let cyan = mix(green, blue, 0.5);
        let ansi = [
            fg,                         // 0: 黑（用默认前景，浅色主题下才可见）
            red,                        // 1: 红
            green,                      // 2: 绿
            yellow,                     // 3: 黄
            blue,                       // 4: 蓝
            magenta,                    // 5: 洋红（红+蓝）
            cyan,                       // 6: 青（绿+蓝）
            p.color(p.label_tertiary),  // 7: 白/浅灰
            p.color(p.label_secondary), // 8: 亮黑
            mix(red, fg, 0.25),         // 9: 亮红
            mix(green, fg, 0.25),       // 10: 亮绿
            mix(yellow, fg, 0.25),      // 11: 亮黄
            mix(blue, fg, 0.25),        // 12: 亮蓝
            mix(magenta, fg, 0.25),     // 13: 亮洋红
            mix(cyan, fg, 0.25),        // 14: 亮青
            p.color(p.label_primary),   // 15: 亮白
        ];
        Self {
            fg,
            bg: p.color(p.code_block),
            ansi,
        }
    }

    fn resolve(&self, color: AnsiColor) -> Rgba {
        match color {
            AnsiColor::Indexed(index) => match index {
                0..=15 => self.ansi[index as usize],
                16..=231 => {
                    let cube = index as u32 - 16;
                    let level = |value: u32| -> u8 {
                        if value == 0 {
                            0
                        } else {
                            (55 + value * 40) as u8
                        }
                    };
                    rgb8(level(cube / 36), level((cube % 36) / 6), level(cube % 6))
                }
                _ => {
                    // 232–255：灰阶 8, 18, …, 238。
                    let gray = (8 + (index as u32 - 232) * 10) as u8;
                    rgb8(gray, gray, gray)
                }
            },
            AnsiColor::Rgb { r, g, b } => rgb8(r, g, b),
        }
    }
}

fn rgb8(r: u8, g: u8, b: u8) -> Rgba {
    rgba(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xff)
}

/// 按 `amount` 把 `other` 混合进 `base`（0.0 = base，1.0 = other）。
fn mix(base: Rgba, other: Rgba, amount: f32) -> Rgba {
    let amount = amount.clamp(0.0, 1.0);
    Rgba {
        r: base.r * (1.0 - amount) + other.r * amount,
        g: base.g * (1.0 - amount) + other.g * amount,
        b: base.b * (1.0 - amount) + other.b * amount,
        a: base.a * (1.0 - amount) + other.a * amount,
    }
}

fn code_font(theme: &Theme) -> Font {
    Font {
        family: SharedString::from(theme.font_code),
        ..Font::default()
    }
}

fn line_view(theme: &Theme, palette: &TermPalette, line: &TermLine) -> AnyElement {
    let t = theme.type_scale;
    let mut text = String::new();
    let mut runs = Vec::new();
    for run in &line.runs {
        let font = Font {
            weight: if run.style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if run.style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..code_font(theme)
        };
        let (fg, bg) = if run.style.reverse {
            (
                run.style
                    .bg
                    .map(|color| palette.resolve(color))
                    .unwrap_or(palette.bg),
                Some(
                    run.style
                        .fg
                        .map(|color| palette.resolve(color))
                        .unwrap_or(palette.fg),
                ),
            )
        } else {
            (
                run.style
                    .fg
                    .map(|color| palette.resolve(color))
                    .unwrap_or(palette.fg),
                run.style.bg.map(|color| palette.resolve(color)),
            )
        };
        let fg = if run.style.dim { fg.opacity(0.6) } else { fg };
        let underline = run.style.underline.then(|| UnderlineStyle {
            thickness: px(1.0),
            color: Some(fg.into()),
            wavy: false,
        });
        runs.push(TextRun {
            len: run.text.len(),
            font,
            color: fg.into(),
            background_color: bg.map(Into::into),
            underline,
            strikethrough: None,
        });
        text.push_str(&run.text);
    }
    if text.is_empty() {
        // 空行也要占一行高度。
        text.push(' ');
        runs.push(TextRun {
            len: 1,
            font: code_font(theme),
            color: palette.fg.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    div()
        .flex()
        .flex_row()
        .items_start()
        .w_full()
        .px(px(10.0))
        .py(px(1.0))
        .whitespace_nowrap()
        .text_size(t.small)
        .line_height(t.small_line)
        .child(StyledText::new(SharedString::from(text)).with_runs(runs))
        .into_any_element()
}

/// 估算横向滚动所需的最小宽度（等宽字体一个字符约 0.62em）。
fn estimated_width(theme: &Theme, lines: &[TermLine]) -> f32 {
    let max_chars = lines
        .iter()
        .map(|line| line.text().chars().count())
        .max()
        .unwrap_or(0);
    let char_width = theme.type_scale.small.as_f32() * 0.62;
    (max_chars as f32 * char_width + 24.0).max(240.0)
}

fn fingerprint<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Palette;

    fn first_line(input: &str) -> TermLine {
        parse_terminal(input)
            .lines
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    #[test]
    fn plain_text_and_newlines() {
        let terminal = parse_terminal("a\nb\r\nc");
        assert_eq!(terminal.lines.len(), 3, "{terminal:?}");
        assert_eq!(terminal.lines[0].text(), "a");
        assert_eq!(terminal.lines[1].text(), "b");
        assert_eq!(terminal.lines[2].text(), "c");
        assert_eq!(terminal.text(), "a\nb\nc");
    }

    #[test]
    fn carriage_return_overwrites_progress_line() {
        let terminal = parse_terminal("progress 10%\rprogress 100%\n");
        assert_eq!(terminal.lines.len(), 1, "{terminal:?}");
        assert_eq!(terminal.lines[0].text(), "progress 100%");
    }

    #[test]
    fn carriage_return_overwrites_only_prefix() {
        // 进度条常见形态：短串覆盖长串的前缀，余下字符保留。
        assert_eq!(first_line("abc\rXY").text(), "XYc");
        assert_eq!(first_line("10%\r9%").text(), "9%%");
    }

    #[test]
    fn backspace_moves_cursor_without_erasing() {
        assert_eq!(first_line("ab\x08X").text(), "aX");
        assert_eq!(first_line("ab\x08\x08\x08").text(), "ab");
    }

    #[test]
    fn tab_expands_to_eight_column_stops() {
        assert_eq!(first_line("a\tb").text(), "a       b");
        assert_eq!(first_line("12345678\tX").text(), "12345678        X");
    }

    #[test]
    fn sgr_sets_and_resets_colors() {
        let terminal = parse_terminal("\x1b[1;31mred\x1b[0mplain");
        assert_eq!(terminal.lines[0].runs.len(), 2, "{terminal:?}");
        let red = &terminal.lines[0].runs[0];
        assert_eq!(red.text, "red");
        assert_eq!(red.style.fg, Some(AnsiColor::Indexed(1)));
        assert!(red.style.bold);
        let plain = &terminal.lines[0].runs[1];
        assert_eq!(plain.text, "plain");
        assert_eq!(plain.style, TermStyle::default());
    }

    #[test]
    fn sgr_attributes_turn_off() {
        let style = first_line("\x1b[1;2;3;4;7m");
        // 只写属性没有文本，结果为空行；改用带文本 + 关闭序列断言。
        assert!(style.is_empty());

        let terminal = parse_terminal("\x1b[1;2;3;4;7mX\x1b[22;23;24;27mY");
        let runs = &terminal.lines[0].runs;
        assert_eq!(runs[0].text, "X");
        assert!(runs[0].style.bold && runs[0].style.dim);
        assert!(runs[0].style.italic && runs[0].style.underline && runs[0].style.reverse);
        assert_eq!(runs[1].text, "Y");
        assert!(!runs[1].style.bold && !runs[1].style.dim);
        assert!(!runs[1].style.italic && !runs[1].style.underline && !runs[1].style.reverse);
    }

    #[test]
    fn sgr_background_bright_and_default_reset() {
        let terminal = parse_terminal("\x1b[42mA\x1b[101mB\x1b[39;49mC");
        let runs = &terminal.lines[0].runs;
        assert_eq!(runs[0].style.bg, Some(AnsiColor::Indexed(2)));
        assert_eq!(runs[1].style.bg, Some(AnsiColor::Indexed(9)));
        assert_eq!(runs[2].style.bg, None);
        assert_eq!(runs[2].style.fg, None);
    }

    #[test]
    fn sgr_256_and_truecolor_are_optional_but_parsed() {
        let terminal = parse_terminal("\x1b[38;5;196mA\x1b[48;5;21mB\x1b[38;2;10;20;30mC");
        let runs = &terminal.lines[0].runs;
        assert_eq!(runs[0].style.fg, Some(AnsiColor::Indexed(196)));
        assert_eq!(runs[1].style.bg, Some(AnsiColor::Indexed(21)));
        assert_eq!(
            runs[2].style.fg,
            Some(AnsiColor::Rgb {
                r: 10,
                g: 20,
                b: 30
            })
        );
        assert_eq!(terminal.unsupported, 0);
    }

    #[test]
    fn cursor_movement_and_erase_line() {
        assert_eq!(first_line("abc\x1b[2DX").text(), "aXc");
        assert_eq!(first_line("abc\x1b[K").text(), "abc");
        assert_eq!(first_line("abc\x1b[2DX\x1b[K").text(), "aX");
        assert_eq!(first_line("abc\x1b[2K").text(), "");
        assert_eq!(first_line("abc\x1b[1K").text(), "");
        assert_eq!(first_line("abc\x1b[1;1HX").text(), "Xbc");
        assert_eq!(first_line("\x1b[2;3Hx\x1b[1;1HY").text(), "Y");
    }

    #[test]
    fn erase_display_clears_output() {
        let terminal = parse_terminal("line1\nline2\x1b[2Jafter");
        assert_eq!(terminal.lines.len(), 1, "{terminal:?}");
        assert_eq!(terminal.lines[0].text(), "after");

        let terminal = parse_terminal("line1\nline2\x1b[1G\x1b[J");
        assert_eq!(terminal.lines.len(), 1, "{terminal:?}");
        assert_eq!(terminal.lines[0].text(), "line1");
    }

    #[test]
    fn osc_is_dropped_and_does_not_leak_into_text() {
        let terminal =
            parse_terminal("\x1b]0;window title\x07ok\x1b]8;;https://example.com\x1b\\link");
        assert_eq!(terminal.text(), "oklink");
        assert_eq!(terminal.unsupported, 0);
    }

    #[test]
    fn unknown_sequences_degrade_safely() {
        // 私有模式（光标可见性）被识别并忽略。
        let terminal = parse_terminal("\x1b[?25lhidden?\x1b[?25h");
        assert_eq!(terminal.text(), "hidden?");
        assert_eq!(terminal.unsupported, 0);

        // 未知 CSI 只计数，不进入文本。
        let terminal = parse_terminal("a\x1b[99zb");
        assert_eq!(terminal.text(), "ab");
        assert_eq!(terminal.unsupported, 1);

        // 未知的两字节 ESC 序列（字符集）不吞掉后续文本。
        let terminal = parse_terminal("\x1b(Btext");
        assert_eq!(terminal.text(), "text");

        // 截断的 CSI 不 panic。
        let terminal = parse_terminal("abc\x1b[31");
        assert_eq!(terminal.text(), "abc");
        assert_eq!(terminal.unsupported, 1);
    }

    #[test]
    fn empty_output_is_empty_and_renders_without_panic() {
        let terminal = parse_terminal("");
        assert!(terminal.lines.is_empty());
        assert_eq!(terminal.unsupported, 0);
        let _ = terminal_view(&Theme::light(), &terminal.lines);
        let _ = terminal_view_from_text(&Theme::dark(), "");
    }

    #[test]
    fn line_cap_drops_oldest_lines() {
        let input = "x\n".repeat(MAX_LINES + 10);
        let terminal = parse_terminal(&input);
        assert!(terminal.truncated, "{:?}", terminal.lines.len());
        assert!(terminal.lines.len() <= MAX_LINES);
    }

    #[test]
    fn ansi_palette_is_derived_from_theme_tokens() {
        let theme = Theme::light();
        let palette = TermPalette::from_theme(&theme);
        let p = Palette::LIGHT;
        assert_eq!(palette.fg, theme.palette.color(p.label_primary));
        assert_eq!(palette.ansi[0], theme.palette.color(p.label_primary));
        assert_eq!(palette.ansi[1], theme.palette.color(p.state_error));
        assert_eq!(palette.ansi[2], theme.palette.color(p.state_success));
        assert_eq!(palette.ansi[3], theme.palette.color(p.state_warn));
        assert_eq!(palette.ansi[4], theme.palette.color(p.link));
        assert_eq!(palette.ansi[7], theme.palette.color(p.label_tertiary));
        assert_eq!(palette.bg, theme.palette.color(p.code_block));
    }

    #[test]
    fn indexed_256_colors_follow_xterm_formula() {
        let palette = TermPalette::from_theme(&Theme::dark());
        // 196 = 16 + 36*5：纯红；232 = 灰阶 8；255 = 灰阶 238。
        assert_eq!(palette.resolve(AnsiColor::Indexed(196)), rgb8(255, 0, 0));
        assert_eq!(palette.resolve(AnsiColor::Indexed(232)), rgb8(8, 8, 8));
        assert_eq!(
            palette.resolve(AnsiColor::Indexed(255)),
            rgb8(238, 238, 238)
        );
    }

    #[test]
    fn view_renders_styled_and_plain_output_without_panic() {
        for input in [
            "",
            "plain\ntext",
            "\x1b[1;31mred\x1b[0m plain",
            "\x1b]0;title\x07\x1b[?25l\x1b[38;2;1;2;3mtruecolor",
            "\x1b[2Jafter",
        ] {
            let terminal = parse_terminal(input);
            let _ = terminal_view(&Theme::light(), &terminal.lines);
            let _ = terminal_view_from_text(&Theme::dark(), input);
        }
    }
}
