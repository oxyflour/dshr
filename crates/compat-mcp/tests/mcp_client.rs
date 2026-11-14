//! `dshr-compat-mcp` 集成测试：真实子进程（`mcp-stub` fixture）+ NDJSON JSON-RPC。
//!
//! 测试 ID 段：`MCP-001..`。全部离线、无网络；每个用例自建/回收 stub 进程。

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dshr_compat_mcp::{
    register_tools, McpCallResult, McpClient, McpClientState, McpError, McpServerSpec,
    MCP_PROTOCOL_VERSION,
};
use dshr_tool_runtime::{AllowAll, ToolOrigin, ToolRegistry};
use serde_json::json;

fn stub_path() -> &'static str {
    env!("CARGO_BIN_EXE_mcp-stub")
}

fn spec(name: &str) -> McpServerSpec {
    McpServerSpec::new(name, stub_path())
}

fn ready_client() -> McpClient {
    let mut client = McpClient::spawn(spec("mcp-stub")).expect("spawn mcp-stub");
    client.initialize().expect("initialize mcp-stub");
    client
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "dshr-compat-mcp-{name}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// MCP-001：`initialize` 协商成功（含幂等），客户端进入 Ready。
#[test]
fn mcp_001_initialize_negotiates_protocol() {
    let mut client = McpClient::spawn(spec("mcp-stub")).expect("spawn");
    assert_eq!(client.state(), McpClientState::Spawned);

    let initialize = client.initialize().expect("initialize");
    assert_eq!(initialize.protocol_version, MCP_PROTOCOL_VERSION);
    assert_eq!(client.negotiated_protocol(), Some(MCP_PROTOCOL_VERSION));
    assert_eq!(
        client.server_info().expect("server info")["name"],
        "mcp-stub"
    );
    assert_eq!(client.state(), McpClientState::Ready);
    assert!(client.is_alive());

    // 幂等：重复 initialize 返回缓存结果，不重复握手。
    let again = client.initialize().expect("idempotent initialize");
    assert_eq!(again.protocol_version, MCP_PROTOCOL_VERSION);

    client.shutdown().expect("shutdown");
}

/// MCP-002：服务端回不支持的协议版本 → 拒绝并断开（规范要求），restart 前不可用。
#[test]
fn mcp_002_rejects_unsupported_protocol_version() {
    let mut client =
        McpClient::spawn(spec("mcp-stub").with_env("MCP_STUB_PROTOCOL_VERSION", "1999-01-01"))
            .expect("spawn");
    let error = client
        .initialize()
        .expect_err("must reject protocol version");
    match error {
        McpError::ProtocolVersion { received, .. } => assert_eq!(received, "1999-01-01"),
        other => panic!("expected ProtocolVersion, got {other:?}"),
    }
    assert_eq!(client.state(), McpClientState::Broken);
    assert!(matches!(
        client.list_tools().expect_err("broken client"),
        McpError::NotReady { .. }
    ));
    client.shutdown().expect("broken shutdown is idempotent");
    assert_eq!(client.state(), McpClientState::Shutdown);
}

/// MCP-003：`tools/list` 跟随 `nextCursor` 聚合两页 → 4 个工具（顺序稳定）。
#[test]
fn mcp_003_tools_list_paginates_and_aggregates() {
    let mut client = ready_client();
    let tools = client.list_tools().expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "fail", "slow", "big"]);
    assert_eq!(client.stats().late_responses, 0);
    client.shutdown().expect("shutdown");
}

/// MCP-004：MCP tool → `ToolDefinition`（origin Mcp、schema 原样、描述截断、timeout 可控）。
#[test]
fn mcp_004_tool_schema_converts_to_tool_definition() {
    let mut client = ready_client();
    let tools = client.list_tools().expect("tools/list");
    let echo = tools
        .iter()
        .find(|tool| tool.name == "echo")
        .expect("echo tool");

    let definition = echo.as_tool_definition("mcp-stub", 12_345);
    assert_eq!(
        definition.origin,
        ToolOrigin::Mcp {
            server: "mcp-stub".to_owned(),
        }
    );
    assert_eq!(definition.origin.kind(), "mcp");
    assert_eq!(definition.name, "echo");
    assert_eq!(definition.timeout_ms, 12_345);
    assert_eq!(
        definition.description.chars().count(),
        dshr_compat_mcp::MAX_TOOL_DESCRIPTION_CHARS,
        "描述必须截断到 1024 字符"
    );
    assert_eq!(
        definition.input_schema["properties"]["echo"]["type"], "string",
        "inputSchema 必须原样保留"
    );
    assert!(definition.output_schema.is_none());
    client.shutdown().expect("shutdown");
}

/// MCP-005：`tools/call` echo 往返；优先 structuredContent。
#[test]
fn mcp_005_tools_call_echo_round_trip() {
    let mut client = ready_client();
    let result: McpCallResult = client
        .call_tool("echo", json!({ "echo": "hi" }))
        .expect("echo call");
    assert!(!result.is_error);
    assert!(!result.truncated);
    assert_eq!(result.value["echo"], "hi");
    client.shutdown().expect("shutdown");
}

/// MCP-006：`isError:true` 映射为 `McpCallResult.is_error`（不是 Err），文本仍可读。
#[test]
fn mcp_006_is_error_is_preserved_in_result() {
    let mut client = ready_client();
    let result = client.call_tool("fail", json!({})).expect("fail call");
    assert!(result.is_error);
    assert!(!result.truncated);
    assert_eq!(result.value, json!("boom"));
    client.shutdown().expect("shutdown");
}

/// MCP-007：慢调用 + 短超时 → `Timeout`，客户端保持可用（迟到答复被丢弃）。
#[test]
fn mcp_007_timeout_then_client_is_still_usable() {
    let mut client =
        McpClient::spawn(spec("mcp-stub").with_timeout(Duration::from_millis(800))).expect("spawn");
    client.initialize().expect("initialize");

    let error = client
        .call_tool("slow", json!({}))
        .expect_err("must time out");
    assert!(matches!(error, McpError::Timeout { .. }), "{error:?}");
    assert_eq!(client.stats().timed_out_requests, 1);
    assert_eq!(client.state(), McpClientState::Ready);

    // stub 的 slow 在后台线程睡眠，主循环仍能响应新请求。
    let result = client
        .call_tool("echo", json!({ "echo": "after-timeout" }))
        .expect("client still usable");
    assert_eq!(result.value["echo"], "after-timeout");
    client.shutdown().expect("shutdown");
}

/// MCP-008：>1 MiB 结果按内联上限截断（字符串前缀），并计数。
#[test]
fn mcp_008_big_result_is_truncated() {
    let mut client = ready_client();
    let result = client.call_tool("big", json!({})).expect("big call");
    assert!(result.truncated, "1.5 MiB text must be truncated");
    let text = result
        .value
        .as_str()
        .expect("truncated text stays a string");
    assert!(text.len() <= dshr_compat_mcp::DEFAULT_MAX_RESULT_BYTES);
    assert!(text.starts_with('x'));
    assert_eq!(client.stats().truncated_results, 1);
    client.shutdown().expect("shutdown");
}

/// MCP-009：进程崩溃 → 在途请求 `ProcessExited`、状态 Broken；`restart` 后可用。
#[test]
fn mcp_009_crash_in_flight_fails_and_restart_recovers() {
    let mut client = ready_client();
    let error = client
        .call_tool("crash", json!({}))
        .expect_err("crash must fail in flight");
    match error {
        McpError::ProcessExited { code } => assert_eq!(code, Some(3), "stub exits with 3"),
        other => panic!("expected ProcessExited, got {other:?}"),
    }
    assert_eq!(client.state(), McpClientState::Broken);
    assert!(!client.is_alive());

    let initialize = client.restart().expect("restart");
    assert_eq!(initialize.protocol_version, MCP_PROTOCOL_VERSION);
    assert_eq!(client.state(), McpClientState::Ready);
    let result = client
        .call_tool("echo", json!({ "echo": "restarted" }))
        .expect("call after restart");
    assert_eq!(result.value["echo"], "restarted");
    client.shutdown().expect("shutdown");
}

/// MCP-010：`shutdown` 幂等且回收进程；关闭后请求 → NotReady。
#[test]
fn mcp_010_shutdown_is_idempotent_without_leftover_process() {
    let mut client = ready_client();
    assert!(client.pid().is_some());
    client.shutdown().expect("first shutdown");
    client.shutdown().expect("second shutdown is idempotent");
    assert_eq!(client.state(), McpClientState::Shutdown);
    assert!(!client.is_alive());
    assert!(client.pid().is_none(), "child handle must be reaped");
    assert!(matches!(
        client.list_tools().expect_err("shutdown client"),
        McpError::NotReady { .. }
    ));

    // 进程已回收：再起一个客户端仍可正常握手（无残留占用）。
    let mut another = ready_client();
    another.shutdown().expect("shutdown another");
}

/// MCP-011：stdout 非法 JSON 行被忽略并计数，不影响后续请求。
#[test]
fn mcp_011_invalid_json_lines_are_counted() {
    let mut client =
        McpClient::spawn(spec("mcp-stub").with_env("MCP_STUB_EMIT_GARBAGE", "1")).expect("spawn");
    client
        .initialize()
        .expect("initialize despite garbage line");
    assert!(
        client.stats().invalid_lines >= 1,
        "garbage line must be counted: {:?}",
        client.stats()
    );
    assert_eq!(client.state(), McpClientState::Ready);
    let tools = client.list_tools().expect("tools/list");
    assert_eq!(tools.len(), 4);
    client.shutdown().expect("shutdown");
}

/// MCP-012：`register_tools` 把工具注册进 Tool Registry（origin/schema/policy 可用）。
#[test]
fn mcp_012_register_tools_into_registry() {
    let mut client = ready_client();
    let mut registry = ToolRegistry::new(AllowAll);
    let names = register_tools(&mut client, &mut registry).expect("register tools");
    assert_eq!(names, vec!["echo", "fail", "slow", "big"]);
    assert_eq!(registry.len(), 4);

    let echo = registry.get("echo").expect("echo registered");
    assert_eq!(
        echo.origin,
        ToolOrigin::Mcp {
            server: "mcp-stub".to_owned(),
        }
    );
    registry
        .authorize("echo", &json!({ "echo": "hi" }))
        .expect("schema allows echo argument");
    assert!(
        registry.authorize("echo", &json!({})).is_err(),
        "required 字段仍然由 Registry 校验"
    );
    client.shutdown().expect("shutdown");
}

/// MCP-013：spawn 失败映射为 `McpError::Spawn`（不是 panic）。
#[test]
fn mcp_013_spawn_failure_is_reported() {
    match McpClient::spawn(McpServerSpec::new(
        "missing",
        "dshr-definitely-missing-mcp-binary-xyz",
    )) {
        Ok(_) => panic!("spawning a missing binary must fail"),
        Err(error) => assert!(matches!(error, McpError::Spawn { .. }), "{error:?}"),
    }
}

/// MCP-014：JSON-RPC error → `McpError::Rpc`（未知工具 -32602），客户端仍可用。
#[test]
fn mcp_014_json_rpc_errors_are_mapped() {
    let mut client = ready_client();
    let error = client
        .call_tool("no-such-tool", json!({}))
        .expect_err("unknown tool must be a JSON-RPC error");
    match error {
        McpError::Rpc { code, message, .. } => {
            assert_eq!(code, -32602);
            assert!(message.contains("unknown tool"));
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
    assert_eq!(client.state(), McpClientState::Ready);
    client.shutdown().expect("shutdown");
}

/// MCP-015：`spec.cwd` 生效（stub 在临时目录中启动并正常往返）。
#[test]
fn mcp_015_cwd_is_passed_to_the_server_process() {
    let dir = temp_dir("cwd");
    let mut client = McpClient::spawn(spec("mcp-stub").with_cwd(&dir)).expect("spawn in cwd");
    client.initialize().expect("initialize");
    let result = client
        .call_tool("echo", json!({ "echo": "cwd" }))
        .expect("echo");
    assert_eq!(result.value["echo"], "cwd");
    client.shutdown().expect("shutdown");
    std::fs::remove_dir_all(&dir).ok();
}

/// MCP-016：`notifications/message` 忽略并计数，不干扰请求/响应配对。
#[test]
fn mcp_016_notifications_are_counted_and_ignored() {
    let mut client =
        McpClient::spawn(spec("mcp-stub").with_env("MCP_STUB_NOTIFY", "1")).expect("spawn");
    client.initialize().expect("initialize");
    // 通知在 initialize result 之后写出；tools/list 的响应必然排在它之后被读到。
    let tools = client.list_tools().expect("tools/list");
    assert_eq!(tools.len(), 4);
    let stats = client.stats();
    assert!(stats.notifications >= 1, "{stats:?}");
    assert!(stats.log_notifications >= 1, "{stats:?}");
    assert_eq!(client.state(), McpClientState::Ready);
    client.shutdown().expect("shutdown");
}

/// MCP-017：服务端忽略取消 → 迟到答复被 reader 丢弃并计数（不串到下一个请求）。
#[test]
fn mcp_017_late_responses_are_dropped() {
    let mut client = McpClient::spawn(
        spec("mcp-stub")
            .with_timeout(Duration::from_millis(800))
            .with_env("MCP_STUB_IGNORE_CANCEL", "1"),
    )
    .expect("spawn");
    client.initialize().expect("initialize");
    let error = client
        .call_tool("slow", json!({}))
        .expect_err("must time out");
    assert!(matches!(error, McpError::Timeout { .. }), "{error:?}");

    let deadline = Instant::now() + Duration::from_secs(3);
    while client.stats().late_responses == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        client.stats().late_responses >= 1,
        "late slow result must be dropped and counted: {:?}",
        client.stats()
    );
    assert_eq!(client.state(), McpClientState::Ready);
    client.shutdown().expect("shutdown");
}
