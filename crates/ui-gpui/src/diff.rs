//! unified diff 解析与原生 GPUI 渲染。
//!
//! 目标：把 git / `patch` / 工具输出里的 unified diff 变成有行号、可滚动的原生视图，
//! 不依赖 WebView，也不引入 diff 库。
//!
//! 设计取舍：
//! - **解析与渲染分离**：`parse_diff` 是纯函数，测试断言模型（文件/hunk/行号/标记），
//!   渲染只做 token → 样式映射；这样 `chat` 的接线不需要重复解析逻辑。
//! - **宽容解析**：缺 `diff --git`、缺文件头、空行无前导空格、未知头部行都安全降级
//!   （进入 `Diff::notices` 或 `DiffFile::metadata`），不 panic、不吞掉正文。
//! - **行长有界**：渲染时给内侧内容估算最小宽度，避免超长行把卡片撑出窗口；
//!   容器最大高度 420px，超出后自身滚动（聊天列表里不再叠加整页滚动）。
//!
//! 限制（见 `docs/m2-ui-views.md`）：
//! - 不支持 combined diff（`diff --cc` / `@@@`），只记为 notice；
//! - 路径里的 git 引号转义只做引号剥离，不展开八进制转义；
//! - 不做语法高亮与折叠未变更行（只有 add/remove/context 三种底色）。

use std::hash::{Hash, Hasher};

use gpui::{
    div, prelude::*, px, AnyElement, ElementId, Font, Rgba, SharedString, StyledText, TextAlign,
    TextRun,
};

use crate::theme::Theme;

/// 单次渲染最多画多少行 diff。模型仍保存全部；超出部分用一行提示收尾，
/// 避免超大 diff 在 UI 线程里构建上万个子元素。
pub const MAX_RENDER_LINES: usize = 2000;

/// unified diff 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Diff {
    /// 按出现顺序排列的文件。
    pub files: Vec<DiffFile>,
    /// 不属于任何文件的说明行（未知头部、combined diff 等），渲染在顶部便于排查。
    pub notices: Vec<String>,
}

impl Diff {
    /// 空 diff：没有文件也没有提示。
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.notices.is_empty()
    }

    /// 所有文件的 `+` 行总数。
    pub fn added(&self) -> usize {
        self.files.iter().map(DiffFile::added).sum()
    }

    /// 所有文件的 `-` 行总数。
    pub fn removed(&self) -> usize {
        self.files.iter().map(DiffFile::removed).sum()
    }
}

/// 一个文件的 diff。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DiffFile {
    /// `---` 行里的旧路径（git 格式通常是 `a/...`，删除文件是 `/dev/null`）。
    pub old_path: String,
    /// `+++` 行里的新路径（git 格式通常是 `b/...`，新增文件是 `/dev/null`）。
    pub new_path: String,
    /// 二进制差异：没有 hunk，只显示提示。
    pub is_binary: bool,
    /// 文件级头部行（`index` / `new file mode` / `rename from` 等），原样保留。
    pub metadata: Vec<String>,
    /// 按出现顺序排列的 hunk。
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    /// 展示用路径：去掉 git 的 `a/` `b/` 前缀，处理新增/删除与重命名。
    pub fn display_path(&self) -> String {
        fn strip(path: &str) -> &str {
            path.strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .unwrap_or(path)
        }

        let old = strip(&self.old_path);
        let new = strip(&self.new_path);
        match (old == "/dev/null", new == "/dev/null") {
            (true, false) => new.to_owned(),
            (false, true) => old.to_owned(),
            _ if old.is_empty() => new.to_owned(),
            _ if new.is_empty() => old.to_owned(),
            _ if old == new => old.to_owned(),
            _ => format!("{old} → {new}"),
        }
    }

    /// 该文件新增行数。
    pub fn added(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Add)
            .count()
    }

    /// 该文件删除行数。
    pub fn removed(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Remove)
            .count()
    }
}

/// 一个 hunk：`@@ -old_start,old_count +new_start,new_count @@ section`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// `@@` 之后的上下文（通常是函数签名），已去掉前导空格。
    pub section: String,
    pub lines: Vec<DiffLine>,
}

/// hunk 内一行的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DiffLineKind {
    /// 未变更的上下文行（前缀一个空格）。
    #[default]
    Context,
    /// 新增行（前缀 `+`）。
    Add,
    /// 删除行（前缀 `-`）。
    Remove,
}

impl DiffLineKind {
    /// 行首标记；上下文行是一个空格，保持等宽对齐。
    pub fn marker(self) -> &'static str {
        match self {
            Self::Context => " ",
            Self::Add => "+",
            Self::Remove => "-",
        }
    }
}

/// hunk 内的一行，带新旧行号（另一侧没有行号时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// 去掉行首标记后的正文。
    pub text: String,
    /// 下一行是 `\ No newline at end of file`：该行尾部没有换行。
    pub no_newline: bool,
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// 解析 unified diff。空输入返回空 `Diff`；无法识别的结构安全降级为 notice/metadata。
pub fn parse_diff(input: &str) -> Diff {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut diff = Diff::default();

    let mut current: Option<DiffFile> = None;
    // 已经读到 `---`、还没读到配对 `+++` 的旧路径。
    let mut pending_old: Option<String> = None;
    // 当前文件是 `diff --git` 建的：紧随其后的 `---`/`+++` 只补路径，不另起文件。
    let mut from_git_header = false;
    let mut in_hunk = false;
    // 当前 hunk 的行号已按头部计数走完；下一行若还是 `\` 说明是 no-newline 标注。
    let mut hunk_exhausted = false;
    // `GIT binary patch` 之后的 base85 负载全部跳过。
    let mut skip_binary_payload = false;

    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut old_end = 0u32;
    let mut new_end = 0u32;

    for (index, raw) in normalized.lines().enumerate() {
        let line = if index == 0 {
            raw.strip_prefix('\u{feff}').unwrap_or(raw)
        } else {
            raw
        };

        if skip_binary_payload {
            if line.starts_with("diff --git ") {
                skip_binary_payload = false;
            } else {
                continue;
            }
        }

        if in_hunk && hunk_exhausted {
            if let Some(rest) = line.strip_prefix('\\') {
                if !rest.trim().is_empty() {
                    mark_no_newline(current.as_mut());
                }
                continue;
            }
            in_hunk = false;
            hunk_exhausted = false;
        }

        if in_hunk {
            // `diff --git` / `@@` 是明确的 hunk 边界；其它以 ` ` / `+` / `-` 开头的行是内容。
            if line.starts_with("diff --git ") || line.starts_with("@@") {
                in_hunk = false;
                hunk_exhausted = false;
            } else {
                if let Some(rest) = line.strip_prefix('\\') {
                    if !rest.trim().is_empty() {
                        mark_no_newline(current.as_mut());
                    }
                    continue;
                }
                let (kind, text) = match line.as_bytes().first() {
                    Some(b' ') => (DiffLineKind::Context, &line[1..]),
                    Some(b'+') => (DiffLineKind::Add, &line[1..]),
                    Some(b'-') => (DiffLineKind::Remove, &line[1..]),
                    // patch 邮件的空上下文行可能没有前导空格，按空上下文处理。
                    None => (DiffLineKind::Context, ""),
                    Some(_) => {
                        // 非标准内容：降级为提示，不当作 hunk 内容吞掉。
                        diff.notices.push(line.to_owned());
                        in_hunk = false;
                        hunk_exhausted = false;
                        continue;
                    }
                };
                push_hunk_line(current.as_mut(), kind, text, &mut old_no, &mut new_no);
                hunk_exhausted = old_no as u64 >= old_end as u64
                    && new_no as u64 >= new_end as u64
                    && current
                        .as_ref()
                        .and_then(|file| file.hunks.last())
                        .is_some_and(|hunk| !hunk.lines.is_empty());
                continue;
            }
        }

        // ---- hunk 之外：文件边界 / 头部 / 元信息 ----

        if let Some(rest) = line.strip_prefix("diff --git ") {
            close_file(&mut current, &mut diff.files);
            pending_old = None;
            let (old_path, new_path) = split_git_paths(rest);
            current = Some(DiffFile {
                old_path,
                new_path,
                ..DiffFile::default()
            });
            from_git_header = true;
            continue;
        }

        if line.starts_with("diff --") || line.starts_with("diff ") {
            // combined diff（`diff --cc` / `diff --combined`）不在 v1 范围。
            close_file(&mut current, &mut diff.files);
            pending_old = None;
            from_git_header = false;
            diff.notices.push(line.to_owned());
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            let path = parse_header_path(rest);
            let reuse = from_git_header
                || current
                    .as_ref()
                    .is_some_and(|file| file.old_path.is_empty() && file.new_path.is_empty());
            from_git_header = false;
            if !reuse {
                close_file(&mut current, &mut diff.files);
                current = Some(DiffFile::default());
            }
            if let Some(file) = current.as_mut() {
                file.old_path = path.clone();
            }
            pending_old = Some(path);
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = parse_header_path(rest);
            let reuse = pending_old.take().is_some() || current.is_none();
            if current.is_none() {
                current = Some(DiffFile::default());
            }
            if reuse {
                if let Some(file) = current.as_mut() {
                    file.new_path = path;
                }
            } else {
                diff.notices.push(line.to_owned());
            }
            continue;
        }

        if line.starts_with("@@ ") {
            pending_old = None;
            from_git_header = false;
            if current.is_none() {
                current = Some(DiffFile::default());
            }
            match parse_hunk_header(line) {
                Some(hunk) => {
                    old_no = hunk.old_start;
                    new_no = hunk.new_start;
                    old_end = hunk.old_start.saturating_add(hunk.old_count);
                    new_end = hunk.new_start.saturating_add(hunk.new_count);
                    if let Some(file) = current.as_mut() {
                        file.hunks.push(hunk);
                    }
                    in_hunk = true;
                    hunk_exhausted = false;
                }
                None => diff.notices.push(line.to_owned()),
            }
            continue;
        }

        if line.starts_with("@@@") {
            diff.notices.push(line.to_owned());
            continue;
        }

        if let Some((old_path, new_path)) = parse_binary_line(line) {
            if current.is_none() {
                current = Some(DiffFile {
                    old_path,
                    new_path,
                    is_binary: true,
                    ..DiffFile::default()
                });
                continue;
            }
            if let Some(file) = current.as_mut() {
                if file.old_path.is_empty() {
                    file.old_path = old_path;
                }
                if file.new_path.is_empty() {
                    file.new_path = new_path;
                }
                file.is_binary = true;
            }
            continue;
        }

        if line == "GIT binary patch" {
            if current.is_none() {
                current = Some(DiffFile::default());
            }
            if let Some(file) = current.as_mut() {
                file.is_binary = true;
            }
            skip_binary_payload = true;
            continue;
        }

        if is_file_metadata(line) {
            if let Some(file) = current.as_mut() {
                file.metadata.push(line.to_owned());
            } else {
                diff.notices.push(line.to_owned());
            }
            continue;
        }

        if !line.trim().is_empty() {
            diff.notices.push(line.to_owned());
        }
    }

    close_file(&mut current, &mut diff.files);
    diff
}

fn close_file(current: &mut Option<DiffFile>, files: &mut Vec<DiffFile>) {
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn mark_no_newline(current: Option<&mut DiffFile>) {
    if let Some(hunk) = current.and_then(|file| file.hunks.last_mut()) {
        if let Some(line) = hunk.lines.last_mut() {
            line.no_newline = true;
        }
    }
}

fn push_hunk_line(
    current: Option<&mut DiffFile>,
    kind: DiffLineKind,
    text: &str,
    old_no: &mut u32,
    new_no: &mut u32,
) {
    let mut line = DiffLine {
        kind,
        text: text.to_owned(),
        ..DiffLine::default()
    };
    match kind {
        DiffLineKind::Context => {
            line.old_line = Some(*old_no);
            line.new_line = Some(*new_no);
            *old_no = old_no.saturating_add(1);
            *new_no = new_no.saturating_add(1);
        }
        DiffLineKind::Add => {
            line.new_line = Some(*new_no);
            *new_no = new_no.saturating_add(1);
        }
        DiffLineKind::Remove => {
            line.old_line = Some(*old_no);
            *old_no = old_no.saturating_add(1);
        }
    }
    if let Some(hunk) = current.and_then(|file| file.hunks.last_mut()) {
        hunk.lines.push(line);
    }
}

/// `--- a/x` / `+++ b/x` 之后的路径：去掉行尾制表符/空白，剥离 git 引号。
fn parse_header_path(rest: &str) -> String {
    let trimmed = rest.trim_end();
    // `path\t时间戳` 是传统 diff 的格式；git 不带时间戳。
    let path = trimmed.split('\t').next().unwrap_or(trimmed).trim();
    unquote(path)
}

fn unquote(path: &str) -> String {
    let path = path.trim();
    if path.len() >= 2 && path.starts_with('"') && path.ends_with('"') {
        path[1..path.len() - 1].to_owned()
    } else {
        path.to_owned()
    }
}

/// 从 `diff --git a/x b/y` 里拆出两个路径（支持 git 的引号形式）。
fn split_git_paths(rest: &str) -> (String, String) {
    let mut tokens = Vec::new();
    let mut rest = rest.trim();
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('"') {
            let mut escaped = false;
            let mut end = None;
            for (offset, ch) in stripped.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                match ch {
                    '\\' => escaped = true,
                    '"' => {
                        end = Some(offset);
                        break;
                    }
                    _ => {}
                }
            }
            match end {
                Some(end) => {
                    tokens.push(unquote(&rest[..=end + 1]));
                    rest = rest[end + 2..].trim_start();
                }
                None => {
                    tokens.push(unquote(rest));
                    rest = "";
                }
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            tokens.push(rest[..end].to_owned());
            rest = rest[end..].trim_start();
        }
    }
    match tokens.as_slice() {
        [only] => (only.clone(), only.clone()),
        [old, new, ..] => (old.clone(), new.clone()),
        [] => (String::new(), String::new()),
    }
}

/// `Binary files a/x and b/x differ`。
fn parse_binary_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("Binary files ")?;
    let rest = rest.strip_suffix(" differ")?;
    let (old, new) = rest.rsplit_once(" and ")?;
    Some((unquote(old.trim()), unquote(new.trim())))
}

fn is_file_metadata(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
        "similarity index ",
        "dissimilarity index ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
    ];
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
}

/// 解析 `@@ -old[,count] +new[,count] @@ section`。
fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let rest = line.strip_prefix("@@ ")?;
    let mut parts = rest.splitn(3, ' ');
    let old_range = parts.next()?.strip_prefix('-')?;
    let new_range = parts.next()?.strip_prefix('+')?;
    // 缺结尾 `@@` 的残缺头像宽松接受：section 取不到就是空。
    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;
    let section = rest
        .splitn(3, ' ')
        .nth(2)
        .unwrap_or("")
        .strip_prefix("@@")
        .unwrap_or("")
        .trim_start()
        .to_owned();
    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
        lines: Vec::new(),
    })
}

fn parse_range(token: &str) -> Option<(u32, u32)> {
    let mut parts = token.splitn(2, ',');
    let start = parts.next()?.trim().parse::<u32>().ok()?;
    let count = match parts.next() {
        Some(count) => count.trim().parse::<u32>().ok()?,
        None => 1,
    };
    Some((start, count))
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 把 `diff` 渲染成可滚动的原生 GPUI 视图。
///
/// 颜色来自 [`crate::theme::Palette`]：新增/删除用 `state_success` / `state_error`
/// 的低透明度底色与前景（透明度是表现层，色相仍由 token 决定）。
pub fn diff_view(theme: &Theme, diff: &Diff) -> AnyElement {
    let p = theme.palette;
    // 滚动容器必须带 id；用内容指纹做后缀，避免同屏多张卡片共享滚动状态。
    let scroll_id = ElementId::NamedInteger(SharedString::from("diff-scroll"), fingerprint(diff));
    let mut container = div()
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

    if diff.is_empty() {
        return container
            .child(caption_row(theme, "（无差异）"))
            .into_any_element();
    }

    if !diff.notices.is_empty() {
        container = container.child(caption_row(theme, &diff.notices.join("\n")));
    }

    let content_width = estimated_width(theme, diff);
    let mut content = div().flex().flex_col().w_full().min_w(px(content_width));

    let total_lines = diff
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| hunk.lines.len())
        .sum::<usize>();
    let mut rendered_lines = 0usize;
    let mut truncated = false;

    'files: for file in &diff.files {
        content = content.child(file_header(theme, file));
        if file.is_binary {
            content = content.child(caption_row(theme, "二进制文件，不显示内容"));
            continue;
        }
        if file.hunks.is_empty() {
            content = content.child(caption_row(theme, "（无文本 hunk）"));
            continue;
        }
        for hunk in &file.hunks {
            content = content.child(hunk_header(theme, hunk));
            for line in &hunk.lines {
                if rendered_lines >= MAX_RENDER_LINES {
                    truncated = true;
                    break 'files;
                }
                content = content.child(line_row(theme, line));
                rendered_lines += 1;
                if line.no_newline {
                    content = content.child(no_newline_row(theme));
                }
            }
        }
    }
    if truncated {
        content = content.child(caption_row(
            theme,
            &format!("（diff 过大，仅渲染前 {MAX_RENDER_LINES} 行，共 {total_lines} 行）"),
        ));
    }

    container.child(content).into_any_element()
}

/// 解析并渲染 unified diff（`chat` 接线用的便捷入口）。
pub fn diff_view_from_text(theme: &Theme, text: &str) -> AnyElement {
    diff_view(theme, &parse_diff(text))
}

fn code_font(theme: &Theme) -> Font {
    Font {
        family: SharedString::from(theme.font_code),
        ..Font::default()
    }
}

fn caption_row(theme: &Theme, text: &str) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    div()
        .w_full()
        .px(px(12.0))
        .py(px(6.0))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_color(p.color(p.label_tertiary))
        .child(text.to_owned())
        .into_any_element()
}

fn file_header(theme: &Theme, file: &DiffFile) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .px(px(10.0))
        .py(px(5.0))
        .bg(p.color(p.code_block_banner))
        .border_b_1()
        .border_color(p.color(p.border_l1))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .text_size(t.small)
                .line_height(t.small_line)
                .text_color(p.color(p.label_primary))
                .font(code_font(theme))
                .child(file.display_path()),
        )
        .child(stat_chip(
            theme,
            format!("+{}", file.added()),
            p.state_success,
        ))
        .child(stat_chip(
            theme,
            format!("−{}", file.removed()),
            p.state_error,
        ));
    if file.is_binary {
        header = header.child(
            div()
                .flex_none()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(theme.radii.xs)
                .bg(p.color(p.tag))
                .text_size(t.small)
                .text_color(p.color(p.label_secondary))
                .child("二进制"),
        );
    }
    header.into_any_element()
}

fn stat_chip(theme: &Theme, text: String, color: u32) -> AnyElement {
    let t = theme.type_scale;
    div()
        .flex_none()
        .whitespace_nowrap()
        .font(code_font(theme))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_color(theme.palette.color(color))
        .child(text)
        .into_any_element()
}

fn hunk_header(theme: &Theme, hunk: &DiffHunk) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    let heading = if hunk.section.is_empty() {
        format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        )
    } else {
        format!(
            "@@ -{},{} +{},{} @@ {}",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count, hunk.section
        )
    };
    div()
        .w_full()
        .px(px(10.0))
        .py(px(3.0))
        .whitespace_nowrap()
        .bg(p.color(p.tag))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_color(p.color(p.label_secondary))
        .font(code_font(theme))
        .child(heading)
        .into_any_element()
}

/// 新增/删除行底色：与 hunk 内容对比度相关，色相来自语义 token。
pub fn line_tint(theme: &Theme, kind: DiffLineKind) -> Option<Rgba> {
    match kind {
        DiffLineKind::Context => None,
        DiffLineKind::Add => Some(
            theme
                .palette
                .color(theme.palette.state_success)
                .opacity(0.10),
        ),
        DiffLineKind::Remove => Some(theme.palette.color(theme.palette.state_error).opacity(0.10)),
    }
}

/// 行号/标记色：新增绿、删除红、上下文灰。
pub fn line_accent(theme: &Theme, kind: DiffLineKind) -> Rgba {
    match kind {
        DiffLineKind::Add => theme.palette.color(theme.palette.state_success),
        DiffLineKind::Remove => theme.palette.color(theme.palette.state_error),
        DiffLineKind::Context => theme.palette.color(theme.palette.label_caption),
    }
}

fn line_row(theme: &Theme, line: &DiffLine) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    let accent = line_accent(theme, line.kind);
    let mut row = div()
        .flex()
        .flex_row()
        .items_start()
        .w_full()
        .px(px(8.0))
        .py(px(1.0))
        .whitespace_nowrap();
    if let Some(tint) = line_tint(theme, line.kind) {
        row = row.bg(tint);
    }
    row.child(number_cell(theme, line.old_line, accent))
        .child(number_cell(theme, line.new_line, accent))
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .font(code_font(theme))
                .text_size(t.small)
                .line_height(t.small_line)
                .text_color(accent)
                .child(line.kind.marker()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(t.small)
                .line_height(t.small_line)
                .text_color(p.color(p.label_primary))
                .child(
                    StyledText::new(SharedString::from(line.text.clone())).with_runs(vec![
                        TextRun {
                            len: line.text.len(),
                            font: code_font(theme),
                            color: p.color(p.label_primary).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        },
                    ]),
                ),
        )
        .into_any_element()
}

fn number_cell(theme: &Theme, number: Option<u32>, color: Rgba) -> AnyElement {
    let t = theme.type_scale;
    div()
        .w(px(40.0))
        .flex_none()
        .font(code_font(theme))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_align(TextAlign::Right)
        .text_color(color)
        .child(number.map(|n| n.to_string()).unwrap_or_default())
        .into_any_element()
}

fn no_newline_row(theme: &Theme) -> AnyElement {
    let p = theme.palette;
    let t = theme.type_scale;
    div()
        .w_full()
        .pl(px(102.0))
        .py(px(1.0))
        .whitespace_nowrap()
        .font(code_font(theme))
        .text_size(t.small)
        .line_height(t.small_line)
        .text_color(p.color(p.label_caption))
        .child("\\ No newline at end of file")
        .into_any_element()
}

/// 估算横向滚动所需的最小宽度（等宽字体一个字符约 0.62em）。
fn estimated_width(theme: &Theme, diff: &Diff) -> f32 {
    let max_chars = diff
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.lines)
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or(0);
    let char_width = theme.type_scale.small.as_f32() * 0.62;
    (max_chars as f32 * char_width + 120.0).max(240.0)
}

fn fingerprint<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Palette;

    const MULTI_FILE: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@ fn main()
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
 let d = 5;
diff --git a/docs/b.md b/docs/b.md
new file mode 100644
--- /dev/null
+++ b/docs/b.md
@@ -0,0 +1,2 @@
+# B
+line
";

    #[test]
    fn multi_file_diff_parses_paths_and_counts() {
        let diff = parse_diff(MULTI_FILE);
        assert_eq!(diff.files.len(), 2, "{diff:?}");
        assert_eq!(diff.files[0].display_path(), "src/a.rs");
        assert_eq!(diff.files[0].hunks.len(), 1);
        assert_eq!(diff.files[0].added(), 2);
        assert_eq!(diff.files[0].removed(), 1);
        assert_eq!(diff.files[1].display_path(), "docs/b.md");
        assert!(diff.files[1]
            .metadata
            .iter()
            .any(|m| m.contains("new file mode")));
        assert_eq!(diff.files[1].added(), 2);
        assert_eq!(diff.added(), 4);
        assert_eq!(diff.removed(), 1);
    }

    #[test]
    fn hunk_headers_and_multiple_hunks() {
        let input = "\
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,2 @@ first
-old
+new
 keep
@@ -10,2 +10,3 @@ second section
 ctx
+extra
 ctx2
";
        let diff = parse_diff(input);
        assert_eq!(diff.files.len(), 1);
        let hunks = &diff.files[0].hunks;
        assert_eq!(hunks.len(), 2, "{hunks:?}");
        assert_eq!((hunks[0].old_start, hunks[0].old_count), (1, 2));
        assert_eq!((hunks[0].new_start, hunks[0].new_count), (1, 2));
        assert_eq!(hunks[0].section, "first");
        assert_eq!(hunks[1].section, "second section");
        assert_eq!((hunks[1].old_start, hunks[1].old_count), (10, 2));
        assert_eq!((hunks[1].new_start, hunks[1].new_count), (10, 3));
    }

    #[test]
    fn line_numbers_follow_both_sides() {
        let diff =
            parse_diff("--- a/f\n+++ b/f\n@@ -5,3 +5,4 @@\n ctx\n-old\n+new\n+added\n tail\n");
        let lines = &diff.files[0].hunks[0].lines;
        assert_eq!(lines.len(), 5, "{lines:?}");

        let ctx = &lines[0];
        assert_eq!(
            (ctx.kind, ctx.old_line, ctx.new_line),
            (DiffLineKind::Context, Some(5), Some(5))
        );
        let removed = &lines[1];
        assert_eq!(
            (removed.kind, removed.old_line, removed.new_line),
            (DiffLineKind::Remove, Some(6), None)
        );
        let add = &lines[2];
        assert_eq!(
            (add.kind, add.old_line, add.new_line),
            (DiffLineKind::Add, None, Some(6))
        );
        let add2 = &lines[3];
        assert_eq!(
            (add2.kind, add2.old_line, add2.new_line),
            (DiffLineKind::Add, None, Some(7))
        );
        let tail = &lines[4];
        assert_eq!(
            (tail.kind, tail.old_line, tail.new_line),
            (DiffLineKind::Context, Some(7), Some(8))
        );
    }

    #[test]
    fn empty_diff_is_empty_and_renders_without_panic() {
        let diff = parse_diff("");
        assert!(diff.is_empty());
        assert!(diff.files.is_empty());
        assert_eq!(diff.added(), 0);
        let _ = diff_view(&Theme::light(), &diff);
    }

    #[test]
    fn binary_diff_is_a_notice_not_a_hunk() {
        let input = "\
diff --git a/assets/logo.png b/assets/logo.png
index 1111111..2222222 100644
Binary files a/assets/logo.png and b/assets/logo.png differ
";
        let diff = parse_diff(input);
        assert_eq!(diff.files.len(), 1);
        let file = &diff.files[0];
        assert!(file.is_binary, "{file:?}");
        assert!(file.hunks.is_empty());
        assert_eq!(file.display_path(), "assets/logo.png");
        assert_eq!(file.added(), 0);
    }

    #[test]
    fn raw_binary_line_without_git_header() {
        let diff = parse_diff("Binary files a/x.bin and b/x.bin differ\n");
        assert_eq!(diff.files.len(), 1);
        assert!(diff.files[0].is_binary);
        assert_eq!(diff.files[0].display_path(), "x.bin");
    }

    #[test]
    fn no_newline_marker_attaches_to_previous_line() {
        let input = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n";
        let diff = parse_diff(input);
        let lines = &diff.files[0].hunks[0].lines;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(!lines[0].no_newline);
        assert!(lines[1].no_newline);
        assert_eq!(lines[1].text, "new");
    }

    #[test]
    fn crlf_and_missing_newline_at_eof() {
        let diff = parse_diff("--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n-a\r\n+b");
        let lines = &diff.files[0].hunks[0].lines;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].text, "a");
        assert_eq!(lines[1].text, "b");
    }

    #[test]
    fn unknown_header_lines_degrade_to_notices_or_metadata() {
        let input = "\
diff --cc merged.rs
index 1,2..3
preamble that belongs to no file
--- a/f
+++ b/f
@@ -1 +1 @@
-a
+b
";
        let diff = parse_diff(input);
        assert!(diff.notices.iter().any(|n| n.contains("diff --cc")));
        assert!(diff.notices.iter().any(|n| n.contains("preamble")));
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn line_colors_come_from_theme_tokens() {
        let light = Theme::light();
        assert_eq!(
            line_accent(&light, DiffLineKind::Add),
            light.palette.color(Palette::LIGHT.state_success)
        );
        assert_eq!(
            line_accent(&light, DiffLineKind::Remove),
            light.palette.color(Palette::LIGHT.state_error)
        );
        assert!(line_tint(&light, DiffLineKind::Context).is_none());
        assert!(line_tint(&light, DiffLineKind::Add).is_some());
    }

    #[test]
    fn huge_diff_is_parsed_fully_but_rendered_bounded() {
        let count = MAX_RENDER_LINES + 100;
        let input = format!(
            "--- a/f\n+++ b/f\n@@ -0,0 +1,{count} @@\n{}",
            "+x\n".repeat(count)
        );
        let diff = parse_diff(&input);
        assert_eq!(diff.files[0].added(), count, "解析不应丢行");
        // 渲染只画前 MAX_RENDER_LINES 行，但必须能安全返回。
        let _ = diff_view(&Theme::light(), &diff);
    }

    #[test]
    fn view_renders_all_diff_shapes_without_panic() {
        for input in [
            MULTI_FILE,
            "",
            "Binary files a/x and b/x differ",
            "garbage\n",
        ] {
            let diff = parse_diff(input);
            let _ = diff_view(&Theme::dark(), &diff);
            let _ = diff_view_from_text(&Theme::light(), input);
        }
    }
}
