#![cfg(test)]

use super::*;

#[test]
fn round_trips_through_json() {
    let env = hello(&["tool.stream"])
        .with_plugin("example.probe")
        .with_session("s-1")
        .with_call("call-1");
    let text = serde_json::to_string(&env).unwrap();
    let back: Envelope = serde_json::from_str(&text).unwrap();
    assert_eq!(env, back);
    assert!(text.contains("\"kind\":\"runtime.hello\""));
}

#[test]
fn optional_identity_fields_are_omitted_when_absent() {
    let env = Envelope::host("health.ping", "req-1", json!({}));
    let value: serde_json::Value = serde_json::to_value(&env).unwrap();
    assert!(value.get("plugin_id").is_none());
    assert!(value.get("call_id").is_none());
    assert_eq!(value["version"], 1);
}

#[test]
fn reply_keeps_correlation_fields() {
    let req = hello(&[])
        .with_plugin("p")
        .with_call("c-1")
        .with_session("s-1");
    let rep = req.reply("runtime.ready", json!({ "generation": "node-0001" }));
    assert_eq!(rep.request_id, req.request_id);
    assert_eq!(rep.plugin_id, req.plugin_id);
    assert_eq!(rep.call_id, req.call_id);
    assert_eq!(rep.session_id, req.session_id);
    assert_eq!(rep.kind, "runtime.ready");
}

#[test]
fn error_codes_are_closed_set() {
    assert!(ERROR_CODES.contains(&"E_CANCELLED"));
    assert!(ERROR_CODES.contains(&"E_RPC_DEPTH"));
}

#[test]
fn retryable_classification_is_conservative() {
    assert!(is_retryable_sidecar_error("E_TIMEOUT"));
    assert!(is_retryable_sidecar_error("E_BACKPRESSURE"));
    assert!(is_retryable_sidecar_error("E_INTERNAL"));
    for code in [
        "E_PROTOCOL",
        "E_VERSION",
        "E_SCHEMA",
        "E_CANCELLED",
        "E_TOO_LARGE",
        "E_UNKNOWN_PLUGIN",
        "E_DUPLICATE_REG",
        "E_RPC_DEPTH",
    ] {
        assert!(
            !is_retryable_sidecar_error(code),
            "{code} must not be auto-retried"
        );
    }
}

#[test]
fn depth_limit_is_declared() {
    assert_eq!(MAX_DEPTH, 4);
}

#[test]
fn host_caps_include_fs_write_only_when_negotiated() {
    let read_only = hello(&[CAP_TOOL_STREAM, CAP_TOOL_CANCEL]);
    assert_eq!(
        read_only.payload["host_caps"],
        json!(["tool.stream", "tool.cancel"])
    );
    assert!(!read_only.payload["host_caps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|cap| cap == CAP_FS_WRITE));

    let caps = vec![
        CAP_TOOL_STREAM.to_owned(),
        CAP_TOOL_CANCEL.to_owned(),
        CAP_FS_WRITE.to_owned(),
    ];
    let writable = hello_with_caps(&caps);
    assert!(writable.payload["host_caps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|cap| cap == CAP_FS_WRITE));
}

#[test]
fn fs_write_request_round_trips() {
    let request = FsWriteRequest {
        call_id: Some("call-1".to_owned()),
        tool: "write".to_owned(),
        operation: FsWriteOperation::Write,
        path: "notes.txt".to_owned(),
        workspace_root: Some("C:\\ws".to_owned()),
        content: Some("hello".to_owned()),
        expected_version: Some("100:5".to_owned()),
        intent: Some(json!({ "kind": "replaceIfVersion", "version": "100:5" })),
        mode: Some("workspace-write".to_owned()),
    };
    let text = serde_json::to_string(&request).unwrap();
    let back: FsWriteRequest = serde_json::from_str(&text).unwrap();
    assert_eq!(request, back);
    assert_eq!(request.summary()["operation"], "write");
}

#[test]
fn fs_write_request_defaults_are_lenient_for_forward_compat() {
    let request: FsWriteRequest =
        serde_json::from_value(json!({ "path": "a.txt", "content": "x" })).unwrap();
    assert_eq!(request.operation, FsWriteOperation::Write);
    assert!(request.call_id.is_none());
    assert!(request.expected_version.is_none());
}

#[test]
fn fs_write_results_carry_structured_errors() {
    let failure = FsWriteResult::error("FS_SANDBOX_DENIED", "outside workspace");
    assert!(!failure.ok);
    assert!(!failure.is_ok());
    let error = failure.error.unwrap();
    assert_eq!(error.code, "FS_SANDBOX_DENIED");
    assert!(!error.retryable);

    let success = FsWriteResult {
        ok: true,
        path: Some("C:\\ws\\a.txt".to_owned()),
        operation: Some("create".to_owned()),
        version: Some("100:5".to_owned()),
        bytes: Some(5),
        snapshot: None,
        error: None,
    };
    let value = serde_json::to_value(&success).unwrap();
    assert_eq!(value["ok"], true);
    assert!(value.get("snapshot").is_none());
}

// ---- v2 服务代理（spec §3.2/§4/§13）：RPC-L2-0xx ----

fn service_method(name: &str, kind: HostServiceKind) -> HostServiceMethod {
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        args_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "path": { "type": "string" } }
        }),
        result_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["cwd"],
            "properties": { "cwd": { "type": "string" } }
        }),
        timeout_ms: 5_000,
        idempotent: true,
        requires_session: false,
    }
}

fn v2_request() -> Envelope {
    Envelope::host(
        KIND_HOST_SERVICE_REQUEST,
        "req-42",
        json!({ "method": "get", "args": {} }),
    )
    .with_version(PROTOCOL_MAX)
    .with_generation("node-0007")
    .with_plugin("example.dsh.tool")
    .with_service_key("working-directory/v1")
    .with_session("s-0001")
    .with_depth(1)
}

/// RPC-L2-001：v2 信封 + `service_key` 往返；答复回填 version/service_key。
#[test]
fn rpc_l2_001_v2_envelope_and_service_key_round_trip() {
    let request = v2_request();
    let text = serde_json::to_string(&request).unwrap();
    assert!(text.contains("\"service_key\":\"working-directory/v1\""));
    let back: Envelope = serde_json::from_str(&text).unwrap();
    assert_eq!(request, back);
    assert_eq!(back.version, 2);

    let reply = request.reply(
        KIND_HOST_SERVICE_RESULT,
        json!({ "result": { "cwd": "C:\\ws" } }),
    );
    assert_eq!(reply.version, 2, "答复复制协商版本，不能退回常量 1");
    assert_eq!(reply.service_key.as_deref(), Some("working-directory/v1"));
    assert_eq!(reply.request_id, "req-42");
}

/// RPC-L2-002：`service_key` 非法形态拒绝（spec §13.2）。
#[test]
fn rpc_l2_002_invalid_service_keys_are_rejected() {
    for key in [
        "working-directory",
        "Working-Directory/v1",
        "working-directory/v0",
        "working-directory/v01",
        "working-directory/1",
        "/v1",
        "working_directory/v1",
    ] {
        assert!(
            validate_service_key(key).is_err(),
            "`{key}` must be rejected"
        );
        let mut envelope = v2_request();
        envelope.service_key = Some(key.to_owned());
        let violation = validate_envelope(&envelope, PROTOCOL_MAX).unwrap_err();
        assert_eq!(violation.code, "E_PROTOCOL", "key `{key}`");
    }
    for key in ["working-directory/v1", "storage-kv/v1", "example-tools/v2"] {
        assert_eq!(validate_service_key(key), Ok(()), "`{key}` must pass");
    }
}

/// RPC-L2-003：未知 kind 拒绝；已知 v1 kind 仍接受（v1 行为不变）。
#[test]
fn rpc_l2_003_unknown_kinds_are_rejected() {
    let unknown = Envelope::host("service.chunk", "req-1", json!({})).with_version(2);
    let violation = validate_envelope(&unknown, PROTOCOL_MAX).unwrap_err();
    assert_eq!(violation.code, "E_PROTOCOL");

    // v1 既有消息全部仍在闭集内，v1 协商下照常通过。
    for kind in [
        "runtime.hello",
        "tool.invoke",
        "tool.result",
        "preset.sync",
        "preset.bind",
        "preset.release",
        "fs.write.request",
    ] {
        let envelope = Envelope::host(kind, "req-1", json!({}));
        assert_eq!(
            validate_envelope(&envelope, PROTOCOL_VERSION),
            Ok(()),
            "kind `{kind}`"
        );
    }
}

/// RPC-L2-004：协商为 1 时 v2-only kind / `service_key` → `E_PROTOCOL`（§3.2 规则 3）。
#[test]
fn rpc_l2_004_v2_only_kinds_require_negotiated_version_two() {
    for kind in V2_ONLY_KINDS {
        let envelope = Envelope::host(*kind, "req-1", json!({})).with_service_key("x/v1");
        let violation = validate_envelope(&envelope, PROTOCOL_VERSION).unwrap_err();
        assert_eq!(violation.code, "E_PROTOCOL", "kind `{kind}`");
    }
    let mut envelope = v2_request();
    envelope.version = PROTOCOL_VERSION;
    // 信封 version 与协商版本不一致本身就是协议错误。
    let violation = validate_envelope(&envelope, PROTOCOL_VERSION).unwrap_err();
    assert_eq!(violation.code, "E_PROTOCOL");

    // 协商为 2 且 service_key 合法 → 通过。
    assert_eq!(validate_envelope(&v2_request(), PROTOCOL_MAX), Ok(()));
    // 服务代理 kind 缺 service_key → E_PROTOCOL（§4 条件必需字段）。
    let missing = Envelope::host(KIND_HOST_SERVICE_REQUEST, "req-1", json!({})).with_version(2);
    let violation = validate_envelope(&missing, PROTOCOL_MAX).unwrap_err();
    assert_eq!(violation.code, "E_PROTOCOL");
}

/// RPC-L2-005：版本协商矩阵（§3.2）与 v1 缺省信封不变。
#[test]
fn rpc_l2_005_version_negotiation_matrix() {
    assert_eq!(PROTOCOL_VERSION, 1, "v1 缺省线格式语义保持不变");
    assert_eq!(PROTOCOL_MAX, 2);
    assert_eq!(negotiate_version(1, 1), Some(1));
    assert_eq!(negotiate_version(2, 1), Some(1));
    assert_eq!(negotiate_version(1, 2), Some(1));
    assert_eq!(negotiate_version(2, 2), Some(2));
    assert_eq!(negotiate_version(0, 2), None, "没有交集必须拒绝");
    assert_eq!(negotiate_version(2, 0), None);
    assert_eq!(negotiate_version(3, 3), None, "超出实现区间要拒绝");

    let v1 = Envelope::host("health.ping", "req-1", json!({}));
    let value = serde_json::to_value(&v1).unwrap();
    assert_eq!(value["version"], 1);
    assert!(
        value.get("service_key").is_none(),
        "v1 消息不带 service_key"
    );
    assert_eq!(validate_envelope(&v1, 1), Ok(()));
}

/// RPC-L2-006：错误码闭集与重试分类（§9/§9.2/§13.4）。
#[test]
fn rpc_l2_006_service_error_codes_and_retry_classification() {
    for code in [
        "E_ALREADY_SETTLED",
        "E_SERVICE_UNAVAILABLE",
        "E_SERVICE_METHOD",
        "E_SERVICE_ARGS",
        "E_SERVICE_TIMEOUT",
        "E_SERVICE_DENIED",
        "E_SERVICE_CONFLICT",
    ] {
        assert!(
            ERROR_CODES.contains(&code),
            "{code} must join the closed set"
        );
    }
    assert!(is_retryable_sidecar_error("E_SERVICE_TIMEOUT"));
    assert!(is_retryable_service_error("E_SERVICE_TIMEOUT"));
    assert!(is_retryable_service_error("E_BACKPRESSURE"));
    assert!(is_retryable_service_error("E_INTERNAL"));
    for code in [
        "E_SERVICE_UNAVAILABLE",
        "E_SERVICE_METHOD",
        "E_SERVICE_ARGS",
        "E_SERVICE_DENIED",
        "E_SERVICE_CONFLICT",
        "E_CANCELLED",
        "E_ALREADY_SETTLED",
        "E_TOO_LARGE",
    ] {
        assert!(
            !is_retryable_service_error(code),
            "{code} must not be auto-retried"
        );
    }
}

/// RPC-L2-007：方法表往返 + 缺省值 + 描述表校验（§13.2）。
#[test]
fn rpc_l2_007_host_service_table_round_trip_and_validation() {
    let table = HostServiceTable::new(
        "working-directory/v1",
        vec![
            service_method("get", HostServiceKind::Read),
            service_method("set", HostServiceKind::Write),
        ],
    );
    assert_eq!(table.validate(), Ok(()));
    assert_eq!(table.limits.max_inline_bytes, DEFAULT_MAX_INLINE_BYTES);
    assert_eq!(table.limits.max_result_bytes, DEFAULT_MAX_RESULT_BYTES);
    assert_eq!(table.method("get").unwrap().timeout_ms, 5_000);
    assert!(table.method("missing").is_none());

    let text = serde_json::to_string(&table).unwrap();
    let back: HostServiceTable = serde_json::from_str(&text).unwrap();
    assert_eq!(table, back);

    // 缺省值：timeout_ms 缺省 30000、idempotent 缺省 false、requires_session 缺省 false。
    let method: HostServiceMethod = serde_json::from_value(json!({
        "name": "get",
        "kind": "read",
        "args_schema": { "type": "object", "additionalProperties": false },
        "result_schema": { "type": "object", "additionalProperties": false }
    }))
    .unwrap();
    assert_eq!(method.timeout_ms, DEFAULT_METHOD_TIMEOUT_MS);
    assert!(!method.idempotent);
    assert!(!method.requires_session);

    // 非法描述表：坏 service_key / 坏方法名 / 重复 / 超时越界 / schema 非 object。
    assert!(HostServiceTable::new(
        "bad key",
        vec![service_method("get", HostServiceKind::Read)]
    )
    .validate()
    .is_err());
    let mut bad_name = service_method("Bad-Name", HostServiceKind::Read);
    bad_name.timeout_ms = 5_000;
    assert!(bad_name.validate().is_err());
    let mut too_fast = service_method("get", HostServiceKind::Read);
    too_fast.timeout_ms = 99;
    assert!(too_fast.validate().is_err());
    let mut bad_schema = service_method("get", HostServiceKind::Read);
    bad_schema.args_schema = json!({ "type": "string" });
    assert!(bad_schema.validate().is_err());
    let duplicated = HostServiceTable::new(
        "working-directory/v1",
        vec![
            service_method("get", HostServiceKind::Read),
            service_method("get", HostServiceKind::Write),
        ],
    );
    assert!(duplicated.validate().is_err());

    // 首批服务的点分方法名（m3-l2-services §3）必须合法；
    // spec §13.2 的 `[a-z][a-z0-9_]*` 与 m3 服务表冲突，实现按超集放行。
    assert_eq!(validate_method_name("unit.open"), Ok(()));
    assert_eq!(validate_method_name("unit.loadAll"), Ok(()));
    assert_eq!(validate_method_name("session.describe"), Ok(()));
    assert!(validate_method_name("Unit.open").is_err());
    assert!(validate_method_name("unit open").is_err());
}

/// RPC-L2-008：`ServiceCall` / `ServiceReply` 载荷形状（§13.4/§13.8）。
#[test]
fn rpc_l2_008_service_call_and_reply_payloads() {
    let call: ServiceCall = serde_json::from_value(json!({
        "method": "set",
        "args": { "path": "src" }
    }))
    .unwrap();
    assert_eq!(call.method, "set");
    assert_eq!(call.args["path"], "src");
    assert_eq!(serde_json::to_value(&call).unwrap()["args"]["path"], "src");

    let reply = ServiceReply::ok(json!({ "cwd": "C:\\ws" }));
    assert_eq!(reply.validate(), Ok(()));
    let value = serde_json::to_value(&reply).unwrap();
    assert!(value.get("content_ref").is_none());
    assert_eq!(value["result"]["cwd"], "C:\\ws");

    let attachment = ServiceReply::content_ref("attachment://sha256:deadbeef");
    assert_eq!(attachment.validate(), Ok(()));
    let both = ServiceReply {
        result: Some(json!({})),
        content_ref: Some("attachment://sha256:deadbeef".to_owned()),
        detail: None,
    };
    assert!(both.validate().is_err());
    assert!(ServiceReply::default().validate().is_err());
}

/// RPC-L2-009：v2 常量与 schema 词表一致；hello 仍按 v1 缺省协商 max=1。
#[test]
fn rpc_l2_009_protocol_constants_match_schema_vocabulary() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schema/sidecar-rpc/envelope.schema.json"
    ))
    .unwrap();
    let mut schema_kinds: Vec<_> = schema["properties"]["kind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect();
    schema_kinds.sort_unstable();
    let mut known_kinds = KNOWN_KINDS.to_vec();
    known_kinds.sort_unstable();
    assert_eq!(known_kinds, schema_kinds);

    assert_eq!(CAP_SERVICE_PROXY, "service.proxy");
    assert_eq!(CAP_SERVICE_EXPOSE, "service.expose");
    assert_eq!(CAP_SERVICE_ATTACH, "service.attach");
    assert_eq!(KIND_SERVICE_CANCEL, "service.cancel");
    assert_eq!(KIND_HOST_SERVICE_REQUEST, "host.service.request");
    assert_eq!(KIND_HOST_SERVICE_RESULT, "host.service.result");
    assert_eq!(KIND_HOST_SERVICE_CANCEL, "host.service.cancel");
    assert_eq!(V2_ONLY_KINDS.len(), 4);
    assert!(is_v2_only_kind("host.service.request"));
    assert!(!is_v2_only_kind("tool.invoke"));
    assert!(is_service_proxy_kind("service.publish"));
    assert_eq!(SERVICE_PROXY_KINDS.len(), 8);

    let hello_v1 = hello(&[CAP_TOOL_STREAM]);
    assert_eq!(hello_v1.payload["protocol_max"], 1);
    let hello_v2 = hello_with_caps_at(&[CAP_TOOL_STREAM.to_owned()], PROTOCOL_MAX);
    assert_eq!(hello_v2.payload["protocol_max"], 2);
}

/// RPC-L2-010：`depth > 4` 与 `request_id` 非法 → `E_RPC_DEPTH`/`E_PROTOCOL`（§4）。
#[test]
fn rpc_l2_010_depth_and_request_id_limits() {
    let deep = v2_request().with_depth(MAX_DEPTH + 1);
    let violation = validate_envelope(&deep, PROTOCOL_MAX).unwrap_err();
    assert_eq!(violation.code, "E_RPC_DEPTH");

    let mut bad_id = v2_request();
    bad_id.request_id = "bad id!".to_owned();
    let violation = validate_envelope(&bad_id, PROTOCOL_MAX).unwrap_err();
    assert_eq!(violation.code, "E_PROTOCOL");

    let mut unsupported = v2_request();
    unsupported.version = 3;
    assert_eq!(
        validate_envelope(&unsupported, PROTOCOL_MAX)
            .unwrap_err()
            .code,
        "E_PROTOCOL"
    );
    assert_eq!(
        validate_envelope(&unsupported, 3).unwrap_err().code,
        "E_VERSION"
    );
}

// M3-DEFAULT-010：`tool.register.timeout_ms` 的跨语言归一化。
#[test]
fn m3_default_010_tool_timeout_normalization() {
    assert_eq!(normalize_tool_timeout_ms(None), None);
    assert_eq!(normalize_tool_timeout_ms(Some(0)), None);
    // 下界/上界 clamp（与 chat 的运行期 clamp 同口径 1s..=600s）。
    assert_eq!(
        normalize_tool_timeout_ms(Some(1)),
        Some(MIN_TOOL_TIMEOUT_MS)
    );
    assert_eq!(normalize_tool_timeout_ms(Some(5_000)), Some(5_000));
    assert_eq!(
        normalize_tool_timeout_ms(Some(u64::MAX)),
        Some(MAX_TOOL_TIMEOUT_MS)
    );
    assert_eq!(DEFAULT_TOOL_TIMEOUT_MS, 120_000);
    assert_eq!(MIN_TOOL_TIMEOUT_MS, 1_000);
    assert_eq!(MAX_TOOL_TIMEOUT_MS, 600_000);
}
