#![cfg(test)]

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
    let diff = parse_diff("--- a/f\n+++ b/f\n@@ -5,3 +5,4 @@\n ctx\n-old\n+new\n+added\n tail\n");
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
