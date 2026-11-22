#![cfg(test)]

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
        parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code
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
        .map(
            |index| serde_json::json!({"type": "Text", "props": {"value": format!("row {index}")}}),
        )
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

// M4-ADC-001：带动作回调的面板入口能渲染嵌套 Button（handler 穿透 Card/Stack），
// 且合法树不产生诊断；点击行为由 chat 产品路径人工复验。
#[test]
fn panel_with_action_handler_accepts_nested_button() {
    let tree = serde_json::json!({
        "type": "Card",
        "props": { "title": "动作" },
        "children": [{
            "type": "Stack",
            "props": { "gap": 8 },
            "children": [{
                "type": "Button",
                "props": {
                    "label": "复制",
                    "a11yLabel": "复制插件 ID",
                    "onPress": { "copy": "demo.echo" }
                }
            }]
        }]
    });
    let handler: AdcActionHandler = std::rc::Rc::new(|_, _, _| {});
    let (body, diagnostics) = adc_panel_body_with_handler(&Theme::light(), &tree, handler);
    assert!(
        diagnostics.is_empty(),
        "合法动作树不应有诊断: {diagnostics:?}"
    );
    drop(body);
}

// M4-ADC-005：Checkbox/Switch 的 onChange 也能穿透 handler 渲染（点击由真机复验）。
#[test]
fn panel_with_action_handler_accepts_toggle_controls() {
    let tree = serde_json::json!({
        "type": "Stack",
        "props": { "gap": 8 },
        "children": [
            {
                "type": "Checkbox",
                "props": {
                    "checked": true,
                    "label": "启用",
                    "a11yLabel": "启用",
                    "onChange": { "call": "toggle", "args": { "on": true } }
                }
            },
            {
                "type": "Switch",
                "props": {
                    "checked": false,
                    "label": "开关",
                    "a11yLabel": "开关",
                    "onChange": { "command": "toggle-switch" }
                }
            }
        ]
    });
    let handler: AdcActionHandler = std::rc::Rc::new(|_, _, _| {});
    let (body, diagnostics) = adc_panel_body_with_handler(&Theme::light(), &tree, handler);
    assert!(
        diagnostics.is_empty(),
        "合法 onChange 树不应有诊断: {diagnostics:?}"
    );
    drop(body);
}

// M4-ADC-006：Select onChange(call) 点击时把选项 value 注入 args；无 handler 时保持只读盒。
#[test]
fn select_on_change_injects_value_and_renders() {
    let tree = serde_json::json!({
        "type": "Select",
        "props": {
            "value": "a",
            "options": [
                { "value": "a", "label": "A" },
                { "value": "b", "label": "B" }
            ],
            "a11yLabel": "选择",
            "onChange": { "call": "pick", "args": { "source": "ui" } }
        }
    });
    let handler: AdcActionHandler = std::rc::Rc::new(|_, _, _| {});
    let (body, diagnostics) = adc_panel_body_with_handler(&Theme::light(), &tree, handler);
    assert!(
        diagnostics.is_empty(),
        "合法 Select onChange 树不应有诊断: {diagnostics:?}"
    );
    drop(body);

    let action = Action::Call {
        method: "pick".to_owned(),
        args: serde_json::json!({ "source": "ui" }),
    };
    match action_with_value(&action, "b") {
        Action::Call { method, args } => {
            assert_eq!(method, "pick");
            assert_eq!(args["value"], "b");
            assert_eq!(args["source"], "ui");
        }
        other => panic!("expected call, got {other:?}"),
    }
}

// M4-ADC-007：Input/Textarea 的编辑请求携带 path/value/on_change，宿主可还原动作。
#[test]
fn input_edit_action_carries_path_value_and_on_change() {
    let on_change = Action::Call {
        method: "save".to_owned(),
        args: serde_json::json!({ "id": "x" }),
    };
    let edit = edit_action("/root/children[0]", "hello", false, on_change);
    match edit {
        Action::Call { method, args } => {
            assert_eq!(method, "__adc_edit");
            assert_eq!(args["path"], "/root/children[0]");
            assert_eq!(args["value"], "hello");
            assert_eq!(args["multiline"], false);
            assert_eq!(args["on_change"]["call"], "save");
            assert_eq!(args["on_change"]["args"]["id"], "x");
        }
        other => panic!("expected edit call, got {other:?}"),
    }
}

// M4-ADC-008：Slider 的步进动作注入数值（不是字符串），树渲染 0 诊断。
#[test]
fn slider_step_action_injects_number_and_renders() {
    let tree = serde_json::json!({
        "type": "Slider",
        "props": {
            "value": 5.0,
            "min": 0.0,
            "max": 10.0,
            "step": 2.5,
            "a11yLabel": "进度",
            "onChange": { "call": "set-progress" }
        }
    });
    let handler: AdcActionHandler = std::rc::Rc::new(|_, _, _| {});
    let (body, diagnostics) = adc_panel_body_with_handler(&Theme::light(), &tree, handler);
    assert!(
        diagnostics.is_empty(),
        "合法 Slider onChange 树不应有诊断: {diagnostics:?}"
    );
    drop(body);

    let action = Action::Call {
        method: "set-progress".to_owned(),
        args: serde_json::json!({}),
    };
    match action_with_json(&action, serde_json::json!(7.5)) {
        Action::Call { args, .. } => {
            assert_eq!(args["value"], serde_json::json!(7.5));
            assert!(args["value"].is_number());
        }
        other => panic!("expected call, got {other:?}"),
    }
}
