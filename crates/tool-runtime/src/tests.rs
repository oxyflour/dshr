#![cfg(test)]

use super::*;
use serde_json::json;

fn native(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        "native probe tool",
        ToolOrigin::Native {
            plugin_id: "builtin.probe".to_owned(),
        },
    )
}

fn sidecar(name: &str, generation: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        "sidecar tool",
        ToolOrigin::Sidecar {
            plugin_id: "probe.plugin".to_owned(),
            registration_id: "reg-1".to_owned(),
            generation: generation.to_owned(),
        },
    )
}

#[test]
fn rejects_duplicate_registration() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry.register(native("probe_native")).unwrap();
    assert_eq!(
        registry.register(native("probe_native")).unwrap_err(),
        RegistryError::Duplicate("probe_native".to_owned())
    );
}

#[test]
fn unregister_is_idempotent() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry.register(native("a")).unwrap();
    assert!(registry.unregister("a"));
    assert!(!registry.unregister("a"));
    assert!(registry.is_empty());
}

#[test]
fn generation_invalidation_drops_only_that_runtime() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry.register(native("native_a")).unwrap();
    registry.register(sidecar("js_a", "node-0001")).unwrap();
    registry.register(sidecar("js_b", "node-0002")).unwrap();

    assert_eq!(registry.invalidate_generation("node-0001"), 1);
    assert!(registry.get("native_a").is_some());
    assert!(registry.get("js_a").is_none());
    assert!(registry.get("js_b").is_some());
}

#[test]
fn policy_denies_before_schema_runs() {
    let mut registry = ToolRegistry::new(DenyList(vec!["probe_native".to_owned()]));
    registry
        .register(native("probe_native").with_schema(json!({ "type": "object" })))
        .unwrap();
    let err = registry.authorize("probe_native", &json!({})).unwrap_err();
    assert!(matches!(err, RegistryError::Denied { .. }));
}

#[test]
fn missing_required_argument_is_rejected() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry
        .register(native("probe_native").with_schema(json!({
            "type": "object",
            "required": ["path"],
            "properties": { "path": { "type": "string" } }
        })))
        .unwrap();
    let err = registry.authorize("probe_native", &json!({})).unwrap_err();
    assert!(matches!(err, RegistryError::InvalidArguments { .. }));
}

#[test]
fn wrong_argument_type_is_rejected() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry
        .register(native("probe_native").with_schema(json!({
            "type": "object",
            "properties": { "limit": { "type": "integer" } }
        })))
        .unwrap();
    let err = registry
        .authorize("probe_native", &json!({ "limit": "ten" }))
        .unwrap_err();
    assert!(matches!(err, RegistryError::InvalidArguments { .. }));
}

#[test]
fn additional_properties_are_rejected_when_forbidden() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry
        .register(native("probe_native").with_schema(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "additionalProperties": false
        })))
        .unwrap();
    let err = registry
        .authorize("probe_native", &json!({ "path": "a", "extra": 1 }))
        .unwrap_err();
    assert!(matches!(err, RegistryError::InvalidArguments { .. }));
}

#[test]
fn authorize_returns_definition_and_lists_sorted() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry.register(native("b")).unwrap();
    registry.register(native("a")).unwrap();

    let tool = registry.authorize("a", &json!({})).unwrap();
    assert_eq!(tool.name, "a");
    let names: Vec<_> = registry.list().iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, vec!["a".to_owned(), "b".to_owned()]);
}

#[test]
fn unknown_tool_is_reported() {
    let registry = ToolRegistry::new(AllowAll);
    assert_eq!(
        registry.authorize("nope", &json!({})).unwrap_err(),
        RegistryError::NotFound("nope".to_owned())
    );
}

#[test]
fn presentation_round_trips_and_defaults_to_generic() {
    for card in Presentation::ALL {
        assert_eq!(Presentation::parse(card.as_str()), Some(card));
        assert_eq!(
            Presentation::parse(&card.as_str().to_uppercase()),
            Some(card)
        );
    }
    assert_eq!(Presentation::parse("markdown"), None);

    let plain = native("a");
    assert_eq!(plain.card(), Presentation::Generic);

    let terminal = native("b").with_presentation("terminal");
    assert_eq!(terminal.card(), Presentation::Terminal);
    assert_eq!(terminal.presentation.as_deref(), Some("terminal"));

    // 未知卡片不能丢掉结果：退回 Generic，原始字符串仍保留以便诊断。
    let unknown = native("c").with_presentation("markdown");
    assert_eq!(unknown.card(), Presentation::Generic);
    assert_eq!(unknown.presentation.as_deref(), Some("markdown"));
}

#[test]
fn desktop_approval_policy_asks_for_mutating_tools() {
    let policy = ApprovalPolicy::desktop();
    let write = native("write");
    assert_eq!(policy.check(&write, &json!({})), PolicyDecision::Ask);

    let terminal = native("anything").with_presentation("terminal");
    assert_eq!(policy.check(&terminal, &json!({})), PolicyDecision::Ask);

    let read = native("read").with_presentation("read");
    assert_eq!(policy.check(&read, &json!({})), PolicyDecision::Allow);
}

#[test]
fn deny_list_wins_over_ask_and_allow() {
    let policy = ApprovalPolicy {
        deny: vec!["bash".to_owned()],
        ask: vec!["bash".to_owned()],
        ask_terminal_and_diff: false,
        ask_everything: false,
        deny_terminal_and_diff: false,
    };
    assert!(matches!(
        policy.check(&native("bash"), &json!({})),
        PolicyDecision::Deny(_)
    ));
}

#[test]
fn ask_everything_policy_asks_for_read_tools_too() {
    let policy = ApprovalPolicy::ask_all();
    assert_eq!(
        policy.check(&native("read"), &json!({})),
        PolicyDecision::Ask
    );
}

#[test]
fn authorize_approved_skips_policy_but_keeps_schema_validation() {
    let mut registry = ToolRegistry::new(ApprovalPolicy::ask_all());
    registry
        .register(native("write").with_schema(json!({
            "type": "object",
            "required": ["path"],
            "properties": { "path": { "type": "string" } }
        })))
        .unwrap();
    assert!(matches!(
        registry.authorize("write", &json!({ "path": "a" })),
        Err(RegistryError::NeedsApproval { .. })
    ));
    assert!(registry
        .authorize_approved("write", &json!({ "path": "a" }))
        .is_ok());
    assert!(matches!(
        registry.authorize_approved("write", &json!({})),
        Err(RegistryError::InvalidArguments { .. })
    ));
}

// ------------------------------------------------------------------
// M3-DEFAULT-050..056：approval 模式（read-only / danger-full-access）
// ------------------------------------------------------------------

#[test]
fn m3_default_050_read_only_denies_mutating_tools() {
    let policy = ApprovalPolicy::read_only();
    for name in ["write", "edit", "bash", "pwsh", "delete", "git_push"] {
        assert!(
            matches!(
                policy.check(&native(name), &json!({})),
                PolicyDecision::Deny(_)
            ),
            "read-only 必须拒绝 {name}"
        );
    }
    // terminal/diff 卡片（执行类）同样拒绝，即使工具名不在名单里。
    for card in ["terminal", "diff"] {
        let tool = native("custom_exec").with_presentation(card);
        assert!(
            matches!(policy.check(&tool, &json!({})), PolicyDecision::Deny(_)),
            "read-only 必须拒绝 {card} 卡片"
        );
    }
}

#[test]
fn m3_default_051_read_only_allows_read_tools() {
    let policy = ApprovalPolicy::read_only();
    assert_eq!(
        policy.check(&native("read").with_presentation("read"), &json!({})),
        PolicyDecision::Allow
    );
    assert_eq!(
        policy.check(&native("grep"), &json!({})),
        PolicyDecision::Allow
    );
}

#[test]
fn m3_default_052_danger_full_access_allows_everything() {
    let policy = ApprovalPolicy::danger_full_access();
    for name in ["write", "bash", "delete", "anything"] {
        assert_eq!(
            policy.check(&native(name), &json!({})),
            PolicyDecision::Allow,
            "danger-full-access 应放行 {name}"
        );
    }
    assert_eq!(
        policy.check(&native("exec").with_presentation("terminal"), &json!({})),
        PolicyDecision::Allow
    );
}

#[test]
fn m3_default_053_desktop_policy_is_unchanged_and_is_workspace_write() {
    let desktop = ApprovalPolicy::desktop();
    assert_eq!(
        desktop.check(&native("write"), &json!({})),
        PolicyDecision::Ask
    );
    assert_eq!(
        desktop.check(&native("read").with_presentation("read"), &json!({})),
        PolicyDecision::Allow
    );
    assert_eq!(
        ApprovalPolicy::workspace_write().check(&native("bash"), &json!({})),
        PolicyDecision::Ask
    );
    // from_env 缺省 = workspace-write（默认行为不回归）。
    assert_eq!(
        ApprovalPolicy::for_mode(PermissionMode::from_env()).check(&native("write"), &json!({})),
        PolicyDecision::Ask
    );
}

#[test]
fn m3_default_054_permission_mode_parses_dsh_values() {
    assert_eq!(
        PermissionMode::parse("read-only"),
        Some(PermissionMode::ReadOnly)
    );
    assert_eq!(
        PermissionMode::parse("READ_ONLY"),
        Some(PermissionMode::ReadOnly)
    );
    assert_eq!(
        PermissionMode::parse("workspace-write"),
        Some(PermissionMode::WorkspaceWrite)
    );
    assert_eq!(
        PermissionMode::parse("danger-full-access"),
        Some(PermissionMode::DangerFullAccess)
    );
    assert_eq!(PermissionMode::parse("bogus"), None);
    assert_eq!(PermissionMode::ENV, "DSHR_PERMISSION_MODE");
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::WorkspaceWrite,
        PermissionMode::DangerFullAccess,
    ] {
        assert_eq!(PermissionMode::parse(mode.as_str()), Some(mode));
    }
}

#[test]
fn m3_default_055_approval_policy_for_mode_maps_constructors() {
    assert!(matches!(
        ApprovalPolicy::for_mode(PermissionMode::ReadOnly).check(&native("bash"), &json!({})),
        PolicyDecision::Deny(_)
    ));
    assert_eq!(
        ApprovalPolicy::for_mode(PermissionMode::DangerFullAccess)
            .check(&native("bash"), &json!({})),
        PolicyDecision::Allow
    );
    assert_eq!(
        ApprovalPolicy::for_mode(PermissionMode::WorkspaceWrite).check(&native("bash"), &json!({})),
        PolicyDecision::Ask
    );
}

#[test]
fn m3_default_056_env_mode_resolution_is_pure_for_tests() {
    // 环境读取只发生在 `from_env`；这里断言纯映射与默认回退，避免并行 env 干扰。
    assert_eq!(
        ApprovalPolicy::for_mode(PermissionMode::WorkspaceWrite)
            .check(&native("write"), &json!({})),
        PolicyDecision::Ask
    );
    assert!(PermissionMode::parse("").is_none());
}

// ------------------------------------------------------------------
// M3-DEFAULT-060..063：per-tool timeout 注册侧透传
// ------------------------------------------------------------------

#[test]
fn m3_default_060_tool_timeout_defaults_to_120s() {
    let tool = native("read");
    assert_eq!(tool.timeout_ms, DEFAULT_TOOL_TIMEOUT_MS);
    assert_eq!(tool.timeout_ms, 120_000);
    assert_eq!(tool.effective_timeout_ms(), 120_000);
}

#[test]
fn m3_default_061_per_tool_timeout_is_preserved_by_registry() {
    let mut registry = ToolRegistry::new(AllowAll);
    registry
        .register(native("slow_tool").with_timeout_ms(45_000))
        .unwrap();
    let registered = registry.get("slow_tool").expect("registered");
    assert_eq!(registered.timeout_ms, 45_000, "注册表不得吞掉声明值");
    assert_eq!(registered.effective_timeout_ms(), 45_000);
}

#[test]
fn m3_default_062_effective_timeout_is_clamped_to_spec_bounds() {
    assert_eq!(
        native("too_fast").with_timeout_ms(0).effective_timeout_ms(),
        TOOL_TIMEOUT_MIN_MS
    );
    assert_eq!(
        native("too_fast")
            .with_timeout_ms(500)
            .effective_timeout_ms(),
        TOOL_TIMEOUT_MIN_MS
    );
    assert_eq!(
        native("too_slow")
            .with_timeout_ms(TOOL_TIMEOUT_MAX_MS + 1)
            .effective_timeout_ms(),
        TOOL_TIMEOUT_MAX_MS
    );
    assert_eq!(TOOL_TIMEOUT_MIN_MS, 1_000);
    assert_eq!(TOOL_TIMEOUT_MAX_MS, 600_000);
}

#[test]
fn m3_default_063_effective_timeout_duration_matches_ms() {
    let tool = native("slow").with_timeout_ms(2_500);
    assert_eq!(
        tool.effective_timeout(),
        std::time::Duration::from_millis(2_500)
    );
    assert_eq!(tool.effective_timeout().as_millis(), 2_500);
}
