//! CPP-MANIFEST-* / CPP-VER-* 契约测试：typed manifest 的严格解析、版本/配额校验。
//!
//! 测试 ID 用于 conformance 注册表登记（docs/review-m3-plan.md §6）；改名时同步登记表。
//! 这里只用公开 API，测试数据不需要 `tempfile`：文件落盘测试用 `std::env::temp_dir()`。

use std::cmp::Ordering;
use std::path::PathBuf;

use dshr_plugin_api::manifest::{
    HostVersion, Manifest, RuntimeKind, ToolSpec, UiLocation, Version,
};
use dshr_plugin_api::PluginError;
use serde_json::{json, Value};

const API: &str = "cordis-portable/v1";

fn host() -> HostVersion {
    HostVersion::new(0, 2, 2)
}

/// 最小合法 manifest：只有 `plugin` + `runtime` 两个必填段。
fn base_manifest() -> Value {
    json!({
        "plugin": { "id": "x.y", "version": "1.0.0", "api": API },
        "runtime": { "kind": "rust-static" }
    })
}

/// 完整 manifest，字段覆盖 spec/cpp-v1.md §3.2 的示例 + 本切片新增的可选字段。
fn full_manifest() -> Value {
    json!({
        "plugin": {
            "id": "cad.validate",
            "version": "0.1.0",
            "api": API,
            "description": "validate STEP parts",
            "min_host_version": "0.2.1"
        },
        "runtime": {
            "kind": "wasm-component",
            "entry": "cad_validate.wasm",
            "fuel": 2000000000_u64,
            "max_memory_mb": 128,
            "timeout_ms": 30000
        },
        "inject": {
            "required": ["tool-registry/v1", "ui-contributions/v1"],
            "optional": ["storage/v1"]
        },
        "permissions": {
            "workspace_read": true,
            "workspace_write": false,
            "network": false,
            "network_allow": []
        },
        "tools": [
            {
                "name": "cad_validate",
                "description": "Validate a STEP file",
                "input_schema": "schemas/cad_validate.input.json",
                "output_schema": "schemas/cad_validate.output.json",
                "timeout_ms": 120000,
                "cancelable": true,
                "presentation": "generic"
            }
        ],
        "ui": [
            {
                "slot": "workspace.panel",
                "id": "cad.inspector",
                "label": "CAD Inspector",
                "view": "cad.inspector",
                "location": "right",
                "order": 100
            }
        ]
    })
}

fn parse(value: &Value) -> Result<Manifest, PluginError> {
    Manifest::from_json_str(&value.to_string())
}

fn temp_manifest_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "dshr-plugin-api-{tag}-{}-{nanos}.json",
        std::process::id()
    ))
}

fn version(raw: &str) -> Version {
    Version::parse(raw).unwrap_or_else(|err| panic!("`{raw}` should parse: {err}"))
}

// ---- CPP-MANIFEST：解析 + 语义校验 ----

#[test]
fn cpp_manifest_001_valid_full_manifest_parses_and_validates() {
    // CPP-MANIFEST-001：合法完整 manifest 解析 + validate 全绿。
    let manifest = parse(&full_manifest()).expect("full manifest should parse");

    assert_eq!(manifest.plugin.id, "cad.validate");
    assert_eq!(manifest.plugin.version, "0.1.0");
    assert_eq!(manifest.plugin.api, API);
    assert_eq!(manifest.plugin.min_host_version.as_deref(), Some("0.2.1"));
    assert_eq!(manifest.runtime.kind, RuntimeKind::WasmComponent);
    assert_eq!(manifest.runtime.entry.as_deref(), Some("cad_validate.wasm"));
    assert_eq!(manifest.runtime.fuel, Some(2_000_000_000));
    assert_eq!(manifest.runtime.max_memory_mb, Some(128));

    let inject = manifest.inject.as_ref().expect("inject section");
    assert_eq!(inject.required.len(), 2);
    assert_eq!(inject.optional, vec!["storage/v1".to_owned()]);

    let permissions = manifest.permissions.as_ref().expect("permissions section");
    assert!(permissions.workspace_read);
    assert!(!permissions.network);

    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "cad_validate");

    assert_eq!(manifest.ui.len(), 1);
    assert_eq!(manifest.ui[0].location, Some(UiLocation::Right));
    assert_eq!(manifest.ui[0].order, Some(100.0));

    manifest
        .validate(&host())
        .expect("full manifest should validate");
}

#[test]
fn cpp_manifest_002_unknown_top_level_field_is_rejected() {
    // CPP-MANIFEST-002：未知顶层字段拒绝（spec §3.3「未知字段 MUST 导致装载失败」）。
    let mut value = base_manifest();
    value["extra"] = json!(true);
    let err = parse(&value).expect_err("unknown top-level field should fail");
    assert!(matches!(err, PluginError::Manifest { .. }), "{err}");
    assert!(err.to_string().contains("unknown field"), "{err}");
    assert!(err.to_string().contains("extra"), "{err}");
}

#[test]
fn cpp_manifest_003_unknown_runtime_field_is_rejected() {
    // CPP-MANIFEST-003：未知 runtime 字段拒绝（例如 spec 没有的 execution）。
    let mut value = base_manifest();
    value["runtime"]["execution"] = json!("inline");
    let err = parse(&value).expect_err("unknown runtime field should fail");
    assert!(matches!(err, PluginError::Manifest { .. }), "{err}");
    assert!(err.to_string().contains("execution"), "{err}");
}

#[test]
fn cpp_manifest_004_api_v2_is_rejected() {
    // CPP-MANIFEST-004：api 必须等于 cordis-portable/v1（解析入口就拒绝）。
    let mut value = base_manifest();
    value["plugin"]["api"] = json!("cordis-portable/v2");
    let err = parse(&value).expect_err("api v2 should fail");
    assert_eq!(
        err,
        PluginError::UnsupportedApi {
            expected: API.to_owned(),
            actual: "cordis-portable/v2".to_owned(),
        }
    );
}

#[test]
fn cpp_manifest_005_fuel_over_limit_is_rejected() {
    // CPP-MANIFEST-005：fuel 上限 1e10（spec §3.5）。
    let mut value = base_manifest();
    value["runtime"] = json!({ "kind": "wasm-component", "fuel": 10_000_000_001_u64 });
    let manifest = parse(&value).expect("shape is valid; quota is checked by validate");
    let err = manifest
        .validate(&host())
        .expect_err("fuel over cap should fail");
    assert!(
        matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("runtime.fuel")),
        "{err}"
    );
}

#[test]
fn cpp_manifest_006_max_memory_out_of_range_is_rejected() {
    // CPP-MANIFEST-006：线性内存 1..=512 MB（spec §3.5）。
    for memory in [0_u64, 513] {
        let mut value = base_manifest();
        value["runtime"] = json!({ "kind": "wasm-component", "max_memory_mb": memory });
        let manifest = parse(&value).expect("shape is valid");
        let err = manifest
            .validate(&host())
            .expect_err("memory out of range should fail");
        assert!(
            matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("runtime.max_memory_mb")),
            "{memory}: {err}"
        );
    }
}

#[test]
fn cpp_manifest_007_timeout_out_of_range_is_rejected() {
    // CPP-MANIFEST-007：runtime.timeout_ms 1000..=600000。
    for timeout in [999_u64, 600_001] {
        let mut value = base_manifest();
        value["runtime"]["timeout_ms"] = json!(timeout);
        let manifest = parse(&value).expect("shape is valid");
        let err = manifest
            .validate(&host())
            .expect_err("timeout out of range should fail");
        assert!(
            matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("runtime.timeout_ms")),
            "{timeout}: {err}"
        );
    }
}

#[test]
fn cpp_manifest_008_min_host_version_newer_than_host_is_rejected() {
    // CPP-MANIFEST-008：min_host_version > 宿主 → HostTooOld；相等/更旧通过。
    let mut too_new = base_manifest();
    too_new["plugin"]["min_host_version"] = json!("9.9.9");
    let manifest = parse(&too_new).expect("shape is valid");
    let err = manifest
        .validate(&host())
        .expect_err("newer min host should fail");
    assert_eq!(
        err,
        PluginError::HostTooOld {
            required: "9.9.9".to_owned(),
            actual: "0.2.2".to_owned(),
        }
    );

    for compatible in ["0.2.2", "0.2.1", "0.1.0", "0.2.2-rc.1"] {
        let mut value = base_manifest();
        value["plugin"]["min_host_version"] = json!(compatible);
        let manifest = parse(&value).expect("shape is valid");
        manifest
            .validate(&host())
            .unwrap_or_else(|err| panic!("{compatible} should be compatible: {err}"));
    }
}

#[test]
fn cpp_manifest_009_inject_key_format_is_checked() {
    // CPP-MANIFEST-009：inject 的 Service key 必须是 `<name>/v<major>`（spec §5.1）。
    for (section, key) in [
        ("required", "tool-registry"),
        ("optional", "Storage/v1"),
        ("required", "storage/v"),
        ("optional", "storage/x1"),
    ] {
        let mut value = base_manifest();
        value["inject"] = json!({ "required": [], "optional": [] });
        value["inject"][section] = json!([key]);
        let manifest = parse(&value).expect("shape is valid");
        let err = manifest
            .validate(&host())
            .expect_err("bad service key should fail");
        assert!(
            matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("inject.")),
            "{key}: {err}"
        );
    }
}

#[test]
fn cpp_manifest_010_duplicate_tool_name_is_rejected() {
    // CPP-MANIFEST-010：tool 重名拒绝（spec §7.1 E_TOOL_CONFLICT 的装载期版本）。
    let mut value = base_manifest();
    let tool = json!({
        "name": "probe",
        "description": "probe tool",
        "input_schema": "schemas/probe.input.json",
        "output_schema": "schemas/probe.output.json"
    });
    value["tools"] = json!([tool.clone(), tool]);
    let manifest = parse(&value).expect("shape is valid");
    let err = manifest
        .validate(&host())
        .expect_err("duplicate tool should fail");
    assert!(
        matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("duplicate tool name")),
        "{err}"
    );
}

#[test]
fn cpp_manifest_011_ui_location_enum_is_checked() {
    // CPP-MANIFEST-011：ui[].location 只接受 left/right/bottom/center。
    let mut invalid = full_manifest();
    invalid["ui"][0]["location"] = json!("top");
    let err = parse(&invalid).expect_err("unknown ui location should fail");
    assert!(matches!(err, PluginError::Manifest { .. }), "{err}");
    assert!(err.to_string().contains("unknown variant"), "{err}");

    for location in ["left", "right", "bottom", "center"] {
        let mut valid = full_manifest();
        valid["ui"][0]["location"] = json!(location);
        let manifest = parse(&valid).unwrap_or_else(|err| panic!("{location}: {err}"));
        manifest
            .validate(&host())
            .unwrap_or_else(|err| panic!("{location}: {err}"));
    }
}

#[test]
fn cpp_manifest_012_empty_ui_slot_is_rejected() {
    // CPP-MANIFEST-012：ui 引用的 slot 不得为空。
    let mut value = full_manifest();
    value["ui"][0]["slot"] = json!("   ");
    let manifest = parse(&value).expect("shape is valid");
    let err = manifest
        .validate(&host())
        .expect_err("empty slot should fail");
    assert!(
        matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("ui[0].slot")),
        "{err}"
    );
}

#[test]
fn cpp_manifest_013_from_path_missing_file_error_contains_path() {
    // CPP-MANIFEST-013：读文件失败的错误必须带路径。
    let path = temp_manifest_path("missing");
    let err = Manifest::from_path(&path).expect_err("missing file should fail");
    assert!(matches!(&err, PluginError::Manifest { .. }), "{err}");
    let text = err.to_string();
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(text.contains("cannot read manifest file"), "{text}");
}

#[test]
fn cpp_manifest_014_from_path_reads_and_parses_file() {
    // CPP-MANIFEST-014：from_path 正常路径（用 %TEMP% 避免引入 tempfile）。
    let path = temp_manifest_path("valid");
    std::fs::write(&path, full_manifest().to_string()).expect("write temp manifest");
    let result = Manifest::from_path(&path);
    let _ = std::fs::remove_file(&path);
    let manifest = result.expect("temp manifest should parse");
    assert_eq!(manifest.plugin.id, "cad.validate");
    manifest
        .validate(&host())
        .expect("temp manifest should validate");
}

#[test]
fn cpp_manifest_015_tool_spec_validation_rejects_bad_fields() {
    // CPP-MANIFEST-015：ToolSpec::validate 的字段级规则（spec §7.1）。
    let good = ToolSpec {
        name: "probe".to_owned(),
        description: "probe tool".to_owned(),
        input_schema: "schemas/probe.input.json".to_owned(),
        output_schema: "schemas/probe.output.json".to_owned(),
        timeout_ms: Some(120_000),
        cancelable: None,
        presentation: Some("markdown".to_owned()),
    };
    good.validate().expect("valid tool spec");

    let bad_name = ToolSpec {
        name: "Probe".to_owned(),
        ..good.clone()
    };
    assert!(bad_name.validate().unwrap_err().starts_with("name:"));

    let bad_description = ToolSpec {
        description: String::new(),
        ..good.clone()
    };
    assert!(bad_description
        .validate()
        .unwrap_err()
        .starts_with("description:"));

    let bad_schema = ToolSpec {
        input_schema: "schemas/probe.input.yaml".to_owned(),
        ..good.clone()
    };
    assert!(bad_schema
        .validate()
        .unwrap_err()
        .starts_with("input_schema:"));

    let bad_timeout = ToolSpec {
        timeout_ms: Some(10),
        ..good
    };
    assert!(bad_timeout
        .validate()
        .unwrap_err()
        .starts_with("timeout_ms:"));
}

#[test]
fn cpp_manifest_016_unknown_nested_fields_are_rejected() {
    // CPP-MANIFEST-016：plugin/inject/tools/ui 内部也不许未知字段。
    let mut plugin_extra = full_manifest();
    plugin_extra["plugin"]["author"] = json!("someone");
    assert!(matches!(
        parse(&plugin_extra),
        Err(PluginError::Manifest { .. })
    ));

    let mut inject_extra = full_manifest();
    inject_extra["inject"]["all"] = json!([]);
    assert!(matches!(
        parse(&inject_extra),
        Err(PluginError::Manifest { .. })
    ));

    let mut tool_extra = full_manifest();
    tool_extra["tools"][0]["retries"] = json!(3);
    assert!(matches!(
        parse(&tool_extra),
        Err(PluginError::Manifest { .. })
    ));

    let mut ui_extra = full_manifest();
    ui_extra["ui"][0]["icon"] = json!("cad");
    assert!(matches!(
        parse(&ui_extra),
        Err(PluginError::Manifest { .. })
    ));
}

#[test]
fn cpp_manifest_017_optional_sections_default_to_empty_and_false() {
    // CPP-MANIFEST-017：inject/tools/ui 缺省为空，permissions 缺省全 false。
    let manifest = parse(&base_manifest()).expect("minimal manifest should parse");
    assert!(manifest.inject.is_none());
    assert!(manifest.permissions.is_none());
    assert!(manifest.tools.is_empty());
    assert!(manifest.ui.is_empty());
    manifest
        .validate(&host())
        .expect("minimal manifest should validate");
}

#[test]
fn cpp_manifest_018_unknown_runtime_kind_is_rejected() {
    // CPP-MANIFEST-018：runtime.kind 只接受 rust-static / wasm-component / external-process。
    let mut value = base_manifest();
    value["runtime"]["kind"] = json!("dll");
    let err = parse(&value).expect_err("unknown runtime kind should fail");
    assert!(matches!(err, PluginError::Manifest { .. }), "{err}");
    assert!(err.to_string().contains("unknown variant"), "{err}");
}

#[test]
fn cpp_manifest_019_invalid_min_host_version_is_rejected() {
    // CPP-MANIFEST-019：min_host_version 不是合法 SemVer → InvalidManifest（不是 panic）。
    let mut value = base_manifest();
    value["plugin"]["min_host_version"] = json!("1.2");
    let manifest = parse(&value).expect("shape is valid");
    let err = manifest
        .validate(&host())
        .expect_err("invalid semver should fail");
    assert!(
        matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("plugin.min_host_version")),
        "{err}"
    );
}

#[test]
fn cpp_manifest_020_validate_rechecks_api_on_mutated_manifest() {
    // CPP-MANIFEST-020：已解析的 Manifest 被改 api 后，validate 仍拒绝（防御性二次检查）。
    let mut manifest = parse(&base_manifest()).expect("minimal manifest should parse");
    manifest.plugin.api = "cordis-portable/v2".to_owned();
    let err = manifest.validate(&host()).expect_err("api v2 should fail");
    assert!(matches!(err, PluginError::UnsupportedApi { .. }), "{err}");
}

#[test]
fn cpp_manifest_021_invalid_plugin_version_is_rejected() {
    // CPP-MANIFEST-021：plugin.version 必须是 SemVer（spec §3.3）。
    let mut value = base_manifest();
    value["plugin"]["version"] = json!("1.2");
    let manifest = parse(&value).expect("shape is valid");
    let err = manifest
        .validate(&host())
        .expect_err("invalid plugin version should fail");
    assert!(
        matches!(err, PluginError::InvalidManifest(ref reason) if reason.contains("plugin.version")),
        "{err}"
    );
}

#[test]
fn cpp_manifest_022_quota_boundaries_are_inclusive() {
    // CPP-MANIFEST-022：配额上界本身允许（fuel=1e10 / memory=512 / timeout=600000）。
    let mut value = base_manifest();
    value["runtime"] = json!({
        "kind": "wasm-component",
        "fuel": 10_000_000_000_u64,
        "max_memory_mb": 512,
        "timeout_ms": 600_000
    });
    let manifest = parse(&value).expect("shape is valid");
    manifest
        .validate(&host())
        .expect("upper bounds are inclusive");
}

// ---- CPP-VER：自写 SemVer 比较 ----

#[test]
fn cpp_ver_001_core_segments_compare_numerically() {
    // CPP-VER-001：数字段按数值比较，不是字符串比较。
    assert!(version("1.2.3") < version("1.10.0"));
    assert!(version("1.9.9") < version("1.10.0"));
    assert!(version("2.0.0") > version("1.99.99"));
    assert_eq!(version("1.2.3").cmp(&version("1.2.3")), Ordering::Equal);
}

#[test]
fn cpp_ver_002_prerelease_is_lower_than_release() {
    // CPP-VER-002：预发布 < 同号正式版。
    assert!(version("1.0.0-alpha") < version("1.0.0"));
    assert!(version("2.3.4-rc.1") < version("2.3.4"));
    assert!(version("2.3.4-rc.1") > version("2.3.3"));
}

#[test]
fn cpp_ver_003_semver_official_precedence_chain() {
    // CPP-VER-003：semver.org §11 的官方优先级链。
    let chain = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
    ];
    let parsed: Vec<Version> = chain.iter().map(|raw| version(raw)).collect();
    for pair in parsed.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        assert!(left < right, "{left} should be < {right}");
    }
}

#[test]
fn cpp_ver_004_numeric_identifier_sorts_before_alphanumeric() {
    // CPP-VER-004：预发布段中数字标识符低于字母标识符；数字之间按数值比较。
    assert!(version("1.0.0-2") < version("1.0.0-alpha"));
    assert!(version("1.0.0-2") < version("1.0.0-10"));
    assert!(version("1.0.0-10") < version("1.0.0-rc.1"));
}

#[test]
fn cpp_ver_005_build_metadata_is_ignored_for_precedence() {
    // CPP-VER-005：build metadata 接受但不参与比较（SemVer §10）。
    assert_eq!(
        version("1.0.0+build.1").cmp(&version("1.0.0+build.2")),
        Ordering::Equal
    );
    assert_eq!(
        version("1.0.0-rc.1+exp.sha.5114f85").cmp(&version("1.0.0-rc.1")),
        Ordering::Equal
    );
    assert!(version("1.0.0-rc.1+build") < version("1.0.0"));
    assert_eq!(version("1.0.0+build.1"), version("1.0.0"));
}

#[test]
fn cpp_ver_006_invalid_versions_are_rejected() {
    // CPP-VER-006：非法 SemVer 一律 Err（不 panic、不静默纠正前导零）。
    for raw in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "v1.2.3",
        "1.2.3-",
        "1.2.3+",
        "1.2.3-alpha..1",
        "1.2.3-alpha_beta",
        "01.2.3",
        "1.0.0-01",
        "1.2.3+build..1",
    ] {
        assert!(Version::parse(raw).is_err(), "`{raw}` should be rejected");
    }
}

#[test]
fn cpp_ver_007_host_version_parse_display_and_compare() {
    // CPP-VER-007：HostVersion 的解析/展示/比较（plugin-host 接入时用作基准）。
    let host = HostVersion::parse("0.2.1-rc.3").expect("host version should parse");
    assert_eq!(host.to_string(), "0.2.1-rc.3");
    assert_eq!(host.version(), &version("0.2.1-rc.3"));

    assert!(HostVersion::new(0, 2, 1) < HostVersion::parse("0.2.2").unwrap());
    assert!(HostVersion::parse("0.2.2-rc.1").unwrap() < HostVersion::new(0, 2, 2));
    assert_eq!(
        HostVersion::from(version("0.2.1")),
        HostVersion::new(0, 2, 1)
    );
    assert!(HostVersion::parse("nope").is_err());
}
