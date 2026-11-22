//! Host Service 路由（`docs/spec/sidecar-rpc.md` §13.4–§13.9，v2）。
//!
//! 这一层只做**路由与裁决**，不做服务实现：
//! - 方法表由宿主声明（§13.2），Sidecar 不能扩张；
//! - 白名单三条件（§11.6/§13.9）：`service.proxy` 已协商、`service_key` 在宿主方法表、
//!   插件 manifest/`inject` 声明允许；
//! - 每次调用对 args 再按 `args_schema` 校验一次（最小 JSON Schema 子集）；
//! - 超时/取消/幂等去重/背压/大小上限都在这里裁决，服务实现只看到合法 args。
//!
//! 同步实现：没有异步运行时。超时用 `Instant` deadline 在 `dispatch` 返回后判定
//! （同步实现无法抢占）；取消用请求 ID 标记，`handle_request` 在 dispatch 前/后各查
//! 一次，迟到结果丢弃并以 `E_CANCELLED` 结算。

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use dshr_compat_dsh_protocol::{
    is_retryable_service_error, validate_envelope, validate_service_key, Envelope, HostServiceKind,
    HostServiceTable, RpcError, ServiceCall, CAP_SERVICE_PROXY, KIND_HOST_SERVICE_REQUEST,
    KIND_HOST_SERVICE_RESULT, PROTOCOL_MAX, PROTOCOL_MIN,
};
use serde_json::{json, Value};

/// 每个插件在途服务调用上限（spec §13.4 规则 5，与工具并发上限独立计数）。
pub const MAX_IN_FLIGHT_PER_PLUGIN: u32 = 4;
/// 每个 Sidecar 在途服务调用上限。
pub const MAX_IN_FLIGHT_TOTAL: u32 = 16;
/// `idempotent:true` 请求的 `request_id` 去重窗口（spec §13.4 规则 4）。
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(60);
/// 取消标记集合的防御性上限（正常只会有几条在途请求）。
const MAX_PENDING_CANCELS: usize = 1_024;
/// 去重缓存条数的防御性上限。
const MAX_SETTLED: usize = 256;

/// 一次服务调用的身份上下文（由信封还原，spec §5）。
///
/// `session_id` 是唯一身份锚点：宿主在此边界把它还原成自己的对象；
/// 服务实现只能看到字符串与深度，拿不到 `Agent`/`Session`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCallContext {
    pub plugin_id: String,
    pub generation: String,
    pub session_id: Option<String>,
    pub call_id: Option<String>,
    pub tool_request_id: Option<String>,
    pub depth: u32,
}

/// 服务实现的失败（映射到信封 `error`，spec §9.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceError {
    /// §9 主表里的 `E_*` 码；实现自有的词表放 [`ServiceError::detail`]。
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub detail: Option<Value>,
}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let retryable = is_retryable_service_error(&code);
        Self {
            code,
            message: message.into(),
            retryable,
            detail: None,
        }
    }

    /// 参数错误：`detail.path` 指向首个失败字段（spec §13.9）。
    pub fn args(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new("E_SERVICE_ARGS", "service args validation failed").with_detail(json!({
            "path": path.into(),
            "reason": reason.into(),
        }))
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

/// 宿主服务实现（spec §13.1 B 路：Sidecar → host）。
///
/// `Send`：`SidecarHost` 会被 chat 放进 `Mutex<ChatState>` 跨线程持有，
/// 因此 `Box<dyn HostService>` 必须可跨线程移动（实现内部状态自行加锁）。
///
/// `dispatch` 是同步的：服务实现不得反向同步调用同一 Sidecar（会双向死锁，§13.4 规则 6）；
/// 需要通知时走事件通道。
pub trait HostService: Send {
    /// 宿主愿意暴露的方法表（白名单子集，不是全量）。
    fn table(&self) -> &HostServiceTable;

    /// 执行一次方法；`args` 已经过路由层的 schema 校验。
    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        ctx: &ServiceCallContext,
    ) -> Result<Value, ServiceError>;

    fn release_tool_call(&mut self, _request_id: &str) {}
}

/// 最小 JSON Schema 校验失败（spec §13.2 的实现子集：type/required/properties/
/// additionalProperties/items/enum + 少量长度/数值边界）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    /// JSON pointer 形态，根对象字段形如 `/path`，args 整体由调用方补 `/args` 前缀。
    pub path: String,
    pub reason: String,
}

/// 白名单函数（spec §11.6 / §13.9）：
/// ① 协商到 `service.proxy`；② `service_key` 合法且由宿主注册（条件②由
/// [`HostServiceRouter`] 的注册表查找保证——未注册回 `E_SERVICE_UNAVAILABLE`）；
/// ③ 插件 manifest/`inject` 声明允许注入。
pub fn allow(
    plugin_id: &str,
    service_key: &str,
    manifest_inject: &[String],
    negotiated_caps: &[String],
) -> bool {
    !plugin_id.is_empty()
        && validate_service_key(service_key).is_ok()
        && negotiated_caps.iter().any(|cap| cap == CAP_SERVICE_PROXY)
        && manifest_inject
            .iter()
            .any(|declared| declared == service_key)
}

/// 按最小 JSON Schema 子集校验（只用于宿主自己声明的方法表，不做通用 schema 实现）。
pub fn validate_json_schema(schema: &Value, value: &Value) -> Result<(), SchemaViolation> {
    validate_schema_at(schema, value, "")
}

fn validate_schema_at(schema: &Value, value: &Value, path: &str) -> Result<(), SchemaViolation> {
    let object = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(violation(path, "schema rejects every value")),
        Value::Object(object) => object,
        // 空 schema / 非 object schema 视为"不限制"。
        _ => return Ok(()),
    };
    if let Some(allowed) = object.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            return Err(violation(path, "value is not one of the enum entries"));
        }
    }
    if let Some(types) = object.get("type") {
        if !type_matches(types, value) {
            return Err(violation(path, format!("expected type {types}")));
        }
    }
    match value {
        Value::Object(map) => {
            if let Some(required) = object.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(name) {
                        return Err(violation(
                            &join_path(path, name),
                            "required property is missing",
                        ));
                    }
                }
            }
            let properties = object.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (name, property_schema) in properties {
                    if let Some(property_value) = map.get(name) {
                        validate_schema_at(
                            property_schema,
                            property_value,
                            &join_path(path, name),
                        )?;
                    }
                }
            }
            if object.get("additionalProperties") == Some(&Value::Bool(false)) {
                for name in map.keys() {
                    let declared =
                        properties.is_some_and(|properties| properties.contains_key(name));
                    if !declared {
                        return Err(violation(
                            &join_path(path, name),
                            "additional property is not allowed",
                        ));
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = object.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_schema_at(item_schema, item, &format!("{path}/{index}"))?;
                }
            }
            if let Some(min_items) = object.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min_items {
                    return Err(violation(
                        path,
                        format!("array needs at least {min_items} items"),
                    ));
                }
            }
            if let Some(max_items) = object.get("maxItems").and_then(Value::as_u64) {
                if (items.len() as u64) > max_items {
                    return Err(violation(
                        path,
                        format!("array allows at most {max_items} items"),
                    ));
                }
            }
        }
        Value::String(text) => {
            if let Some(min_length) = object.get("minLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) < min_length {
                    return Err(violation(
                        path,
                        format!("string needs at least {min_length} chars"),
                    ));
                }
            }
            if let Some(max_length) = object.get("maxLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) > max_length {
                    return Err(violation(
                        path,
                        format!("string allows at most {max_length} chars"),
                    ));
                }
            }
        }
        Value::Number(number) => {
            if let Some(minimum) = object.get("minimum").and_then(Value::as_f64) {
                if number.as_f64().is_some_and(|number| number < minimum) {
                    return Err(violation(path, format!("number must be >= {minimum}")));
                }
            }
            if let Some(maximum) = object.get("maximum").and_then(Value::as_f64) {
                if number.as_f64().is_some_and(|number| number > maximum) {
                    return Err(violation(path, format!("number must be <= {maximum}")));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn violation(path: &str, reason: impl Into<String>) -> SchemaViolation {
    SchemaViolation {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn join_path(path: &str, field: &str) -> String {
    format!("{path}/{field}")
}

fn type_matches(types: &Value, value: &Value) -> bool {
    match types {
        Value::String(name) => single_type_matches(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| single_type_matches(name, value)),
        _ => true,
    }
}

fn single_type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        // 未知 type 词表不拒绝：最小校验器宁可放过，也不误伤宿主扩展。
        _ => true,
    }
}

/// 一次 `host.service.request` 的裁决结果缓存（幂等去重窗口，spec §13.4 规则 4）。
#[derive(Debug, Clone)]
struct SettledReply {
    reply: Envelope,
    at: Instant,
}

/// v2 服务路由：注册表 + 白名单 + 全量裁决（spec §13.4–§13.9）。
///
/// 由 `compat-dsh-host` 的读循环在收到 `host.service.request` 时调用
/// [`HostServiceRouter::handle_request`]；同步实现意味着服务调用的耗时计入
/// 协议读循环，因此服务方法必须做小、做快（首批三个服务都满足）。
pub struct HostServiceRouter {
    generation: String,
    negotiated_version: u32,
    caps: Vec<String>,
    services: BTreeMap<String, Box<dyn HostService>>,
    /// `plugin_id` → manifest/`inject` 声明的服务键（白名单条件③）。
    inject: BTreeMap<String, Vec<String>>,
    in_flight: BTreeMap<String, u32>,
    total_in_flight: u32,
    cancelled: BTreeSet<String>,
    settled: BTreeMap<(String, String), SettledReply>,
}

impl HostServiceRouter {
    /// `negotiated_version` 必须来自 [`dshr_compat_dsh_protocol::negotiate_version`]。
    pub fn new(
        generation: impl Into<String>,
        negotiated_version: u32,
        caps: Vec<String>,
    ) -> Result<Self, ServiceError> {
        if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&negotiated_version) {
            return Err(ServiceError::new(
                "E_VERSION",
                format!("unsupported negotiated protocol version {negotiated_version}"),
            ));
        }
        Ok(Self {
            generation: generation.into(),
            negotiated_version,
            caps,
            services: BTreeMap::new(),
            inject: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            total_in_flight: 0,
            cancelled: BTreeSet::new(),
            settled: BTreeMap::new(),
        })
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn negotiated_version(&self) -> u32 {
        self.negotiated_version
    }

    pub fn has_cap(&self, cap: &str) -> bool {
        self.caps.iter().any(|negotiated| negotiated == cap)
    }

    /// 注册一个宿主服务实现；描述表非法 → `E_SCHEMA`，键冲突 → `E_SERVICE_CONFLICT`。
    pub fn register(&mut self, service: Box<dyn HostService>) -> Result<(), ServiceError> {
        service.table().validate().map_err(|reason| {
            ServiceError::new("E_SCHEMA", format!("invalid service table: {reason}"))
        })?;
        let service_key = service.table().service_key.clone();
        if self.services.contains_key(&service_key) {
            return Err(ServiceError::new(
                "E_SERVICE_CONFLICT",
                format!("service `{service_key}` is already registered"),
            ));
        }
        self.services.insert(service_key, service);
        Ok(())
    }

    /// 声明某插件 manifest/`inject` 允许注入的服务键（白名单条件③）。
    pub fn register_plugin_inject(
        &mut self,
        plugin_id: impl Into<String>,
        service_keys: Vec<String>,
    ) {
        self.inject.insert(plugin_id.into(), service_keys);
    }

    /// `runtime.ready.payload.host_services` / `capabilities.list` 的白名单方法表。
    ///
    /// 两份输出必须逐条一致（spec §13.2）；集成轮直接序列化这个返回值。
    pub fn host_services(&self) -> Vec<HostServiceTable> {
        self.services
            .values()
            .map(|service| service.table().clone())
            .collect()
    }

    pub fn host_services_payload(&self) -> Value {
        Value::Array(
            self.host_services()
                .into_iter()
                .map(|table| serde_json::to_value(table).unwrap_or(Value::Null))
                .collect(),
        )
    }

    pub fn service_keys(&self) -> Vec<&str> {
        self.services.keys().map(String::as_str).collect()
    }

    pub(crate) fn release_tool_call(&mut self, request_id: &str) {
        for service in self.services.values_mut() {
            service.release_tool_call(request_id);
        }
    }

    /// generation 变化（spec §13.5）：去重窗口清空、取消标记清空、在途计数归零；
    /// 已发布服务与在途调用由调用方负责 revoke/结算（本层同步实现无在途）。
    pub fn set_generation(&mut self, generation: impl Into<String>) {
        self.generation = generation.into();
        self.settled.clear();
        self.cancelled.clear();
        self.in_flight.clear();
        self.total_in_flight = 0;
    }

    /// 白名单三条件（实例视图）：caps + 宿主注册表 + manifest `inject`。
    pub fn allow(&self, plugin_id: &str, service_key: &str) -> bool {
        self.negotiated_version >= 2
            && self.services.contains_key(service_key)
            && allow(
                plugin_id,
                service_key,
                self.inject.get(plugin_id).map(Vec::as_slice).unwrap_or(&[]),
                &self.caps,
            )
    }

    /// 标记一条在途请求为已取消（spec §8/§13.6）。
    ///
    /// 幂等：已结算后再取消被忽略（不报错、不计诊断）；取消先到则请求在 dispatch 前
    /// 直接以 `E_CANCELLED` 结算、不产生副作用。
    pub fn cancel(&mut self, request_id: &str) {
        if self
            .settled
            .keys()
            .any(|(_, settled_id)| settled_id == request_id)
        {
            return;
        }
        if self.cancelled.len() >= MAX_PENDING_CANCELS {
            self.cancelled.pop_first();
        }
        self.cancelled.insert(request_id.to_owned());
    }

    /// 占用一个在途名额（背压，spec §13.4 规则 5）；`handle_request` 内部会调用，
    /// 传输层若把 dispatch 挪到工作线程，也应先占用再派发。
    pub fn acquire_slot(&mut self, plugin_id: &str) -> Result<(), ServiceError> {
        if self.total_in_flight >= MAX_IN_FLIGHT_TOTAL {
            return Err(ServiceError::new(
                "E_BACKPRESSURE",
                format!("sidecar has {MAX_IN_FLIGHT_TOTAL} in-flight service requests"),
            ));
        }
        let in_flight = self.in_flight.get(plugin_id).copied().unwrap_or(0);
        if in_flight >= MAX_IN_FLIGHT_PER_PLUGIN {
            return Err(ServiceError::new(
                "E_BACKPRESSURE",
                format!("plugin `{plugin_id}` has {MAX_IN_FLIGHT_PER_PLUGIN} in-flight service requests"),
            ));
        }
        self.in_flight.insert(plugin_id.to_owned(), in_flight + 1);
        self.total_in_flight += 1;
        Ok(())
    }

    /// 释放一个在途名额（与 [`HostServiceRouter::acquire_slot`] 配对）。
    pub fn release_slot(&mut self, plugin_id: &str) {
        if let Some(in_flight) = self.in_flight.get_mut(plugin_id) {
            *in_flight = in_flight.saturating_sub(1);
            if *in_flight == 0 {
                self.in_flight.remove(plugin_id);
            }
        }
        self.total_in_flight = self.total_in_flight.saturating_sub(1);
    }

    /// 完整路由一条 `host.service.request`（spec §13.4）。
    ///
    /// 任何失败都变成一条带信封 `error` 的 `host.service.result`（恰好一次答复），
    /// 不 panic、不挂起。
    pub fn handle_request(&mut self, request: &Envelope) -> Envelope {
        match self.try_handle_request(request) {
            Ok(reply) => reply,
            Err(error) => error_reply(request, &error),
        }
    }

    fn try_handle_request(&mut self, request: &Envelope) -> Result<Envelope, ServiceError> {
        // 协议门（§3.2/§4/§5.5）：kind/version/service_key/depth/request_id。
        if request.kind != KIND_HOST_SERVICE_REQUEST {
            return Err(ServiceError::new(
                "E_PROTOCOL",
                format!(
                    "expected kind `{KIND_HOST_SERVICE_REQUEST}`, got `{}`",
                    request.kind
                ),
            ));
        }
        validate_envelope(request, self.negotiated_version).map_err(|violation| {
            ServiceError::new(violation.code, violation.message).with_retryable(false)
        })?;
        let plugin_id = request.plugin_id.clone().ok_or_else(|| {
            ServiceError::new("E_PROTOCOL", "host.service.request requires plugin_id")
        })?;
        let service_key = request.service_key.clone().ok_or_else(|| {
            ServiceError::new("E_PROTOCOL", "host.service.request requires service_key")
        })?;

        // ① 能力协商。未协商 service.proxy → E_SERVICE_DENIED（§13.9）。
        if !self.has_cap(CAP_SERVICE_PROXY) || self.negotiated_version < 2 {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "service.proxy was not negotiated",
            ));
        }
        // ② 宿主方法表（未发布/未暴露 → E_SERVICE_UNAVAILABLE，§13.9）。
        if !self.services.contains_key(&service_key) {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("host does not expose service `{service_key}`"),
            ));
        }
        // ③ manifest/inject 白名单（运行期 → E_SERVICE_DENIED，§11.6）。
        if !self.allow(&plugin_id, &service_key) {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                format!("service `{service_key}` is not injected into plugin `{plugin_id}`"),
            ));
        }
        // generation 失效：请求丢弃（§13.5）。
        if request.runtime_generation != self.generation {
            return Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!(
                    "stale generation `{}` (current `{}`)",
                    request.runtime_generation, self.generation
                ),
            ));
        }

        let call: ServiceCall =
            serde_json::from_value(request.payload.clone()).map_err(|error| {
                ServiceError::new(
                    "E_PROTOCOL",
                    format!("invalid host.service.request payload: {error}"),
                )
            })?;
        let meta = self.method_meta(&service_key, &call.method)?;

        // 幂等去重（§13.4 规则 4）：idempotent 重发同结果；非幂等重发 E_PROTOCOL。
        let dedupe_key = (plugin_id.clone(), request.request_id.clone());
        let cached = self.settled.get(&dedupe_key).cloned();
        if let Some(settled) = cached {
            if settled.at.elapsed() <= DEDUPE_WINDOW {
                if meta.idempotent {
                    return Ok(settled.reply);
                }
                return Err(ServiceError::new(
                    "E_PROTOCOL",
                    format!(
                        "duplicate request_id `{}` for non-idempotent method `{}`",
                        request.request_id, meta.name
                    ),
                ));
            }
            self.settled.remove(&dedupe_key);
        }

        // args 内联上限（§13.8）；超限不 dispatch。
        let args_bytes = serde_json::to_vec(&call.args)
            .map_err(|error| ServiceError::new("E_SERVICE_ARGS", format!("args: {error}")))?
            .len() as u64;
        if args_bytes > meta.max_inline_bytes {
            return Err(ServiceError::new(
                "E_TOO_LARGE",
                format!(
                    "args inline size {args_bytes} exceeds {} bytes",
                    meta.max_inline_bytes
                ),
            ));
        }

        // 会话授权（§5.5）：方法声明需要会话时，信封必须带 session_id。
        if meta.requires_session && request.session_id.is_none() {
            return Err(ServiceError::args(
                "/session_id",
                "session-level method requires the envelope session_id",
            ));
        }

        // args 再校验一次（§11.7：宿主裁决，不信任 Sidecar 的 schema）。
        validate_json_schema(&meta.args_schema, &call.args).map_err(|violation| {
            ServiceError::args(format!("/args{}", violation.path), violation.reason)
        })?;

        // 背压（§13.4 规则 5）。
        self.acquire_slot(&plugin_id)?;

        // 取消先到：不 dispatch、无副作用，以 E_CANCELLED 结算并计入去重缓存。
        if self.cancelled.remove(&request.request_id) {
            let reply = error_reply(
                request,
                &ServiceError::new("E_CANCELLED", "request was cancelled before dispatch"),
            );
            self.release_slot(&plugin_id);
            self.note_settled(dedupe_key, &reply);
            return Ok(reply);
        }

        let ctx = ServiceCallContext {
            plugin_id: plugin_id.clone(),
            generation: self.generation.clone(),
            session_id: request.session_id.clone(),
            call_id: request.call_id.clone(),
            tool_request_id: request.payload["tool_request_id"]
                .as_str()
                .map(str::to_owned),
            depth: request.depth,
        };
        let started = Instant::now();
        let outcome = match self.services.get_mut(&service_key) {
            Some(service) => service.dispatch(&meta.name, &call.args, &ctx),
            None => Err(ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("service `{service_key}` disappeared during dispatch"),
            )),
        };
        let elapsed = started.elapsed();
        let cancelled_during_dispatch = self.cancelled.remove(&request.request_id);
        self.release_slot(&plugin_id);

        // 取消优先于超时（§13.4 规则 3 / §8）。
        if cancelled_during_dispatch {
            let reply = error_reply(
                request,
                &ServiceError::new("E_CANCELLED", "request was cancelled during dispatch"),
            );
            self.note_settled(dedupe_key, &reply);
            return Ok(reply);
        }
        // 同步实现的 deadline 检查：无法抢占，只能丢弃迟到结果（§13.4 规则 3）。
        if elapsed > meta.timeout {
            let reply = error_reply(
                request,
                &ServiceError::new(
                    "E_SERVICE_TIMEOUT",
                    format!(
                        "method `{}` took {}ms, exceeding timeout {}ms",
                        meta.name,
                        elapsed.as_millis(),
                        meta.timeout.as_millis()
                    ),
                )
                .with_retryable(true),
            );
            self.note_settled(dedupe_key, &reply);
            return Ok(reply);
        }

        let reply = match outcome {
            Ok(result) => {
                if let Err(violation) = validate_json_schema(&meta.result_schema, &result) {
                    // 结果不合 result_schema 是宿主实现错误，不向插件暴露内部形状。
                    let error = ServiceError::new(
                        "E_INTERNAL",
                        format!(
                            "service `{service_key}` returned a result violating result_schema at {}",
                            violation.path
                        ),
                    )
                    .with_detail(json!({
                        "path": violation.path,
                        "reason": violation.reason,
                    }))
                    .with_retryable(false);
                    let reply = error_reply(request, &error);
                    self.note_settled(dedupe_key, &reply);
                    return Ok(reply);
                }
                let result_bytes = serde_json::to_vec(&result)
                    .map_err(|error| {
                        ServiceError::new("E_INTERNAL", format!("serialize result: {error}"))
                    })?
                    .len() as u64;
                if result_bytes > meta.max_result_bytes {
                    error_reply(
                        request,
                        &ServiceError::new(
                            "E_TOO_LARGE",
                            format!(
                                "result size {result_bytes} exceeds {} bytes (service.attach not implemented)",
                                meta.max_result_bytes
                            ),
                        ),
                    )
                } else {
                    request.reply(KIND_HOST_SERVICE_RESULT, json!({ "result": result }))
                }
            }
            Err(error) => error_reply(request, &error),
        };
        self.note_settled(dedupe_key, &reply);
        Ok(reply)
    }

    fn method_meta(
        &self,
        service_key: &str,
        method_name: &str,
    ) -> Result<MethodMeta, ServiceError> {
        let service = self.services.get(service_key).ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                format!("host does not expose service `{service_key}`"),
            )
        })?;
        let table = service.table();
        let method = table.method(method_name).ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown method `{method_name}` on `{service_key}`"),
            )
        })?;
        if method.kind == HostServiceKind::Stream {
            return Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!(
                    "method `{service_key}.{method_name}` declares reserved kind `stream`; v2 does not support streaming services"
                ),
            ));
        }
        Ok(MethodMeta {
            name: method.name.clone(),
            timeout: Duration::from_millis(method.timeout_ms),
            idempotent: method.idempotent,
            requires_session: method.requires_session,
            args_schema: method.args_schema.clone(),
            result_schema: method.result_schema.clone(),
            max_inline_bytes: table.limits.max_inline_bytes,
            max_result_bytes: table.limits.max_result_bytes,
        })
    }

    fn note_settled(&mut self, key: (String, String), reply: &Envelope) {
        self.settled.insert(
            key,
            SettledReply {
                reply: reply.clone(),
                at: Instant::now(),
            },
        );
        if self.settled.len() > MAX_SETTLED {
            self.settled
                .retain(|_, settled| settled.at.elapsed() <= DEDUPE_WINDOW);
            if self.settled.len() > MAX_SETTLED {
                self.settled.pop_first();
            }
        }
    }
}

#[derive(Debug, Clone)]
struct MethodMeta {
    name: String,
    timeout: Duration,
    idempotent: bool,
    requires_session: bool,
    args_schema: Value,
    result_schema: Value,
    max_inline_bytes: u64,
    max_result_bytes: u64,
}

fn error_reply(request: &Envelope, error: &ServiceError) -> Envelope {
    let payload = match &error.detail {
        Some(detail) => json!({ "detail": detail }),
        None => json!({}),
    };
    let mut reply = request.reply(KIND_HOST_SERVICE_RESULT, payload);
    reply.error = Some(RpcError {
        code: error.code.clone(),
        message: error.message.clone(),
        retryable: error.retryable,
    });
    reply
}

#[cfg(test)]
mod tests {
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
        let mut router = HostServiceRouter::new("node-0001", negotiated_version, caps)
            .expect("router negotiates");
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
}
