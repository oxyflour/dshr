//! ADC-S 安全 / 配额测试。
//!
//! 测试 ID：`ADC-SEC-*`（spec §11 conformance 前缀，对应 spec §10 安全与配额）。

use dshr_ui_contributions::{
    parse_action, parse_tree, ActionError, DiagnosticCode, ElementKind, PlaceholderReason,
};
use serde_json::{json, Value};

fn parse(value: Value) -> dshr_ui_contributions::ParsedTree {
    parse_tree(&value)
}

#[test]
fn adc_sec_001_depth_over_limit_is_clipped_with_diagnostic() {
    // 10 层 Stack；MAX_DEPTH=8（含根），第 9 层起变 Placeholder。
    let mut value = json!({"type": "Text", "props": {"value": "leaf"}});
    for _ in 0..10 {
        value = json!({"type": "Stack", "children": [value]});
    }
    let tree = parse(value);
    assert_eq!(tree.root.kind, ElementKind::Stack);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::DepthExceeded)
            .count(),
        1,
        "diagnostics: {:?}",
        tree.diagnostics
    );
    // 8 个合法 Stack + 1 个 Placeholder（不再递归进叶子）。
    assert_eq!(tree.node_count(), 9);
    let mut last = &tree.root;
    while let Some(child) = last.children.first() {
        last = child;
    }
    assert_eq!(last.kind, ElementKind::Placeholder);
    assert!(matches!(
        last.placeholder,
        Some(PlaceholderReason::DepthExceeded {
            depth: 9,
            max_depth: 8
        })
    ));
}

#[test]
fn adc_sec_002_node_budget_is_enforced() {
    let children: Vec<Value> = (0..300)
        .map(|index| json!({"type": "Text", "props": {"value": format!("n{index}")}}))
        .collect();
    let tree = parse(json!({"type": "Stack", "children": children}));
    // 配额裁剪丢弃剩余兄弟节点：根 + 199 个 Text = 200 个节点。
    assert_eq!(tree.root.children.len(), 199);
    assert_eq!(tree.node_count(), 200);
    assert!(tree
        .root
        .children
        .iter()
        .all(|child| child.kind == ElementKind::Text));
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::NodeBudgetExceeded)
            .count(),
        1
    );
}

#[test]
fn adc_sec_003_text_over_eight_kib_is_truncated() {
    let long = "a".repeat(9000);
    let tree = parse(json!({"type": "Text", "props": {"value": long}}));
    let value = tree.root.str_prop("value").expect("value");
    assert_eq!(value.len(), 8192);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::TextTruncated)
            .count(),
        1
    );
    // 截断必须落在 UTF-8 边界上（多字节文本不得 panic）。
    let multibyte = "汉".repeat(4000); // 12000 bytes
    let tree = parse(json!({"type": "Markdown", "props": {"source": multibyte}}));
    let source = tree.root.str_prop("source").expect("source");
    assert!(source.len() <= 8192);
    assert!(source.is_char_boundary(source.len()));
}

#[test]
fn adc_sec_004_open_external_is_https_only() {
    assert!(matches!(
        parse_action(&json!({"openExternal": "https://dshr.local/docs"})),
        Ok(dshr_ui_contributions::Action::OpenExternal { .. })
    ));
    for url in [
        "http://dshr.local",
        "javascript:alert(1)",
        "file:///C:/secret",
        "https://exa mple.com",
        "data:text/html,<script>x</script>",
    ] {
        assert_eq!(
            parse_action(&json!({"openExternal": url})),
            Err(ActionError::InvalidExternalUrl),
            "url must be rejected: {url}"
        );
    }
}

#[test]
fn adc_sec_005_copy_quota_is_64_kib() {
    let allowed = "a".repeat(65_536);
    assert!(matches!(
        parse_action(&json!({"copy": allowed})),
        Ok(dshr_ui_contributions::Action::Copy { .. })
    ));
    let rejected = "a".repeat(65_537);
    assert_eq!(
        parse_action(&json!({"copy": rejected})),
        Err(ActionError::InvalidCopy)
    );
}

#[test]
fn adc_sec_006_data_arrays_are_truncated_to_schema_quota() {
    let items: Vec<Value> = (0..300)
        .map(|index| json!({"key": format!("k{index}"), "primary": format!("row {index}")}))
        .collect();
    let tree = parse(json!({"type": "List", "props": {"items": items}}));
    let normalized = tree.root.array_prop("items").expect("items");
    assert_eq!(normalized.len(), 200);
    assert_eq!(
        tree.diagnostics_with_code(DiagnosticCode::PropTruncated)
            .count(),
        1
    );
}

#[test]
fn adc_sec_007_malformed_trees_never_panic() {
    // 任意畸形输入都必须返回 Placeholder/降级节点而不是 panic 或 Err。
    let malformed = [
        Value::Null,
        json!(42),
        json!("just a string"),
        json!([]),
        json!({}),
        json!({"type": 7}),
        json!({"type": ""}),
        json!({"type": "Text", "props": []}),
        json!({"type": "Text", "children": "not-an-array"}),
        json!({"type": "Stack", "children": [null, 1, "x", []]}),
        json!({"type": "Accordion", "props": {"sections": "not-an-array"}}),
        json!({"type": "Accordion", "props": {"sections": [{"key": "a"}]}}),
        json!({"type": "Grid", "props": {"columns": 0}, "children": []}),
        json!({"type": "Image", "props": {"attachmentId": 5}}),
        json!({"type": "unknown", "children": [{"type": "unknown"}]}),
    ];
    for value in malformed {
        let tree = parse(value.clone());
        assert!(
            !tree.root.kind.as_str().is_empty(),
            "root must always exist for {value}"
        );
    }
}

#[test]
fn adc_sec_008_image_requires_content_addressed_attachment_id() {
    let bad = parse(json!({"type": "Image", "props": {"attachmentId": "https://evil/x.png"}}));
    assert!(bad.root.prop("attachmentId").is_none());
    assert_eq!(
        bad.diagnostics_with_code(DiagnosticCode::InvalidProp)
            .count(),
        1
    );

    let good =
        parse(json!({"type": "Image", "props": {"attachmentId": "att_01HX:2", "fit": "contain"}}));
    assert_eq!(good.root.str_prop("attachmentId"), Some("att_01HX:2"));
    assert_eq!(good.root.str_prop("fit"), Some("contain"));
    assert!(
        good.diagnostics.is_empty(),
        "diagnostics: {:?}",
        good.diagnostics
    );
}

#[test]
fn adc_sec_009_security_quotas_are_public_constants() {
    // 渲染器 / conformance 需要复用同一组配额，避免双份真相。
    assert_eq!(dshr_ui_contributions::MAX_DEPTH, 8);
    assert_eq!(dshr_ui_contributions::MAX_NODES, 200);
    assert_eq!(dshr_ui_contributions::MAX_TEXT_BYTES, 8192);
}
