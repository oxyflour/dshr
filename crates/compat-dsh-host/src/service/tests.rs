#![cfg(test)]

use super::*;
use dshr_compat_dsh_protocol::{HostServiceMethod, PROTOCOL_VERSION};
use std::sync::{Arc, Mutex};

const SERVICE_KEY: &str = "test-service/v1";
const PLUGIN: &str = "example.dsh.tool";

fn object_schema(properties: Value, required: Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn empty_object_schema() -> Value {
    object_schema(json!({}), json!([]))
}

fn args_name_schema() -> Value {
    object_schema(json!({ "name": { "type": "string" } }), json!(["name"]))
}

fn result_echo_schema() -> Value {
    object_schema(json!({ "echo": {} }), json!(["echo"]))
}

fn method(
    name: &str,
    kind: HostServiceKind,
    args_schema: Value,
    result_schema: Value,
    timeout_ms: u64,
    idempotent: bool,
    requires_session: bool,
) -> HostServiceMethod {
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        args_schema,
        result_schema,
        timeout_ms,
        idempotent,
        requires_session,
    }
}

#[derive(Clone)]
struct Counters(Arc<Mutex<BTreeMap<String, usize>>>);

impl Counters {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(BTreeMap::new())))
    }

    fn count(&self, method: &str) -> usize {
        self.0
            .lock()
            .expect("counters")
            .get(method)
            .copied()
            .unwrap_or(0)
    }

    fn total(&self) -> usize {
        self.0.lock().expect("counters").values().sum()
    }

    fn bump(&self, method: &str) {
        *self
            .0
            .lock()
            .expect("counters")
            .entry(method.to_owned())
            .or_insert(0) += 1;
    }
}

struct TestService {
    table: HostServiceTable,
    counters: Counters,
}

impl TestService {
    fn new() -> Self {
        let table = HostServiceTable::new(
            SERVICE_KEY,
            vec![
                method(
                    "get",
                    HostServiceKind::Read,
                    args_name_schema(),
                    result_echo_schema(),
                    5_000,
                    true,
                    false,
                ),
                method(
                    "put",
                    HostServiceKind::Write,
                    args_name_schema(),
                    empty_object_schema(),
                    5_000,
                    false,
                    false,
                ),
                method(
                    "slow",
                    HostServiceKind::Read,
                    empty_object_schema(),
                    result_echo_schema(),
                    100,
                    true,
                    false,
                ),
                method(
                    "big",
                    HostServiceKind::Read,
                    empty_object_schema(),
                    result_echo_schema(),
                    5_000,
                    true,
                    false,
                ),
                method(
                    "fail",
                    HostServiceKind::Read,
                    empty_object_schema(),
                    empty_object_schema(),
                    5_000,
                    true,
                    false,
                ),
                method(
                    "session",
                    HostServiceKind::Read,
                    empty_object_schema(),
                    result_echo_schema(),
                    5_000,
                    true,
                    true,
                ),
                method(
                    "stream",
                    HostServiceKind::Stream,
                    empty_object_schema(),
                    result_echo_schema(),
                    5_000,
                    true,
                    false,
                ),
                method(
                    "bad_result",
                    HostServiceKind::Read,
                    empty_object_schema(),
                    result_echo_schema(),
                    5_000,
                    true,
                    false,
                ),
            ],
        );
        Self {
            table,
            counters: Counters::new(),
        }
    }
}

impl HostService for TestService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        self.counters.bump(method);
        match method {
            "get" => Ok(json!({ "echo": args.get("name").cloned().unwrap_or(Value::Null) })),
            "put" => Ok(json!({})),
            "slow" => {
                std::thread::sleep(Duration::from_millis(160));
                Ok(json!({ "echo": "slow" }))
            }
            "big" => Ok(json!({ "echo": "x".repeat(800_000) })),
            "fail" => Err(ServiceError::new("E_INTERNAL", "boom")),
            "session" => Ok(json!({
                "echo": ctx.session_id.clone().unwrap_or_default()
            })),
            "bad_result" => Ok(json!({ "nope": true })),
            other => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unexpected method `{other}`"),
            )),
        }
    }
}

fn router_with(
    negotiated_version: u32,
    caps: Vec<String>,
    with_inject: bool,
) -> (HostServiceRouter, Counters) {
    let service = TestService::new();
    let counters = service.counters.clone();
    let mut router =
        HostServiceRouter::new("node-0001", negotiated_version, caps).expect("router negotiates");
    router
        .register(Box::new(service))
        .expect("register service");
    if with_inject {
        router.register_plugin_inject(PLUGIN, vec![SERVICE_KEY.to_owned()]);
    }
    (router, counters)
}

fn v2_router() -> (HostServiceRouter, Counters) {
    router_with(PROTOCOL_MAX, vec![CAP_SERVICE_PROXY.to_owned()], true)
}

fn request(method: &str, args: Value, request_id: &str) -> Envelope {
    Envelope::host(
        KIND_HOST_SERVICE_REQUEST,
        request_id,
        json!({ "method": method, "args": args }),
    )
    .with_version(PROTOCOL_MAX)
    .with_generation("node-0001")
    .with_plugin(PLUGIN)
    .with_service_key(SERVICE_KEY)
    .with_session("s-0001")
    .with_depth(1)
}

fn error_code(envelope: &Envelope) -> &str {
    envelope
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("expected an error envelope, got {envelope:?}"))
        .code
        .as_str()
}

fn error_retryable(envelope: &Envelope) -> bool {
    envelope
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("expected an error envelope, got {envelope:?}"))
        .retryable
}

fn assert_success(reply: &Envelope) {
    assert!(reply.error.is_none(), "unexpected error: {reply:?}");
    assert_eq!(reply.kind, KIND_HOST_SERVICE_RESULT);
    assert_eq!(reply.version, PROTOCOL_MAX, "答复必须沿用协商版本");
    assert_eq!(reply.service_key.as_deref(), Some(SERVICE_KEY));
}

/// RPC-L2-101：读方法真实路由成功；答复回填关联字段。
#[test]
fn rpc_l2_101_routes_read_method() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("get", json!({ "name": "src" }), "req-101"));
    assert_success(&reply);
    assert_eq!(reply.request_id, "req-101");
    assert_eq!(reply.plugin_id.as_deref(), Some(PLUGIN));
    assert_eq!(reply.session_id.as_deref(), Some("s-0001"));
    assert_eq!(reply.depth, 1);
    assert_eq!(reply.payload["result"]["echo"], "src");
    assert_eq!(counters.count("get"), 1);
    assert_eq!(router.service_keys(), vec![SERVICE_KEY]);
}

/// RPC-L2-102：未发布的 `service_key` → `E_SERVICE_UNAVAILABLE`。
#[test]
fn rpc_l2_102_unknown_service_key_is_unavailable() {
    let (mut router, counters) = v2_router();
    let mut envelope = request("get", json!({ "name": "x" }), "req-102");
    envelope.service_key = Some("missing-service/v1".to_owned());
    let reply = router.handle_request(&envelope);
    assert_eq!(error_code(&reply), "E_SERVICE_UNAVAILABLE");
    assert_eq!(counters.total(), 0);
}

/// RPC-L2-103：未知 method → `E_SERVICE_METHOD`。
#[test]
fn rpc_l2_103_unknown_method_is_method_error() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("missing", json!({}), "req-103"));
    assert_eq!(error_code(&reply), "E_SERVICE_METHOD");
    assert_eq!(counters.total(), 0);
}

/// RPC-L2-104：保留 kind `stream` → `E_SERVICE_METHOD`（spec §13.2/§13.9）。
#[test]
fn rpc_l2_104_stream_method_is_rejected() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("stream", json!({}), "req-104"));
    assert_eq!(error_code(&reply), "E_SERVICE_METHOD");
    assert_eq!(counters.total(), 0);
}

/// RPC-L2-105：缺必填字段 → `E_SERVICE_ARGS` 且 `detail.path` 指向首个失败字段；
/// 宿主不执行任何副作用（spec §12.1 RPC-L2-004）。
#[test]
fn rpc_l2_105_missing_required_arg_has_no_side_effect() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("get", json!({}), "req-105"));
    assert_eq!(error_code(&reply), "E_SERVICE_ARGS");
    assert_eq!(reply.payload["detail"]["path"], "/args/name");
    assert!(reply.payload["detail"]["reason"].is_string());
    assert_eq!(counters.total(), 0, "schema 失败不得执行服务");
}

/// RPC-L2-106：类型错 / 多余字段 → `E_SERVICE_ARGS`。
#[test]
fn rpc_l2_106_wrong_type_and_extra_field_are_args_errors() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("get", json!({ "name": 42 }), "req-106a"));
    assert_eq!(error_code(&reply), "E_SERVICE_ARGS");
    assert_eq!(reply.payload["detail"]["path"], "/args/name");

    let reply = router.handle_request(&request(
        "get",
        json!({ "name": "x", "extra": true }),
        "req-106b",
    ));
    assert_eq!(error_code(&reply), "E_SERVICE_ARGS");
    assert_eq!(reply.payload["detail"]["path"], "/args/extra");
    assert_eq!(counters.total(), 0);
}

/// RPC-L2-107：args 内联 > 256 KiB → `E_TOO_LARGE`，不执行（spec §13.8）。
#[test]
fn rpc_l2_107_oversized_args_are_rejected() {
    let (mut router, counters) = v2_router();
    let args = json!({ "name": "x".repeat(300_000) });
    let reply = router.handle_request(&request("get", args, "req-107"));
    assert_eq!(error_code(&reply), "E_TOO_LARGE");
    assert_eq!(counters.total(), 0);
}

/// RPC-L2-108：结果 > 768 KiB 且无 `service.attach` → `E_TOO_LARGE`（spec §13.8）。
#[test]
fn rpc_l2_108_oversized_result_is_rejected() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("big", json!({}), "req-108"));
    assert_eq!(error_code(&reply), "E_TOO_LARGE");
    assert_eq!(counters.count("big"), 1, "结果超限发生在执行之后");
}

/// RPC-L2-109：方法 `timeout_ms` 到期 → `E_SERVICE_TIMEOUT`（retryable=true）。
#[test]
fn rpc_l2_109_timeout_is_retryable() {
    let (mut router, _counters) = v2_router();
    let started = Instant::now();
    let reply = router.handle_request(&request("slow", json!({}), "req-109"));
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert_eq!(error_code(&reply), "E_SERVICE_TIMEOUT");
    assert!(error_retryable(&reply));
    assert!(reply.error.as_ref().unwrap().message.contains("timeout"));
}

/// RPC-L2-110：请求 generation 失效 → `E_SERVICE_UNAVAILABLE`；换 generation 后可恢复。
#[test]
fn rpc_l2_110_stale_generation_is_unavailable() {
    let (mut router, _counters) = v2_router();
    let stale = request("get", json!({ "name": "x" }), "req-110").with_generation("node-9999");
    let reply = router.handle_request(&stale);
    assert_eq!(error_code(&reply), "E_SERVICE_UNAVAILABLE");

    router.set_generation("node-0002");
    let stale = request("get", json!({ "name": "x" }), "req-110b");
    assert_eq!(
        error_code(&router.handle_request(&stale)),
        "E_SERVICE_UNAVAILABLE"
    );
    let fresh = request("get", json!({ "name": "x" }), "req-110c").with_generation("node-0002");
    assert_success(&router.handle_request(&fresh));
}

/// RPC-L2-111：未协商 `service.proxy` → `E_SERVICE_DENIED`；协商为 1 的 v2-only kind
/// → `E_PROTOCOL`（spec §3.2/§13.9）。
#[test]
fn rpc_l2_111_proxy_capability_and_version_gate() {
    let (mut denied, _counters) = router_with(PROTOCOL_MAX, vec![], true);
    let reply = denied.handle_request(&request("get", json!({ "name": "x" }), "req-111a"));
    assert_eq!(error_code(&reply), "E_SERVICE_DENIED");

    let (mut v1, _counters) = router_with(PROTOCOL_VERSION, vec![], true);
    let mut envelope = request("get", json!({ "name": "x" }), "req-111b");
    envelope.version = PROTOCOL_VERSION;
    assert_eq!(
        error_code(&v1.handle_request(&envelope)),
        "E_PROTOCOL",
        "协商为 1 时收到 host.service.request 必须回 E_PROTOCOL"
    );
}

/// RPC-L2-112：manifest `inject` 未声明 → `E_SERVICE_DENIED`；白名单函数的三条件。
#[test]
fn rpc_l2_112_manifest_inject_whitelist() {
    let (mut router, _counters) =
        router_with(PROTOCOL_MAX, vec![CAP_SERVICE_PROXY.to_owned()], false);
    let reply = router.handle_request(&request("get", json!({ "name": "x" }), "req-112"));
    assert_eq!(error_code(&reply), "E_SERVICE_DENIED");

    let caps = vec![CAP_SERVICE_PROXY.to_owned()];
    let inject = vec![SERVICE_KEY.to_owned()];
    assert!(allow(PLUGIN, SERVICE_KEY, &inject, &caps));
    assert!(!allow(PLUGIN, SERVICE_KEY, &[], &caps), "缺 inject 声明");
    assert!(
        !allow(PLUGIN, SERVICE_KEY, &inject, &[]),
        "未协商 service.proxy"
    );
    assert!(!allow("", SERVICE_KEY, &inject, &caps), "空 plugin_id");
    assert!(
        !allow(PLUGIN, "bad key", &inject, &caps),
        "非法 service_key"
    );
    assert!(
        !allow(PLUGIN, "other-service/v1", &inject, &caps),
        "manifest 未声明该 key"
    );
}

/// RPC-L2-113：取消幂等；取消先到 → `E_CANCELLED` 且无副作用；已结算后取消被忽略。
#[test]
fn rpc_l2_113_cancel_is_idempotent_and_prevents_dispatch() {
    let (mut router, counters) = v2_router();
    let cancelled = request("get", json!({ "name": "x" }), "req-113");
    router.cancel("req-113");
    router.cancel("req-113");
    let reply = router.handle_request(&cancelled);
    assert_eq!(error_code(&reply), "E_CANCELLED");
    assert_eq!(counters.total(), 0, "取消先到不得执行服务");

    // 幂等去重窗口内重发：返回同一条 E_CANCELLED 结算，不执行。
    let reply = router.handle_request(&cancelled);
    assert_eq!(error_code(&reply), "E_CANCELLED");
    assert_eq!(counters.total(), 0);

    // 已结算后取消：幂等忽略，不影响正常请求。
    let ok = request("get", json!({ "name": "y" }), "req-113b");
    assert_success(&router.handle_request(&ok));
    router.cancel("req-113b");
    assert_eq!(counters.count("get"), 1);
}

/// RPC-L2-114：`idempotent:true` 的同 `request_id` 重发命中 60 s 去重窗口，
/// 只执行一次；`idempotent:false` 重发 → `E_PROTOCOL`（spec §13.4 规则 4）。
#[test]
fn rpc_l2_114_idempotent_dedupe_window() {
    let (mut router, counters) = v2_router();
    let first = request("get", json!({ "name": "x" }), "req-114");
    let first_reply = router.handle_request(&first);
    assert_success(&first_reply);
    let second_reply = router.handle_request(&first);
    assert_success(&second_reply);
    assert_eq!(second_reply.payload, first_reply.payload);
    assert_eq!(counters.count("get"), 1, "同 request_id 只执行一次");

    let put = request("put", json!({ "name": "x" }), "req-114b");
    assert_success(&router.handle_request(&put));
    let duplicate = router.handle_request(&put);
    assert_eq!(error_code(&duplicate), "E_PROTOCOL");
    assert_eq!(counters.count("put"), 1, "非幂等重复请求不得执行");
}

/// RPC-L2-115：背压上限（每插件 4 / 每 Sidecar 16）→ `E_BACKPRESSURE`。
#[test]
fn rpc_l2_115_backpressure_limits() {
    let (mut router, _counters) = v2_router();
    for _ in 0..MAX_IN_FLIGHT_PER_PLUGIN {
        router.acquire_slot(PLUGIN).expect("slot");
    }
    let error = router.acquire_slot(PLUGIN).unwrap_err();
    assert_eq!(error.code, "E_BACKPRESSURE");
    assert!(error.retryable);
    router.release_slot(PLUGIN);
    assert!(router.acquire_slot(PLUGIN).is_ok());
    for _ in 0..MAX_IN_FLIGHT_PER_PLUGIN {
        router.release_slot(PLUGIN);
    }

    for plugin in ["p0", "p1", "p2", "p3"] {
        for _ in 0..MAX_IN_FLIGHT_PER_PLUGIN {
            router.acquire_slot(plugin).expect("slot");
        }
    }
    let error = router.acquire_slot("p4").unwrap_err();
    assert_eq!(error.code, "E_BACKPRESSURE", "Sidecar 总在途上限 16");
}

/// RPC-L2-116：depth > 4 → `E_RPC_DEPTH`；缺 plugin_id/service_key → `E_PROTOCOL`；
/// 会话级方法缺 session_id → `E_SERVICE_ARGS`（spec §4/§5.5/§7.6）。
#[test]
fn rpc_l2_116_depth_identity_and_session_gates() {
    let (mut router, counters) = v2_router();
    let deep = request("get", json!({ "name": "x" }), "req-116a").with_depth(5);
    assert_eq!(error_code(&router.handle_request(&deep)), "E_RPC_DEPTH");

    let mut no_plugin = request("get", json!({ "name": "x" }), "req-116b");
    no_plugin.plugin_id = None;
    assert_eq!(error_code(&router.handle_request(&no_plugin)), "E_PROTOCOL");

    let mut no_service_key = request("get", json!({ "name": "x" }), "req-116c");
    no_service_key.service_key = None;
    assert_eq!(
        error_code(&router.handle_request(&no_service_key)),
        "E_PROTOCOL"
    );

    let mut no_session = request("session", json!({}), "req-116d");
    no_session.session_id = None;
    let reply = router.handle_request(&no_session);
    assert_eq!(error_code(&reply), "E_SERVICE_ARGS");
    assert_eq!(reply.payload["detail"]["path"], "/session_id");

    let with_session = request("session", json!({}), "req-116e");
    assert_success(&router.handle_request(&with_session));
    assert_eq!(counters.count("session"), 1);
}

/// RPC-L2-117：`host_services` / `capabilities.list` 白名单表逐条一致（spec §13.2）。
#[test]
fn rpc_l2_117_host_services_table_is_consistent() {
    let (router, _counters) = v2_router();
    let tables = router.host_services();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].service_key, SERVICE_KEY);
    assert_eq!(tables[0].methods.len(), 8);
    assert_eq!(tables[0].validate(), Ok(()));

    let payload = router.host_services_payload();
    let array = payload.as_array().expect("array payload");
    assert_eq!(array.len(), 1);
    assert_eq!(array[0]["service_key"], SERVICE_KEY);
    assert_eq!(
        array[0]["methods"].as_array().map(Vec::len),
        Some(tables[0].methods.len())
    );
}

/// RPC-L2-118：结果违反 `result_schema` → `E_INTERNAL`（宿主实现错误，detail 指向字段）。
#[test]
fn rpc_l2_118_result_schema_violation_is_internal() {
    let (mut router, counters) = v2_router();
    let reply = router.handle_request(&request("bad_result", json!({}), "req-118"));
    assert_eq!(error_code(&reply), "E_INTERNAL");
    assert!(!error_retryable(&reply));
    assert_eq!(reply.payload["detail"]["path"], "/echo");
    assert_eq!(counters.count("bad_result"), 1);
}

/// RPC-L2-119：服务实现的结构化错误原样进信封 `error`（spec §9.3）。
#[test]
fn rpc_l2_119_service_errors_are_passed_through() {
    let (mut router, _counters) = v2_router();
    let reply = router.handle_request(&request("fail", json!({}), "req-119"));
    assert_eq!(error_code(&reply), "E_INTERNAL");
    assert!(error_retryable(&reply), "E_INTERNAL 属于瞬时错误分类");
    assert_eq!(
        reply.error.as_ref().map(|error| error.message.as_str()),
        Some("boom")
    );
}
