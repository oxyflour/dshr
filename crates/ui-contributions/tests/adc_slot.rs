//! ADC-S（ui-contributions 服务端）slot 目录与注册表测试。
//!
//! 测试 ID：`ADC-SLOT-*`（spec §11 conformance 前缀）。仓库离线，测试只读内存 /
//! 编译期嵌入的 schema，不写 `docs/` 或任何临时目录。

use dshr_ui_contributions::{
    CatalogError, ContributionRegistry, DiagnosticCode, FieldTypeSpec, KeyDomainMode,
    RegistrationError, ResolvedSlot, SlotCatalog, SlotKind, SlotScope,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

fn catalog_json(slots: Value) -> String {
    let catalog = json!({
        "catalog": "test.slots/v1",
        "version": 1,
        "status": "Review",
        "spec": "test",
        "kinds": {
            "single": {"seats": "one", "required_signature": ["id"]},
            "list": {"seats": "many", "required_signature": ["id"]},
            "keyed": {"seats": "one-per-key", "required_signature": ["key"]},
            "chain": {"seats": "pipeline", "required_signature": []}
        },
        "scopes": {
            "root": "应用级，全局一份",
            "session": "每个 Session 一份",
            "session-maybe": "允许空态"
        },
        "replacement_risks": ["none", "shadows-shipped-ui"],
        "contributions": {"panel": {"doc": "test", "slots": []}},
        "field_types": {
            "id": {"kind": "string", "minLength": 1, "maxLength": 64, "pattern": "^[A-Za-z0-9._-]+$"},
            "label": {"kind": "string", "minLength": 1, "maxLength": 128},
            "view": {"kind": "string", "pattern": "^[A-Za-z0-9._-]+$"},
            "key": {"kind": "string", "minLength": 1, "maxLength": 64},
            "order": {"kind": "number", "minimum": 0, "maximum": 1000000},
            "run": {"kind": "string", "pattern": "^[A-Za-z0-9._-]+$"},
            "plugin_id": {"kind": "string", "pattern": "^[a-z0-9][a-z0-9._-]{1,63}$"}
        },
        "slots": slots
    });
    catalog.to_string()
}

fn test_catalog(slots: Value) -> SlotCatalog {
    SlotCatalog::from_json(&catalog_json(slots)).expect("test catalog must parse")
}

fn single_slot(exclusive: bool) -> Value {
    json!({
        "slot_id": "test.single",
        "kind": "single",
        "scope": "root",
        "exclusive": exclusive,
        "replacement_risk": "none",
        "contribution": "panel",
        "registration": {"required": ["id", "view"], "optional": ["order"]}
    })
}

fn chain_slot() -> Value {
    json!({
        "slot_id": "test.chain",
        "kind": "chain",
        "scope": "root",
        "exclusive": false,
        "replacement_risk": "none",
        "contribution": "panel",
        "registration": {"required": ["id"], "optional": ["order"]}
    })
}

fn panel(id: &str, order: Option<f64>) -> Value {
    let mut payload = json!({
        "slot_id": "workspace.panel",
        "id": id,
        "label": format!("Panel {id}"),
        "location": "right",
        "view": format!("{id}.view")
    });
    if let Some(order) = order {
        payload["order"] = json!(order);
    }
    payload
}

type SpecRow = (
    &'static str,
    SlotKind,
    SlotScope,
    Vec<&'static str>,
    Vec<&'static str>,
    &'static str,
);

/// spec §5 的 12 个 slot 人读镜像（目录必须逐项一致）。
fn spec_table() -> Vec<SpecRow> {
    vec![
        (
            "workspace.panel",
            SlotKind::List,
            SlotScope::Root,
            vec!["id", "label", "location", "view"],
            vec!["order"],
            "panel",
        ),
        (
            "viewer.tab",
            SlotKind::Keyed,
            SlotScope::Session,
            vec!["key", "view"],
            vec![],
            "viewer",
        ),
        (
            "settings.section",
            SlotKind::List,
            SlotScope::Root,
            vec!["id", "label", "view"],
            vec!["order"],
            "settings",
        ),
        (
            "settings.general.item",
            SlotKind::List,
            SlotScope::Root,
            vec!["id", "label", "view"],
            vec!["order"],
            "settings",
        ),
        (
            "command",
            SlotKind::List,
            SlotScope::Root,
            vec!["id", "label", "run"],
            vec!["keys"],
            "command",
        ),
        (
            "menu.item",
            SlotKind::Keyed,
            SlotScope::Root,
            vec!["key", "id", "label", "run"],
            vec!["order"],
            "menu",
        ),
        (
            "conversation.chat.node",
            SlotKind::Keyed,
            SlotScope::Session,
            vec!["key", "view"],
            vec![],
            "widget",
        ),
        (
            "conversation.input.left",
            SlotKind::List,
            SlotScope::Session,
            vec!["id", "label", "view"],
            vec!["order"],
            "input",
        ),
        (
            "conversation.input.right",
            SlotKind::List,
            SlotScope::Session,
            vec!["id", "label", "view"],
            vec!["order"],
            "input",
        ),
        (
            "tool.call.toolview",
            SlotKind::Keyed,
            SlotScope::Session,
            vec!["key", "view"],
            vec![],
            "tool_renderer",
        ),
        (
            "tool.result.extra",
            SlotKind::List,
            SlotScope::Session,
            vec!["id", "label", "view"],
            vec!["order"],
            "tool_extra",
        ),
        (
            "status.bar.item",
            SlotKind::List,
            SlotScope::Root,
            vec!["id", "label", "view"],
            vec!["order"],
            "status",
        ),
    ]
}

// ---------------------------------------------------------------------------
// 目录
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_001_builtin_catalog_loads_twelve_slots() {
    let catalog = SlotCatalog::builtin();
    assert_eq!(catalog.len(), 12);
    assert_eq!(catalog.version(), 1);
    assert_eq!(catalog.catalog_name(), Some("adc.slots/v1"));
    assert_eq!(catalog.status(), Some("Review"));
    assert!(!catalog.is_empty());
}

#[test]
fn adc_slot_002_catalog_matches_spec_table() {
    // 目录与 spec §5 表格必须逐项一致（防止单边漂移）。
    let catalog = SlotCatalog::builtin();
    let table = spec_table();
    assert_eq!(catalog.all().len(), table.len());
    for (index, (slot_id, kind, scope, required, optional, contribution)) in
        table.iter().enumerate()
    {
        let slot = &catalog.all()[index];
        assert_eq!(slot.slot_id, *slot_id, "slot #{index} id");
        assert_eq!(slot.kind, *kind, "{slot_id} kind");
        assert_eq!(slot.scope, *scope, "{slot_id} scope");
        assert_eq!(
            slot.registration.required,
            required
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>(),
            "{slot_id} required"
        );
        assert_eq!(
            slot.registration.optional,
            optional
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>(),
            "{slot_id} optional"
        );
        assert_eq!(slot.contribution, *contribution, "{slot_id} contribution");
        assert!(!slot.exclusive, "v1 {slot_id} must not be exclusive");
        assert_eq!(
            slot.replacement_risk,
            dshr_ui_contributions::ReplacementRisk::None,
            "{slot_id} replacement risk"
        );
    }
}

#[test]
fn adc_slot_003_builtin_catalog_self_validates() {
    let catalog = SlotCatalog::builtin();
    assert_eq!(catalog.validate(), Ok(()));
}

#[test]
fn adc_slot_004_key_domains() {
    let catalog = SlotCatalog::builtin();
    for slot_id in ["viewer.tab", "conversation.chat.node", "tool.call.toolview"] {
        let domain = catalog
            .get(slot_id)
            .and_then(|slot| slot.key_domain.as_ref())
            .unwrap_or_else(|| panic!("{slot_id} must declare key_domain"));
        assert_eq!(domain.mode, KeyDomainMode::Open, "{slot_id}");
        assert_eq!(domain.source, None, "{slot_id}");
    }
    let menu = catalog.get("menu.item").expect("menu.item");
    let domain = menu.key_domain.as_ref().expect("menu.item key_domain");
    assert_eq!(domain.mode, KeyDomainMode::Closed);
    assert_eq!(domain.source.as_deref(), Some("host"));
    for slot in catalog.all() {
        if slot.kind != SlotKind::Keyed {
            assert!(
                slot.key_domain.is_none(),
                "{} must not have key_domain",
                slot.slot_id
            );
        }
    }
}

#[test]
fn adc_slot_005_catalog_rejects_unknown_kind() {
    let slots = vec![json!({
        "slot_id": "test.bad",
        "kind": "weird",
        "scope": "root",
        "contribution": "panel",
        "registration": {"required": [], "optional": []}
    })];
    let error = SlotCatalog::from_json(&catalog_json(Value::Array(slots)))
        .expect_err("unknown kind must fail");
    assert!(matches!(error, CatalogError::InvalidSlot { .. }));
    assert!(error.to_string().contains("weird"), "error: {error}");
}

#[test]
fn adc_slot_006_catalog_rejects_duplicate_slot_id() {
    let slot = single_slot(false);
    let error = SlotCatalog::from_json(&catalog_json(json!([slot.clone(), slot])))
        .expect_err("duplicate slot_id");
    assert!(matches!(error, CatalogError::InvalidSlot { .. }));
    assert!(error.to_string().contains("duplicate"));
}

#[test]
fn adc_slot_007_catalog_rejects_unknown_scope() {
    let mut slot = single_slot(false);
    slot["scope"] = json!("global");
    let error = SlotCatalog::from_json(&catalog_json(json!([slot]))).expect_err("unknown scope");
    assert!(matches!(error, CatalogError::InvalidSlot { .. }));
}

#[test]
fn adc_slot_008_field_type_checks() {
    let catalog = SlotCatalog::builtin();
    assert!(catalog
        .check_field("id", &json!("cad.inspector_1-A"))
        .is_ok());
    assert!(catalog.check_field("id", &json!("bad id!")).is_err());
    assert!(catalog.check_field("id", &json!("")).is_err());
    assert!(catalog.check_field("id", &json!(5)).is_err());
    assert!(catalog.check_field("view", &json!("cad.inspector")).is_ok());
    assert!(catalog.check_field("view", &json!("")).is_err());
    assert!(catalog.check_field("location", &json!("right")).is_ok());
    assert!(catalog.check_field("location", &json!("middle")).is_err());
    assert!(catalog.check_field("order", &json!(0)).is_ok());
    assert!(catalog.check_field("order", &json!(1_000_000)).is_ok());
    assert!(catalog.check_field("order", &json!(-1)).is_err());
    assert!(catalog.check_field("order", &json!(1_000_001)).is_err());
    assert!(catalog
        .check_field("keys", &json!(["Ctrl+K", "Ctrl+Shift+P"]))
        .is_ok());
    assert!(catalog.check_field("keys", &json!(["Ctrl K"])).is_err());
    assert!(catalog.check_field("keys", &json!([";"])).is_err());
    assert!(matches!(
        catalog.field_type("order"),
        Some(FieldTypeSpec::Number { .. })
    ));
}

#[test]
fn adc_slot_009_catalog_validate_flags_contract_drift() {
    let slot = json!({
        "slot_id": "test.bad",
        "kind": "list",
        "scope": "root",
        "exclusive": false,
        "replacement_risk": "none",
        "contribution": "panel",
        "registration": {"required": ["id", "missing_field"], "optional": []}
    });
    let catalog = test_catalog(json!([slot]));
    let issues = catalog
        .validate()
        .expect_err("contract drift must be caught");
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("missing_field")),
        "issues: {issues:?}"
    );
}

// ---------------------------------------------------------------------------
// 注册表：single
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_010_single_replaces_previous_and_records_diagnostic() {
    let catalog = test_catalog(json!([single_slot(false)]));
    let mut registry = ContributionRegistry::with_catalog(catalog);
    let first = registry
        .register(
            &json!({"slot_id": "test.single", "id": "a", "view": "a.view"}),
            1,
        )
        .expect("first register");
    assert_eq!(first.replaced, None);
    let second = registry
        .register(
            &json!({"slot_id": "test.single", "id": "b", "view": "b.view"}),
            1,
        )
        .expect("second register");
    assert_eq!(second.replaced, Some(first.handle));
    assert!(second.diagnostic.is_some());
    let resolved = registry.resolve_single("test.single").expect("resolved");
    assert_eq!(resolved.registration_id.as_deref(), Some("b"));
    assert_eq!(registry.registration_count(), 1);
    assert_eq!(
        registry
            .diagnostics_with_code(DiagnosticCode::Replaced)
            .count(),
        1
    );
}

#[test]
fn adc_slot_011_single_exclusive_rejects_second() {
    let catalog = test_catalog(json!([single_slot(true)]));
    let mut registry = ContributionRegistry::with_catalog(catalog);
    registry
        .register(
            &json!({"slot_id": "test.single", "id": "a", "view": "a.view"}),
            1,
        )
        .expect("first register");
    let error = registry
        .register(
            &json!({"slot_id": "test.single", "id": "b", "view": "b.view"}),
            1,
        )
        .expect_err("exclusive slot rejects replacement");
    assert!(matches!(error, RegistrationError::Forbidden { .. }));
    assert_eq!(
        registry
            .resolve_single("test.single")
            .and_then(|record| record.registration_id.as_deref()),
        Some("a")
    );
}

// ---------------------------------------------------------------------------
// 注册表：list
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_012_list_sorted_by_order_then_registration_id() {
    let mut registry = ContributionRegistry::new();
    for payload in [
        panel("c", Some(5.0)),
        panel("b", Some(10.0)),
        panel("a", Some(10.0)),
        panel("d", None),
    ] {
        registry.register(&payload, 1).expect("register");
    }
    let ordered: Vec<&str> = registry
        .resolve_list("workspace.panel")
        .iter()
        .map(|record| record.registration_id.as_deref().unwrap_or(""))
        .collect();
    // order 0（d）在最前，order 5（c），order 10 按 id 升序（a, b）。
    assert_eq!(ordered, vec!["d", "c", "a", "b"]);
}

#[test]
fn adc_slot_013_list_duplicate_id_rejected() {
    let mut registry = ContributionRegistry::new();
    registry
        .register(&panel("dup", Some(1.0)), 1)
        .expect("first");
    let error = registry
        .register(&panel("dup", Some(2.0)), 1)
        .expect_err("duplicate id");
    assert!(matches!(error, RegistrationError::DuplicateId { .. }));
    assert_eq!(registry.resolve_list("workspace.panel").len(), 1);
    assert_eq!(
        registry
            .diagnostics_with_code(DiagnosticCode::DuplicateId)
            .count(),
        1
    );
}

#[test]
fn adc_slot_014_unregister_is_idempotent() {
    let mut registry = ContributionRegistry::new();
    let outcome = registry.register(&panel("a", None), 1).expect("register");
    assert!(registry.unregister(outcome.handle));
    assert!(
        !registry.unregister(outcome.handle),
        "second unregister is a no-op"
    );
    assert!(registry.resolve_list("workspace.panel").is_empty());
    assert_eq!(registry.registration_count(), 0);
}

// ---------------------------------------------------------------------------
// 注册表：keyed
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_015_keyed_replaces_same_key_and_isolates_keys() {
    let mut registry = ContributionRegistry::new();
    registry
        .register(
            &json!({"slot_id": "viewer.tab", "key": "txt", "view": "look.txt"}),
            1,
        )
        .expect("register txt");
    registry
        .register(
            &json!({"slot_id": "viewer.tab", "key": "md", "view": "look.md"}),
            1,
        )
        .expect("register md");
    let replaced = registry
        .register(
            &json!({"slot_id": "viewer.tab", "key": "txt", "view": "look.txt.v2"}),
            1,
        )
        .expect("replace txt");
    assert!(replaced.replaced.is_some());
    assert_eq!(
        registry
            .resolve_keyed("viewer.tab", "txt")
            .and_then(|record| record.view.as_deref()),
        Some("look.txt.v2")
    );
    assert_eq!(
        registry
            .resolve_keyed("viewer.tab", "md")
            .and_then(|record| record.view.as_deref()),
        Some("look.md")
    );
    assert!(registry.resolve_keyed("viewer.tab", "pdf").is_none());
}

#[test]
fn adc_slot_016_keyed_closed_domain_rejects_unknown_key() {
    let mut registry = ContributionRegistry::new();
    let menu = |key: &str| {
        json!({
            "slot_id": "menu.item",
            "key": key,
            "id": format!("menu.{key}"),
            "label": key,
            "run": format!("menu.{key}")
        })
    };
    // 未注入闭合域时，任何 key 都拒绝。
    let error = registry
        .register(&menu("file"), 1)
        .expect_err("closed domain not configured");
    assert!(matches!(error, RegistrationError::InvalidField { .. }));

    registry
        .set_closed_key_domain("menu.item", ["file".to_owned(), "edit".to_owned()])
        .expect("configure closed keys");
    registry.register(&menu("file"), 1).expect("known key");
    let error = registry
        .register(&menu("view"), 1)
        .expect_err("unknown key");
    assert!(matches!(error, RegistrationError::InvalidField { ref field, .. } if field == "key"));
}

#[test]
fn adc_slot_017_closed_domain_only_for_closed_keyed_slots() {
    let mut registry = ContributionRegistry::new();
    let error = registry
        .set_closed_key_domain("viewer.tab", ["txt".to_owned()])
        .expect_err("open domain cannot be configured");
    assert!(matches!(error, RegistrationError::Forbidden { .. }));
    let error = registry
        .set_closed_key_domain("workspace.panel", ["x".to_owned()])
        .expect_err("list slot has no key domain");
    assert!(matches!(error, RegistrationError::Forbidden { .. }));
    let error = registry
        .set_closed_key_domain("unknown.slot", ["x".to_owned()])
        .expect_err("unknown slot");
    assert!(matches!(error, RegistrationError::UnknownSlot { .. }));
}

// ---------------------------------------------------------------------------
// 注册表：chain
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_018_chain_resolves_in_order() {
    let catalog = test_catalog(json!([chain_slot()]));
    let mut registry = ContributionRegistry::with_catalog(catalog);
    for (id, order) in [("late", 40.0), ("early", 10.0), ("middle", 20.0)] {
        registry
            .register(
                &json!({"slot_id": "test.chain", "id": id, "order": order}),
                1,
            )
            .expect("chain register");
    }
    let chain: Vec<&str> = registry
        .resolve_chain("test.chain")
        .iter()
        .map(|record| record.registration_id.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(chain, vec!["early", "middle", "late"]);
    assert!(matches!(
        registry.resolve("test.chain"),
        Some(ResolvedSlot::Chain(items)) if items.len() == 3
    ));
}

// ---------------------------------------------------------------------------
// 注册表：generation
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_019_invalidate_generation_removes_only_that_generation() {
    let mut registry = ContributionRegistry::new();
    registry
        .register(&panel("a", Some(1.0)), 7)
        .expect("gen7 a");
    registry
        .register(&panel("b", Some(2.0)), 7)
        .expect("gen7 b");
    registry
        .register(&panel("c", Some(3.0)), 8)
        .expect("gen8 c");
    let removed = registry.invalidate_generation(7);
    assert_eq!(removed, 2);
    assert_eq!(registry.resolve_list("workspace.panel").len(), 1);
    assert_eq!(
        registry.resolve_list("workspace.panel")[0]
            .registration_id
            .as_deref(),
        Some("c")
    );
    assert_eq!(registry.invalidate_generation(7), 0, "idempotent");
}

#[test]
fn adc_slot_020_generation_invalidation_is_reverse_registration_order() {
    let mut registry = ContributionRegistry::new();
    let first = registry.register(&panel("a", Some(1.0)), 9).expect("a");
    let second = registry.register(&panel("b", Some(2.0)), 9).expect("b");
    let removed = registry.invalidate_generation_handles(9);
    assert_eq!(removed, vec![second.handle, first.handle]);
}

// ---------------------------------------------------------------------------
// 注册校验
// ---------------------------------------------------------------------------

#[test]
fn adc_slot_021_registration_validation_errors_are_structured() {
    let mut registry = ContributionRegistry::new();

    assert!(matches!(
        registry.register(&json!({"slot_id": "nope", "id": "x"}), 1),
        Err(RegistrationError::UnknownSlot { .. })
    ));
    assert!(matches!(
        registry.register(&json!({"id": "x", "label": "X", "location": "left", "view": "v"}), 1),
        Err(RegistrationError::MissingField { ref field, .. }) if field == "slot_id"
    ));
    assert!(matches!(
        registry.register(
            &json!({"slot_id": "workspace.panel", "id": "x", "label": "X", "location": "left"}),
            1
        ),
        Err(RegistrationError::MissingField { ref field, .. }) if field == "view"
    ));
    assert!(matches!(
        registry.register(
            &json!({
                "slot_id": "workspace.panel", "id": "x", "label": "X",
                "location": "left", "view": "v", "bogus": 1
            }),
            1
        ),
        Err(RegistrationError::InvalidField { ref field, .. }) if field == "bogus"
    ));
    assert!(matches!(
        registry.register(
            &json!({
                "slot_id": "workspace.panel", "id": "x", "label": "X",
                "location": "middle", "view": "v"
            }),
            1
        ),
        Err(RegistrationError::InvalidField { ref field, .. }) if field == "location"
    ));
    assert!(matches!(
        registry.register(
            &json!({
                "slot_id": "workspace.panel", "id": "bad id!", "label": "X",
                "location": "left", "view": "v"
            }),
            1
        ),
        Err(RegistrationError::InvalidField { ref field, .. }) if field == "id"
    ));
    assert!(matches!(
        registry.register(
            &json!({
                "slot_id": "workspace.panel", "id": "x", "label": "X",
                "location": "left", "view": ""
            }),
            1
        ),
        Err(RegistrationError::InvalidField { ref field, .. }) if field == "view"
    ));
    assert!(matches!(
        registry.register(&json!({"slot_id": "viewer.tab", "view": "v"}), 1),
        Err(RegistrationError::MissingField { ref field, .. }) if field == "key"
    ));
    assert!(matches!(
        registry.register(&json!("not-an-object"), 1),
        Err(RegistrationError::InvalidField { .. })
    ));
    // 每次拒绝都写入结构化诊断。
    assert_eq!(
        registry.diagnostics().len(),
        9,
        "every rejection must be diagnosed: {:?}",
        registry.diagnostics()
    );
    assert!(registry
        .diagnostics()
        .iter()
        .all(|diagnostic| diagnostic.is_error()));
}

#[test]
fn adc_slot_022_plugin_id_is_optional_but_checked_when_present() {
    let mut registry = ContributionRegistry::new();
    registry
        .register(
            &json!({
                "slot_id": "workspace.panel", "plugin_id": "acme.cad",
                "id": "a", "label": "A", "location": "left", "view": "a.view"
            }),
            1,
        )
        .expect("valid plugin_id");
    let error = registry
        .register(
            &json!({
                "slot_id": "workspace.panel", "plugin_id": "Acme!",
                "id": "b", "label": "B", "location": "left", "view": "b.view"
            }),
            1,
        )
        .expect_err("invalid plugin_id");
    assert!(
        matches!(error, RegistrationError::InvalidField { ref field, .. } if field == "plugin_id")
    );
}

#[test]
fn adc_slot_023_resolve_dispatches_by_kind() {
    let mut registry = ContributionRegistry::new();
    registry.register(&panel("a", None), 1).expect("list");
    registry
        .register(
            &json!({"slot_id": "viewer.tab", "key": "txt", "view": "look.txt"}),
            1,
        )
        .expect("keyed");
    assert!(matches!(
        registry.resolve("workspace.panel"),
        Some(ResolvedSlot::List(items)) if items.len() == 1
    ));
    assert!(matches!(
        registry.resolve("viewer.tab"),
        Some(ResolvedSlot::Keyed(entries)) if entries.len() == 1
    ));
    assert!(registry.resolve("unknown.slot").is_none());
    assert_eq!(registry.registrations_for("workspace.panel").len(), 1);
}
