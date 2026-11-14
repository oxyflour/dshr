//! ADC-v1 元素树 → GPUI 原生元素渲染器。
//!
//! 输入是 `dshr-ui-contributions` 解析后的 [`ElementNode`]（纯数据，见
//! [`docs/spec/adc-v1.md`](../../docs/spec/adc-v1.md) §6），输出 [`AnyElement`]。
//!
//! 设计取舍：
//! - **渲染是纯函数**：同一节点 + 同一主题必然得到同一棵元素树（spec §7.2）；
//!   渲染器不读取任何窗口/焦点状态，交互元素本轮只画视觉。
//! - **配额在渲染侧再兜底**：解析期已按 §6.1/§10 裁剪，渲染期仍按上限裁剪，
//!   防止别处直接构造的树把 UI 线程撑爆（`docs/m1-log.md` A1c：每行元素数是真正预算）。
//! - **降级而不是 panic**：未知元素按 P4 渲染成 [`Placeholder`] 灰框（带 `testId` 与诊断），
//!   单棵树里其余节点照常渲染。
//! - **纯 helper 不依赖窗口**：spacing/对齐/裁剪/a11y 检查都是纯函数，CI 直接单测。
//!
//! 无障碍（spec §9）：此 GPUI rev 的 `div()` 通过 `StatefulInteractiveElement` 暴露
//! `role` / `aria_label`；`a11yLabel` 在渲染时写到这些属性上，缺失时由
//! [`a11y_issues_in`] 给出 `ADC-ELEM-A11Y-001` 诊断（解析期已诊断一次，这里是渲染侧兜底）。

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, AlignContent, AlignItems, AnyElement, Div, Font, Pixels, Rgba, Role,
    SharedString, Stateful, TextAlign, Toggled,
};

use dshr_ui_contributions::{
    parse_tree, parse_tree_str, ElementKind, ElementNode, PlaceholderReason,
};

// 渲染入口的返回类型对 chat/tool card 是公开契约，这里一并再导出。
pub use dshr_ui_contributions::{Diagnostic, DiagnosticCode, Severity};

use crate::markdown::markdown_view;
use crate::theme::{Palette, Theme};

/// 布局间距只接受这 8 个 token（spec §6.3、`element.schema.json` `$defs/spacing`）。
pub const SPACING_TOKENS: [i64; 8] = [0, 2, 4, 8, 12, 16, 24, 32];

/// 单个 View 一次提交的文本字节上限（spec §6.1 / §10，`8 KB`）。
pub const MAX_TEXT_BYTES: usize = 8 * 1024;

/// `Chart` 单图数据点上限（spec §6.7 / §12 Q2）。
pub const MAX_CHART_POINTS: usize = 8;

/// `Chart.series` 条数上限（spec §12 Q2：最多 4 条）。
pub const MAX_CHART_SERIES: usize = 4;

/// `Table.columns` 列数上限（`element.schema.json`：`maxItems: 12`）。
pub const MAX_TABLE_COLUMNS: usize = 12;

/// `Table.rows` 行数上限（`element.schema.json`：`maxItems: 200`）。
pub const MAX_TABLE_ROWS: usize = 200;

/// `List.items` 行数上限（`element.schema.json`：`maxItems: 200`）。
pub const MAX_LIST_ITEMS: usize = 200;

/// 交互元素：运行时 MUST 带 `a11yLabel`（spec §9 `ADC-ELEM-A11Y-001`）。
///
/// 与 spec §6.5/§6.6/§6.8 对齐：7 个表单控件 + `Tabs` + 4 个数据选择面。
pub const INTERACTIVE_KINDS: [&str; 11] = [
    "Button", "Input", "Textarea", "Select", "Checkbox", "Switch", "Slider", "Tabs", "List",
    "Table", "Tree",
];

/// `Icon.name`（宿主图标集）→ 文本符号。
///
/// GPUI 此 rev 没有 SVG 图标集注入点，v1 用等宽字体里的几何符号渲染；
/// 未知名显示 `▢`（spec §6.7 的降级要求），不 panic。
const ICON_GLYPHS: [(&str, &str); 14] = [
    ("check", "✓"),
    ("close", "✕"),
    ("plus", "+"),
    ("minus", "−"),
    ("arrow-right", "→"),
    ("arrow-left", "←"),
    ("arrow-up", "↑"),
    ("arrow-down", "↓"),
    ("info", "ⓘ"),
    ("warning", "⚠"),
    ("error", "✕"),
    ("search", "⌕"),
    ("folder", "▸"),
    ("file", "▤"),
];

/// 未知图标名 / 未知元素的兜底符号。
pub const UNKNOWN_GLYPH: &str = "▢";

// ---------------------------------------------------------------------------
// 纯 helper（无窗口，可单测）
// ---------------------------------------------------------------------------

/// `spacing` token 是否合法（只接受 [`SPACING_TOKENS`]）。
pub fn is_spacing_token(value: i64) -> bool {
    SPACING_TOKENS.contains(&value)
}

/// `gap`/`padding` → 像素；`None` 或非法值按 `0` 处理（解析期已剔除非法值，这里兜底）。
pub fn spacing_px(value: Option<i64>) -> Pixels {
    match value {
        Some(token) if is_spacing_token(token) => px(token as f32),
        _ => px(0.0),
    }
}

/// `align` 枚举 → `AlignItems`（spec §6.3：`start/center/end/stretch`）。
pub fn align_items(raw: &str) -> AlignItems {
    match raw {
        "center" => AlignItems::Center,
        "end" => AlignItems::End,
        "stretch" => AlignItems::Stretch,
        _ => AlignItems::Start,
    }
}

/// `justify` 枚举 → `AlignContent`（spec §6.3：`start/center/end/space-between`）。
pub fn justify_content(raw: &str) -> AlignContent {
    match raw {
        "center" => AlignContent::Center,
        "end" => AlignContent::End,
        "space-between" => AlignContent::SpaceBetween,
        _ => AlignContent::Start,
    }
}

/// `Grid.columns`（必填 1–12）→ 列数；非法值按 1 处理。
pub fn grid_columns(raw: Option<i64>) -> u16 {
    match raw {
        Some(value) if (1..=12).contains(&value) => value as u16,
        _ => 1,
    }
}

/// `span`（1–12，仅 Grid 子项生效）→ 跨列数；缺省或非法按 1 处理。
pub fn grid_span(raw: Option<u8>) -> u16 {
    match raw {
        Some(value) if (1..=12).contains(&value) => u16::from(value),
        _ => 1,
    }
}

/// `Chart.data` 数据点裁剪（≤ 8）。
pub fn clip_chart_points(len: usize) -> usize {
    len.min(MAX_CHART_POINTS)
}

/// `Chart.series` 条数裁剪（≤ 4）。
pub fn clip_chart_series(len: usize) -> usize {
    len.min(MAX_CHART_SERIES)
}

/// `Table.columns` 列数裁剪（≤ 12）。
pub fn clip_table_columns(len: usize) -> usize {
    len.min(MAX_TABLE_COLUMNS)
}

/// `Table.rows` 行数裁剪（≤ 200）。
pub fn clip_table_rows(len: usize) -> usize {
    len.min(MAX_TABLE_ROWS)
}

/// `List.items` 行数裁剪（≤ 200）。
pub fn clip_list_items(len: usize) -> usize {
    len.min(MAX_LIST_ITEMS)
}

/// 文本按 [`MAX_TEXT_BYTES`] 字节上限裁剪，且保证落在 UTF-8 字符边界。
pub fn truncate_text(text: &str) -> &str {
    truncate_text_to(text, MAX_TEXT_BYTES)
}

/// 按给定字节上限裁剪文本；超限时回退到最近的字符边界，绝不 panic。
pub fn truncate_text_to(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 柱/折图高度：按数据点最大值归一化到 `max_height`，负值与非法值按 0 处理。
///
/// 返回长度是先经 [`clip_chart_points`] 裁剪后的点数，可直接逐点建 div。
pub fn chart_bar_heights(values: &[f64], max_height: f32) -> Vec<f32> {
    let points = &values[..clip_chart_points(values.len())];
    let peak = points
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(0.0_f64, f64::max);
    if peak <= 0.0 || !max_height.is_finite() || max_height <= 0.0 {
        return vec![0.0; points.len()];
    }
    points
        .iter()
        .map(|value| {
            if !value.is_finite() || *value <= 0.0 {
                0.0
            } else {
                ((*value / peak) as f32 * max_height).clamp(0.0, max_height)
            }
        })
        .collect()
}

/// `Slider` 值 → `0.0..=1.0` 的填充比例；`min >= max` 或非法输入按 0 处理。
pub fn slider_fraction(value: f64, min: f64, max: f64) -> f32 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return 0.0;
    }
    (((value - min) / (max - min)).clamp(0.0, 1.0)) as f32
}

/// `tone` → 主题语义色 token（spec §6.2：只用 token，插件不能指定色值）。
///
/// `neutral`/缺省 → `label_secondary`；`info` → `business_primary`；
/// `success`/`warning`/`error` → 对应 `state_*`。
pub fn tone_token(palette: &Palette, tone: Option<&str>) -> u32 {
    match tone {
        Some("info") => palette.business_primary,
        Some("success") => palette.state_success,
        Some("warning") => palette.state_warn,
        Some("error") => palette.state_error,
        _ => palette.label_secondary,
    }
}

/// `tone` → 语义前景色。
pub fn tone_color(palette: &Palette, tone: Option<&str>) -> Rgba {
    palette.color(tone_token(palette, tone))
}

/// `Icon.name` → 文本符号；未知名称返回 [`UNKNOWN_GLYPH`]。
pub fn icon_glyph(name: &str) -> &'static str {
    ICON_GLYPHS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, glyph)| *glyph)
        .unwrap_or(UNKNOWN_GLYPH)
}

/// 该元素类型是否为交互元素（必须带 `a11yLabel`）。
pub fn is_interactive_kind(kind: &str) -> bool {
    INTERACTIVE_KINDS.contains(&kind)
}

/// 交互元素是否缺 `a11yLabel`（`None`、空串、纯空白都算缺失）。
pub fn a11y_label_missing(kind: &str, label: Option<&str>) -> bool {
    if !is_interactive_kind(kind) {
        return false;
    }
    match label {
        Some(value) => value.trim().is_empty(),
        None => true,
    }
}

/// 一条无障碍诊断（渲染侧兜底，对应 `ADC-ELEM-A11Y-001`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yIssue {
    /// 诊断码（spec §11 的 `ADC-ELEM-*` 前缀）。
    pub code: &'static str,
    /// 触发诊断的元素类型。
    pub kind: String,
    /// 节点在树里的路径（`$.children[2]` 风格），便于定位。
    pub path: String,
    /// `testId`（spec §9：Placeholder 与诊断都带测试钩子；这里尽量带上）。
    pub test_id: Option<String>,
}

/// `ADC-ELEM-A11Y-001` 诊断码。
pub const A11Y_MISSING_CODE: &str = "ADC-ELEM-A11Y-001";

/// 对单个节点检查 `a11yLabel`；缺标签的交互元素返回一条诊断。
pub fn a11y_issue(
    kind: &str,
    label: Option<&str>,
    path: &str,
    test_id: Option<&str>,
) -> Option<A11yIssue> {
    if !a11y_label_missing(kind, label) {
        return None;
    }
    Some(A11yIssue {
        code: A11Y_MISSING_CODE,
        kind: kind.to_owned(),
        path: path.to_owned(),
        test_id: test_id.map(str::to_owned),
    })
}

/// 表格/键值单元格取值 → 展示文本（`null` 显示 `—`）。
pub fn cell_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "—".to_owned(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Placeholder 的标题行（spec §9：带类型名与诊断）。
pub fn placeholder_label(kind: &str, detail: Option<&str>) -> String {
    match detail {
        Some(text) if !text.trim().is_empty() => format!("未知元素: {kind}（{text}）"),
        _ => format!("未知元素: {kind}"),
    }
}

/// 正文/说明/标题的文字度量。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    /// 字号。
    pub size: Pixels,
    /// 行高。
    pub line_height: Pixels,
    /// 是否等宽（`style=mono`）。
    pub mono: bool,
}

/// `Text.style` → 度量：`body`/`caption`/`title`/`mono`，缺省按 `body`。
///
/// 字号全部来自 [`crate::theme::Typography`]，不造新数值。
pub fn text_metrics(theme: &Theme, style: Option<&str>) -> TextMetrics {
    let scale = theme.type_scale;
    match style {
        Some("caption") => TextMetrics {
            size: scale.small,
            line_height: scale.small_line,
            mono: false,
        },
        Some("title") => TextMetrics {
            size: scale.h3,
            line_height: scale.h3_line,
            mono: false,
        },
        Some("mono") => TextMetrics {
            size: scale.small,
            line_height: scale.small_line,
            mono: true,
        },
        _ => TextMetrics {
            size: scale.body,
            line_height: scale.body_line,
            mono: false,
        },
    }
}

// ---------------------------------------------------------------------------
// 渲染（输入是 dshr-ui-contributions 解析后的 ElementNode）
// ---------------------------------------------------------------------------

/// 代码块一次最多渲染多少行；超出用一行提示收尾（避免上万子元素）。
pub const MAX_CODE_LINES: usize = 400;

/// Tree 一次最多渲染多少个节点（与 schema `maxItems: 200` 对齐）。
pub const MAX_TREE_ROWS: usize = 200;

/// `Tabs` 一次最多画多少个标签（schema：12）。
pub const MAX_TABS: usize = 12;

/// `Accordion` 一次最多画多少个 section（schema：12）。
pub const MAX_ACCORDION_SECTIONS: usize = 12;

/// 图表绘图区高度（px）；柱/折线共用同一归一化基准。
pub const CHART_HEIGHT: f32 = 96.0;

/// 把 ADC-v1 元素节点渲染成 GPUI 元素。
///
/// 纯函数（无窗口、无事件）：同一 `theme` + 同一 `node` 必然得到同一棵元素树，
/// 满足 spec §7.2。本轮交互元素只画视觉，`onPress`/`onChange` 由 Lead 下一轮接线。
pub fn adc_view(theme: &Theme, node: &ElementNode) -> AnyElement {
    render_node(theme, node, "$")
}

/// 收集整棵树里缺 `a11yLabel` 的交互元素（`ADC-ELEM-A11Y-001`，渲染侧兜底）。
///
/// 遍历 `children` 与 `nested_children`（Accordion 的 section 内容），路径按
/// `$.children[0]` / `$.sections[key].children[1]` 生成，便于和解析期诊断对账。
pub fn a11y_issues_in(node: &ElementNode) -> Vec<A11yIssue> {
    let mut issues = Vec::new();
    collect_a11y_issues(node, "$", &mut issues);
    issues
}

/// 把一棵 ADC-v1 JSON 元素树渲染成可嵌入 panel / tool card 的元素。
///
/// 这是 chat 与工具卡接线的**唯一入口**（`parse_tree` + [`adc_view`]）：
/// 返回渲染好的元素和解析期诊断（未知元素、配额裁剪、a11y 缺失等）。
///
/// - 未知元素/非法节点按 P4 渲染为 `Placeholder`，不 panic、不丢整棵树；
/// - 空输入（`null`）同样是 Placeholder + 诊断，宿主可据此显示"内容不可渲染"；
/// - 返回的 `Vec<Diagnostic>` 可直接用于面板角标/日志，不需要再解析一遍。
pub fn adc_panel_body(theme: &Theme, tree: &serde_json::Value) -> (AnyElement, Vec<Diagnostic>) {
    let parsed = parse_tree(tree);
    (
        panel_shell(adc_view(theme, &parsed.root)),
        parsed.diagnostics,
    )
}

/// [`adc_panel_body`] 的字符串版本（会话日志 / patch 里拿到的是 JSON 文本）。
///
/// 非法 JSON → Placeholder 根节点 + `InvalidJson` 诊断，不 panic。
pub fn adc_panel_body_from_str(theme: &Theme, json: &str) -> (AnyElement, Vec<Diagnostic>) {
    let parsed = parse_tree_str(json);
    (
        panel_shell(adc_view(theme, &parsed.root)),
        parsed.diagnostics,
    )
}

/// panel 级外壳：列布局 + 撑满可用宽度，避免根元素是纯 `Text` 时贴边。
fn panel_shell(body: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .child(body)
        .into_any_element()
}

fn collect_a11y_issues(node: &ElementNode, path: &str, issues: &mut Vec<A11yIssue>) {
    if node.is_interactive()
        && node
            .a11y_label
            .as_deref()
            .is_none_or(|label| label.trim().is_empty())
    {
        let issue = a11y_issue(
            node.kind.as_str(),
            node.a11y_label.as_deref(),
            path,
            node.test_id.as_deref(),
        );
        if let Some(issue) = issue {
            issues.push(issue);
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        collect_a11y_issues(child, &child_path(path, index), issues);
    }
    for (key, children) in &node.nested_children {
        for (index, child) in children.iter().enumerate() {
            collect_a11y_issues(
                child,
                &format!("{path}.sections[{key}].children[{index}]"),
                issues,
            );
        }
    }
}

fn render_node(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    // `visible=false` 不占位；节点仍保留在树里供 diff/诊断。
    if !node.visible {
        return div().into_any_element();
    }
    // `ElementKind` 是 `#[non_exhaustive]`：新增元素走兜底分支渲染成灰框（P4）。
    match node.kind {
        ElementKind::Stack => render_stack(theme, node, path, false),
        ElementKind::Row => render_stack(theme, node, path, true),
        ElementKind::Grid => render_grid(theme, node, path),
        ElementKind::Divider => render_divider(theme, node),
        ElementKind::Spacer => div().flex_1().min_h(px(0.0)).into_any_element(),
        ElementKind::Text => render_text(theme, node),
        ElementKind::Markdown => render_markdown(theme, node),
        ElementKind::Code => render_code(theme, node, path),
        ElementKind::Badge => render_badge(theme, node),
        ElementKind::Button => render_button(theme, node, path),
        ElementKind::Input => render_input(theme, node, path),
        ElementKind::Textarea => render_textarea(theme, node, path),
        ElementKind::Select => render_select(theme, node, path),
        ElementKind::Checkbox => render_checkbox(theme, node, path),
        ElementKind::Switch => render_switch(theme, node, path),
        ElementKind::Slider => render_slider(theme, node, path),
        ElementKind::List => render_list(theme, node, path),
        ElementKind::Table => render_table(theme, node, path),
        ElementKind::Tree => render_tree(theme, node, path),
        ElementKind::KeyValue => render_key_value(theme, node),
        ElementKind::Image => render_image(theme, node, path),
        ElementKind::Icon => render_icon(theme, node, path),
        ElementKind::Chart => render_chart(theme, node),
        ElementKind::Card => render_card(theme, node, path),
        ElementKind::Tabs => render_tabs(theme, node, path),
        ElementKind::Accordion => render_accordion(theme, node, path),
        _ => render_placeholder(theme, node),
    }
}

// --- 布局 -----------------------------------------------------------------

fn render_stack(theme: &Theme, node: &ElementNode, path: &str, row: bool) -> AnyElement {
    let mut container = layout_box(node, row);
    for (index, child) in node.children.iter().enumerate() {
        container = container.child(render_node(theme, child, &child_path(path, index)));
    }
    container.into_any_element()
}

/// Stack/Row 共用的 flex 容器：gap/padding 只接受 spacing token，align/justify 映射 flex 属性。
fn layout_box(node: &ElementNode, row: bool) -> Div {
    let mut element = div().flex().w_full().min_w(px(0.0));
    element = if row {
        element.flex_row()
    } else {
        element.flex_col()
    };
    if row && node.bool_prop("wrap").unwrap_or(false) {
        element = element.flex_wrap();
    }
    element.style().align_items = Some(match node.str_prop("align") {
        Some(value) => align_items(value),
        None => AlignItems::Stretch,
    });
    element.style().justify_content = Some(match node.str_prop("justify") {
        Some(value) => justify_content(value),
        None => AlignContent::Start,
    });
    element
        .gap(spacing_px(node.i64_prop("gap")))
        .p(spacing_px(node.i64_prop("padding")))
}

fn render_grid(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let mut grid = div()
        .grid()
        .w_full()
        .grid_cols(grid_columns(node.i64_prop("columns")))
        .gap(spacing_px(node.i64_prop("gap")))
        .p(spacing_px(node.i64_prop("padding")));
    for (index, child) in node.children.iter().enumerate() {
        // 跨列写在网格项（包装 div）上，子元素本身不感知布局。
        let cell = div()
            .col_span(grid_span(child.span))
            .min_w(px(0.0))
            .child(render_node(theme, child, &child_path(path, index)));
        grid = grid.child(cell);
    }
    grid.into_any_element()
}

fn render_divider(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let mut row = div().flex().flex_row().items_center().w_full().gap(px(8.0));
    row = row.child(divider_line(theme));
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row
            .child(
                div()
                    .flex_none()
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_tertiary))
                    .child(label.to_owned()),
            )
            .child(divider_line(theme));
    }
    row.into_any_element()
}

fn divider_line(theme: &Theme) -> Div {
    let palette = theme.palette;
    div()
        .flex_1()
        .h(px(1.0))
        .bg(palette.color(palette.border_l2))
}

// --- 文本 -----------------------------------------------------------------

fn render_text(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let metrics = text_metrics(theme, node.str_prop("style"));
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_primary),
    };
    let text = truncate_text(node.str_prop("value").unwrap_or_default()).to_owned();
    let mut element = text_block(text, metrics, color).w_full();
    if metrics.mono {
        element = element.font(code_font(theme));
    }
    if let Some(lines) = max_lines(node) {
        element = element.line_clamp(lines);
    }
    element.into_any_element()
}

fn render_markdown(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let source = truncate_text(node.str_prop("source").unwrap_or_default());
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_primary),
    };
    let mut element = div()
        .w_full()
        .child(markdown_view(source, theme, &Font::default(), color));
    // 降级：`maxLines` 按文本块逐块 line_clamp（Markdown 是多块元素，无法整段裁剪）。
    if let Some(lines) = max_lines(node) {
        element = element.line_clamp(lines);
    }
    element.into_any_element()
}

fn render_code(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let source = truncate_text(node.str_prop("source").unwrap_or_default());
    let show_numbers = node.bool_prop("lineNumbers").unwrap_or(true);
    let mut container = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(theme.radii.md)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.code_block))
        .overflow_hidden();
    if let Some(language) = node.str_prop("language").filter(|value| !value.is_empty()) {
        container = container.child(
            div()
                .flex_none()
                .w_full()
                .px(px(10.0))
                .py(px(3.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .bg(palette.color(palette.code_block_banner))
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_tertiary))
                .child(language.to_owned()),
        );
    }
    // 横向滚动容器必须带 id（GPUI 用 id 维持滚动状态）。
    let mut body = div()
        .id(format!("adc-code-{path}"))
        .flex()
        .flex_col()
        .w_full()
        .p(px(8.0))
        .overflow_x_scroll()
        .overflow_y_hidden();
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().take(MAX_CODE_LINES).enumerate() {
        let mut row = div()
            .flex()
            .flex_row()
            .items_start()
            .w_full()
            .whitespace_nowrap();
        if show_numbers {
            row = row.child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .text_align(TextAlign::Right)
                    .pr(px(8.0))
                    .font(code_font(theme))
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_caption))
                    .child((index + 1).to_string()),
            );
        }
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .font(code_font(theme))
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_primary))
                .child(if line.is_empty() {
                    " ".to_owned()
                } else {
                    (*line).to_owned()
                }),
        );
        body = body.child(row);
    }
    let total = lines.len();
    if total > MAX_CODE_LINES {
        body = body.child(caption(
            theme,
            format!("（仅渲染前 {MAX_CODE_LINES} 行，共 {total} 行）"),
        ));
    }
    container.child(body).into_any_element()
}

fn render_badge(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let tone = node.tone.map(|tone| tone.as_str());
    let foreground = tone_color(&palette, tone);
    let background = if tone.is_some() {
        foreground.opacity(0.12)
    } else {
        palette.color(palette.tag)
    };
    div()
        .flex_none()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(theme.radii.xs)
        .bg(background)
        .text_size(type_scale.small)
        .line_height(type_scale.small_line)
        .text_color(if tone.is_some() {
            foreground
        } else {
            palette.color(palette.label_secondary)
        })
        .child(truncate_text(node.str_prop("label").unwrap_or_default()).to_owned())
        .into_any_element()
}

// --- 交互（只画视觉，不接事件） -------------------------------------------

fn render_button(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let disabled = node.bool_prop("disabled").unwrap_or(false);
    let tone = node.tone.map(|tone| tone.as_str());
    let accent = match tone {
        Some("neutral") => palette.color(palette.tag),
        Some(tone) => tone_color(&palette, Some(tone)),
        None => palette.color(palette.business_primary),
    };
    let foreground = if disabled {
        palette.color(palette.label_tertiary)
    } else if tone == Some("neutral") {
        palette.color(palette.label_primary)
    } else {
        palette.color(palette.bg_base)
    };
    let label = truncate_text(node.str_prop("label").unwrap_or_default()).to_owned();
    let mut button = interactive_base(div(), node, path, Role::Button)
        .flex_none()
        .px(px(12.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .bg(if disabled {
            palette.color(palette.interactive_hover_solid)
        } else {
            accent
        })
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(foreground)
        .child(label);
    if disabled {
        button = button.opacity(0.6);
    }
    button.into_any_element()
}

fn render_input(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let multiline = node.bool_prop("multiline").unwrap_or(false);
    let value = truncate_text(node.str_prop("value").unwrap_or_default());
    let placeholder = node.str_prop("placeholder");
    let role = if multiline {
        Role::MultilineTextInput
    } else {
        Role::TextInput
    };
    let mut input = interactive_base(div(), node, path, role)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line);
    if let Some(placeholder) = placeholder {
        input = input.aria_placeholder(placeholder);
    }
    input = input.aria_value(value);
    if value.is_empty() {
        let hint = placeholder.unwrap_or("（空）").to_owned();
        input = input
            .text_color(palette.color(palette.label_caption))
            .child(hint);
    } else {
        input = input
            .text_color(palette.color(palette.label_primary))
            .child(value.to_owned());
    }
    input.into_any_element()
}

fn render_textarea(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let rows = node.i64_prop("rows").unwrap_or(3).clamp(1, 100);
    let value = truncate_text(node.str_prop("value").unwrap_or_default());
    let min_height = type_scale.secondary_line.as_f32() * rows as f32 + 10.0;
    interactive_base(div(), node, path, Role::MultilineTextInput)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .min_h(px(min_height))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(if value.is_empty() {
            palette.color(palette.label_caption)
        } else {
            palette.color(palette.label_primary)
        })
        .child(if value.is_empty() {
            "（空）".to_owned()
        } else {
            value.to_owned()
        })
        .into_any_element()
}

fn render_select(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let value = node.str_prop("value").unwrap_or_default();
    let options = node.array_prop("options").map(Vec::as_slice).unwrap_or(&[]);
    // 命中 options 时显示 label，否则原样显示 value（解析期不保证 value 在选项里）。
    let label = options
        .iter()
        .find(|option| option.get("value").and_then(|value| value.as_str()) == Some(value))
        .and_then(|option| option.get("label").and_then(|label| label.as_str()))
        .unwrap_or(value)
        .to_owned();
    interactive_base(div(), node, path, Role::ComboBox)
        .flex_none()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .text_size(type_scale.secondary)
        .line_height(type_scale.secondary_line)
        .text_color(palette.color(palette.label_primary))
        .aria_value(label.clone())
        .child(div().flex_1().min_w(px(0.0)).child(label))
        .child(
            div()
                .flex_none()
                .text_color(palette.color(palette.label_tertiary))
                .child("▾"),
        )
        .into_any_element()
}

fn render_checkbox(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let checked = node.bool_prop("checked").unwrap_or(false);
    let accent = palette.color(palette.business_primary);
    let mut box_mark = div()
        .flex_none()
        .size(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radii.xs)
        .border_1()
        .border_color(if checked {
            accent
        } else {
            palette.color(palette.border_l3)
        })
        .bg(if checked {
            accent
        } else {
            palette.color(palette.input_major)
        })
        .text_size(px(11.0))
        .line_height(px(11.0))
        .text_color(palette.color(palette.bg_base));
    if checked {
        box_mark = box_mark.child("✓");
    }
    let mut row = interactive_base(div(), node, path, Role::CheckBox)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_toggled(Toggled::from(checked))
        .child(box_mark);
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(label.to_owned()),
        );
    }
    row.into_any_element()
}

fn render_switch(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let checked = node.bool_prop("checked").unwrap_or(false);
    let accent = palette.color(palette.business_primary);
    let knob = div()
        .flex_none()
        .size(px(14.0))
        .rounded_full()
        .bg(palette.color(palette.bg_base));
    let mut track = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .w(px(32.0))
        .h(px(18.0))
        .px(px(2.0))
        .rounded_full()
        .bg(if checked {
            accent
        } else {
            palette.color(palette.border_l4)
        });
    track = if checked {
        track.justify_end()
    } else {
        track.justify_start()
    };
    let mut row = interactive_base(div(), node, path, Role::Switch)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_toggled(Toggled::from(checked))
        .child(track.child(knob));
    if let Some(label) = node.str_prop("label").filter(|label| !label.is_empty()) {
        row = row.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(label.to_owned()),
        );
    }
    row.into_any_element()
}

fn render_slider(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let value = node.f64_prop("value").unwrap_or(0.0);
    let min = node.f64_prop("min").unwrap_or(0.0);
    let max = node.f64_prop("max").unwrap_or(100.0);
    let fraction = slider_fraction(value, min, max);
    let accent = palette.color(palette.business_primary);
    // 填充/剩余两段都用 flex_grow 表达比例，不依赖百分比宽度。
    let mut track = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w(px(0.0))
        .h(px(6.0))
        .rounded_full()
        .overflow_hidden()
        .bg(palette.color(palette.border_l3))
        .child(
            div()
                .flex_grow(fraction)
                .flex_basis(px(0.0))
                .h_full()
                .bg(accent),
        )
        .child(div().flex_grow(1.0 - fraction).flex_basis(px(0.0)).h_full());
    if fraction <= 0.0 {
        // flex_grow(0) 的段宽为 0；再垫一个 0 高元素避免空段触发布局歧义。
        track = track.child(div().flex_none().h(px(0.0)));
    }
    interactive_base(div(), node, path, Role::Slider)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .aria_numeric_value(value)
        .aria_min_numeric_value(min)
        .aria_max_numeric_value(max)
        .child(track)
        .child(
            div()
                .flex_none()
                .w(px(46.0))
                .text_align(TextAlign::Right)
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_secondary))
                .child(format_number(value)),
        )
        .into_any_element()
}

// --- 数据 -----------------------------------------------------------------

fn render_list(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let items = node.array_prop("items").map(Vec::as_slice).unwrap_or(&[]);
    let total = items.len();
    let shown = clip_list_items(total);
    let mut list = interactive_base(div(), node, path, Role::List)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0));
    if shown == 0 {
        return list.child(caption(theme, "（空列表）")).into_any_element();
    }
    for item in items.iter().take(shown) {
        let mut row = div()
            .flex()
            .flex_col()
            .w_full()
            .px(px(6.0))
            .py(px(4.0))
            .rounded(theme.radii.xs);
        let primary = item
            .get("primary")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        row = row.child(
            div()
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(primary.to_owned()),
        );
        if let Some(secondary) = item.get("secondary").and_then(|value| value.as_str()) {
            row = row.child(
                div()
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_tertiary))
                    .child(secondary.to_owned()),
            );
        }
        list = list.child(row);
    }
    if total > shown {
        list = list.child(caption(
            theme,
            format!("（仅渲染前 {shown} 项，共 {total} 项）"),
        ));
    }
    list.into_any_element()
}

fn render_table(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let columns = node.array_prop("columns").map(Vec::as_slice).unwrap_or(&[]);
    let rows = node.array_prop("rows").map(Vec::as_slice).unwrap_or(&[]);
    let column_count = clip_table_columns(columns.len());
    let row_count = clip_table_rows(rows.len());
    let mut table = interactive_base(div(), node, path, Role::Table)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.input_major))
        .overflow_hidden();
    if column_count == 0 {
        return table
            .child(caption(theme, "（表格无列）"))
            .into_any_element();
    }
    // 列宽策略简化：按列均分（`width` 先忽略，见实现说明）。
    let mut header = div()
        .flex()
        .flex_row()
        .w_full()
        .bg(palette.color(palette.tag))
        .border_b_1()
        .border_color(palette.color(palette.border_l2));
    let visible_columns: Vec<&serde_json::Value> = columns.iter().take(column_count).collect();
    for column in &visible_columns {
        let title = column
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        header = header.child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .px(px(8.0))
                .py(px(4.0))
                .whitespace_nowrap()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(palette.color(palette.label_secondary))
                .child(title.to_owned()),
        );
    }
    table = table.child(header);
    if row_count == 0 {
        return table
            .child(caption(theme, "（无数据行）"))
            .into_any_element();
    }
    for (row_index, row_value) in rows.iter().take(row_count).enumerate() {
        let mut row = div()
            .flex()
            .flex_row()
            .w_full()
            .border_b_1()
            .border_color(palette.color(palette.border_l1));
        if row_index + 1 == row_count {
            row = row.border_b_0();
        }
        for column in &visible_columns {
            let key = column
                .get("key")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let text = row_value.get(key).map(cell_text).unwrap_or_default();
            row = row.child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(type_scale.small)
                    .line_height(type_scale.small_line)
                    .text_color(palette.color(palette.label_primary))
                    .child(text),
            );
        }
        table = table.child(row);
    }
    let total_rows = rows.len();
    if total_rows > row_count {
        table = table.child(caption(
            theme,
            format!("（仅渲染前 {row_count} 行，共 {total_rows} 行）"),
        ));
    }
    table.into_any_element()
}

fn render_tree(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let nodes = node.array_prop("nodes").map(Vec::as_slice).unwrap_or(&[]);
    let expanded: HashSet<&str> = node
        .array_prop("expanded")
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str())
                .collect::<HashSet<&str>>()
        })
        .unwrap_or_default();
    let mut tree = interactive_base(div(), node, path, Role::Tree)
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0));
    let mut rendered = 0usize;
    for tree_node in nodes {
        render_tree_node(theme, tree_node, &expanded, 0, &mut tree, &mut rendered);
        if rendered >= MAX_TREE_ROWS {
            break;
        }
    }
    if rendered == 0 {
        return tree.child(caption(theme, "（空树）")).into_any_element();
    }
    if nodes.len() > 1 && rendered >= MAX_TREE_ROWS {
        tree = tree.child(caption(
            theme,
            format!("（仅渲染前 {MAX_TREE_ROWS} 个节点）"),
        ));
    }
    tree.into_any_element()
}

fn render_tree_node(
    theme: &Theme,
    value: &serde_json::Value,
    expanded: &HashSet<&str>,
    depth: usize,
    tree: &mut Stateful<Div>,
    rendered: &mut usize,
) {
    if *rendered >= MAX_TREE_ROWS {
        return;
    }
    *rendered += 1;
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let key = value
        .get("key")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let label = value
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let children: &[serde_json::Value] = value
        .get("children")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let open = expanded.contains(key);
    let glyph = if children.is_empty() {
        "·"
    } else if open {
        "▾"
    } else {
        "▸"
    };
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(px(4.0))
        .pl(px(8.0 + depth as f32 * 14.0))
        .py(px(2.0))
        .child(
            div()
                .flex_none()
                .w(px(12.0))
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_tertiary))
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(type_scale.secondary)
                .line_height(type_scale.secondary_line)
                .text_color(palette.color(palette.label_primary))
                .child(label.to_owned()),
        )
        .into_any_element();
    tree.extend(std::iter::once(row));
    if open {
        for child in children {
            render_tree_node(theme, child, expanded, depth + 1, tree, rendered);
            if *rendered >= MAX_TREE_ROWS {
                return;
            }
        }
    }
}

fn render_key_value(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let pairs = node.array_prop("pairs").map(Vec::as_slice).unwrap_or(&[]);
    let shown = clip_table_rows(pairs.len());
    let mut list = div().flex().flex_col().w_full().gap(px(4.0));
    if shown == 0 {
        return list
            .child(caption(theme, "（无键值对）"))
            .into_any_element();
    }
    for pair in pairs.iter().take(shown) {
        let key = pair
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let value = pair.get("value").map(cell_text).unwrap_or_default();
        list = list.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .w_full()
                .py(px(2.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(96.0))
                        .text_size(type_scale.small)
                        .line_height(type_scale.small_line)
                        .text_color(palette.color(palette.label_tertiary))
                        .child(key.to_owned()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(type_scale.secondary)
                        .line_height(type_scale.secondary_line)
                        .text_color(palette.color(palette.label_primary))
                        .child(value),
                ),
        );
    }
    if pairs.len() > shown {
        list = list.child(caption(
            theme,
            format!("（仅渲染前 {shown} 组，共 {} 组）", pairs.len()),
        ));
    }
    list.into_any_element()
}

// --- 媒体 -----------------------------------------------------------------

fn render_image(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let attachment = node.str_prop("attachmentId").unwrap_or_default();
    let alt = node.str_prop("alt").unwrap_or_default();
    // v1 不做网络/解码：只画占位框 + alt（spec §6.7 / §10.1，附件由 Host 解析后替换本元素）。
    let mut placeholder = div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(CHART_HEIGHT))
        .gap(px(4.0))
        .rounded(theme.radii.md)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .bg(palette.color(palette.tag))
        .child(
            div()
                .text_size(px(22.0))
                .line_height(px(26.0))
                .text_color(palette.color(palette.label_caption))
                .child("▢"),
        );
    let label = if alt.is_empty() { "图片" } else { alt };
    placeholder = placeholder.child(
        div()
            .text_size(type_scale.small)
            .line_height(type_scale.small_line)
            .text_color(palette.color(palette.label_secondary))
            .child(label.to_owned()),
    );
    if !attachment.is_empty() {
        placeholder = placeholder.child(
            div()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_caption))
                .child(format!("附件 {attachment}")),
        );
    }
    interactive_base(div(), node, path, Role::Image)
        .aria_label(label)
        .child(placeholder)
        .into_any_element()
}

fn render_icon(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let name = node.str_prop("name").unwrap_or_default();
    let glyph = icon_glyph(name);
    let color = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.label_secondary),
    };
    let label = node.a11y_label.clone().unwrap_or_else(|| name.to_owned());
    interactive_base(div(), node, path, Role::Image)
        .flex_none()
        .text_size(px(16.0))
        .line_height(px(18.0))
        .text_color(color)
        .aria_label(label)
        .child(glyph)
        .into_any_element()
}

fn render_chart(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let accent = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.business_primary),
    };
    let data = node.array_prop("data").map(Vec::as_slice).unwrap_or(&[]);
    let series = node.array_prop("series").map(Vec::as_slice).unwrap_or(&[]);

    let mut labels: Vec<String> = Vec::new();
    let mut values: Vec<f64> = Vec::new();
    if !data.is_empty() {
        for point in data.iter().take(clip_chart_points(data.len())) {
            labels.push(
                point
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
            values.push(
                point
                    .get("value")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
            );
        }
    } else if let Some(first) = series.first() {
        // 多条 series 只画第一条（降级）；其余只出现在图例里。
        values = series_points(first);
        labels = (1..=values.len())
            .map(|index| format!("#{index}"))
            .collect();
    }

    let mut container = div().flex().flex_col().w_full().gap(px(8.0));
    if values.is_empty() {
        return container
            .child(caption(theme, "（图表无数据）"))
            .into_any_element();
    }
    let body = match node.str_prop("kind").unwrap_or("bars") {
        // 降级：GPUI 此 rev 没有折线绘制原语，line 用细柱近似（见实现说明）。
        "line" => chart_columns(theme, &values, accent, Some(4.0)),
        // 降级：donut 用按比例分段的横条 + 图例表达占比（无弧线绘制原语）。
        "donut" => chart_donut(theme, &values, accent),
        _ => chart_columns(theme, &values, accent, None),
    };
    container = container.child(body);
    container = container.child(chart_legend(theme, &labels, &values));
    container.into_any_element()
}

fn series_points(series: &serde_json::Value) -> Vec<f64> {
    let values = series
        .get("values")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    values
        .iter()
        .take(clip_chart_points(values.len()))
        .map(|value| value.as_f64().unwrap_or(0.0))
        .collect()
}

fn chart_columns(theme: &Theme, values: &[f64], accent: Rgba, bar_width: Option<f32>) -> Div {
    let heights = chart_bar_heights(values, CHART_HEIGHT);
    let mut row = div()
        .flex()
        .flex_row()
        .items_end()
        .w_full()
        .h(px(CHART_HEIGHT))
        .gap(px(6.0));
    for height in heights {
        let mut bar = div().h(px(height)).rounded(theme.radii.xs).bg(accent);
        bar = match bar_width {
            Some(width) => bar.w(px(width)),
            None => bar.flex_1().min_w(px(2.0)),
        };
        row = row.child(bar);
    }
    row
}

fn chart_donut(theme: &Theme, values: &[f64], accent: Rgba) -> Div {
    let palette = theme.palette;
    let positive: Vec<f64> = values
        .iter()
        .map(|value| {
            if value.is_finite() && *value > 0.0 {
                *value
            } else {
                0.0
            }
        })
        .collect();
    let total: f64 = positive.iter().sum();
    let mut row = div()
        .flex()
        .flex_row()
        .w_full()
        .h(px(18.0))
        .rounded_full()
        .overflow_hidden()
        .bg(palette.color(palette.tag));
    if total <= 0.0 {
        return row;
    }
    for (index, value) in positive.iter().enumerate() {
        let ratio = (value / total) as f32;
        let tint = if index % 2 == 0 {
            accent
        } else {
            accent.opacity(0.55)
        };
        row = row.child(div().flex_grow(ratio).flex_basis(px(0.0)).h_full().bg(tint));
    }
    row
}

fn chart_legend(theme: &Theme, labels: &[String], values: &[f64]) -> Div {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let mut legend = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(10.0))
        .w_full();
    for (index, label) in labels.iter().enumerate() {
        let value = values.get(index).copied().unwrap_or(0.0);
        let text = if label.is_empty() {
            format_number(value)
        } else {
            format!("{label} {}", format_number(value))
        };
        legend = legend.child(
            div()
                .flex_none()
                .text_size(type_scale.small)
                .line_height(type_scale.small_line)
                .text_color(palette.color(palette.label_tertiary))
                .child(text),
        );
    }
    legend
}

// --- 容器 -----------------------------------------------------------------

fn render_card(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let border = match node.tone {
        Some(tone) => tone_color(&palette, Some(tone.as_str())),
        None => palette.color(palette.border_l2),
    };
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.md)
        .border_1()
        .border_color(border)
        .bg(palette.color(palette.bg_layer_2))
        .overflow_hidden();
    if let Some(title) = node.str_prop("title").filter(|title| !title.is_empty()) {
        card = card.child(
            div()
                .w_full()
                .px(px(12.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .text_size(type_scale.body)
                .line_height(type_scale.body_line)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(palette.color(palette.label_primary))
                .child(title.to_owned()),
        );
    }
    let mut body = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .p(px(12.0))
        .gap(px(8.0));
    for (index, child) in node.children.iter().enumerate() {
        body = body.child(render_node(theme, child, &child_path(path, index)));
    }
    card.child(body).into_any_element()
}

fn render_tabs(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let tabs = node.array_prop("tabs").map(Vec::as_slice).unwrap_or(&[]);
    let active = node.str_prop("activeKey").unwrap_or_default();
    let mut bar = div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(px(4.0))
        .border_b_1()
        .border_color(palette.color(palette.border_l1));
    for tab in tabs.iter().take(MAX_TABS) {
        let key = tab
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let label = tab
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let is_active = !active.is_empty() && key == active;
        let mut chip = div()
            .flex_none()
            .px(px(10.0))
            .py(px(5.0))
            .text_size(type_scale.secondary)
            .line_height(type_scale.secondary_line);
        chip = if is_active {
            chip.bg(palette.color(palette.bubble))
                .text_color(palette.color(palette.business_primary))
        } else {
            chip.text_color(palette.color(palette.label_secondary))
        };
        bar = bar.child(chip.child(label.to_owned()));
    }
    let mut container = interactive_base(
        div().flex().flex_col().w_full().min_w(px(0.0)).gap(px(8.0)),
        node,
        path,
        Role::TabList,
    )
    .child(bar);
    // `Tabs` 的节点级 children 就是当前 active 页内容（spec §6.8）。
    for (index, child) in node.children.iter().enumerate() {
        container = container.child(render_node(theme, child, &child_path(path, index)));
    }
    container.into_any_element()
}

fn render_accordion(theme: &Theme, node: &ElementNode, path: &str) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    let sections = node
        .array_prop("sections")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let expanded: HashSet<&str> = node
        .array_prop("expanded")
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str())
                .collect::<HashSet<&str>>()
        })
        .unwrap_or_default();
    let mut container = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_color(palette.color(palette.border_l2))
        .overflow_hidden();
    if sections.is_empty() {
        return container
            .child(caption(theme, "（无 section）"))
            .into_any_element();
    }
    for section in sections.iter().take(MAX_ACCORDION_SECTIONS) {
        let key = section
            .get("key")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let title = section
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let open = expanded.contains(key);
        let glyph = if open { "▾" } else { "▸" };
        container = container.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .w_full()
                .px(px(10.0))
                .py(px(6.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1))
                .child(
                    div()
                        .flex_none()
                        .w(px(12.0))
                        .text_size(type_scale.small)
                        .line_height(type_scale.small_line)
                        .text_color(palette.color(palette.label_tertiary))
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(type_scale.secondary)
                        .line_height(type_scale.secondary_line)
                        .text_color(palette.color(palette.label_primary))
                        .child(title.to_owned()),
                ),
        );
        if !open {
            continue;
        }
        // `sections[].children` 在数据层是 `nested_children[key]`（spec §6.8 的容器规则）。
        if let Some(children) = node.nested_children.get(key) {
            let mut body = div()
                .flex()
                .flex_col()
                .w_full()
                .min_w(px(0.0))
                .px(px(10.0))
                .py(px(8.0))
                .gap(px(8.0))
                .border_b_1()
                .border_color(palette.color(palette.border_l1));
            for (index, child) in children.iter().enumerate() {
                body = body.child(render_node(
                    theme,
                    child,
                    &format!("{path}.sections[{key}].children[{index}]"),
                ));
            }
            container = container.child(body);
        }
    }
    container.into_any_element()
}

// --- 降级 -----------------------------------------------------------------

fn render_placeholder(theme: &Theme, node: &ElementNode) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    // 未知元素：灰框 + 类型名 + testId（spec §9；不 panic、不丢整棵树）。
    let label = match &node.placeholder {
        Some(PlaceholderReason::UnknownType { type_name }) => placeholder_label(type_name, None),
        Some(PlaceholderReason::MissingType) => "缺少 type 的节点".to_owned(),
        Some(PlaceholderReason::InvalidNode { detail }) => {
            placeholder_label(node.kind.as_str(), Some(detail))
        }
        Some(PlaceholderReason::InvalidProps { detail }) => {
            placeholder_label(node.kind.as_str(), Some(detail))
        }
        Some(PlaceholderReason::DepthExceeded { depth, max_depth }) => {
            format!("超过深度配额（{depth}/{max_depth}）")
        }
        Some(PlaceholderReason::NodeBudgetExceeded { max_nodes }) => {
            format!("超过节点配额（{max_nodes}）")
        }
        // `PlaceholderReason` 是 `#[non_exhaustive]`：未知原因按未知元素处理。
        _ => placeholder_label(node.kind.as_str(), None),
    };
    let mut placeholder = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w(px(0.0))
        .gap(px(2.0))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(theme.radii.sm)
        .border_1()
        .border_dashed()
        .border_color(palette.color(palette.border_l3))
        .bg(palette.color(palette.tag))
        .text_size(type_scale.small)
        .line_height(type_scale.small_line)
        .text_color(palette.color(palette.label_secondary))
        .child(label);
    if let Some(test_id) = &node.test_id {
        placeholder = placeholder.child(
            div()
                .text_color(palette.color(palette.label_caption))
                .child(format!("testId: {test_id}")),
        );
    }
    placeholder.into_any_element()
}

// --- 渲染小工具 -----------------------------------------------------------

/// 交互元素统一入口：分配稳定元素 id、写 AccessKit role 与 `a11yLabel`。
///
/// `testId` 同时写到 `accessibility_id`（Windows UIA 的 `AutomationId`），这样截图/自动化
/// 与解析期诊断能对上同一个钩子。
fn interactive_base(element: Div, node: &ElementNode, path: &str, role: Role) -> Stateful<Div> {
    let element = element.id(format!("adc-{path}")).role(role);
    let element = match &node.a11y_label {
        Some(label) => element.aria_label(label.as_str()),
        None => element,
    };
    match &node.test_id {
        Some(test_id) => element.accessibility_id(test_id.as_str()),
        None => element,
    }
}

fn child_path(parent: &str, index: usize) -> String {
    format!("{parent}.children[{index}]")
}

fn code_font(theme: &Theme) -> Font {
    Font {
        family: SharedString::from(theme.font_code),
        ..Font::default()
    }
}

fn text_block(text: impl Into<String>, metrics: TextMetrics, color: Rgba) -> Div {
    div()
        .text_size(metrics.size)
        .line_height(metrics.line_height)
        .text_color(color)
        .child(text.into())
}

fn caption(theme: &Theme, text: impl Into<String>) -> AnyElement {
    let palette = theme.palette;
    let type_scale = theme.type_scale;
    div()
        .w_full()
        .px(px(8.0))
        .py(px(4.0))
        .text_size(type_scale.small)
        .line_height(type_scale.small_line)
        .text_color(palette.color(palette.label_tertiary))
        .child(text.into())
        .into_any_element()
}

/// `Text.maxLines` / `Markdown.maxLines`：1..=1000，非法值忽略。
fn max_lines(node: &ElementNode) -> Option<usize> {
    node.i64_prop("maxLines")
        .filter(|value| *value >= 1)
        .map(|value| value.min(1000) as usize)
}

/// 数值展示：整数不带小数点，其余用最短表示。
fn format_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    // ADC-ELEM-SPACING-001
    #[test]
    fn spacing_accepts_only_documented_tokens() {
        for token in SPACING_TOKENS {
            assert!(is_spacing_token(token), "{token} 应当是合法 spacing token");
        }
        for invalid in [-4_i64, -1, 1, 3, 5, 6, 10, 15, 20, 40, 1000] {
            assert!(
                !is_spacing_token(invalid),
                "{invalid} 不应被当作合法 spacing token"
            );
        }
    }

    // ADC-ELEM-SPACING-002
    #[test]
    fn spacing_px_maps_tokens_and_defaults_invalid_to_zero() {
        assert_eq!(spacing_px(Some(0)), px(0.0));
        assert_eq!(spacing_px(Some(8)), px(8.0));
        assert_eq!(spacing_px(Some(32)), px(32.0));
        assert_eq!(spacing_px(Some(9)), px(0.0));
        assert_eq!(spacing_px(None), px(0.0));
    }

    // ADC-ELEM-ALIGN-001
    #[test]
    fn align_maps_the_four_documented_values() {
        assert_eq!(align_items("start"), AlignItems::Start);
        assert_eq!(align_items("center"), AlignItems::Center);
        assert_eq!(align_items("end"), AlignItems::End);
        assert_eq!(align_items("stretch"), AlignItems::Stretch);
        // 未知值降级为起点，不 panic。
        assert_eq!(align_items("baseline"), AlignItems::Start);
        assert_eq!(align_items(""), AlignItems::Start);
    }

    // ADC-ELEM-JUSTIFY-001
    #[test]
    fn justify_maps_the_four_documented_values() {
        assert_eq!(justify_content("start"), AlignContent::Start);
        assert_eq!(justify_content("center"), AlignContent::Center);
        assert_eq!(justify_content("end"), AlignContent::End);
        assert_eq!(justify_content("space-between"), AlignContent::SpaceBetween);
        // 未知值降级为起点。
        assert_eq!(justify_content("space-around"), AlignContent::Start);
        assert_eq!(justify_content(""), AlignContent::Start);
    }

    // ADC-ELEM-GRID-001
    #[test]
    fn grid_columns_stay_within_one_to_twelve() {
        assert_eq!(grid_columns(Some(1)), 1);
        assert_eq!(grid_columns(Some(7)), 7);
        assert_eq!(grid_columns(Some(12)), 12);
        assert_eq!(grid_columns(Some(0)), 1);
        assert_eq!(grid_columns(Some(13)), 1);
        assert_eq!(grid_columns(Some(-2)), 1);
        assert_eq!(grid_columns(None), 1);
    }

    // ADC-ELEM-GRID-002
    #[test]
    fn grid_span_stays_within_one_to_twelve() {
        assert_eq!(grid_span(Some(1)), 1);
        assert_eq!(grid_span(Some(12)), 12);
        assert_eq!(grid_span(Some(13)), 1);
        assert_eq!(grid_span(Some(0)), 1);
        assert_eq!(grid_span(None), 1);
    }

    // ADC-ELEM-CHART-001
    #[test]
    fn chart_points_and_series_are_clipped() {
        assert_eq!(clip_chart_points(0), 0);
        assert_eq!(clip_chart_points(3), 3);
        assert_eq!(clip_chart_points(8), 8);
        assert_eq!(clip_chart_points(9), 8);
        assert_eq!(clip_chart_points(100), 8);
        assert_eq!(clip_chart_series(0), 0);
        assert_eq!(clip_chart_series(4), 4);
        assert_eq!(clip_chart_series(5), 4);
    }

    // ADC-ELEM-TABLE-001
    #[test]
    fn table_columns_and_rows_are_clipped() {
        assert_eq!(clip_table_columns(0), 0);
        assert_eq!(clip_table_columns(12), 12);
        assert_eq!(clip_table_columns(13), 12);
        assert_eq!(clip_table_columns(1000), 12);
        assert_eq!(clip_table_rows(0), 0);
        assert_eq!(clip_table_rows(200), 200);
        assert_eq!(clip_table_rows(201), 200);
        assert_eq!(clip_table_rows(50_000), 200);
    }

    // ADC-ELEM-LIST-001
    #[test]
    fn list_items_are_clipped() {
        assert_eq!(clip_list_items(0), 0);
        assert_eq!(clip_list_items(199), 199);
        assert_eq!(clip_list_items(200), 200);
        assert_eq!(clip_list_items(201), 200);
    }

    // ADC-ELEM-TEXT-001
    #[test]
    fn truncate_text_keeps_utf8_boundaries() {
        let ascii = "a".repeat(MAX_TEXT_BYTES + 10);
        assert_eq!(truncate_text(&ascii).len(), MAX_TEXT_BYTES);

        // 每个汉字 3 字节：截止点落在字符中间时必须回退到边界。
        let han = "汉".repeat(MAX_TEXT_BYTES / 3 + 10);
        let clipped = truncate_text(&han);
        assert!(clipped.len() <= MAX_TEXT_BYTES);
        assert!(clipped.is_char_boundary(clipped.len()));
        assert_eq!(clipped.len() % 3, 0);
        assert!(clipped.chars().all(|ch| ch == '汉'));
    }

    // ADC-ELEM-TEXT-002
    #[test]
    fn truncate_text_to_handles_short_and_degenerate_input() {
        assert_eq!(truncate_text_to("short", MAX_TEXT_BYTES), "short");
        assert_eq!(truncate_text_to("short", 0), "");
        assert_eq!(truncate_text_to("short", 3), "sho");
        assert_eq!(truncate_text_to("汉字", 4), "汉");
        assert_eq!(truncate_text_to("", 8), "");
    }

    // ADC-ELEM-CHART-002
    #[test]
    fn chart_bar_heights_normalize_and_survive_bad_values() {
        let heights = chart_bar_heights(&[2.0, 4.0, 1.0], 100.0);
        assert_eq!(heights, vec![50.0, 100.0, 25.0]);

        // 全 0 / 负数 / NaN 都退化为 0 高度。
        assert_eq!(chart_bar_heights(&[0.0, 0.0], 80.0), vec![0.0, 0.0]);
        assert_eq!(chart_bar_heights(&[-1.0, -2.0], 80.0), vec![0.0, 0.0]);
        assert_eq!(chart_bar_heights(&[f64::NAN, 0.0], 80.0), vec![0.0, 0.0]);

        // 超过 8 点先裁剪再归一化。
        let many: Vec<f64> = (0..20).map(|index| index as f64 + 1.0).collect();
        assert_eq!(chart_bar_heights(&many, 10.0).len(), MAX_CHART_POINTS);

        // 高度为零/非法时全部退化为 0，不出现 NaN。
        assert_eq!(chart_bar_heights(&[1.0, 2.0], 0.0), vec![0.0, 0.0]);
        assert_eq!(chart_bar_heights(&[1.0, 2.0], f32::NAN), vec![0.0, 0.0]);
    }

    // ADC-ELEM-SLIDER-001
    #[test]
    fn slider_fraction_clamps_and_handles_degenerate_range() {
        assert_eq!(slider_fraction(0.0, 0.0, 100.0), 0.0);
        assert_eq!(slider_fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(slider_fraction(100.0, 0.0, 100.0), 1.0);
        assert_eq!(slider_fraction(-10.0, 0.0, 100.0), 0.0);
        assert_eq!(slider_fraction(150.0, 0.0, 100.0), 1.0);
        // min >= max 与非法值按 0。
        assert_eq!(slider_fraction(5.0, 10.0, 10.0), 0.0);
        assert_eq!(slider_fraction(5.0, 10.0, -10.0), 0.0);
        assert_eq!(slider_fraction(f64::NAN, 0.0, 100.0), 0.0);
        // 负区间同样线性。
        assert_eq!(slider_fraction(-5.0, -10.0, 10.0), 0.25);
    }

    // ADC-ELEM-TONE-001
    #[test]
    fn tone_uses_theme_tokens_only() {
        let palette = Palette::LIGHT;
        assert_eq!(tone_token(&palette, None), palette.label_secondary);
        assert_eq!(
            tone_token(&palette, Some("neutral")),
            palette.label_secondary
        );
        assert_eq!(tone_token(&palette, Some("info")), palette.business_primary);
        assert_eq!(tone_token(&palette, Some("success")), palette.state_success);
        assert_eq!(tone_token(&palette, Some("warning")), palette.state_warn);
        assert_eq!(tone_token(&palette, Some("error")), palette.state_error);
        // 未知名回落 neutral。
        assert_eq!(tone_token(&palette, Some("brand")), palette.label_secondary);

        let dark = Palette::DARK;
        assert_eq!(
            tone_color(&dark, Some("error")),
            dark.color(dark.state_error)
        );
    }

    // ADC-ELEM-ICON-001
    #[test]
    fn icon_glyph_maps_known_names_and_falls_back() {
        assert_eq!(icon_glyph("check"), "✓");
        assert_eq!(icon_glyph("arrow-right"), "→");
        assert_eq!(icon_glyph("does.not.exist"), UNKNOWN_GLYPH);
        assert_eq!(icon_glyph(""), UNKNOWN_GLYPH);
    }

    // ADC-ELEM-A11Y-001
    #[test]
    fn a11y_label_missing_flags_interactive_elements_only() {
        // 交互元素缺标签 → 缺失。
        assert!(a11y_label_missing("Button", None));
        assert!(a11y_label_missing("Input", Some("  ")));
        assert!(!a11y_label_missing("Button", Some("运行")));
        assert!(!a11y_label_missing("Switch", Some("启用")));
        // 非交互元素不要求标签。
        assert!(!a11y_label_missing("Text", None));
        assert!(!a11y_label_missing("Card", None));
        assert!(!a11y_label_missing("Placeholder", None));
    }

    // ADC-ELEM-A11Y-002
    #[test]
    fn a11y_issue_carries_code_path_and_test_id() {
        let issue = a11y_issue("Slider", None, "$.children[3]", Some("volume")).expect("应有诊断");
        assert_eq!(issue.code, A11Y_MISSING_CODE);
        assert_eq!(issue.code, "ADC-ELEM-A11Y-001");
        assert_eq!(issue.kind, "Slider");
        assert_eq!(issue.path, "$.children[3]");
        assert_eq!(issue.test_id.as_deref(), Some("volume"));

        assert!(a11y_issue("Slider", Some("音量"), "$.children[3]", None).is_none());
        assert!(a11y_issue("Text", None, "$.root", None).is_none());
    }

    // ADC-ELEM-TABLE-002
    #[test]
    fn cell_text_renders_scalar_values() {
        assert_eq!(cell_text(&serde_json::json!("文本")), "文本");
        assert_eq!(cell_text(&serde_json::json!(42)), "42");
        assert_eq!(cell_text(&serde_json::json!(1.5)), "1.5");
        assert_eq!(cell_text(&serde_json::json!(true)), "true");
        assert_eq!(cell_text(&serde_json::json!(null)), "—");
    }

    // ADC-ELEM-PLACEHOLDER-001
    #[test]
    fn placeholder_label_includes_type_and_detail() {
        assert_eq!(placeholder_label("Sparkline", None), "未知元素: Sparkline");
        assert_eq!(
            placeholder_label("Sparkline", Some("unknown type")),
            "未知元素: Sparkline（unknown type）"
        );
        // 空白 detail 不额外加括号。
        assert_eq!(
            placeholder_label("Sparkline", Some("   ")),
            "未知元素: Sparkline"
        );
    }

    // ADC-ELEM-TEXT-003
    #[test]
    fn text_metrics_follow_theme_token_scale() {
        let theme = Theme::new(ThemeKind::Light);
        let body = text_metrics(&theme, None);
        assert_eq!(body.size, theme.type_scale.body);
        assert_eq!(body.line_height, theme.type_scale.body_line);
        assert!(!body.mono);

        let caption = text_metrics(&theme, Some("caption"));
        assert_eq!(caption.size, theme.type_scale.small);

        let title = text_metrics(&theme, Some("title"));
        assert_eq!(title.size, theme.type_scale.h3);

        let mono = text_metrics(&theme, Some("mono"));
        assert!(mono.mono);

        // 未知 style 按 body。
        assert_eq!(text_metrics(&theme, Some("weird")), body);
    }

    // ADC-ELEM-NUM-001
    #[test]
    fn format_number_keeps_integers_short() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-7.0), "-7");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(0.25), "0.25");
        assert_eq!(format_number(f64::NAN), "NaN");
    }

    // ADC-ELEM-A11Y-003
    #[test]
    fn interactive_kinds_match_the_contributions_crate() {
        // 渲染侧的 a11y 判断必须与服务端 `ElementKind::is_interactive` 一致。
        for name in dshr_ui_contributions::ELEMENT_TYPE_NAMES {
            let kind = ElementKind::from_name(name).expect("规范元素类型");
            assert_eq!(
                is_interactive_kind(kind.as_str()),
                kind.is_interactive(),
                "{name} 的交互性判断两边不一致"
            );
        }
        assert!(!is_interactive_kind("Placeholder"));
    }

    // ADC-ELEM-RENDER-001
    #[test]
    fn every_element_kind_renders_without_a_window() {
        let theme = Theme::light();
        // 26 个规范元素在缺省 props 下都必须能构造元素（不 panic、不要求窗口）。
        for name in dshr_ui_contributions::ELEMENT_TYPE_NAMES {
            let kind = ElementKind::from_name(name).expect("规范元素类型");
            let node = ElementNode::new(kind);
            let _ = adc_view(&theme, &node);
        }
        // Placeholder（未知元素降级）同样不 panic。
        let unknown = ElementNode::placeholder(PlaceholderReason::UnknownType {
            type_name: "Sparkline".to_owned(),
        });
        let _ = adc_view(&theme, &unknown);
        let missing = ElementNode::placeholder(PlaceholderReason::MissingType);
        let _ = adc_view(&theme, &missing);
        // visible=false 渲染为空元素。
        let mut hidden = ElementNode::new(ElementKind::Text);
        hidden.visible = false;
        let _ = adc_view(&Theme::dark(), &hidden);
    }

    // ADC-ELEM-RENDER-002
    #[test]
    fn representative_tree_renders_with_props() {
        let theme = Theme::dark();
        let mut table = ElementNode::new(ElementKind::Table);
        table.props = serde_json::json!({
            "columns": [
                {"key": "name", "title": "名称"},
                {"key": "count", "title": "数量", "width": 80},
            ],
            "rows": [
                {"name": "甲", "count": 1},
                {"name": "乙", "count": 2.5},
            ],
        });
        table.a11y_label = Some("明细表".to_owned());
        table.test_id = Some("detail-table".to_owned());

        let mut chart = ElementNode::new(ElementKind::Chart);
        chart.props = serde_json::json!({
            "kind": "donut",
            "data": [{"label": "a", "value": 1}, {"label": "b", "value": 2}],
            "series": [{"label": "s1", "values": [1, 2, 3]}],
        });

        let mut code = ElementNode::new(ElementKind::Code);
        code.props = serde_json::json!({
            "source": "fn main() {\n    println!(\"hi\");\n}",
            "language": "rust",
            "lineNumbers": true,
        });

        let mut accordion = ElementNode::new(ElementKind::Accordion);
        accordion.props = serde_json::json!({
            "sections": [{"key": "s1", "title": "高级"}],
            "expanded": ["s1"],
        });
        let mut button = ElementNode::new(ElementKind::Button);
        button.a11y_label = Some("运行".to_owned());
        accordion
            .nested_children
            .insert("s1".to_owned(), vec![button]);

        let mut stack = ElementNode::new(ElementKind::Stack);
        stack.props = serde_json::json!({
            "gap": 8,
            "padding": 16,
            "align": "center",
            "justify": "space-between",
        });
        stack.children = vec![table, chart, code, accordion];

        let view = adc_view(&theme, &stack);
        // 构造完成即成功；这里只断言它能转成元素（类型由返回值保证）。
        let _: AnyElement = view;
    }

    // ADC-ELEM-A11Y-004
    #[test]
    fn a11y_issues_walk_children_and_accordion_sections() {
        let mut root = ElementNode::new(ElementKind::Accordion);
        root.props = serde_json::json!({
            "sections": [{"key": "s1", "title": "高级"}],
            "expanded": ["s1"],
        });
        let mut missing_button = ElementNode::new(ElementKind::Button);
        missing_button.test_id = Some("run".to_owned());
        root.nested_children
            .insert("s1".to_owned(), vec![missing_button]);
        let mut ok_switch = ElementNode::new(ElementKind::Switch);
        ok_switch.a11y_label = Some("启用".to_owned());
        root.children = vec![ok_switch];

        let issues = a11y_issues_in(&root);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, A11Y_MISSING_CODE);
        assert_eq!(issues[0].kind, "Button");
        assert_eq!(issues[0].path, "$.sections[s1].children[0]");
        assert_eq!(issues[0].test_id.as_deref(), Some("run"));

        // 补齐标签后不再有诊断。
        let mut filled = ElementNode::new(ElementKind::Button);
        filled.a11y_label = Some("运行".to_owned());
        root.nested_children.insert("s1".to_owned(), vec![filled]);
        assert!(a11y_issues_in(&root).is_empty());
    }

    // ADC-ELEM-RENDER-003
    #[test]
    fn parse_tree_output_renders_end_to_end() {
        // 契约测试：`parse_tree` 的真实输出必须能直接喂给渲染器；
        // 同时验证 26 个元素按 spec 顺序解析成预期 kind，且 Accordion 的
        // section children 走 `nested_children`（渲染器按 key 取）。
        let json = serde_json::json!({
            "type": "Stack",
            "props": {"gap": 8},
            "children": [
                {"type": "Stack", "props": {"gap": 4}, "children": [{"type": "Text", "props": {"value": "hi"}}]},
                {"type": "Row", "props": {"gap": 4, "wrap": true}, "children": []},
                {"type": "Grid", "props": {"columns": 2}, "children": []},
                {"type": "Divider", "props": {"label": "分隔"}},
                {"type": "Spacer"},
                {"type": "Text", "props": {"value": "正文", "style": "body", "maxLines": 2}},
                {"type": "Markdown", "props": {"source": "# 标题\n\n- 项"}},
                {"type": "Code", "props": {"source": "x = 1\ny = 2", "language": "python", "lineNumbers": true}},
                {"type": "Badge", "props": {"label": "新", "tone": "success"}},
                {"type": "Button", "props": {"label": "运行", "a11yLabel": "运行"}},
                {"type": "Input", "props": {"value": "abc", "placeholder": "输入", "a11yLabel": "名称"}},
                {"type": "Textarea", "props": {"value": "多行", "rows": 3, "a11yLabel": "备注"}},
                {"type": "Select", "props": {"value": "a", "options": [{"value": "a", "label": "甲"}], "a11yLabel": "选择"}},
                {"type": "Checkbox", "props": {"checked": true, "label": "启用", "a11yLabel": "启用"}},
                {"type": "Switch", "props": {"checked": false, "label": "开关", "a11yLabel": "开关"}},
                {"type": "Slider", "props": {"value": 3, "min": 0, "max": 10, "a11yLabel": "比例"}},
                {"type": "List", "props": {"items": [{"key": "a", "primary": "甲", "secondary": "副"}], "a11yLabel": "列表"}},
                {"type": "Table", "props": {
                    "columns": [{"key": "name", "title": "名称", "width": 120}],
                    "rows": [{"name": "甲", "n": 1, "ok": true, "none": null}],
                    "a11yLabel": "表格"}},
                {"type": "Tree", "props": {"nodes": [{"key": "root", "label": "根", "children": [{"key": "leaf", "label": "叶"}]}], "expanded": ["root"], "a11yLabel": "树"}},
                {"type": "KeyValue", "props": {"pairs": [{"key": "k", "value": "v"}]}},
                {"type": "Image", "props": {"attachmentId": "sha256:abc", "alt": "示意图", "fit": "contain"}},
                {"type": "Icon", "props": {"name": "check", "a11yLabel": "完成"}},
                {"type": "Chart", "props": {"kind": "bars", "data": [{"label": "a", "value": 1}, {"label": "b", "value": 3}]}},
                {"type": "Card", "props": {"title": "卡片"}, "children": [{"type": "Text", "props": {"value": "正文"}}]},
                {"type": "Tabs", "props": {"tabs": [{"key": "t1", "label": "一"}], "activeKey": "t1", "a11yLabel": "页签"}, "children": [{"type": "Text", "props": {"value": "页内容"}}]},
                {"type": "Accordion", "props": {
                    "sections": [{"key": "s1", "title": "高级", "children": [{"type": "Text", "props": {"value": "折叠内容"}}]}],
                    "expanded": ["s1"]}},
                {"type": "Sparkline", "props": {}},
                {"type": "Text", "children": [{"type": "Text", "props": {"value": "叶子带 children"}}]}
            ]
        });
        let parsed = dshr_ui_contributions::parse_tree(&json);
        let theme = Theme::light();
        let _ = adc_view(&theme, &parsed.root);

        // 26 个规范元素按顺序解析成预期 kind；未知元素降级为 Placeholder。
        for (index, name) in dshr_ui_contributions::ELEMENT_TYPE_NAMES.iter().enumerate() {
            assert_eq!(
                parsed.root.children[index].kind.as_str(),
                *name,
                "第 {index} 个子节点应为 {name}"
            );
        }
        assert_eq!(parsed.root.children[26].kind.as_str(), "Placeholder");
        // Accordion 的 section children 落在 nested_children，渲染器按 section key 取。
        let accordion = &parsed.root.children[25];
        assert!(accordion.nested_children.contains_key("s1"));
        assert_eq!(
            accordion.nested_children["s1"][0].kind.as_str(),
            "Text",
            "section 子节点应被解析"
        );
        // 叶子带 children 属契约错误：字段被忽略并诊断，节点保留原类型（不 panic）。
        let leaf = &parsed.root.children[27];
        assert_eq!(leaf.kind.as_str(), "Text");
        assert!(leaf.children.is_empty());
        assert!(
            parsed.diagnostics.iter().any(|diagnostic| diagnostic.code
                == dshr_ui_contributions::DiagnosticCode::LeafWithChildren),
            "叶子带 children 必须产生 LeafWithChildren 诊断"
        );
    }

    // ---- panel / tool-card 渲染入口（ADC-ELEM-PANEL-*） -------------------

    // ADC-ELEM-PANEL-001
    #[test]
    fn panel_renders_valid_tree_without_diagnostics() {
        let tree = serde_json::json!({
            "type": "Stack",
            "props": {"gap": 8},
            "children": [
                {"type": "Text", "props": {"value": "hello"}},
                {"type": "Badge", "props": {"label": "ok", "tone": "success"}}
            ]
        });
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(diagnostics.is_empty(), "合法树不应有诊断: {diagnostics:?}");
        let body: AnyElement = body;
        drop(body);
    }

    // ADC-ELEM-PANEL-002
    #[test]
    fn panel_unknown_root_falls_back_to_placeholder() {
        let tree = serde_json::json!({"type": "Sparkline", "props": {}});
        let (body, diagnostics) = adc_panel_body(&Theme::dark(), &tree);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownElement),
            "未知根元素必须产生 UnknownElement 诊断: {diagnostics:?}"
        );
        // 未知元素 -> Placeholder（P4），不 panic、不丢整棵树。
        let parsed = parse_tree(&tree);
        assert_eq!(parsed.root.kind.as_str(), "Placeholder");
        drop(body);
    }

    // ADC-ELEM-PANEL-003
    #[test]
    fn panel_node_budget_is_clipped_and_diagnosed() {
        let children: Vec<serde_json::Value> = (0..250)
            .map(|index| {
                serde_json::json!({"type": "Text", "props": {"value": format!("row {index}")}})
            })
            .collect();
        let tree = serde_json::json!({"type": "Stack", "children": children});
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::NodeBudgetExceeded),
            "超过 200 节点必须产生 NodeBudgetExceeded 诊断: {diagnostics:?}"
        );
        drop(body);
    }

    // ADC-ELEM-PANEL-004
    #[test]
    fn panel_depth_exceeded_still_renders() {
        let mut tree = serde_json::json!({"type": "Text", "props": {"value": "leaf"}});
        for _ in 0..12 {
            tree = serde_json::json!({"type": "Stack", "children": [tree]});
        }
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DepthExceeded),
            "超过 8 层必须产生 DepthExceeded 诊断: {diagnostics:?}"
        );
        drop(body);
    }

    // ADC-ELEM-PANEL-005
    #[test]
    fn panel_null_tree_degrades_to_placeholder() {
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &serde_json::Value::Null);
        // `null` 不是 JSON 对象：解析期记 InvalidNode（缺 type 的节点走 MissingType）。
        assert!(
            diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.code,
                DiagnosticCode::MissingType | DiagnosticCode::InvalidNode
            )),
            "空输入必须产生节点级诊断: {diagnostics:?}"
        );
        assert_eq!(
            parse_tree(&serde_json::Value::Null).root.kind.as_str(),
            "Placeholder"
        );
        drop(body);
    }

    // ADC-ELEM-PANEL-006
    #[test]
    fn panel_empty_children_is_valid() {
        let tree = serde_json::json!({"type": "Stack", "children": []});
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(
            diagnostics.is_empty(),
            "空 children 是合法的: {diagnostics:?}"
        );
        drop(body);
    }

    // ADC-ELEM-PANEL-007
    #[test]
    fn panel_from_str_handles_invalid_and_valid_json() {
        let theme = Theme::light();
        let (body, invalid) = adc_panel_body_from_str(&theme, "{not json");
        assert!(
            invalid
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidJson),
            "非法 JSON 必须产生 InvalidJson 诊断: {invalid:?}"
        );
        drop(body);

        let (body, valid) =
            adc_panel_body_from_str(&theme, r#"{"type":"Text","props":{"value":"hi"}}"#);
        assert!(valid.is_empty(), "合法 JSON 不应有诊断: {valid:?}");
        drop(body);
    }

    // ADC-ELEM-PANEL-008
    #[test]
    fn panel_a11y_missing_matches_render_side_issues() {
        let tree = serde_json::json!({
            "type": "Stack",
            "children": [{"type": "Button", "props": {"label": "run"}, "key": "run"}]
        });
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::A11yMissing),
            "交互元素缺 a11yLabel 必须产生 A11yMissing 诊断: {diagnostics:?}"
        );
        // 解析期诊断与渲染侧 a11y 兜底必须一致。
        let parsed = parse_tree(&tree);
        assert_eq!(parsed.a11y_missing().len(), 1);
        let render_side = a11y_issues_in(&parsed.root);
        assert_eq!(render_side.len(), 1);
        assert_eq!(render_side[0].kind, "Button");
        drop(body);
    }

    // ADC-ELEM-PANEL-009
    #[test]
    fn panel_leaf_with_children_is_diagnosed_and_rendered() {
        let tree = serde_json::json!({
            "type": "Card",
            "children": [{
                "type": "Text",
                "props": {"value": "x"},
                "children": [{"type": "Text", "props": {"value": "y"}}]
            }]
        });
        let (body, diagnostics) = adc_panel_body(&Theme::light(), &tree);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::LeafWithChildren),
            "叶子带 children 必须产生 LeafWithChildren 诊断: {diagnostics:?}"
        );
        let parsed = parse_tree(&tree);
        assert_eq!(parsed.root.children[0].kind.as_str(), "Text");
        assert!(parsed.root.children[0].children.is_empty());
        drop(body);
    }

    // ADC-ELEM-PANEL-010
    #[test]
    fn panel_containers_render_nested_content() {
        let tree = serde_json::json!({
            "type": "Card",
            "props": {"title": "卡", "tone": "info"},
            "children": [
                {
                    "type": "Grid",
                    "props": {"columns": 2, "gap": 8},
                    "children": [{
                        "type": "Tabs",
                        "props": {
                            "tabs": [{"key": "a", "label": "A"}],
                            "activeKey": "a",
                            "a11yLabel": "页签"
                        },
                        "children": [{"type": "Text", "props": {"value": "页内容"}}]
                    }]
                },
                {
                    "type": "Accordion",
                    "props": {
                        "sections": [
                            {"key": "s", "title": "S", "children": [{"type": "Text", "props": {"value": "折叠内容"}}]}
                        ],
                        "expanded": ["s"]
                    }
                }
            ]
        });
        let (body, diagnostics) = adc_panel_body(&Theme::dark(), &tree);
        assert!(
            diagnostics.is_empty(),
            "合法容器树不应有诊断: {diagnostics:?}"
        );
        // Accordion 的 section 内容落在 nested_children。
        let parsed = parse_tree(&tree);
        let accordion = &parsed.root.children[1];
        assert_eq!(accordion.nested_children["s"][0].kind.as_str(), "Text");
        drop(body);
    }
}
