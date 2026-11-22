//! Sidecar RPC 信封（`docs/spec/sidecar-rpc.md` §4）的可执行实现。
//!
//! 只做三件事：构造、序列化、反序列化。传输（NDJSON）与生命周期在 `compat-dsh-host`。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// v1 线格式版本：`Envelope::host`/`hello*` 的缺省 `version`。
///
/// 为什么不是 2（对齐 spec §13.10 的"`PROTOCOL_VERSION = 2`"建议但本轮不改）：
/// `Envelope::host` 在**协商前**就要能构造消息，而 `compat-dsh-host` 的握手
/// （`SidecarHost::from_parts`）当前只等 `runtime.ready`、**不读** `ready.protocol`，
/// 所有请求信封都写这个常量。把它改成 2 会让 v1 缺省信封变成 `version:2`，
/// 与"`envelope.version` = 协商结果"（spec §3.2 规则 2）冲突，并让既有 v1 测试
/// 与未升级的 v1 sidecar 语义漂移。v2 调用方改用 [`Envelope::with_version`]
/// 显式写 2；`PROTOCOL_MAX` 声明本 crate 实现到的最高版本，供握手集成轮做协商。
pub const PROTOCOL_VERSION: u32 = 1;
/// 本 crate 实现的最高协议版本（spec §3.2：双方取 `min(protocol_max)`）。
pub const PROTOCOL_MAX: u32 = 2;
/// 本 crate 实现的最低协议版本（没有交集即拒绝）。
pub const PROTOCOL_MIN: u32 = 1;
pub const MAX_DEPTH: u32 = 4;

// ---------------------------------------------------------------------------
// 工具声明（`tool.register`）的跨语言字段
// ---------------------------------------------------------------------------
//
// DSH 的 `ToolDefinition` 允许声明 `timeoutMs`（每个工具自己的超时，见
// `@deepseek-ai/dsh-tools` 的 `definition.timeoutMs`；未声明则不限）。dshr 早先把
// sidecar 工具统一写死 5s，慢工具会被误杀；现在由 sidecar 透传、宿主归一化，
// 运行期再按 `ToolDefinition.timeout_ms` 执行（`chat::sidecar_tool_timeout`）。

/// 工具声明超时的缺省值（对齐 `tool-runtime::DEFAULT_TOOL_TIMEOUT_MS`）。
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;
/// `tool.register.timeout_ms` 的合法下界（太小会误伤正常工具）。
pub const MIN_TOOL_TIMEOUT_MS: u64 = 1_000;
/// 合法上界（与 `chat` 的运行期 clamp 一致，避免界面像卡死）。
pub const MAX_TOOL_TIMEOUT_MS: u64 = 600_000;

/// 归一化 sidecar 声明的 `timeout_ms`：非正数视为"未声明"；其余 clamp 到
/// [`MIN_TOOL_TIMEOUT_MS`]..=[`MAX_TOOL_TIMEOUT_MS`]，与运行期 clamp 同口径。
pub fn normalize_tool_timeout_ms(raw: Option<u64>) -> Option<u64> {
    raw.filter(|millis| *millis > 0)
        .map(|millis| millis.clamp(MIN_TOOL_TIMEOUT_MS, MAX_TOOL_TIMEOUT_MS))
}

/// 能力协商（`docs/spec/sidecar-rpc.md` §3）：host → sidecar 的 `host_caps` 词表。
///
/// `fs.write` 是 M2 写工具的门闩：sidecar 未收到它时必须保持只读行为，
/// 因此旧宿主（只发 tool.stream/tool.cancel）与 `--selftest-dsh` 完全不受影响。
pub const CAP_TOOL_STREAM: &str = "tool.stream";
pub const CAP_TOOL_CANCEL: &str = "tool.cancel";
pub const CAP_FS_WRITE: &str = "fs.write";
pub const CAP_IMAGE_ATTACHMENTS: &str = "attachments.image";
pub const IMAGE_ATTACHMENTS_V1: &str = "image-attachments/v1";
pub const CAP_SEARCH_PROCESS: &str = "process.search";
pub const CAP_SHELL_PROCESS: &str = "process.shell";
pub const TOOL_PROCESS_V1: &str = "tool-process/v1";
pub const CAP_USER_QUESTIONS: &str = "user.questions";
pub const USER_QUESTIONS_V1: &str = "user-questions/v1";
pub const CAP_AGENT_TOOL_CONTEXT: &str = "agent.tool-context";
pub const CAP_BACKGROUND_JOBS: &str = "process.jobs";
pub const TOOL_JOBS_V1: &str = "tool-jobs/v1";
pub const CAP_TERMINAL_PROCESS: &str = "process.terminal";
pub const TOOL_TERMINALS_V1: &str = "tool-terminals/v1";
pub const CAP_TOOL_GOALS: &str = "agent.goals";
pub const TOOL_GOALS_V1: &str = "tool-goals/v1";
pub const CAP_TOOL_SESSION_QUERY: &str = "agent.session-query";
pub const TOOL_SESSION_QUERY_V1: &str = "tool-session-query/v1";
pub const CAP_TOOL_SUBAGENTS: &str = "agent.subagents";
pub const TOOL_SUBAGENTS_V1: &str = "tool-subagents/v1";
pub const CAP_TOOL_TEAMS: &str = "agent.teams";
pub const TOOL_TEAMS_V1: &str = "tool-teams/v1";
pub const CAP_TOOL_SCHEDULES: &str = "agent.schedules";
pub const TOOL_SCHEDULES_V1: &str = "tool-schedules/v1";
pub const CAP_PROGRAM_TOOLS: &str = "agent.program-tools";
pub const TOOL_PROGRAM_V1: &str = "tool-program/v1";
/// v2：宿主实现了白名单内的 Host Service，并接受 `host.service.request`（spec §13.4）。
pub const CAP_SERVICE_PROXY: &str = "service.proxy";
/// v2：允许 Sidecar 用 `service.publish` 暴露 JS 服务（spec §13.3）；v2 默认关闭。
pub const CAP_SERVICE_EXPOSE: &str = "service.expose";
/// v2：宿主支持服务结果的附件引用（spec §13.8）。
pub const CAP_SERVICE_ATTACH: &str = "service.attach";

/// Sidecar → Host 的写入回调（`docs/spec/sidecar-rpc.md` §7.5）。
pub const KIND_FS_WRITE_REQUEST: &str = "fs.write.request";
/// Host → Sidecar 的写入答复。
pub const KIND_FS_WRITE_RESULT: &str = "fs.write.result";

/// 宿主 → Sidecar 的取消（v2：JS 服务代理路径；spec §6/§13.6）。
pub const KIND_SERVICE_CANCEL: &str = "service.cancel";
/// Sidecar → Host：JS 插件消费宿主服务（v2，spec §13.4）。
pub const KIND_HOST_SERVICE_REQUEST: &str = "host.service.request";
/// Host → Sidecar：宿主服务的答复（恰好一次）。
pub const KIND_HOST_SERVICE_RESULT: &str = "host.service.result";
/// Sidecar → Host：插件取消一条在途的宿主服务调用。
pub const KIND_HOST_SERVICE_CANCEL: &str = "host.service.cancel";

/// v2 新增的 4 个服务代理 kind（spec §13.10 的"只新增 4 个 kind"）。
pub const V2_ONLY_KINDS: &[&str] = &[
    KIND_SERVICE_CANCEL,
    KIND_HOST_SERVICE_REQUEST,
    KIND_HOST_SERVICE_RESULT,
    KIND_HOST_SERVICE_CANCEL,
];

/// 全部服务代理 kind（含 v1 正文已声明、v2 钉死方向的 `service.*`）。
///
/// 这些 kind 在协商为 1 时一律不可用（spec §13.10：v1 协商时发了也是 `E_PROTOCOL`），
/// 在协商为 2 时必须带信封 `service_key`（spec §4）。
pub const SERVICE_PROXY_KINDS: &[&str] = &[
    "service.publish",
    "service.revoke",
    "service.invoke",
    "service.result",
    KIND_SERVICE_CANCEL,
    KIND_HOST_SERVICE_REQUEST,
    KIND_HOST_SERVICE_RESULT,
    KIND_HOST_SERVICE_CANCEL,
];

/// 是否 v2-only kind（spec §3.2 规则 3）。
pub fn is_v2_only_kind(kind: &str) -> bool {
    V2_ONLY_KINDS.contains(&kind)
}

/// 是否服务代理 kind（含 v1 表中已声明但 v2 才落地的 `service.*`）。
pub fn is_service_proxy_kind(kind: &str) -> bool {
    SERVICE_PROXY_KINDS.contains(&kind)
}

/// 写操作的原子语义：`write` 是 create-or-replace（内容即新文件全文），
/// `delete` 删除现有文件（写前快照仍在 Rust 侧完成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FsWriteOperation {
    #[default]
    Write,
    Delete,
}

impl FsWriteOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

/// Sidecar 发起、Rust 裁决的一次写请求。
///
/// `request_id`/`call_id` 由信封承载：`request_id` 关联 `fs.write.result`，
/// `call_id` 关联发起这次写入的工具调用（审批审计用同一身份）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteRequest {
    /// 发起写入的工具调用 ID（`tool.invoke` 的 `call_id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// 工具名（`write` / `edit` / `apply_patch`），用于审批展示。
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub operation: FsWriteOperation,
    /// 目标路径：工作区相对路径，或工作区内绝对路径（Rust 负责归一化/边界校验）。
    pub path: String,
    /// sidecar 认为的工作区根；与宿主不一致时直接拒绝（防混淆代理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// `write` 必填：新文件全文；`delete` 必须缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 可选版本守卫（`mtime_ms:size`）：不匹配即 `FS_STALE_VERSION`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    /// 官方 DSH `fs/write-intent` 水位（当前只透传审计，不参与裁决）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Value>,
    /// 官方沙箱模式（当前为 `workspace-write`/`read-only` 的说明性字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl FsWriteRequest {
    pub fn summary(&self) -> Value {
        json!({
            "path": self.path,
            "operation": self.operation.as_str(),
            "bytes": self.content.as_ref().map(|content| content.len()).unwrap_or(0),
            "tool": self.tool,
        })
    }
}

/// 写请求的结构化错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsWriteError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl FsWriteError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

/// Rust 对一次写请求的答复（`fs.write.result.payload`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FsWriteResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// 写入后的版本（`mtime_ms:size`）；`delete` 无版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// 写前快照 ID（覆盖/删除已有文件时产生）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FsWriteError>,
}

impl FsWriteResult {
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(FsWriteError::new(code, message)),
            ..Self::default()
        }
    }

    pub fn is_ok(&self) -> bool {
        self.ok
    }
}

/// 版本协商（spec §3.2）：取 `min(host_max, sidecar_max)`；结果必须落在本 crate
/// 实现区间 `[PROTOCOL_MIN, PROTOCOL_MAX]` 内，否则没有交集、调用方必须拒绝
/// （不得静默降级）。
pub fn negotiate_version(host_max: u32, sidecar_max: u32) -> Option<u32> {
    let negotiated = host_max.min(sidecar_max);
    (PROTOCOL_MIN..=PROTOCOL_MAX)
        .contains(&negotiated)
        .then_some(negotiated)
}

/// 带完整 host_caps 的握手（客户端能力协商）。
pub fn hello_with_caps(caps: &[String]) -> Envelope {
    hello_with_caps_at(caps, PROTOCOL_VERSION)
}

/// 指定 `protocol_max` 的握手；集成轮协商到 v2 时传 [`PROTOCOL_MAX`]。
pub fn hello_with_caps_at(caps: &[String], protocol_max: u32) -> Envelope {
    Envelope::host(
        "runtime.hello",
        "req-hello",
        json!({
            "protocol_max": protocol_max,
            "dsh_version": "0.2.2-rc.1",
            "host_caps": caps,
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub kind: String,
    pub request_id: String,
    pub runtime_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// v2 服务代理消息的路由锚点（spec §4/§5.5）：`<name>/v<major>`。
    /// v1 消息一律缺省；`serde(default)` 保证 v1 实现对新增字段保持宽松。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_key: Option<String>,
    pub depth: u32,
    pub trace_id: String,
    pub ts: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Envelope {
    /// 宿主侧发起：`runtime_generation` 在握手前用占位值，握手后由 host 覆写。
    pub fn host(kind: impl Into<String>, request_id: impl Into<String>, payload: Value) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind: kind.into(),
            request_id: request_id.into(),
            runtime_generation: "node-pending".to_owned(),
            plugin_id: None,
            registration_id: None,
            session_id: None,
            call_id: None,
            service_key: None,
            depth: 0,
            trace_id: "t-local".to_owned(),
            ts: "1970-01-01T00:00:00Z".to_owned(),
            payload,
            error: None,
        }
    }

    /// 显式写信封 `version`（spec §3.2 规则 2：它等于协商结果）。
    ///
    /// v2 服务代理消息在握手协商到 2 后必须用 2；本 crate 不擅自把缺省值改成 2
    /// （见 [`PROTOCOL_VERSION`] 的说明）。
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    pub fn with_service_key(mut self, service_key: impl Into<String>) -> Self {
        self.service_key = Some(service_key.into());
        self
    }

    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    pub fn with_generation(mut self, generation: impl Into<String>) -> Self {
        self.runtime_generation = generation.into();
        self
    }

    pub fn with_plugin(mut self, plugin_id: impl Into<String>) -> Self {
        self.plugin_id = Some(plugin_id.into());
        self
    }

    pub fn with_call(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// 结束一次请求：复制关联字段，替换 kind/payload。
    ///
    /// `version` 复制请求的协商版本（不是常量），`service_key` 也回填
    /// （spec §13.4 规则 2：答复 MUST 回填同一 `request_id` 与 `service_key`）。
    pub fn reply(&self, kind: impl Into<String>, payload: Value) -> Self {
        Self {
            version: self.version,
            kind: kind.into(),
            request_id: self.request_id.clone(),
            runtime_generation: self.runtime_generation.clone(),
            plugin_id: self.plugin_id.clone(),
            registration_id: self.registration_id.clone(),
            session_id: self.session_id.clone(),
            call_id: self.call_id.clone(),
            service_key: self.service_key.clone(),
            depth: self.depth,
            trace_id: self.trace_id.clone(),
            ts: self.ts.clone(),
            payload,
            error: None,
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// 规范 §9 的错误码闭集（v2 按 §9.3 补入 `E_*_SERVICE_*` 与 `E_ALREADY_SETTLED`）。
pub const ERROR_CODES: &[&str] = &[
    "E_PROTOCOL",
    "E_VERSION",
    "E_UNKNOWN_PLUGIN",
    "E_DUPLICATE_REG",
    "E_TOOL_UNAVAILABLE",
    "E_PRESET",
    "E_SCHEMA",
    "E_TIMEOUT",
    "E_CANCELLED",
    "E_RPC_DEPTH",
    "E_TOO_LARGE",
    "E_BACKPRESSURE",
    "E_INTERNAL",
    "E_ALREADY_SETTLED",
    "E_SERVICE_UNAVAILABLE",
    "E_SERVICE_METHOD",
    "E_SERVICE_ARGS",
    "E_SERVICE_TIMEOUT",
    "E_SERVICE_DENIED",
    "E_SERVICE_CONFLICT",
];

/// 可安全重试的 sidecar 错误码（规范 §9.2）。
///
/// 注意：这里只回答"错误本身是否瞬时可重试"；是否真的重试还要看工具是否幂等。
/// 写工具（write/edit/apply_patch）**不适合自动重试**——重试会重复审批/重复写，
/// 应当把结构化错误交给模型，让它重读现场后再决定。
pub fn is_retryable_sidecar_error(code: &str) -> bool {
    matches!(
        code,
        "E_TIMEOUT" | "E_BACKPRESSURE" | "E_INTERNAL" | "E_SERVICE_TIMEOUT"
    )
}

/// 服务方法错误的重试建议（spec §13.4 规则 4）：只有 `E_SERVICE_TIMEOUT`、
/// `E_BACKPRESSURE`、`E_INTERNAL` 属于瞬时错误，且仅对 `idempotent:true` 的方法
/// 允许自动重试（重试必须换新 `request_id`）。
pub fn is_retryable_service_error(code: &str) -> bool {
    matches!(code, "E_SERVICE_TIMEOUT" | "E_BACKPRESSURE" | "E_INTERNAL")
}

/// `service_key` 形态：`<name>/v<major>`（spec §13.2），`name` 为 kebab-case
/// `[a-z][a-z0-9-]*`，`major` 为 ≥ 1 的整数。与 `schema/sidecar-rpc/envelope.schema.json`
/// 的 `^[a-z][a-z0-9-]*/v[1-9][0-9]*$` 一致。
pub fn validate_service_key(key: &str) -> Result<(), String> {
    let Some((name, version)) = key.rsplit_once('/') else {
        return Err(format!("service_key `{key}` must be `<name>/v<major>`"));
    };
    if !is_kebab_name(name) {
        return Err(format!(
            "service_key `{key}` has invalid name; expected `[a-z][a-z0-9-]*`"
        ));
    }
    let Some(major) = version.strip_prefix('v') else {
        return Err(format!("service_key `{key}` must end with `/v<major>`"));
    };
    // `v0` 与 `v01` 都不合法：major 必须 ≥ 1，且是 `[1-9][0-9]*`。
    let canonical = major.parse::<u32>().ok().filter(|value| *value >= 1);
    match canonical {
        Some(value) if value.to_string() == major => Ok(()),
        _ => Err(format!(
            "service_key `{key}` has invalid major; expected `v1`, `v2`, ..."
        )),
    }
}

fn is_kebab_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// 方法名的词表：spec §13.2 写的是 `[a-z][a-z0-9_]*`，但首批三个服务的方法用的是
/// 点分 + camelCase 命名（`unit.loadAll`/`session.describe`/`events.read`，见
/// `docs/m3-l2-services.md` §3）。实现按**更宽的超集** `[a-z][A-Za-z0-9_.]*` 放行，
/// 由 Lead 在 spec 里对齐（§13.2 的 regex 是本节与 m3 服务表的冲突点）。
pub fn validate_method_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return Err(format!(
            "method name `{name}` must match `[a-z][A-Za-z0-9_.]*`"
        ));
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
    {
        return Err(format!(
            "method name `{name}` must match `[a-z][A-Za-z0-9_.]*`"
        ));
    }
    Ok(())
}

/// `request_id` 形态（spec §13.4 规则 2）。
pub fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 64 {
        return Err("request_id must be 1..=64 chars".to_owned());
    }
    if !request_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
    {
        return Err("request_id must match `[A-Za-z0-9:_-]{1,64}`".to_owned());
    }
    Ok(())
}

/// 方法 `timeout_ms` 的缺省值与合法区间（spec §13.2）。
pub const DEFAULT_METHOD_TIMEOUT_MS: u64 = 30_000;
pub const MIN_METHOD_TIMEOUT_MS: u64 = 100;
pub const MAX_METHOD_TIMEOUT_MS: u64 = 120_000;
/// `args_schema` 内联上限（spec §13.2）。
pub const MAX_METHOD_SCHEMA_BYTES: usize = 8 * 1024;

fn default_method_timeout_ms() -> u64 {
    DEFAULT_METHOD_TIMEOUT_MS
}

/// 方法 `kind`（spec §13.2）：v2 只允许 `read`/`write`；`stream` 是保留值，
/// 声明可以穿过方法表，但调用时必须回 `E_SERVICE_METHOD`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostServiceKind {
    Read,
    Write,
    Stream,
}

/// 宿主侧方法表的一项（spec §13.2 `ServiceDescriptor.methods[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostServiceMethod {
    pub name: String,
    pub kind: HostServiceKind,
    pub args_schema: Value,
    pub result_schema: Value,
    #[serde(default = "default_method_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub idempotent: bool,
    /// 实现扩展（spec §5.5/§13.9 的会话授权判定需要方法级声明）：
    /// 为 `true` 时信封必须带 `session_id`，否则 `E_SERVICE_ARGS`。
    /// 不在 §13.2 的方法表字段里，`serde(default)` 让 v2 方法表保持宽松。
    #[serde(default)]
    pub requires_session: bool,
}

/// 载荷上限（spec §13.2 `limits` / §13.8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostServiceLimits {
    #[serde(default = "default_max_inline_bytes")]
    pub max_inline_bytes: u64,
    #[serde(default = "default_max_result_bytes")]
    pub max_result_bytes: u64,
}

pub const DEFAULT_MAX_INLINE_BYTES: u64 = 262_144;
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 786_432;

fn default_max_inline_bytes() -> u64 {
    DEFAULT_MAX_INLINE_BYTES
}

fn default_max_result_bytes() -> u64 {
    DEFAULT_MAX_RESULT_BYTES
}

impl Default for HostServiceLimits {
    fn default() -> Self {
        Self {
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }
}

/// 宿主愿意暴露给 Sidecar 的服务方法表（spec §13.2）。
///
/// `runtime.ready.payload.host_services` 与 `capabilities.list` 都必须回填这份表
/// （白名单子集，不是全量）；`compat-dsh-host` 的 `HostServiceRouter::host_services`
/// 提供它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostServiceTable {
    pub service_key: String,
    pub methods: Vec<HostServiceMethod>,
    #[serde(default)]
    pub limits: HostServiceLimits,
}

impl HostServiceTable {
    pub fn new(service_key: impl Into<String>, methods: Vec<HostServiceMethod>) -> Self {
        Self {
            service_key: service_key.into(),
            methods,
            limits: HostServiceLimits::default(),
        }
    }

    pub fn method(&self, name: &str) -> Option<&HostServiceMethod> {
        self.methods.iter().find(|method| method.name == name)
    }

    /// 描述表校验（spec §13.2）：失败是宿主配置/实现错误，路由层映射为 `E_SCHEMA`。
    pub fn validate(&self) -> Result<(), String> {
        validate_service_key(&self.service_key)
            .map_err(|reason| format!("service_key: {reason}"))?;
        if self.methods.is_empty() {
            return Err("methods must not be empty".to_owned());
        }
        for (index, method) in self.methods.iter().enumerate() {
            method
                .validate()
                .map_err(|reason| format!("methods[{index}]: {reason}"))?;
            if self.methods[..index]
                .iter()
                .any(|existing| existing.name == method.name)
            {
                return Err(format!(
                    "methods[{index}]: duplicate name `{}`",
                    method.name
                ));
            }
        }
        Ok(())
    }
}

impl HostServiceMethod {
    /// 单项校验（spec §13.2 字段规则）。
    pub fn validate(&self) -> Result<(), String> {
        validate_method_name(&self.name)?;
        if !(MIN_METHOD_TIMEOUT_MS..=MAX_METHOD_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!(
                "timeout_ms {} must be within {MIN_METHOD_TIMEOUT_MS}..={MAX_METHOD_TIMEOUT_MS}",
                self.timeout_ms
            ));
        }
        validate_method_schema("args_schema", &self.args_schema)?;
        validate_method_schema("result_schema", &self.result_schema)?;
        Ok(())
    }
}

fn validate_method_schema(field: &str, schema: &Value) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Err(format!("{field} must be a JSON object"));
    };
    if object.get("type").and_then(Value::as_str) != Some("object") {
        return Err(format!("{field}.type must be \"object\""));
    }
    if object.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err(format!("{field}.additionalProperties must be false"));
    }
    let bytes = serde_json::to_vec(schema)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if bytes > MAX_METHOD_SCHEMA_BYTES {
        return Err(format!(
            "{field} inlined size {bytes} exceeds {MAX_METHOD_SCHEMA_BYTES} bytes"
        ));
    }
    Ok(())
}

/// `host.service.request` 的 payload（spec §13.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCall {
    pub method: String,
    #[serde(default)]
    pub args: Value,
}

/// `host.service.result` / `service.result` 的 payload（spec §13.8）。
///
/// `result` 与 `content_ref` 互斥；实现自有的错误词表放 `detail`（§9.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ServiceReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl ServiceReply {
    pub fn ok(result: Value) -> Self {
        Self {
            result: Some(result),
            content_ref: None,
            detail: None,
        }
    }

    pub fn content_ref(content_ref: impl Into<String>) -> Self {
        Self {
            result: None,
            content_ref: Some(content_ref.into()),
            detail: None,
        }
    }

    /// `result`/`content_ref` 恰好一个（spec §13.8）。
    pub fn validate(&self) -> Result<(), String> {
        if self.result.is_some() && self.content_ref.is_some() {
            return Err("service reply must not carry both result and content_ref".to_owned());
        }
        if self.result.is_none() && self.content_ref.is_none() {
            return Err("service reply must carry result or content_ref".to_owned());
        }
        Ok(())
    }
}

/// 信封 kind 闭集（与 `schema/sidecar-rpc/envelope.schema.json` 的枚举一致）。
pub const KNOWN_KINDS: &[&str] = &[
    "runtime.hello",
    "runtime.ready",
    "runtime.reject",
    "runtime.crashed",
    "plugin.load",
    "plugin.loaded",
    "plugin.failed",
    "plugin.unload",
    "plugin.unloaded",
    "tool.register",
    "tool.unregister",
    "preset.sync",
    "preset.bind",
    "preset.release",
    "agent.step",
    "tool.invoke",
    "tool.chunk",
    "tool.result",
    "tool.cancel",
    "tool.aborted",
    KIND_FS_WRITE_REQUEST,
    KIND_FS_WRITE_RESULT,
    "service.publish",
    "service.revoke",
    "service.invoke",
    "service.result",
    KIND_SERVICE_CANCEL,
    KIND_HOST_SERVICE_REQUEST,
    KIND_HOST_SERVICE_RESULT,
    KIND_HOST_SERVICE_CANCEL,
    "ui.contribute",
    "ui.retract",
    "event.subscribe",
    "event.unsubscribe",
    "event.emit",
    "health.ping",
    "health.pong",
    "log.emit",
    "capabilities.list",
    "diagnostics.snapshot",
];

/// 信封校验失败（spec §3.2/§4/§9）：`code` 是 §9 主表里的错误码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeViolation {
    pub code: &'static str,
    pub message: String,
}

impl EnvelopeViolation {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 按协商结果校验一个信封（spec §3.2 规则 2/3、§4、§5.5）。
///
/// 这是**协议规则**的运行时入口，不是 serde 严格模式：v1 消息里出现未知字段仍
/// 宽松接受（向前兼容），但未知 kind、v2-only kind 出现在 v1 协商里、服务代理消息
/// 缺 `service_key`、`depth > 4` 都会被拒绝。
pub fn validate_envelope(
    envelope: &Envelope,
    negotiated_version: u32,
) -> Result<(), EnvelopeViolation> {
    if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&negotiated_version) {
        return Err(EnvelopeViolation::new(
            "E_VERSION",
            format!("unsupported negotiated protocol version {negotiated_version}"),
        ));
    }
    if envelope.version != negotiated_version {
        return Err(EnvelopeViolation::new(
            "E_PROTOCOL",
            format!(
                "envelope.version {} must equal negotiated protocol version {negotiated_version}",
                envelope.version
            ),
        ));
    }
    if !KNOWN_KINDS.contains(&envelope.kind.as_str()) {
        return Err(EnvelopeViolation::new(
            "E_PROTOCOL",
            format!("unknown kind `{}`", envelope.kind),
        ));
    }
    if negotiated_version < 2 && is_service_proxy_kind(&envelope.kind) {
        return Err(EnvelopeViolation::new(
            "E_PROTOCOL",
            format!(
                "kind `{}` is v2-only but the negotiated protocol version is {negotiated_version}",
                envelope.kind
            ),
        ));
    }
    if envelope.depth > MAX_DEPTH {
        return Err(EnvelopeViolation::new(
            "E_RPC_DEPTH",
            format!("depth {} exceeds {MAX_DEPTH}", envelope.depth),
        ));
    }
    validate_request_id(&envelope.request_id)
        .map_err(|reason| EnvelopeViolation::new("E_PROTOCOL", reason))?;
    match &envelope.service_key {
        Some(service_key) => {
            if negotiated_version < 2 {
                return Err(EnvelopeViolation::new(
                    "E_PROTOCOL",
                    "service_key is v2-only; v1 messages must not carry it",
                ));
            }
            validate_service_key(service_key)
                .map_err(|reason| EnvelopeViolation::new("E_PROTOCOL", reason))?;
        }
        None => {
            if is_service_proxy_kind(&envelope.kind) {
                return Err(EnvelopeViolation::new(
                    "E_PROTOCOL",
                    format!("kind `{}` requires the envelope service_key", envelope.kind),
                ));
            }
        }
    }
    Ok(())
}

pub fn hello(caps: &[&str]) -> Envelope {
    Envelope::host(
        "runtime.hello",
        "req-hello",
        json!({
            "protocol_max": PROTOCOL_VERSION,
            "dsh_version": "0.2.1-rc.3",
            "host_caps": caps,
        }),
    )
}

#[cfg(test)]
mod tests;
