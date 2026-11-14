//! ADC-S 元素模型 / 降级 / a11y 测试。
//!
//! 测试 ID：`ADC-ELEM-*`（spec §11 conformance 前缀）。

use dshr_ui_contributions::{
    parse_tree, parse_tree_str, Action, DiagnosticCode, ElementKind, ElementNode,
    PlaceholderReason, Tone, ELEMENT_TYPE_NAMES,
};
use serde_json::{json, Value};

fn parse(value: Value) -> dshr_ui_contributions::ParsedTree {
    parse_tree(&value)
}

fn text(value: &str) -> Value {
    json!({"type": "Text", "props": {"value": value}})
}

// ---------------------------------------------------------------------------
// 基本解析
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_001_parses_layout_and_leaf_elements() {
    let tree = parse(json!({
        "type": "Stack",
        "key": "root",
        "props": {
            "gap": 8, "padding": 16, "align": "center", "justify": "space-between"
        },
        "children": [
            {"type": "Text", "props": {"value": "Hello", "tone": "info", "style": "title"}},
            {"type": "Badge", "props": {"label": "beta"}}
        ]
    }));
    assert_eq!(tree.root.kind, ElementKind::Stack);
    assert_eq!(tree.root.key.as_deref(), Some("root"));
    assert_eq!(tree.root.i64_prop("gap"), Some(8));
    assert_eq!(tree.root.str_prop("align"), Some("center"));
    assert_eq!(tree.root.children.len(), 2);
    assert_eq!(tree.root.children[0].kind, ElementKind::Text);
    assert_eq!(tree.root.children[0].str_prop("value"), Some("Hello"));
    assert_eq!(tree.root.children[0].tone, Some(Tone::Info));
    assert_eq!(tree.root.children[1].kind, ElementKind::Badge);
    assert!(
        tree.diagnostics.is_empty(),
        "diagnostics: {:?}",
        tree.diagnostics
    );
}

#[test]
fn adc_elem_002_unknown_element_degrades_to_placeholder_without_losing_tree() {
    // P4：未知元素 → Placeholder + 诊断 + 同树其余元素仍渲染，不 panic。
    let tree = parse(json!({
        "type": "Stack",
        "children": [
            {"type": "WebView", "props": {"testId": "wv-1"}},
            {"type": "Text", "props": {"value": "still rendered"}}
        ]
    }));
    let placeholder = &tree.root.children[0];
    assert_eq!(placeholder.kind, ElementKind::Placeholder);
    assert_eq!(
        placeholder.placeholder,
        Some(PlaceholderReason::UnknownType {
            type_name: "WebView".to_owned()
        })
    );
    assert_eq!(placeholder.test_id.as_deref(), Some("wv-1"));
    assert_eq!(
        tree.root.children[1].str_prop("value"),
        Some("still rendered")
    );
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::UnknownElement)
            .count(),
        1
    );
}

#[test]
fn adc_elem_003_missing_type_becomes_placeholder_root() {
    let tree = parse(json!({"props": {"value": "orphan"}}));
    assert_eq!(tree.root.kind, ElementKind::Placeholder);
    assert_eq!(tree.root.placeholder, Some(PlaceholderReason::MissingType));
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::MissingType)
            .count(),
        1
    );
}

#[test]
fn adc_elem_004_leaf_with_children_is_rejected_but_node_kept() {
    let tree = parse(json!({
        "type": "Text",
        "props": {"value": "leaf"},
        "children": [{"type": "Badge", "props": {"label": "nested"}}]
    }));
    assert_eq!(tree.root.kind, ElementKind::Text);
    assert_eq!(tree.root.str_prop("value"), Some("leaf"));
    assert!(
        tree.root.children.is_empty(),
        "leaf children must be dropped"
    );
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::LeafWithChildren)
            .count(),
        1
    );
}

#[test]
fn adc_elem_005_unknown_props_ignored_and_diagnosed() {
    let tree = parse(json!({
        "type": "Text",
        "props": {"value": "x", "customColor": "#ff0000", "onHover": {"call": "x"}}
    }));
    assert_eq!(tree.root.str_prop("value"), Some("x"));
    assert!(tree.root.prop("customColor").is_none());
    assert!(tree.root.prop("onHover").is_none());
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::UnknownProp)
            .count(),
        2
    );
}

#[test]
fn adc_elem_006_invalid_prop_values_are_dropped() {
    let tree = parse(json!({
        "type": "Stack",
        "props": {"gap": 5, "padding": 8, "align": "diagonal"}
    }));
    assert!(
        tree.root.prop("gap").is_none(),
        "gap 5 is not a spacing token"
    );
    assert_eq!(tree.root.i64_prop("padding"), Some(8));
    assert!(tree.root.prop("align").is_none());
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::InvalidProp)
            .count(),
        2
    );
}

#[test]
fn adc_elem_007_missing_required_prop_is_diagnosed_but_node_kept() {
    let tree = parse(json!({"type": "Text", "props": {"tone": "info"}}));
    assert_eq!(tree.root.kind, ElementKind::Text);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::MissingProp)
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// a11y
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_008_interactive_element_without_a11y_label_is_flagged() {
    let tree = parse(json!({"type": "Button", "props": {"label": "Run"}}));
    assert_eq!(tree.root.kind, ElementKind::Button);
    assert!(!tree.root.a11y_ok());
    assert_eq!(tree.a11y_missing().len(), 1);
    assert_eq!(tree.a11y_missing()[0].code, DiagnosticCode::A11yMissing);
    assert!(tree.a11y_missing()[0].is_error());

    let labelled = parse(json!({
        "type": "Button",
        "props": {"label": "Run", "a11yLabel": "运行"}
    }));
    assert!(labelled.a11y_missing().is_empty());
    assert!(labelled.root.a11y_ok());

    // 非交互元素不要求 a11yLabel。
    let plain = parse(text("hello"));
    assert!(plain.a11y_missing().is_empty());
    assert!(plain.root.a11y_ok());
}

// ---------------------------------------------------------------------------
// 动作
// ---------------------------------------------------------------------------

fn button_with_action(action: Value) -> Value {
    json!({
        "type": "Button",
        "props": {"label": "Go", "a11yLabel": "Go", "onPress": action}
    })
}

#[test]
fn adc_elem_009_valid_actions_are_parsed() {
    let call = parse(button_with_action(
        json!({"call": "cad.validate", "args": {"strict": true}}),
    ));
    assert_eq!(
        call.root.action("onPress"),
        Some(Action::Call {
            method: "cad.validate".to_owned(),
            args: json!({"strict": true})
        })
    );
    let command = parse(button_with_action(json!({"command": "cad.run"})));
    assert!(matches!(
        command.root.action("onPress"),
        Some(Action::Command { id }) if id == "cad.run"
    ));
    let external = parse(button_with_action(
        json!({"openExternal": "https://example.com/docs"}),
    ));
    assert!(matches!(
        external.root.action("onPress"),
        Some(Action::OpenExternal { url }) if url == "https://example.com/docs"
    ));
    let copy = parse(button_with_action(json!({"copy": "hello"})));
    assert!(matches!(
        copy.root.action("onPress"),
        Some(Action::Copy { text }) if text == "hello"
    ));
    assert_eq!(
        call.diagnostics_with_code(DiagnosticCode::ActionDowngraded)
            .count(),
        0
    );
}

#[test]
fn adc_elem_010_invalid_actions_are_downgraded() {
    let cases = [
        json!({"openExternal": "http://example.com"}),
        json!({"openExternal": "javascript:alert(1)"}),
        json!({"openExternal": "file:///C:/secret"}),
        json!({"navigate": "home"}),
        json!({"call": "a", "extra": 1}),
        json!({"call": ["a", "b"]}),
        json!({"command": ""}),
    ];
    for action in cases {
        let tree = parse(button_with_action(action.clone()));
        assert!(
            tree.root.action("onPress").is_none(),
            "action must be downgraded: {action}"
        );
        assert!(tree.root.prop("onPress").is_none());
        assert_eq!(
            tree.diagnostics_with_code(DiagnosticCode::ActionDowngraded)
                .count(),
            1,
            "action: {action}"
        );
    }
}

#[test]
fn adc_elem_011_oversized_copy_action_is_downgraded() {
    let big = "a".repeat(65_537);
    let tree = parse(button_with_action(json!({"copy": big})));
    assert!(tree.root.action("onPress").is_none());
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::ActionDowngraded)
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// 元素全集
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_012_all_twenty_six_elements_map_to_kinds() {
    assert_eq!(ELEMENT_TYPE_NAMES.len(), 26);
    for name in ELEMENT_TYPE_NAMES {
        let kind = ElementKind::from_name(name).unwrap_or_else(|| panic!("missing kind {name}"));
        assert_eq!(kind.as_str(), name);
        let tree = parse(json!({"type": name}));
        assert_ne!(
            tree.root.kind,
            ElementKind::Placeholder,
            "{name} must not degrade"
        );
        assert_eq!(tree.root.kind, kind);
    }
    assert_eq!(ElementKind::from_name("WebView"), None);
    assert_eq!(ElementKind::Placeholder.as_str(), "Placeholder");
}

// ---------------------------------------------------------------------------
// 通用属性
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_013_common_props_are_hoisted() {
    let tree = parse(json!({
        "type": "Text",
        "props": {
            "value": "x", "visible": false, "testId": "text-1",
            "a11yLabel": "说明", "tone": "warning", "span": 2
        }
    }));
    assert!(!tree.root.visible);
    assert_eq!(tree.root.test_id.as_deref(), Some("text-1"));
    assert_eq!(tree.root.a11y_label.as_deref(), Some("说明"));
    assert_eq!(tree.root.tone, Some(Tone::Warning));
    assert_eq!(tree.root.span, Some(2));

    let invalid_tone = parse(json!({
        "type": "Text",
        "props": {"value": "x", "tone": "pink", "span": 13}
    }));
    assert_eq!(invalid_tone.root.tone, None);
    assert_eq!(invalid_tone.root.span, None);
    assert_eq!(
        invalid_tone
            .diagnostics_with_code(DiagnosticCode::InvalidProp)
            .count(),
        2
    );
}

// ---------------------------------------------------------------------------
// 容器
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_014_accordion_sections_expose_parsed_children() {
    let tree = parse(json!({
        "type": "Accordion",
        "props": {
            "expanded": ["a"],
            "sections": [
                {
                    "key": "a",
                    "title": "Section A",
                    "children": [
                        {"type": "Text", "props": {"value": "A body"}},
                        {"type": "Badge", "props": {"label": "tag"}}
                    ]
                },
                {
                    "key": "b",
                    "title": "Section B",
                    "children": [{"type": "Text", "props": {"value": "B body"}}]
                }
            ]
        }
    }));
    assert_eq!(tree.root.kind, ElementKind::Accordion);
    let sections = tree
        .root
        .array_prop("sections")
        .expect("normalized sections");
    assert_eq!(sections.len(), 2);
    // props 里只保留 key/title，children 移入 nested_children。
    assert!(sections[0].get("children").is_none());
    assert_eq!(sections[0]["key"], json!("a"));
    assert_eq!(sections[1]["title"], json!("Section B"));
    assert_eq!(tree.root.nested_children.len(), 2);
    assert_eq!(
        tree.root.nested_children["a"][0].str_prop("value"),
        Some("A body")
    );
    assert_eq!(tree.root.nested_children["b"].len(), 1);
    // walk 同时遍历嵌套内容：Accordion + section a 的 2 个子节点 + section b 的 1 个。
    assert_eq!(tree.node_count(), 4);
}

#[test]
fn adc_elem_015_invalid_accordion_sections_are_dropped() {
    let tree = parse(json!({
        "type": "Accordion",
        "props": {
            "sections": [
                {"title": "no key", "children": [{"type": "Text", "props": {"value": "x"}}]},
                {"key": "ok", "title": "OK", "children": [{"type": "Text", "props": {"value": "y"}}]},
                {"key": "dupe", "title": "1", "children": [{"type": "Text", "props": {"value": "z"}}]},
                {"key": "dupe", "title": "2", "children": [{"type": "Text", "props": {"value": "w"}}]}
            ]
        }
    }));
    let sections = tree.root.array_prop("sections").expect("sections");
    assert_eq!(sections.len(), 2, "invalid sections dropped");
    assert_eq!(tree.root.nested_children.len(), 2);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::InvalidProp)
            .count(),
        2
    );
}

#[test]
fn adc_elem_016_card_requires_children_and_tabs_carries_active_page() {
    let card_missing = parse(json!({"type": "Card", "props": {"title": "T"}}));
    assert_eq!(card_missing.root.kind, ElementKind::Card);
    assert_eq!(
        card_missing
            .diagnostics_with_code(DiagnosticCode::MissingProp)
            .count(),
        1
    );

    let card = parse(json!({
        "type": "Card",
        "props": {"title": "T"},
        "children": [text("body")]
    }));
    assert_eq!(card.root.children.len(), 1);

    let tabs = parse(json!({
        "type": "Tabs",
        "props": {
            "tabs": [{"key": "one", "label": "One"}, {"key": "two", "label": "Two"}],
            "activeKey": "two"
        },
        "children": [text("active page")]
    }));
    assert_eq!(tabs.root.kind, ElementKind::Tabs);
    assert_eq!(tabs.root.children.len(), 1);
    assert_eq!(tabs.root.children[0].str_prop("value"), Some("active page"));
}

// ---------------------------------------------------------------------------
// 遍历 / 文本输入
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_017_walk_visits_depth_first_with_depth() {
    let tree = parse(json!({
        "type": "Stack",
        "children": [
            {"type": "Row", "children": [text("a"), {"type": "Badge", "props": {"label": "b"}}]},
            {"type": "Divider"}
        ]
    }));
    let mut order = Vec::new();
    tree.root.walk(&mut |node| order.push(node.kind.as_str()));
    assert_eq!(order, vec!["Stack", "Row", "Text", "Badge", "Divider"]);

    let mut depths = Vec::new();
    tree.root
        .walk_with_depth(&mut |node, depth| depths.push((node.kind.as_str(), depth)));
    assert_eq!(
        depths,
        vec![
            ("Stack", 1),
            ("Row", 2),
            ("Text", 3),
            ("Badge", 3),
            ("Divider", 2)
        ]
    );
}

#[test]
fn adc_elem_018_parse_tree_str_degrades_invalid_json() {
    let tree = parse_tree_str("{not valid json");
    assert_eq!(tree.root.kind, ElementKind::Placeholder);
    assert!(matches!(
        tree.root.placeholder,
        Some(PlaceholderReason::InvalidNode { .. })
    ));
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::InvalidJson)
            .count(),
        1
    );

    let valid = parse_tree_str(r#"{"type":"Text","props":{"value":"ok"}}"#);
    assert_eq!(valid.root.kind, ElementKind::Text);
}

#[test]
fn adc_elem_019_non_object_props_degrades_to_placeholder() {
    let tree = parse(json!({"type": "Text", "props": ["not", "an", "object"]}));
    assert_eq!(tree.root.kind, ElementKind::Placeholder);
    assert!(matches!(
        tree.root.placeholder,
        Some(PlaceholderReason::InvalidProps { .. })
    ));
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::InvalidProp)
            .count(),
        1
    );
}

#[test]
fn adc_elem_020_unknown_node_level_field_is_diagnosed() {
    let tree = parse(json!({
        "type": "Text",
        "props": {"value": "x"},
        "bogusNodeField": true
    }));
    assert_eq!(tree.root.kind, ElementKind::Text);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::UnknownNodeField)
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// 构造器
// ---------------------------------------------------------------------------

#[test]
fn adc_elem_021_constructors_produce_usable_nodes() {
    let node: ElementNode = ElementNode::new(ElementKind::Text);
    assert!(node.visible);
    let placeholder = ElementNode::placeholder(PlaceholderReason::MissingType);
    assert_eq!(placeholder.kind, ElementKind::Placeholder);
    let defaulted = ElementNode::default();
    assert_eq!(defaulted.kind, ElementKind::Placeholder);
}

#[test]
fn adc_elem_022_full_vocabulary_tree_has_no_props_diagnostics() {
    // 每个元素按 element.schema.json 给一份合法 props；任何 schema 解释缺口都会
    // 表现为 UnknownProp/InvalidProp/MissingProp 诊断，这里把它们压成 0。
    let tree = parse(json!({
        "type": "Stack",
        "props": {"gap": 8, "padding": 16, "align": "stretch", "justify": "center"},
        "children": [
            {"type": "Row", "props": {"wrap": true, "gap": 4}, "children": []},
            {"type": "Grid", "props": {"columns": 2, "gap": 4}, "children": [
                {"type": "Text", "props": {"value": "cell", "span": 2}}
            ]},
            {"type": "Divider", "props": {"label": "or"}},
            {"type": "Spacer"},
            {"type": "Text", "props": {
                "value": "hello", "style": "title", "tone": "info", "maxLines": 2
            }},
            {"type": "Markdown", "props": {"source": "# hi", "maxLines": 5}},
            {"type": "Code", "props": {
                "source": "fn main() {}", "language": "rust", "lineNumbers": true
            }},
            {"type": "Badge", "props": {"label": "beta", "tone": "success"}},
            {"type": "Button", "props": {
                "label": "Go", "a11yLabel": "Go", "disabled": false,
                "onPress": {"command": "cad.run"}
            }},
            {"type": "Input", "props": {
                "value": "v", "placeholder": "p", "multiline": false,
                "a11yLabel": "input", "onChange": {"call": "cad.edit"}
            }},
            {"type": "Textarea", "props": {
                "value": "v", "rows": 3, "a11yLabel": "textarea",
                "onChange": {"call": "cad.edit"}
            }},
            {"type": "Select", "props": {
                "value": "a", "options": [{"value": "a", "label": "A"}],
                "a11yLabel": "select", "onChange": {"command": "cad.pick"}
            }},
            {"type": "Checkbox", "props": {
                "checked": true, "label": "check", "a11yLabel": "checkbox",
                "onChange": {"call": "cad.toggle"}
            }},
            {"type": "Switch", "props": {
                "checked": false, "label": "switch", "a11yLabel": "switch",
                "onChange": {"call": "cad.toggle"}
            }},
            {"type": "Slider", "props": {
                "value": 0.5, "min": 0.0, "max": 1.0, "step": 0.1,
                "a11yLabel": "slider", "onChange": {"call": "cad.slide"}
            }},
            {"type": "List", "props": {
                "items": [{"key": "k", "primary": "p", "secondary": "s"}],
                "a11yLabel": "list", "onSelect": {"command": "cad.select"}
            }},
            {"type": "Table", "props": {
                "columns": [{"key": "k", "title": "T", "width": 10.0}],
                "rows": [{"k": "v", "n": 1, "b": true, "z": null}],
                "a11yLabel": "table", "onRowClick": {"command": "cad.row"}
            }},
            {"type": "Tree", "props": {
                "nodes": [{"key": "root", "label": "Root", "children": [
                    {"key": "child", "label": "Child"}
                ]}],
                "expanded": ["root"], "a11yLabel": "tree",
                "onToggle": {"command": "cad.toggle"}
            }},
            {"type": "KeyValue", "props": {
                "pairs": [{"key": "k", "value": "v"}]
            }},
            {"type": "Image", "props": {
                "attachmentId": "att_1", "alt": "pic", "fit": "cover"
            }},
            {"type": "Icon", "props": {"name": "settings"}},
            {"type": "Chart", "props": {
                "kind": "bars",
                "data": [{"label": "a", "value": 1.0}],
                "series": [{"label": "s", "values": [1.0, 2.0]}]
            }},
            {"type": "Card", "props": {"title": "T", "tone": "info"}, "children": [
                {"type": "Text", "props": {"value": "body"}}
            ]},
            {"type": "Tabs", "props": {
                "tabs": [{"key": "a", "label": "A"}], "activeKey": "a",
                "a11yLabel": "tabs", "onChange": {"command": "cad.tab"}
            }, "children": [
                {"type": "Text", "props": {"value": "page"}}
            ]},
            {"type": "Accordion", "props": {
                "expanded": ["s1"],
                "sections": [{"key": "s1", "title": "S", "children": [
                    {"type": "Text", "props": {"value": "section body"}}
                ]}]
            }}
        ]
    }));
    let diagnostics: Vec<_> = tree
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.code,
                DiagnosticCode::UnknownProp
                    | DiagnosticCode::InvalidProp
                    | DiagnosticCode::MissingProp
                    | DiagnosticCode::UnknownElement
                    | DiagnosticCode::A11yMissing
            )
        })
        .collect();
    assert!(
        diagnostics.is_empty(),
        "unexpected props diagnostics: {diagnostics:#?}"
    );
    // 26 个元素 + 根 + 4 个容器子节点（Grid/Card/Tabs/Accordion）全部在树里。
    assert_eq!(tree.node_count(), 1 + 25 + 4);
    let mut kinds = std::collections::BTreeSet::new();
    tree.root.walk(&mut |node| {
        kinds.insert(node.kind);
    });
    assert_eq!(kinds.len(), 26, "all 26 element kinds present: {kinds:?}");
}
