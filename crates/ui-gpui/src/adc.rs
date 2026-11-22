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
    div, prelude::*, px, AlignContent, AlignItems, AnyElement, Div, Font, Pixels, Render, Rgba,
    Role, SharedString, Stateful, TextAlign, Toggled,
};

use dshr_ui_contributions::{
    parse_tree, parse_tree_str, Action, ElementKind, ElementNode, PlaceholderReason,
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

/// 交互元素的回调：M4 W3 起 Button 的 `onPress` 会带着已校验的 [`Action`] 回到宿主。
///
/// `App` 参数用于宿主动作（如写剪贴板）；插件 `call`/`command` 由 Host 在后续轮次接线。
pub type AdcActionHandler = std::rc::Rc<dyn Fn(&Action, &mut gpui::Window, &mut gpui::App)>;

/// 把 ADC-v1 元素节点渲染成 GPUI 元素。
///
/// 纯函数（无窗口、无事件）：同一 `theme` + 同一 `node` 必然得到同一棵元素树，
/// 满足 spec §7.2。默认入口不接事件，保持 M3 的只读语义。
pub fn adc_view(theme: &Theme, node: &ElementNode) -> AnyElement {
    render_node(theme, node, "$", None)
}

/// 带 `onPress` 回调的渲染入口（M4 W3）：Button 动作交给 `handler`，其余元素仍只读。
pub fn adc_view_with_handler(
    theme: &Theme,
    node: &ElementNode,
    handler: AdcActionHandler,
) -> AnyElement {
    render_node(theme, node, "$", Some(&handler))
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

/// 带 `onPress` 回调的面板入口：插件 panel 的 Button 动作回到 `handler`。
pub fn adc_panel_body_with_handler(
    theme: &Theme,
    tree: &serde_json::Value,
    handler: AdcActionHandler,
) -> (AnyElement, Vec<Diagnostic>) {
    let parsed = parse_tree(tree);
    (
        panel_shell(adc_view_with_handler(theme, &parsed.root, handler)),
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

fn render_node(
    theme: &Theme,
    node: &ElementNode,
    path: &str,
    handler: Option<&AdcActionHandler>,
) -> AnyElement {
    // `visible=false` 不占位；节点仍保留在树里供 diff/诊断。
    if !node.visible {
        return div().into_any_element();
    }
    // `ElementKind` 是 `#[non_exhaustive]`：新增元素走兜底分支渲染成灰框（P4）。
    match node.kind {
        ElementKind::Stack => render_stack(theme, node, path, false, handler),
        ElementKind::Row => render_stack(theme, node, path, true, handler),
        ElementKind::Grid => render_grid(theme, node, path, handler),
        ElementKind::Divider => render_divider(theme, node),
        ElementKind::Spacer => div().flex_1().min_h(px(0.0)).into_any_element(),
        ElementKind::Text => render_text(theme, node),
        ElementKind::Markdown => render_markdown(theme, node),
        ElementKind::Code => render_code(theme, node, path),
        ElementKind::Badge => render_badge(theme, node),
        ElementKind::Button => render_button(theme, node, path, handler),
        ElementKind::Input => render_input(theme, node, path, handler),
        ElementKind::Textarea => render_textarea(theme, node, path, handler),
        ElementKind::Select => render_select(theme, node, path, handler),
        ElementKind::Checkbox => render_checkbox(theme, node, path, handler),
        ElementKind::Switch => render_switch(theme, node, path, handler),
        ElementKind::Slider => render_slider(theme, node, path, handler),
        ElementKind::List => render_list(theme, node, path),
        ElementKind::Table => render_table(theme, node, path),
        ElementKind::Tree => render_tree(theme, node, path),
        ElementKind::KeyValue => render_key_value(theme, node),
        ElementKind::Image => render_image(theme, node, path),
        ElementKind::Icon => render_icon(theme, node, path),
        ElementKind::Chart => render_chart(theme, node),
        ElementKind::Card => render_card(theme, node, path, handler),
        ElementKind::Tabs => render_tabs(theme, node, path, handler),
        ElementKind::Accordion => render_accordion(theme, node, path, handler),
        _ => render_placeholder(theme, node),
    }
}

// --- 布局 -----------------------------------------------------------------

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
    let element = element
        .id(format!("adc-{path}"))
        .debug_selector(|| format!("adc-{path}"))
        .role(role);
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

mod actions;
mod controls;
mod data_views;
mod layout;
mod text;

use actions::{action_with_json, action_with_value, edit_action};
use controls::*;
use data_views::*;
use layout::*;
use text::*;

#[cfg(test)]
mod tests;
