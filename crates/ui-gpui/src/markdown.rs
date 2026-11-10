//! Markdown → GPUI 元素（DSH 对话正文的渲染子集）。
//!
//! 目标不是完整 CommonMark，而是模型回复里真正会出现的结构：
//! 标题、段落、加粗/斜体/行内代码/链接、围栏代码块、列表、引用、表格、分隔线。
//!
//! 设计取舍：
//! - **解析与渲染分离**：`parse` 是纯函数，测试断言块结构；渲染只做样式映射。
//! - 行内样式走 `StyledText` 的 `TextRun`，这样换行/折行由 GPUI 文本系统处理，
//!   不会出现"每个 span 一个 div 导致断行错位"的问题。
//! - 未闭合的标记按字面量渲染，不丢文本。

use gpui::{
    div, prelude::*, px, AnyElement, Font, FontStyle, FontWeight, Rgba, SharedString, StyledText,
    TextRun, UnderlineStyle,
};

use crate::theme::Theme;

/// 行内片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Strong(String),
    Emph(String),
    Code(String),
    Link { text: String, href: String },
}

/// 列表项：缩进级别 + 行内内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub level: usize,
    pub spans: Vec<Inline>,
}

/// 块级结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Code {
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Quote(Vec<Block>),
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    Blank,
}

/// 解析 Markdown。空行合并为一个 `Blank`，渲染时忽略即可。
pub fn parse(input: &str) -> Vec<Block> {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    parse_lines(&normalized.split('\n').collect::<Vec<_>>())
}

fn parse_lines(lines: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if !matches!(blocks.last(), Some(Block::Blank)) {
                blocks.push(Block::Blank);
            }
            index += 1;
            continue;
        }

        // 围栏代码块
        if let Some((lang, fence)) = fence_start(trimmed) {
            let mut code = Vec::new();
            index += 1;
            while index < lines.len() {
                let candidate = lines[index].trim_start();
                if candidate.starts_with(fence) {
                    index += 1;
                    break;
                }
                code.push(lines[index]);
                index += 1;
            }
            blocks.push(Block::Code {
                lang,
                text: code.join("\n"),
            });
            continue;
        }

        // 标题
        if let Some((level, rest)) = heading(trimmed) {
            blocks.push(Block::Heading {
                level,
                spans: parse_inline(rest),
            });
            index += 1;
            continue;
        }

        // 分隔线
        if is_rule(trimmed) {
            blocks.push(Block::Rule);
            index += 1;
            continue;
        }

        // 引用：收集连续 `>` 行，内部递归解析
        if trimmed.starts_with('>') {
            let mut inner = Vec::new();
            while index < lines.len() {
                let current = lines[index].trim_start();
                let Some(rest) = current.strip_prefix('>') else {
                    break;
                };
                inner.push(rest.strip_prefix(' ').unwrap_or(rest));
                index += 1;
            }
            blocks.push(Block::Quote(parse_lines(&inner)));
            continue;
        }

        // 表格：当前行含 `|` 且下一行是分隔行
        if trimmed.contains('|') && index + 1 < lines.len() && is_table_separator(lines[index + 1])
        {
            let header = split_row(trimmed);
            let mut rows = Vec::new();
            index += 2;
            while index < lines.len() {
                let candidate = lines[index].trim();
                if !candidate.contains('|') {
                    break;
                }
                rows.push(split_row(candidate));
                index += 1;
            }
            blocks.push(Block::Table { header, rows });
            continue;
        }

        // 列表
        if let Some((ordered, start, _, _)) = list_marker(line) {
            let mut items = Vec::new();
            while index < lines.len() {
                let current = lines[index];
                if current.trim().is_empty() {
                    break;
                }
                let Some((_, _, content, indent)) = list_marker(current) else {
                    break;
                };
                items.push(ListItem {
                    level: (indent / 2).min(3),
                    spans: parse_inline(content),
                });
                index += 1;
            }
            blocks.push(Block::List {
                ordered,
                start,
                items,
            });
            continue;
        }

        // 段落：连续普通行，用空格连接（Markdown 语义）
        let mut paragraph = String::new();
        while index < lines.len() {
            let current = lines[index];
            let current_trimmed = current.trim();
            if current_trimmed.is_empty()
                || heading(current_trimmed).is_some()
                || is_rule(current_trimmed)
                || current_trimmed.starts_with('>')
                || current_trimmed.starts_with("```")
                || current_trimmed.starts_with("~~~")
                || list_marker(current).is_some()
                || (current_trimmed.contains('|')
                    && index + 1 < lines.len()
                    && is_table_separator(lines[index + 1]))
            {
                break;
            }
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(current_trimmed);
            index += 1;
        }
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(parse_inline(&paragraph)));
        }
    }

    blocks
}

fn fence_start(trimmed: &str) -> Option<(Option<String>, &'static str)> {
    if let Some(rest) = trimmed.strip_prefix("```") {
        let lang = rest.trim();
        return Some(((!lang.is_empty()).then(|| lang.to_owned()), "```"));
    }
    if trimmed.starts_with("~~~") {
        return Some((None, "~~~"));
    }
    None
}

fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed[level..].strip_prefix(' ').unwrap_or("");
    if rest.is_empty() || rest.starts_with('#') {
        return None;
    }
    Some((level as u8, rest))
}

fn is_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    matches!(compact.as_str(), "---" | "***" | "___")
}

fn list_marker(line: &str) -> Option<(bool, u64, &str, usize)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            if let Some(content) = rest.strip_prefix(' ') {
                return Some((false, 1, content, indent));
            }
        }
    }
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &trimmed[digits.len()..];
    if let Some(content) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
        return Some((true, digits.parse().unwrap_or(0), content, indent));
    }
    None
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('-') {
        return false;
    }
    trimmed
        .chars()
        .all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t'))
}

fn split_row(line: &str) -> Vec<Vec<Inline>> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    trimmed
        .split('|')
        .map(|cell| parse_inline(cell.trim()))
        .collect()
}

/// 行内解析：`**粗体**`、`*斜体*`、`` `代码` ``、`[文本](链接)`。
pub fn parse_inline(input: &str) -> Vec<Inline> {
    let chars: Vec<char> = input.chars().collect();
    let mut spans = Vec::new();
    let mut text = String::new();
    let mut index = 0usize;

    let flush = |text: &mut String, spans: &mut Vec<Inline>| {
        if !text.is_empty() {
            spans.push(Inline::Text(std::mem::take(text)));
        }
    };

    while index < chars.len() {
        let ch = chars[index];
        // **strong**
        if ch == '*' && chars.get(index + 1) == Some(&'*') {
            if let Some(end) = find_sequence(&chars, index + 2, &['*', '*']) {
                flush(&mut text, &mut spans);
                let content: String = chars[index + 2..end].iter().collect();
                spans.push(Inline::Strong(content));
                index = end + 2;
                continue;
            }
        }
        // *emph*（不匹配 **）
        if ch == '*' {
            if let Some(end) = find_char(&chars, index + 1, '*') {
                if end > index + 1 {
                    flush(&mut text, &mut spans);
                    let content: String = chars[index + 1..end].iter().collect();
                    spans.push(Inline::Emph(content));
                    index = end + 1;
                    continue;
                }
            }
        }
        // `code`
        if ch == '`' {
            if let Some(end) = find_char(&chars, index + 1, '`') {
                flush(&mut text, &mut spans);
                let content: String = chars[index + 1..end].iter().collect();
                spans.push(Inline::Code(content));
                index = end + 1;
                continue;
            }
        }
        // [text](href)
        if ch == '[' {
            if let Some((label, href, next)) = parse_link(&chars, index) {
                flush(&mut text, &mut spans);
                spans.push(Inline::Link { text: label, href });
                index = next;
                continue;
            }
        }
        text.push(ch);
        index += 1;
    }

    flush(&mut text, &mut spans);
    if spans.is_empty() {
        spans.push(Inline::Text(String::new()));
    }
    spans
}

fn find_char(chars: &[char], start: usize, needle: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == needle)
}

fn find_sequence(chars: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if needle.len() > chars.len() {
        return None;
    }
    (start..=chars.len() - needle.len())
        .find(|index| chars[*index..*index + needle.len()] == *needle)
}

fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find_char(chars, start + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let href_close = find_char(chars, close + 2, ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    let href: String = chars[close + 2..href_close].iter().collect();
    Some((label, href, href_close + 1))
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 把 Markdown 渲染成 GPUI 元素。
///
/// `base_font` / `base_color` 来自当前文本样式（`window.text_style()`），
/// 这样正文字体与页面其它文本一致，只有行内强调/代码/链接会覆盖。
pub fn markdown_view(text: &str, theme: &Theme, base_font: &Font, base_color: Rgba) -> AnyElement {
    let blocks = parse(text);
    let mut column = div().flex().flex_col().gap(px(10.0)).w_full();
    for block in &blocks {
        if matches!(block, Block::Blank) {
            continue;
        }
        column = column.child(render_block(block, theme, base_font, base_color));
    }
    if blocks.is_empty() {
        column = column.child(
            div()
                .text_size(theme.type_scale.body)
                .text_color(theme.palette.color(theme.palette.label_primary))
                .child(text.to_owned()),
        );
    }
    column.into_any_element()
}

fn render_block(block: &Block, theme: &Theme, base_font: &Font, base_color: Rgba) -> AnyElement {
    let t = theme.type_scale;
    let p = theme.palette;
    match block {
        Block::Heading { level, spans } => {
            let (size, line, weight) = match level {
                1 => (t.h1, t.h1_line, FontWeight::BOLD),
                2 => (t.h2, t.h2_line, FontWeight::BOLD),
                3 => (t.h3, t.h3_line, FontWeight::SEMIBOLD),
                _ => (t.body, t.body_line, FontWeight::SEMIBOLD),
            };
            text_element(
                spans,
                theme,
                base_font,
                base_color,
                size,
                line,
                weight,
                p.label_primary,
            )
            .into_any_element()
        }
        Block::Paragraph(spans) => text_element(
            spans,
            theme,
            base_font,
            base_color,
            t.body,
            t.body_line,
            FontWeight::NORMAL,
            p.label_primary,
        )
        .into_any_element(),
        Block::Code { lang, text } => {
            let mut container = div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(theme.radii.md)
                .border_1()
                .border_color(p.color(p.border_l2))
                .bg(p.color(p.code_block))
                .overflow_hidden();
            if let Some(lang) = lang {
                container = container.child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(6.0))
                        .border_b_1()
                        .border_color(p.color(p.border_l1))
                        .bg(p.color(p.code_block_banner))
                        .text_size(t.small)
                        .text_color(p.color(p.label_tertiary))
                        .child(lang.clone()),
                );
            }
            let (code_text, runs) = inline_runs(
                &[Inline::Code(text.clone())],
                theme,
                base_font,
                base_color,
                FontWeight::NORMAL,
            );
            container
                .child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(10.0))
                        .text_size(t.small)
                        .line_height(t.small_line)
                        .text_color(p.color(p.label_primary))
                        .child(StyledText::new(SharedString::from(code_text)).with_runs(runs)),
                )
                .into_any_element()
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            let mut list = div().flex().flex_col().gap(px(4.0)).w_full();
            for (index, item) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}.", start + index as u64)
                } else {
                    "•".to_owned()
                };
                let (row_text, runs) = inline_runs(
                    &item.spans,
                    theme,
                    base_font,
                    base_color,
                    FontWeight::NORMAL,
                );
                list = list.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(8.0))
                        .pl(px(item.level as f32 * 16.0))
                        .child(
                            div()
                                .w(px(20.0))
                                .flex_none()
                                .pt(px(2.0))
                                .text_size(t.body)
                                .line_height(t.body_line)
                                .text_color(p.color(p.label_secondary))
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(t.body)
                                .line_height(t.body_line)
                                .text_color(p.color(p.label_primary))
                                .child(
                                    StyledText::new(SharedString::from(row_text)).with_runs(runs),
                                ),
                        ),
                );
            }
            list.into_any_element()
        }
        Block::Quote(inner) => {
            let mut quoted = div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .pl(px(12.0))
                .py(px(2.0))
                .border_l_1()
                .border_color(p.color(p.border_l4));
            for block in inner {
                if matches!(block, Block::Blank) {
                    continue;
                }
                quoted = quoted.child(render_block(block, theme, base_font, base_color));
            }
            quoted.into_any_element()
        }
        Block::Table { header, rows } => {
            let mut table = div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(theme.radii.sm)
                .border_1()
                .border_color(p.color(p.border_l2))
                .overflow_hidden();
            let mut header_row = div()
                .flex()
                .w_full()
                .bg(p.color(p.tag))
                .border_b_1()
                .border_color(p.color(p.border_l1));
            for cell in header {
                header_row = header_row.child(cell_element(
                    cell,
                    theme,
                    base_font,
                    base_color,
                    FontWeight::SEMIBOLD,
                ));
            }
            table = table.child(header_row);
            for row in rows {
                let mut row_element = div()
                    .flex()
                    .w_full()
                    .border_b_1()
                    .border_color(p.color(p.border_l1));
                for cell in row {
                    row_element = row_element.child(cell_element(
                        cell,
                        theme,
                        base_font,
                        base_color,
                        FontWeight::NORMAL,
                    ));
                }
                table = table.child(row_element);
            }
            table.into_any_element()
        }
        Block::Rule => div()
            .w_full()
            .py(px(6.0))
            .child(div().w_full().h(px(1.0)).bg(p.color(p.border_l2)))
            .into_any_element(),
        Block::Blank => div().into_any_element(),
    }
}

fn cell_element(
    spans: &[Inline],
    theme: &Theme,
    base_font: &Font,
    base_color: Rgba,
    weight: FontWeight,
) -> AnyElement {
    let t = theme.type_scale;
    let p = theme.palette;
    let (text, runs) = inline_runs(spans, theme, base_font, base_color, weight);
    div()
        .flex_1()
        .min_w(px(0.0))
        .px(px(10.0))
        .py(px(6.0))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_color(p.color(p.label_primary))
        .child(StyledText::new(SharedString::from(text)).with_runs(runs))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn text_element(
    spans: &[Inline],
    theme: &Theme,
    base_font: &Font,
    base_color: Rgba,
    size: gpui::Pixels,
    line: gpui::Pixels,
    weight: FontWeight,
    color: u32,
) -> impl IntoElement {
    let (text, runs) = inline_runs(spans, theme, base_font, base_color, weight);
    div()
        .w_full()
        .text_size(size)
        .line_height(line)
        .text_color(theme.palette.color(color))
        .child(StyledText::new(SharedString::from(text)).with_runs(runs))
}

/// 把行内片段编译成 `(纯文本, TextRun 序列)`；`len` 是 UTF-8 字节数。
fn inline_runs(
    spans: &[Inline],
    theme: &Theme,
    base_font: &Font,
    base_color: Rgba,
    default_weight: FontWeight,
) -> (String, Vec<TextRun>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    for span in spans {
        let (content, font, color, background, underline) = match span {
            Inline::Text(value) => (
                value.clone(),
                Font {
                    weight: default_weight,
                    ..base_font.clone()
                },
                base_color.into(),
                None,
                None,
            ),
            Inline::Strong(value) => (
                value.clone(),
                Font {
                    weight: FontWeight::BOLD,
                    ..base_font.clone()
                },
                base_color.into(),
                None,
                None,
            ),
            Inline::Emph(value) => (
                value.clone(),
                Font {
                    weight: default_weight,
                    style: FontStyle::Italic,
                    ..base_font.clone()
                },
                base_color.into(),
                None,
                None,
            ),
            Inline::Code(value) => (
                value.clone(),
                Font {
                    family: SharedString::from(theme.font_code),
                    ..base_font.clone()
                },
                base_color.into(),
                Some(theme.palette.color(theme.palette.tag).into()),
                None,
            ),
            Inline::Link { text: value, .. } => (
                value.clone(),
                Font {
                    weight: default_weight,
                    ..base_font.clone()
                },
                theme.palette.color(theme.palette.link).into(),
                None,
                Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(theme.palette.color(theme.palette.link).into()),
                    wavy: false,
                }),
            ),
        };
        if content.is_empty() {
            continue;
        }
        runs.push(TextRun {
            len: content.len(),
            font,
            color,
            background_color: background,
            underline,
            strikethrough: None,
        });
        text.push_str(&content);
    }
    if runs.is_empty() {
        runs.push(TextRun {
            len: 0,
            font: base_font.clone(),
            color: base_color.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    (text, runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(spans: &[Inline]) -> String {
        spans
            .iter()
            .map(|span| match span {
                Inline::Text(text)
                | Inline::Strong(text)
                | Inline::Emph(text)
                | Inline::Code(text) => text.clone(),
                Inline::Link { text, .. } => text.clone(),
            })
            .collect()
    }

    #[test]
    fn headings_and_paragraphs() {
        let blocks = parse("## 标题\n\n正文 **粗体** 和 `code`。");
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(matches!(
            &blocks[0],
            Block::Heading { level: 2, spans } if text_of(spans) == "标题"
        ));
        assert!(matches!(&blocks[1], Block::Blank));
        assert!(matches!(
            &blocks[2],
            Block::Paragraph(spans)
                if text_of(spans) == "正文 粗体 和 code。"
                    && spans.iter().any(|s| matches!(s, Inline::Strong(_)))
                    && spans.iter().any(|s| matches!(s, Inline::Code(_)))
        ));
    }

    #[test]
    fn fenced_code_keeps_language_and_lines() {
        let blocks = parse("```rust\nlet a = 1;\nlet b = 2;\n```\n");
        assert!(
            matches!(
                &blocks[0],
                Block::Code { lang: Some(lang), text }
                    if lang == "rust" && text == "let a = 1;\nlet b = 2;"
            ),
            "{blocks:?}"
        );
    }

    #[test]
    fn unclosed_fence_takes_rest_of_input() {
        let blocks = parse("```\nabc\n");
        assert!(matches!(
            &blocks[0],
            Block::Code { lang: None, text } if text.trim_end() == "abc"
        ));
    }

    #[test]
    fn unordered_and_ordered_lists() {
        let blocks = parse("- one\n- two\n\n1. first\n2. second\n");
        assert!(matches!(
            &blocks[0],
            Block::List { ordered: false, items, .. }
                if items.len() == 2 && text_of(&items[1].spans) == "two"
        ));
        assert!(matches!(
            &blocks[2],
            Block::List { ordered: true, start: 1, items }
                if items.len() == 2 && text_of(&items[0].spans) == "first"
        ));
    }

    #[test]
    fn nested_list_indent_is_preserved() {
        let blocks = parse("- top\n  - child\n");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("expected list: {blocks:?}");
        };
        assert_eq!(items[0].level, 0);
        assert_eq!(items[1].level, 1);
    }

    #[test]
    fn tables_parse_header_and_rows() {
        let blocks = parse("| 名称 | 值 |\n|---|---|\n| a | 1 |\n| b | 2 |\n");
        let Block::Table { header, rows } = &blocks[0] else {
            panic!("expected table: {blocks:?}");
        };
        assert_eq!(header.len(), 2);
        assert_eq!(text_of(&header[0]), "名称");
        assert_eq!(rows.len(), 2);
        assert_eq!(text_of(&rows[1][1]), "2");
    }

    #[test]
    fn blockquote_parses_inner_blocks() {
        let blocks = parse("> 引用 **重点**\n");
        let Block::Quote(inner) = &blocks[0] else {
            panic!("expected quote: {blocks:?}");
        };
        assert!(matches!(
            &inner[0],
            Block::Paragraph(spans) if text_of(spans) == "引用 重点"
        ));
    }

    #[test]
    fn link_and_emphasis_inline() {
        let spans = parse_inline("看 [文档](https://example.com) *稍后*");
        assert!(matches!(
            &spans[1],
            Inline::Link { text, href }
                if text == "文档" && href == "https://example.com"
        ));
        assert!(matches!(&spans[3], Inline::Emph(text) if text == "稍后"));
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        let spans = parse_inline("a * b");
        assert_eq!(text_of(&spans), "a * b");
        let spans = parse_inline("**未闭合");
        assert_eq!(text_of(&spans), "**未闭合");
    }

    #[test]
    fn rule_is_recognized() {
        assert!(matches!(parse("---\n")[0], Block::Rule));
        assert!(matches!(parse("***\n")[0], Block::Rule));
        assert!(!matches!(parse("--\n")[0], Block::Rule));
    }
}
