//! `presentResult` 卡片：把工具结果折叠成可渲染的文本视图。
//!
//! 三条硬约束：
//! 1. **确定性**：同一份 `value` / `content` 必须得到同一张卡片（回放才能逐项相等）；
//! 2. **有界**：预览行数与单行长度都有上限，巨大输出不能撑爆 UI；
//! 3. **不丢结果**：形状不认识时退回 `Generic`，而不是把结果藏起来。
//!
//! 卡片只做"内容 → 文本"的翻译，不依赖 GPUI：这样卡片逻辑可以在无窗口测试里断言，
//! 与回放（spec §6）用的是同一条代码路径。

use serde_json::Value;

use dshr_tool_runtime::Presentation;

/// 预览最多几行（超出用"还有 N 行"收尾）。
const MAX_PREVIEW_LINES: usize = 6;
/// 单行最多多少字符（按 char 截断，避免切坏 UTF-8）。
const MAX_LINE_CHARS: usize = 120;
/// 原生视图（Diff/Terminal/代码块）可用的完整文本上限。预览只够列表，视图要更多内容。
const MAX_BODY_CHARS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Generic,
    Terminal,
    Diff,
    Search,
    Read,
    Web,
    /// 失败或中止：不是工具声明的卡片，而是终态本身。
    Error,
}

/// 一张工具结果卡片的**全部可渲染内容**。
///
/// 故意全部用 `String`/`Vec<String>`：卡片一旦折叠出来就不再依赖 JSON，
/// 回放/快照/测试都只比较这些字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    pub kind: CardKind,
    pub title: String,
    pub detail: String,
    pub preview: Vec<String>,
    /// 原生视图用的完整文本（已按 `MAX_BODY_CHARS` 截断）；预览之外的补充。
    pub body: String,
    /// 工具卡附加 ADC 区域（`tool.result.meta.ui`；M4 W3）。None = 没有附加区。
    pub extra: Option<Value>,
}

impl ToolCard {
    /// 单行摘要：给只能显示一行的界面（以及自检输出）用。
    pub fn summary(&self) -> String {
        let kind = match self.kind {
            CardKind::Generic => "通用",
            CardKind::Terminal => "终端",
            CardKind::Diff => "差异",
            CardKind::Search => "搜索",
            CardKind::Read => "读取",
            CardKind::Web => "网页",
            CardKind::Error => "错误",
        };
        let mut line = if self.detail.is_empty() {
            format!("{kind} · {}", self.title)
        } else {
            format!("{kind} · {} · {}", self.title, self.detail)
        };
        if let Some(preview) = self.preview.first() {
            // 与 detail 相同就不重复：通用卡片两者都源自同一份 JSON。
            if *preview != self.detail {
                line.push_str(" · ");
                line.push_str(preview);
            }
        }
        // 单行展示必须有界，否则长输出会把一行撑到屏幕外。
        if line.chars().count() > 110 {
            line = line.chars().take(109).collect::<String>() + "…";
        }
        line
    }
}

/// 按插件声明的卡片类型折叠结果；形状不匹配时退回 `Generic`。
pub fn card_for(
    tool: &str,
    presentation: Presentation,
    value: &Value,
    content: &Value,
) -> ToolCard {
    card_for_with_extra(tool, presentation, value, content, None)
}

/// 与 [`card_for`] 相同，但保留 `tool.result.meta.ui` 的 ADC 附加区域。
pub fn card_for_with_extra(
    tool: &str,
    presentation: Presentation,
    value: &Value,
    content: &Value,
    extra: Option<&Value>,
) -> ToolCard {
    let typed = match presentation {
        Presentation::Terminal => terminal(value),
        Presentation::Diff => diff(value),
        Presentation::Search => search(value),
        Presentation::Read => read(value),
        Presentation::Web => web(value),
        Presentation::Generic => None,
    };
    let mut card = typed.unwrap_or_else(|| generic(tool, value, content));
    if let Some(extra) = extra {
        card.extra = Some(extra.clone());
    }
    card
}

/// 失败 / 中止的卡片：标题区分终态，正文是原因或错误内容。
pub fn error_card(title: &str, detail: &str) -> ToolCard {
    ToolCard {
        kind: CardKind::Error,
        title: title.to_owned(),
        detail: clip(detail, MAX_LINE_CHARS),
        preview: preview_lines(detail, MAX_PREVIEW_LINES),
        body: clip_body(detail),
        extra: None,
    }
}

fn generic(tool: &str, value: &Value, content: &Value) -> ToolCard {
    let detail = match value {
        Value::Null => clip(&content_text(content), MAX_LINE_CHARS),
        other => clip(&other.to_string(), MAX_LINE_CHARS),
    };
    ToolCard {
        kind: CardKind::Generic,
        title: tool.to_owned(),
        detail,
        preview: match value {
            Value::Null => preview_lines(&content_text(content), MAX_PREVIEW_LINES),
            other => pretty_lines(other, MAX_PREVIEW_LINES),
        },
        // 通用卡片没有专用视图；body 留空避免每张卡片都复制一份 JSON。
        body: String::new(),
        extra: None,
    }
}

fn terminal(value: &Value) -> Option<ToolCard> {
    let object = value.as_object()?;
    if !["exit_code", "code", "stdout", "stderr"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return None;
    }
    let code = object
        .get("exit_code")
        .or_else(|| object.get("code"))
        .and_then(Value::as_i64);
    let stdout = object.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = object.get("stderr").and_then(Value::as_str).unwrap_or("");

    let mut preview = preview_lines(stdout, MAX_PREVIEW_LINES);
    if !stderr.is_empty() {
        // stderr 与 stdout 同样重要：分开标记，且在预览里给它留位置。
        preview.push("stderr:".to_owned());
        preview.extend(
            stderr
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(3)
                .map(|line| clip(&format!("! {line}"), MAX_LINE_CHARS)),
        );
    }

    let mut body = stdout.to_owned();
    if !stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(stderr);
    }

    Some(ToolCard {
        kind: CardKind::Terminal,
        title: match code {
            Some(code) => format!("exit {code}"),
            None => "exit ?".to_owned(),
        },
        detail: format!(
            "{} 行输出 · {} 行错误",
            stdout.lines().count(),
            stderr.lines().count()
        ),
        preview,
        // 终端视图需要 stdout + stderr 的完整输出（有界），供 ANSI 渲染。
        body: clip_body(&body),
        extra: None,
    })
}

fn diff(value: &Value) -> Option<ToolCard> {
    let object = value.as_object()?;
    let text = object
        .get("diff")
        .or_else(|| object.get("patch"))
        .and_then(Value::as_str);
    let declared_added = object.get("added").and_then(Value::as_u64);
    let declared_removed = object.get("removed").and_then(Value::as_u64);
    if text.is_none() && declared_added.is_none() && declared_removed.is_none() {
        return None;
    }
    let (added, removed) = match (declared_added, declared_removed, text) {
        (Some(added), Some(removed), _) => (added, removed),
        (_, _, Some(text)) => count_diff(text),
        _ => (0, 0),
    };
    let path = object
        .get("path")
        .or_else(|| object.get("file"))
        .and_then(Value::as_str)
        .unwrap_or("diff");

    let preview = match text {
        Some(text) => text
            .lines()
            .filter(|line| {
                (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
            })
            .take(MAX_PREVIEW_LINES)
            .map(|line| clip(line, MAX_LINE_CHARS))
            .collect(),
        None => Vec::new(),
    };

    Some(ToolCard {
        kind: CardKind::Diff,
        title: path.to_owned(),
        detail: format!("+{added} −{removed}"),
        preview,
        // 差异视图需要完整 patch（有界），行号/hunk 由 UI 侧解析。
        body: clip_body(text.unwrap_or("")),
        extra: None,
    })
}

fn search(value: &Value) -> Option<ToolCard> {
    let object = value.as_object()?;
    let matches = object
        .get("matches")
        .or_else(|| object.get("results"))
        .and_then(Value::as_array)?;
    let total = object
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(matches.len() as u64);
    let query = object
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("search");

    Some(ToolCard {
        kind: CardKind::Search,
        title: query.to_owned(),
        detail: format!("{total} 处匹配"),
        preview: matches
            .iter()
            .take(MAX_PREVIEW_LINES)
            .map(match_line)
            .collect(),
        body: String::new(),
        extra: None,
    })
}

fn match_line(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return clip(&value.to_string(), MAX_LINE_CHARS);
    };
    let path = object
        .get("path")
        .or_else(|| object.get("file"))
        .and_then(Value::as_str);
    let line = object.get("line").and_then(Value::as_u64);
    let text = object
        .get("text")
        .or_else(|| object.get("match"))
        .and_then(Value::as_str);
    let text = text.map(str::trim).unwrap_or_default();
    match (path, line) {
        (Some(path), Some(line)) => clip(&format!("{path}:{line}  {text}"), MAX_LINE_CHARS),
        (Some(path), None) => clip(&format!("{path}  {text}"), MAX_LINE_CHARS),
        _ => clip(text, MAX_LINE_CHARS),
    }
}

fn read(value: &Value) -> Option<ToolCard> {
    let object = value.as_object()?;
    let path = object.get("path").and_then(Value::as_str)?;
    let text = object
        .get("text")
        .or_else(|| object.get("content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            object.get("lines").and_then(Value::as_array).map(|lines| {
                lines
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })?;

    Some(ToolCard {
        kind: CardKind::Read,
        title: path.to_owned(),
        detail: format!("{} 行", text.lines().count()),
        preview: preview_lines(&text, MAX_PREVIEW_LINES),
        body: clip_body(&text),
        extra: None,
    })
}

fn web(value: &Value) -> Option<ToolCard> {
    let object = value.as_object()?;
    let url = object.get("url").and_then(Value::as_str)?;
    let title = object.get("title").and_then(Value::as_str).unwrap_or(url);
    let snippet = object
        .get("snippet")
        .or_else(|| object.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");

    Some(ToolCard {
        kind: CardKind::Web,
        title: title.to_owned(),
        detail: url.to_owned(),
        preview: preview_lines(snippet, 3),
        body: clip_body(snippet),
        extra: None,
    })
}

fn count_diff(text: &str) -> (u64, u64) {
    let mut added = 0;
    let mut removed = 0;
    for line in text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// `content` 块的纯文本投影（text 块取 `text`，其余取 `value` 的紧凑 JSON）。
pub fn content_text(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().map(str::to_owned).unwrap_or_default();
    };
    blocks
        .iter()
        .map(|block| {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                text.to_owned()
            } else if let Some(value) = block.get("value") {
                value.to_string()
            } else {
                String::new()
            }
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn preview_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(limit)
        .map(|line| clip(line, MAX_LINE_CHARS))
        .collect();
    let total = text.lines().filter(|line| !line.trim().is_empty()).count();
    if total > lines.len() {
        lines.push(format!("… 还有 {} 行", total - lines.len()));
    }
    lines
}

fn pretty_lines(value: &Value, limit: usize) -> Vec<String> {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    // 只看括号的行没有信息量（`{` / `}` / `[` / `]`），预览从有意义的那一行开始。
    let meaningful: Vec<&str> = text
        .lines()
        .filter(|line| !matches!(line.trim(), "{" | "}" | "[" | "]" | "{}" | "[]"))
        .collect();
    preview_lines(&meaningful.join("\n"), limit)
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 原生视图正文：有界截断，保留换行（终端/diff 直接消费）。
fn clip_body(text: &str) -> String {
    clip(text, MAX_BODY_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(presentation: Presentation, value: Value) -> ToolCard {
        card_for("probe", presentation, &value, &Value::Null)
    }

    #[test]
    fn terminal_card_summarizes_exit_and_output() {
        let card = card(
            Presentation::Terminal,
            json!({ "exit_code": 1, "stdout": "a\nb\nc", "stderr": "boom" }),
        );
        assert_eq!(card.kind, CardKind::Terminal);
        assert_eq!(card.title, "exit 1");
        assert_eq!(card.detail, "3 行输出 · 1 行错误");
        assert_eq!(card.preview[0], "a");
        assert!(card.preview.iter().any(|line| line == "! boom"));
    }

    #[test]
    fn terminal_card_marks_missing_exit_code() {
        let card = card(Presentation::Terminal, json!({ "stdout": "done" }));
        assert_eq!(card.title, "exit ?");
        assert_eq!(card.detail, "1 行输出 · 0 行错误");
    }

    #[test]
    fn diff_card_counts_hunks_and_lists_changed_lines() {
        let card = card(
            Presentation::Diff,
            json!({
                "path": "src/lib.rs",
                "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n-old\n+new\n+extra\n"
            }),
        );
        assert_eq!(card.kind, CardKind::Diff);
        assert_eq!(card.title, "src/lib.rs");
        assert_eq!(card.detail, "+2 −1");
        assert_eq!(card.preview, vec!["-old", "+new", "+extra"]);
    }

    #[test]
    fn search_card_lists_match_locations() {
        let card = card(
            Presentation::Search,
            json!({
                "query": "ToolCard",
                "matches": [
                    { "path": "src/card.rs", "line": 42, "text": "pub struct ToolCard" },
                    { "path": "src/lib.rs", "line": 7, "text": "ToolCard" }
                ]
            }),
        );
        assert_eq!(card.kind, CardKind::Search);
        assert_eq!(card.title, "ToolCard");
        assert_eq!(card.detail, "2 处匹配");
        assert_eq!(card.preview[0], "src/card.rs:42  pub struct ToolCard");
    }

    #[test]
    fn read_card_reports_line_count() {
        let card = card(
            Presentation::Read,
            json!({ "path": "README.md", "text": "# 标题\n\n正文" }),
        );
        assert_eq!(card.kind, CardKind::Read);
        assert_eq!(card.title, "README.md");
        assert_eq!(card.detail, "3 行");
        assert_eq!(card.preview[0], "# 标题");
    }

    #[test]
    fn web_card_prefers_title_and_keeps_url() {
        let card = card(
            Presentation::Web,
            json!({
                "url": "https://example.com/a",
                "title": "示例",
                "snippet": "第一行\n第二行"
            }),
        );
        assert_eq!(card.kind, CardKind::Web);
        assert_eq!(card.title, "示例");
        assert_eq!(card.detail, "https://example.com/a");
        assert_eq!(card.preview, vec!["第一行", "第二行"]);
    }

    #[test]
    fn mismatched_shape_falls_back_to_generic() {
        // 插件声明了 terminal，实际却返回了别的东西：结果不能消失。
        let card = card(Presentation::Terminal, json!({ "echo": { "text": "hi" } }));
        assert_eq!(card.kind, CardKind::Generic);
        assert!(card.detail.contains("hi"));
    }

    #[test]
    fn null_value_falls_back_to_content_text() {
        let card = card_for(
            "probe_echo",
            Presentation::Generic,
            &Value::Null,
            &json!([{ "type": "json", "value": { "echo": { "text": "x" } } }]),
        );
        assert_eq!(card.kind, CardKind::Generic);
        assert_eq!(card.title, "probe_echo");
        assert!(card.detail.contains("echo"));
        assert!(card.preview[0].contains("echo"));
    }

    #[test]
    fn long_output_is_truncated_with_a_remainder_line() {
        let lines: Vec<String> = (0..20).map(|index| format!("line-{index}")).collect();
        let card = card(
            Presentation::Terminal,
            json!({ "exit_code": 0, "stdout": lines.join("\n") }),
        );
        assert_eq!(card.preview.len(), MAX_PREVIEW_LINES + 1);
        assert_eq!(card.preview[MAX_PREVIEW_LINES], "… 还有 14 行");
        assert_eq!(card.detail, "20 行输出 · 0 行错误");
    }

    #[test]
    fn single_long_line_is_clipped_by_chars() {
        let long = "中".repeat(MAX_LINE_CHARS * 2);
        let card = card(
            Presentation::Read,
            json!({ "path": "big.txt", "text": long }),
        );
        assert_eq!(card.preview[0].chars().count(), MAX_LINE_CHARS + 1);
        assert!(card.preview[0].ends_with('…'));
    }

    #[test]
    fn error_card_keeps_reason() {
        let card = error_card("已中止", "sidecar crashed\nsecond line");
        assert_eq!(card.kind, CardKind::Error);
        assert_eq!(card.title, "已中止");
        assert_eq!(card.preview, vec!["sidecar crashed", "second line"]);
    }

    #[test]
    fn summary_is_one_bounded_line() {
        let card = card(
            Presentation::Terminal,
            json!({ "exit_code": 0, "stdout": "a\nb" }),
        );
        assert_eq!(card.summary(), "终端 · exit 0 · 2 行输出 · 0 行错误 · a");

        let long = card_for(
            "x",
            Presentation::Generic,
            &json!({ "k": "v".repeat(200) }),
            &Value::Null,
        );
        assert!(long.summary().chars().count() <= 110);
    }

    #[test]
    fn body_keeps_full_text_for_native_views() {
        let terminal = card(
            Presentation::Terminal,
            json!({ "exit_code": 0, "stdout": "a\nb", "stderr": "warn" }),
        );
        assert_eq!(terminal.body, "a\nb\nwarn");

        let diff = card(
            Presentation::Diff,
            json!({ "diff": "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n" }),
        );
        assert!(diff.body.contains("@@ -1 +1 @@"));
        assert_eq!(diff.body.lines().count(), 5);

        // 正文有界：超长输出截断到 MAX_BODY_CHARS + 省略号，避免卡片撑爆内存。
        let huge = "x".repeat(MAX_BODY_CHARS + 500);
        let read = card(
            Presentation::Read,
            json!({ "path": "big.txt", "text": huge }),
        );
        assert_eq!(read.body.chars().count(), MAX_BODY_CHARS + 1);
        assert!(read.body.ends_with('…'));
    }
}
