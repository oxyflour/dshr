//! Sidecar 生命周期与 L1 调用路径的最小实现（`docs/spec/sidecar-rpc.md`）。
//!
//! 目的不是立刻做成生产级 Host，而是把**协议成本**测出来：
//! 进程启动 → 握手 → 注册 → invoke（含 chunk 流）→ 取消 → 崩溃。
//!
//! 传输固定为 NDJSON over stdio：规范允许抽象 transport，探针先用最便宜的一种。

#![forbid(unsafe_code)]
// 服务错误携带结构化 detail（code/message/retryable/可选 JSON），`Result<T, ServiceError>`
// 会在每个方法签名里放大 Err 尺寸。用 Box 包错误会改动公开 API 且不改善调用方（协议层本就
// 逐条分配 message/detail）；这是有意的取舍，用 crate 级 allow 固定，避免逐函数标注。
#![allow(clippy::result_large_err)]

mod callbacks;
mod invocation;
mod program_tools;
mod registrations;
mod transport;

use invocation::ActiveCall;
use transport::ChildHandle;
pub mod service;
pub mod services;
pub mod storage_kv;
pub mod tool_jobs;
pub mod tool_process;
pub mod tool_terminals;
pub use program_tools::ProgramToolScope;

// 集成点（chat.rs / sidecar 读循环）的常用入口，避免深层路径。
pub use service::{
    allow as allow_host_service, HostService, HostServiceRouter, ServiceCallContext, ServiceError,
};
pub use services::{
    EventPage, SessionAccess, SessionEventView, SessionHeaderView, SessionLogService,
    SessionProjectionsService, WorkingDirectoryService, SESSION_LOG_V1, SESSION_PROJECTIONS_V1,
    WORKING_DIRECTORY_CHANGED, WORKING_DIRECTORY_V1,
};
pub use storage_kv::{StorageKvService, STORAGE_KV_V1};
pub use tool_jobs::ToolJobService;
pub use tool_process::{ShellProgram, ToolProcessService};
pub use tool_terminals::ToolTerminalService;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dshr_compat_dsh_fs::FsWriteService;
use dshr_compat_dsh_protocol::{
    hello_with_caps, hello_with_caps_at, FsWriteResult, RpcError, CAP_FS_WRITE, CAP_TOOL_CANCEL,
    CAP_TOOL_STREAM, KIND_FS_WRITE_REQUEST, KIND_FS_WRITE_RESULT, KIND_HOST_SERVICE_CANCEL,
    KIND_HOST_SERVICE_REQUEST, KIND_HOST_SERVICE_RESULT, MAX_DEPTH, PROTOCOL_VERSION,
};
/// 集成方（chat.rs）构造 v2 Router 需要的协议常量；避免直接依赖 protocol crate。
pub use dshr_compat_dsh_protocol::{
    Envelope, HostServiceKind, HostServiceMethod, HostServiceTable, CAP_AGENT_TOOL_CONTEXT,
    CAP_BACKGROUND_JOBS, CAP_IMAGE_ATTACHMENTS, CAP_PROGRAM_TOOLS, CAP_SEARCH_PROCESS,
    CAP_SERVICE_PROXY, CAP_SHELL_PROCESS, CAP_TERMINAL_PROCESS, CAP_TOOL_GOALS, CAP_TOOL_SCHEDULES,
    CAP_TOOL_SESSION_QUERY, CAP_TOOL_SUBAGENTS, CAP_TOOL_TEAMS, CAP_USER_QUESTIONS,
    IMAGE_ATTACHMENTS_V1, PROTOCOL_MAX, TOOL_GOALS_V1, TOOL_JOBS_V1, TOOL_PROCESS_V1,
    TOOL_PROGRAM_V1, TOOL_SCHEDULES_V1, TOOL_SESSION_QUERY_V1, TOOL_SUBAGENTS_V1, TOOL_TEAMS_V1,
    TOOL_TERMINALS_V1, USER_QUESTIONS_V1,
};
use dshr_sidecar_guard::{GuardLimits, GuardedChild};
use dshr_tool_runtime::ToolOrigin;
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout waiting for {0}")]
    Timeout(String),
    /// 调用被宿主取消（已发 `tool.cancel`，宽限期内 sidecar 仍未给出终态）。
    #[error("cancelled waiting for {0}")]
    Cancelled(String),
    #[error("sidecar closed the pipe")]
    Closed,
    #[error("rpc error {code}: {message}")]
    Rpc { code: String, message: String },
}

/// 宿主启动选项：默认行为与 M2 之前完全一致（只读、无 fs.write）。
#[derive(Clone, Default)]
pub struct HostOptions {
    /// 默认 `tool.stream` / `tool.cancel` 之外的能力；`fs.write` 只能由
    /// [`HostOptions::with_fs_write`] 打开（手工塞进来会在协商时被剔除）。
    pub extra_caps: Vec<String>,
    /// 写入裁决服务：Some 时握手带上 `fs.write`，并自动给 sidecar 设置
    /// `DSHR_WORKSPACE`（其 `fs` 根）。
    pub fs_write: Option<Arc<FsWriteService>>,
    /// 注入 sidecar 子进程的环境变量（覆盖继承值）。
    pub env: Vec<(String, String)>,
    /// sidecar 子进程工作目录。
    pub current_dir: Option<PathBuf>,
    /// 注册收集窗口（缺省 2 s）；插件多/机器慢时可调大，自检用。
    pub registration_window: Option<Duration>,
    /// Node 可执行文件路径；缺省用 PATH 上的 `node`。
    pub program: Option<PathBuf>,
    /// 是否用 Windows Job Object 包裹 sidecar（资源上限 + 随宿主退出）。
    /// 默认关：测试/开发机行为不变；chat 启动时显式打开（见 docs/m2-runtime.md）。
    pub guard: bool,
    /// Job Object 限额（仅 `guard = true` 生效）。
    pub guard_limits: GuardLimits,
    /// v2 Host Service 路由（spec §13）：`None` = M2 行为（不发 `service.proxy`、
    /// 不服务 `host.service.*`）。Some 时握手用 `protocol_max = PROTOCOL_MAX`，
    /// 协商到 2 且 sidecar 回声 `service.proxy` 才启用。
    pub host_services: Option<Arc<Mutex<HostServiceRouter>>>,
}

impl HostOptions {
    /// 打开官方写工具：审批、沙箱、原子写都在 Rust 侧 [`FsWriteService`] 完成。
    pub fn with_fs_write(mut self, service: Arc<FsWriteService>) -> Self {
        self.fs_write = Some(service);
        self
    }

    /// 注入 v2 Host Service 路由（router 的 negotiated_version 应为 [`PROTOCOL_MAX`]；
    /// 实际是否生效由握手协商结果决定，v1 回退时 router 完全不被使用）。
    pub fn with_host_services(mut self, router: Arc<Mutex<HostServiceRouter>>) -> Self {
        self.host_services = Some(router);
        self
    }

    /// 指定 Node 可执行文件（例如 `runtime/node/node.exe`）。
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// 用 Job Object 包裹 sidecar：进程数/内存受限，Drop 时连子进程一起杀掉。
    pub fn with_guard(mut self, limits: GuardLimits) -> Self {
        self.guard = true;
        self.guard_limits = limits;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub fn with_registration_window(mut self, window: Duration) -> Self {
        self.registration_window = Some(window);
        self
    }

    /// 合并默认能力；`fs.write` 只跟随 `fs_write` 服务出现，`service.proxy` 只跟随
    /// `host_services` 路由出现——避免"协商了能力却没有裁决层"。
    pub fn effective_caps(&self) -> Vec<String> {
        let mut caps: Vec<String> = vec![CAP_TOOL_STREAM.to_owned(), CAP_TOOL_CANCEL.to_owned()];
        if self.host_services.is_some() {
            caps.push(CAP_SERVICE_PROXY.to_owned());
        }
        for cap in &self.extra_caps {
            if cap == CAP_FS_WRITE && self.fs_write.is_none() {
                continue;
            }
            if !caps.iter().any(|existing| existing == cap) {
                caps.push(cap.clone());
            }
        }
        if self.fs_write.is_some() && !caps.iter().any(|cap| cap == CAP_FS_WRITE) {
            caps.push(CAP_FS_WRITE.to_owned());
        }
        caps
    }
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub name: String,
    pub description: String,
    pub input_schema: Option<Value>,
    pub session_id: Option<String>,
    pub preset_revision: Option<String>,
    register_request_id: String,
    /// 对齐 DSH 的 `output.schema`；L1 暂不校验，但必须能穿过协议。
    pub output_schema: Option<serde_json::Value>,
    /// 对齐 DSH 的 `presentResult` 卡片类型。
    pub presentation: Option<String>,
    /// 对齐 DSH 工具声明的 `timeoutMs`（已归一化到 1s..=600s；未声明为 None）。
    pub timeout_ms: Option<u64>,
    pub registration_id: String,
    pub plugin_id: String,
}

impl Registration {
    /// 映射成注册表可登记的 `ToolDefinition`（携带 per-tool 超时）。
    pub fn to_tool_definition(&self, generation: &str) -> dshr_tool_runtime::ToolDefinition {
        let mut definition = dshr_tool_runtime::ToolDefinition::new(
            self.name.clone(),
            self.description.clone(),
            dshr_tool_runtime::ToolOrigin::Sidecar {
                plugin_id: self.plugin_id.clone(),
                registration_id: self.registration_id.clone(),
                generation: generation.to_owned(),
            },
        );
        if let Some(schema) = &self.input_schema {
            definition.input_schema = schema.clone();
        }
        if let Some(schema) = &self.output_schema {
            definition = definition.with_output_schema(schema.clone());
        }
        if let Some(card) = &self.presentation {
            definition = definition.with_presentation(card.clone());
        }
        if let Some(timeout_ms) = self.timeout_ms {
            definition = definition.with_timeout_ms(timeout_ms);
        }
        definition
    }
}

/// 从 `tool.register` 载荷构造 [`Registration`]（纯函数：不需要 sidecar 进程）。
///
/// `timeout_ms` 对齐 DSH 工具定义的 `timeoutMs`：sidecar 原样上报，这里归一化
/// 到 `MIN_TOOL_TIMEOUT_MS..=MAX_TOOL_TIMEOUT_MS`（与运行期 clamp 同口径）。
pub fn parse_tool_registration(
    payload: &serde_json::Value,
    plugin_id: &str,
    registration_id: impl Into<String>,
) -> Registration {
    Registration {
        name: payload["name"].as_str().unwrap_or_default().to_owned(),
        description: payload["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        input_schema: payload
            .get("input_schema")
            .filter(|value| value.is_object())
            .cloned(),
        session_id: None,
        preset_revision: None,
        register_request_id: String::new(),
        output_schema: payload
            .get("output_schema")
            .cloned()
            .filter(|value| !value.is_null()),
        presentation: payload
            .get("presentation")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        timeout_ms: dshr_compat_dsh_protocol::normalize_tool_timeout_ms(
            payload
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64),
        ),
        registration_id: registration_id.into(),
        plugin_id: plugin_id.to_owned(),
    }
}

#[derive(Debug)]
pub struct InvokeOutcome {
    pub chunks: Vec<String>,
    pub terminal: Envelope,
    pub elapsed: Duration,
}

/// `SidecarHost::invoke_batch` 的一次工具调用（M3 并行工具）。
///
/// `timeout` 是**本调用**的绝对截止时间（从写出 `tool.invoke` 起算），不共享批的超时。
#[derive(Debug, Clone)]
pub struct BatchInvokeCall {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub timeout: Duration,
    /// 会话身份（信封 `session_id`）；`None` = 兼容旧的 `s-probe` 占位。
    pub session_id: Option<String>,
    binding: Option<ToolOrigin>,
    approval_key: Option<String>,
    turn: Option<OpenToolTurn>,
    model_route: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy)]
pub struct OpenToolTurn {
    pub number: u64,
    pub start_seq: u64,
}

impl BatchInvokeCall {
    pub fn new(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        arguments: Value,
        timeout: Duration,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool: tool.into(),
            arguments,
            timeout,
            session_id: None,
            binding: None,
            approval_key: None,
            turn: None,
            model_route: None,
        }
    }

    pub fn with_binding(mut self, binding: ToolOrigin) -> Self {
        self.binding = Some(binding);
        self
    }

    pub fn with_approval_key(mut self, key: Option<String>) -> Self {
        self.approval_key = key;
        self
    }

    /// 显式透传会话 id（L2 服务方法按会话授权时必需）。
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_turn(mut self, turn: Option<OpenToolTurn>) -> Self {
        self.turn = turn;
        self
    }

    pub fn with_model_route(mut self, provider: &str, model: &str) -> Self {
        self.model_route = Some((provider.to_owned(), model.to_owned()));
        self
    }
}

pub struct SidecarHost {
    child: ChildHandle,
    stdin: ChildStdin,
    rx: Receiver<Envelope>,
    generation: String,
    script: PathBuf,
    plugin: PathBuf,
    seq: u64,
    registrations: Vec<Registration>,
    retired_sessions: HashSet<String>,
    agent_presets_supported: bool,
    agent_tool_context_supported: bool,
    preparing_step: Option<String>,
    preset_definitions: Option<Value>,
    preset_status: Value,
    preset_bindings: HashMap<String, (String, String)>,
    ready: Duration,
    host_caps: Vec<String>,
    fs_write: Option<Arc<FsWriteService>>,
    /// v2 Host Service 路由（None = M2 行为；Some 但协商 < 2 时也不会被使用）。
    host_services: Option<Arc<Mutex<HostServiceRouter>>>,
    /// 握手协商出的协议版本（v1 缺省）。
    negotiated_protocol: u32,
    /// 真正的服务开关：router 存在 && 协商到 2 && sidecar 回声 `service.proxy`。
    host_services_active: bool,
    /// 按 tool.invoke 的 request_id 追踪，模型给出的 call_id 可在其他调用中重复。
    active_calls: HashMap<String, ActiveCall>,
    program_tools: Option<ProgramToolScope>,
    /// sidecar 在启动/装载阶段上报的错误（`log.emit` level=error），供 UI 提示。
    boot_errors: Vec<String>,
}

impl SidecarHost {
    /// 本次握手协商出的 host_caps（诊断/自检用）。
    pub fn host_caps(&self) -> &[String] {
        &self.host_caps
    }

    pub fn fs_write_enabled(&self) -> bool {
        self.fs_write.is_some()
    }

    /// sidecar 启动/插件装载阶段上报的错误（最多保留 8 条），供 UI 提示。
    pub fn boot_errors(&self) -> &[String] {
        &self.boot_errors
    }

    pub fn ready_latency(&self) -> Duration {
        self.ready
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn has_exited(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_some())
    }

    /// 握手协商出的协议版本（v1 缺省；配置了 host_services 且 sidecar 支持时才可能为 2）。
    pub fn negotiated_protocol(&self) -> u32 {
        self.negotiated_protocol
    }

    /// v2 Host Service 是否真正启用（router + 协商到 2 + sidecar 回声 `service.proxy`）。
    pub fn host_services_active(&self) -> bool {
        self.host_services_active
    }

    pub fn host_services_router(&self) -> Option<&Arc<Mutex<HostServiceRouter>>> {
        self.host_services.as_ref()
    }

    /// 白名单方法表（`runtime.ready.host_services` / `capabilities.list` 的形状）；
    /// 未启用 v2 时返回 `None`。
    pub fn host_services_payload(&self) -> Option<Value> {
        if !self.host_services_active {
            return None;
        }
        self.host_services
            .as_ref()
            .and_then(|router| router.lock().ok())
            .map(|router| router.host_services_payload())
    }

    pub fn script(&self) -> &Path {
        &self.script
    }

    pub fn plugin(&self) -> &Path {
        &self.plugin
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use dshr_tool_runtime::ApprovalBroker;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dshr-host-caps-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn default_caps_stay_read_only() {
        let caps = HostOptions::default().effective_caps();
        assert_eq!(caps, vec![CAP_TOOL_STREAM, CAP_TOOL_CANCEL]);
        assert!(!caps.iter().any(|cap| cap == CAP_FS_WRITE));
    }

    #[test]
    fn fs_write_cap_requires_the_write_service() {
        // 手工塞 fs.write 但没有裁决层：协商时必须剔除，避免"能力开了但没人裁决"。
        let smuggled = HostOptions {
            extra_caps: vec![CAP_FS_WRITE.to_owned()],
            ..HostOptions::default()
        };
        assert!(!smuggled
            .effective_caps()
            .iter()
            .any(|cap| cap == CAP_FS_WRITE));

        let root = temp_root("caps");
        let service =
            Arc::new(FsWriteService::open(&root, ApprovalBroker::new()).expect("write service"));
        let caps = HostOptions::default()
            .with_fs_write(service)
            .effective_caps();
        assert_eq!(
            caps.iter().filter(|cap| *cap == CAP_FS_WRITE).count(),
            1,
            "fs.write 只出现一次: {caps:?}"
        );
        assert!(caps.iter().any(|cap| cap == CAP_TOOL_STREAM));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn env_override_is_recorded_for_the_child_process() {
        let options = HostOptions::default()
            .with_env("DSHR_DSH_PLUGINS", "[]")
            .with_current_dir(".")
            .with_registration_window(Duration::from_millis(1500));
        assert_eq!(options.env.len(), 1);
        assert_eq!(options.env[0].0, "DSHR_DSH_PLUGINS");
        assert_eq!(
            options.registration_window,
            Some(Duration::from_millis(1500))
        );
    }

    // M3-DEFAULT-011..013：`tool.register` 的 per-tool timeout 往返。
    #[test]
    fn m3_default_011_registration_carries_declared_timeout() {
        let payload = json!({
            "name": "bash",
            "description": "run a command",
            "output_schema": {"type": "object"},
            "presentation": "terminal",
            "timeout_ms": 5_000,
        });
        let registration = parse_tool_registration(&payload, "dsh-official", "reg-1");
        assert_eq!(registration.name, "bash");
        assert_eq!(registration.timeout_ms, Some(5_000));
        assert_eq!(registration.presentation.as_deref(), Some("terminal"));

        // 与 ToolDefinition 的往返（chat 注册进 Registry 的就是它）。
        let definition = registration.to_tool_definition("gen-1");
        assert_eq!(definition.name, "bash");
        assert_eq!(definition.timeout_ms, 5_000);
        assert_eq!(definition.effective_timeout_ms(), 5_000);
        assert_eq!(definition.card(), dshr_tool_runtime::Presentation::Terminal);
        assert_eq!(definition.origin.generation(), Some("gen-1"));
    }

    #[test]
    fn m3_default_012_absent_timeout_keeps_tool_default() {
        let registration = parse_tool_registration(&json!({"name": "read"}), "p", "reg-2");
        assert_eq!(registration.timeout_ms, None);
        let definition = registration.to_tool_definition("gen-2");
        assert_eq!(
            definition.timeout_ms,
            dshr_tool_runtime::DEFAULT_TOOL_TIMEOUT_MS
        );
    }

    #[test]
    fn m3_default_013_timeout_is_normalized_on_the_way_in() {
        // 0 / 非法值 = 未声明；过小/过大按运行期口径 clamp（1s..=600s）。
        for raw in [json!(0), json!(-5), json!("3000"), json!(null)] {
            let registration =
                parse_tool_registration(&json!({"name": "t", "timeout_ms": raw}), "p", "reg-3");
            assert_eq!(registration.timeout_ms, None, "raw={raw}");
        }
        let small = parse_tool_registration(&json!({"name": "t", "timeout_ms": 10}), "p", "reg-4");
        assert_eq!(small.timeout_ms, Some(1_000));
        let huge = parse_tool_registration(
            &json!({"name": "t", "timeout_ms": 5_000_000u64}),
            "p",
            "reg-5",
        );
        assert_eq!(huge.timeout_ms, Some(600_000));
    }
}
