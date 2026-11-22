//! Sidecar 写请求的 Rust 裁决（`docs/spec/sidecar-rpc.md` §7.5、`docs/m2-official-write.md`）。
//!
//! 职责边界：
//! - **Sidecar 只发意图**：`fs.write.request` 携带路径/内容/期望版本；它不直接落盘。
//! - **Rust 是唯一权威**：路径归一化 → 工作区边界（`dshr-sandbox::Workspace`）→
//!   `ApprovalBroker` 审批（写前快照）→ 原子写 → 返回版本（`mtime_ms:size`）。
//! - 拒绝、超时、越界都返回**结构化错误**（官方 `FS_*` 错误码词表），不静默失败。
//!
//! 这是 M2 "官方 write/edit/apply_patch 在审批 + 沙箱下可用" 的裁决层；
//! Node 侧 `fs` 服务只是把官方 dsh-tool-fs 的调用翻译成这里的请求。

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use dshr_compat_dsh_protocol::{FsWriteOperation, FsWriteRequest, FsWriteResult};
use dshr_sandbox::{SandboxError, Workspace};
use dshr_tool_runtime::{ApprovalBroker, ApprovalOutcome, LiveApproval, PolicyDecision};
use serde_json::{json, Value};

/// 审批等待上限：必须小于宿主侧工具调用 timeout（默认 120 s），
/// 否则等待循环会先超时，写入答复无人消费。
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

// 错误码采纳官方 `@deepseek-ai/dsh-fs` 的 `FS_*` 词表，node 侧可直接映射成 FsError。
const E_SANDBOX_DENIED: &str = "FS_SANDBOX_DENIED";
const E_STALE_VERSION: &str = "FS_STALE_VERSION";
const E_TOO_LARGE: &str = "FS_TOO_LARGE";
const E_NOT_FOUND: &str = "FS_NOT_FOUND";
const E_NOT_OBSERVED: &str = "FS_NOT_OBSERVED";
const E_NOT_REGULAR_FILE: &str = "FS_NOT_REGULAR_FILE";
const E_PERMISSION_DENIED: &str = "FS_PERMISSION_DENIED";
const E_IO: &str = "FS_IO_ERROR";
/// 请求形状错误（协议层，不属于官方 fs 词表）。
const E_REQUEST: &str = "E_FS_REQUEST";

static REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_request_seq() -> u64 {
    REQUEST_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// 结构化的写入失败（`code` 进协议，`message` 面向模型/日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    pub code: String,
    pub message: String,
}

impl WriteFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// 写请求裁决服务：一个工作区 + 一个审批 broker。
pub struct FsWriteService {
    workspace: Workspace,
    approvals: Arc<ApprovalBroker>,
    approval_timeout: Duration,
    live: Option<LiveWritePolicy>,
}

struct LiveWritePolicy {
    decide: Box<dyn Fn() -> PolicyDecision + Send + Sync>,
    cancel: Arc<AtomicBool>,
}

impl FsWriteService {
    /// 打开工作区（不存在会创建）并绑定审批 broker。
    pub fn open(
        workspace_root: impl Into<PathBuf>,
        approvals: Arc<ApprovalBroker>,
    ) -> Result<Self, SandboxError> {
        Ok(Self {
            workspace: Workspace::new(workspace_root)?,
            approvals,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            live: None,
        })
    }

    pub fn with_live_controls(
        mut self,
        decide: impl Fn() -> PolicyDecision + Send + Sync + 'static,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        self.live = Some(LiveWritePolicy {
            decide: Box::new(decide),
            cancel,
        });
        self
    }

    pub fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace.root()
    }

    pub fn approvals(&self) -> &Arc<ApprovalBroker> {
        &self.approvals
    }

    pub fn approval_timeout(&self) -> Duration {
        self.approval_timeout
    }

    /// 协议入口：解析 payload，任何失败都折成结构化结果（宿主不会因此 panic）。
    pub fn handle_payload(&self, payload: &Value) -> FsWriteResult {
        self.handle_tool_payload(payload, None)
    }

    pub fn handle_tool_payload(&self, payload: &Value, grant_key: Option<&str>) -> FsWriteResult {
        match serde_json::from_value::<FsWriteRequest>(payload.clone()) {
            Ok(request) => self.handle_with_grant(&request, grant_key),
            Err(err) => FsWriteResult::error(
                E_REQUEST,
                format!("invalid fs.write.request payload: {err}"),
            ),
        }
    }

    /// 裁决一次写请求。成功返回新版本；失败返回 `FsWriteResult { ok: false, error }`。
    pub fn handle(&self, request: &FsWriteRequest) -> FsWriteResult {
        self.handle_with_grant(request, None)
    }

    fn handle_with_grant(
        &self,
        request: &FsWriteRequest,
        grant_key: Option<&str>,
    ) -> FsWriteResult {
        match self.try_handle(request, grant_key) {
            Ok(result) => result,
            Err(failure) => FsWriteResult::error(failure.code, failure.message),
        }
    }

    fn try_handle(
        &self,
        request: &FsWriteRequest,
        grant_key: Option<&str>,
    ) -> Result<FsWriteResult, WriteFailure> {
        let relative = self.resolve_target(request)?;
        let absolute = self.workspace.root().join(&relative);
        let metadata = fs::metadata(&absolute).ok();
        if let Some(meta) = &metadata {
            if meta.is_dir() {
                return Err(WriteFailure::new(
                    E_NOT_REGULAR_FILE,
                    format!(
                        "cannot modify \"{}\": not a regular file",
                        absolute.display()
                    ),
                ));
            }
        }
        let current_version = match metadata.as_ref() {
            Some(meta) => Some(version_of(meta)?),
            None => None,
        };

        match request.operation {
            FsWriteOperation::Write => {
                let content = request.content.as_deref().ok_or_else(|| {
                    WriteFailure::new(E_REQUEST, "write request requires a `content` string")
                })?;
                let bytes = content.len() as u64;
                if bytes > dshr_sandbox::MAX_WRITE_BYTES {
                    return Err(WriteFailure::new(
                        E_TOO_LARGE,
                        format!(
                            "write to \"{}\" exceeds the size limit: {bytes} > {}",
                            absolute.display(),
                            dshr_sandbox::MAX_WRITE_BYTES
                        ),
                    ));
                }
                let create_only = request
                    .intent
                    .as_ref()
                    .and_then(|intent| intent["kind"].as_str())
                    == Some("createIfAbsent");
                if create_only && fs::symlink_metadata(&absolute).is_ok() {
                    return Err(map_sandbox_error(SandboxError::AlreadyExists(
                        absolute.display().to_string(),
                    )));
                }
                self.check_version(request, &absolute, current_version.as_deref())?;
                let operation = if metadata.is_some() {
                    "update"
                } else {
                    "create"
                };
                self.approve(request, &absolute, operation, bytes, grant_key)?;
                self.check_version_after_approval(request, &absolute)?;
                let requested = relative.to_string_lossy();
                let receipt = if create_only {
                    self.workspace.create_text(&requested, content)
                } else {
                    self.workspace.write_text(&requested, content)
                }
                .map_err(map_sandbox_error)?;
                let after = fs::metadata(&absolute).map_err(map_io_error)?;
                let version = version_of(&after)?;
                Ok(FsWriteResult {
                    ok: true,
                    path: Some(display_path(&absolute)),
                    operation: Some(operation.to_owned()),
                    version: Some(version),
                    bytes: Some(bytes),
                    snapshot: receipt.snapshot.map(|snapshot| snapshot.id),
                    error: None,
                })
            }
            FsWriteOperation::Delete => {
                if request.content.is_some() {
                    return Err(WriteFailure::new(
                        E_REQUEST,
                        "delete request must not carry a `content` string",
                    ));
                }
                let meta = metadata.ok_or_else(|| {
                    WriteFailure::new(
                        E_NOT_FOUND,
                        format!("cannot delete \"{}\": not found", absolute.display()),
                    )
                })?;
                self.check_version(request, &absolute, current_version.as_deref())?;
                let bytes = meta.len();
                self.approve(request, &absolute, "delete", bytes, grant_key)?;
                self.check_version_after_approval(request, &absolute)?;
                let snapshot = self
                    .workspace
                    .snapshot(&absolute)
                    .map_err(map_sandbox_error)?;
                fs::remove_file(&absolute).map_err(map_io_error)?;
                Ok(FsWriteResult {
                    ok: true,
                    path: Some(display_path(&absolute)),
                    operation: Some("delete".to_owned()),
                    version: None,
                    bytes: Some(bytes),
                    snapshot: Some(snapshot.id),
                    error: None,
                })
            }
        }
    }

    /// 把请求路径归一化成"工作区相对路径"。
    ///
    /// - 相对路径：`..`/绝对路径交由 `Workspace::resolve` 拒绝（`FS_SANDBOX_DENIED`）。
    /// - 绝对路径：只接受工作区内路径（词法归一后按组件比较，Windows 大小写不敏感），
    ///   再交给 `Workspace::resolve` 做父目录存在性与符号链接逃逸检查。
    /// - `workspace_root` 与宿主工作区不一致：直接拒绝（防混淆代理）。
    fn resolve_target(&self, request: &FsWriteRequest) -> Result<PathBuf, WriteFailure> {
        let requested = request.path.trim();
        if requested.is_empty() {
            return Err(WriteFailure::new(
                E_REQUEST,
                "path must be a non-empty string",
            ));
        }
        if let Some(claimed) = &request.workspace_root {
            let claimed = std::path::absolute(Path::new(claimed)).map_err(map_io_error)?;
            if !paths_equal(&claimed, self.workspace.root()) {
                return Err(WriteFailure::new(
                    E_SANDBOX_DENIED,
                    format!(
                        "workspace root mismatch: sidecar claims \"{}\", host workspace is \"{}\"",
                        claimed.display(),
                        self.workspace.root().display()
                    ),
                ));
            }
        }

        let candidate = Path::new(requested);
        if candidate.is_absolute() {
            let absolute = std::path::absolute(candidate).map_err(map_io_error)?;
            let relative = strip_workspace_prefix(&absolute, self.workspace.root())
                .filter(|relative| !relative.as_os_str().is_empty())
                .ok_or_else(|| {
                    WriteFailure::new(
                        E_SANDBOX_DENIED,
                        format!(
                            "path escapes the workspace root \"{}\": {}",
                            self.workspace.root().display(),
                            absolute.display()
                        ),
                    )
                })?;
            self.workspace
                .resolve(&relative.to_string_lossy())
                .map_err(map_sandbox_error)?;
            Ok(relative)
        } else {
            self.workspace
                .resolve(requested)
                .map_err(map_sandbox_error)?;
            Ok(PathBuf::from(requested))
        }
    }

    fn check_version(
        &self,
        request: &FsWriteRequest,
        absolute: &Path,
        current: Option<&str>,
    ) -> Result<(), WriteFailure> {
        let Some(expected) = request.expected_version.as_deref() else {
            return Ok(());
        };
        if current == Some(expected) {
            return Ok(());
        }
        Err(WriteFailure::new(
            E_STALE_VERSION,
            format!(
                "cannot modify \"{}\": file changed since it was read (expected version {expected}, found {})",
                absolute.display(),
                current.unwrap_or("<absent>")
            ),
        ))
    }

    fn check_version_after_approval(
        &self,
        request: &FsWriteRequest,
        absolute: &Path,
    ) -> Result<(), WriteFailure> {
        if request.expected_version.is_none() {
            return Ok(());
        }
        let version = match fs::metadata(absolute) {
            Ok(info) => Some(version_of(&info)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(map_io_error(error)),
        };
        self.check_version(request, absolute, version.as_deref())
    }

    fn approve(
        &self,
        request: &FsWriteRequest,
        absolute: &Path,
        operation: &str,
        bytes: u64,
        grant_key: Option<&str>,
    ) -> Result<(), WriteFailure> {
        if self.write_policy()? == PolicyDecision::Allow {
            return Ok(());
        }
        let tool = if request.tool.trim().is_empty() {
            "write"
        } else {
            request.tool.as_str()
        };
        let call_id = request
            .call_id
            .clone()
            .unwrap_or_else(|| format!("fs-write-{}", next_request_seq()));
        let arguments = json!({
            "path": display_path(absolute),
            "operation": operation,
            "bytes": bytes,
            "tool": tool,
            "sandbox": "workspace-write",
        });
        let outcome = if let Some(live) = &self.live {
            self.approvals.request_with_policy(
                &call_id,
                tool,
                &arguments,
                self.approval_timeout,
                &live.cancel,
                LiveApproval {
                    decide: live.decide.as_ref(),
                    grant_key,
                },
            )
        } else {
            self.approvals
                .request(&call_id, tool, &arguments, self.approval_timeout)
        };
        self.write_policy()?;
        match outcome {
            ApprovalOutcome::AllowOnce | ApprovalOutcome::AllowAlways => Ok(()),
            ApprovalOutcome::Deny => Err(WriteFailure::new(
                E_SANDBOX_DENIED,
                format!(
                    "write to \"{}\" was denied or the approval timed out; nothing changed \
                     (sandbox escalation is not available in this composition)",
                    absolute.display()
                ),
            )),
        }
    }

    fn write_policy(&self) -> Result<PolicyDecision, WriteFailure> {
        let Some(live) = &self.live else {
            return Ok(PolicyDecision::Ask);
        };
        if live.cancel.load(Ordering::Relaxed) {
            return Err(WriteFailure::new(
                E_SANDBOX_DENIED,
                "file write denied: turn stopped",
            ));
        }
        match (live.decide)() {
            PolicyDecision::Deny(reason) => Err(WriteFailure::new(E_SANDBOX_DENIED, reason)),
            decision => Ok(decision),
        }
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `mtime_ms:size` —— 与 sidecar 的 `fs.stat().version` 同一格式（Node 侧取 floor(mtimeMs)）。
fn version_of(meta: &fs::Metadata) -> Result<String, WriteFailure> {
    let modified = meta.modified().map_err(map_io_error)?;
    let millis = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WriteFailure::new(E_IO, "file mtime is before the Unix epoch"))?
        .as_millis();
    Ok(format!("{millis}:{}", meta.len()))
}

fn map_sandbox_error(error: SandboxError) -> WriteFailure {
    let (code, message) = match error {
        SandboxError::AbsolutePath(path) => (
            E_SANDBOX_DENIED,
            format!("path must stay inside the workspace root: {path}"),
        ),
        SandboxError::Escape(path) => (
            E_SANDBOX_DENIED,
            format!("path escapes the workspace root: {path}"),
        ),
        SandboxError::InvalidPath(path) => (E_REQUEST, format!("invalid path: {path}")),
        SandboxError::TooLarge { size, limit } => (
            E_TOO_LARGE,
            format!("write exceeds the size limit: {size} > {limit}"),
        ),
        SandboxError::MissingParent(path) => (
            E_NOT_FOUND,
            format!("parent directory does not exist: {path}"),
        ),
        SandboxError::AlreadyExists(path) => (
            E_NOT_OBSERVED,
            format!("cannot create \"{path}\": a file already exists"),
        ),
        SandboxError::Io(message) => (E_IO, message),
    };
    WriteFailure::new(code, message)
}

fn map_io_error(error: std::io::Error) -> WriteFailure {
    let code = match error.kind() {
        std::io::ErrorKind::PermissionDenied => E_PERMISSION_DENIED,
        std::io::ErrorKind::NotFound => E_NOT_FOUND,
        _ => E_IO,
    };
    WriteFailure::new(code, format!("io error: {error}"))
}

fn component_eq(left: &Component<'_>, right: &Component<'_>) -> bool {
    if cfg!(windows) {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    } else {
        left == right
    }
}

/// 去掉工作区根前缀；只接受由 `Normal` 组件组成的相对尾巴（拒绝 `..`/前缀差异）。
fn strip_workspace_prefix(path: &Path, root: &Path) -> Option<PathBuf> {
    let path_components: Vec<Component<'_>> = path.components().collect();
    let root_components: Vec<Component<'_>> = root.components().collect();
    if path_components.len() <= root_components.len() {
        return None;
    }
    if !path_components
        .iter()
        .zip(root_components.iter())
        .all(|(path_component, root_component)| component_eq(path_component, root_component))
    {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &path_components[root_components.len()..] {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => return None,
        }
    }
    Some(relative)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let left_components: Vec<Component<'_>> = left.components().collect();
    let right_components: Vec<Component<'_>> = right.components().collect();
    left_components.len() == right_components.len()
        && left_components
            .iter()
            .zip(right_components.iter())
            .all(|(left_component, right_component)| component_eq(left_component, right_component))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dshr-compat-fs-{name}-{}-{}",
            std::process::id(),
            next_request_seq()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp workspace");
        dir
    }

    fn service(name: &str) -> (Arc<FsWriteService>, PathBuf) {
        let root = temp_root(name);
        let service = FsWriteService::open(&root, ApprovalBroker::new()).expect("service");
        (Arc::new(service), root)
    }

    fn write_request(path: &str, content: &str) -> FsWriteRequest {
        FsWriteRequest {
            call_id: Some("call-t3".to_owned()),
            tool: "write".to_owned(),
            operation: FsWriteOperation::Write,
            path: path.to_owned(),
            workspace_root: None,
            content: Some(content.to_owned()),
            expected_version: None,
            intent: None,
            mode: Some("workspace-write".to_owned()),
        }
    }

    #[test]
    fn editor_respects_concurrent_changes_during_approval() {
        for (create, expected_error) in [(true, E_NOT_OBSERVED), (false, E_STALE_VERSION)] {
            let (service, root) = service("editor-approval-race");
            let path = root.join("editor.txt");
            let mut request = write_request("editor.txt", "editor contents");
            if create {
                request.intent = Some(json!({ "kind": "createIfAbsent" }));
            } else {
                fs::write(&path, "old").unwrap();
                request.expected_version = Some(version_of(&fs::metadata(&path).unwrap()).unwrap());
            }
            let worker_service = service.clone();
            let worker = thread::spawn(move || worker_service.handle(&request));
            let deadline = Instant::now() + Duration::from_secs(5);
            let pending = loop {
                if let Some(pending) = service.approvals().pending().into_iter().next() {
                    break pending;
                }
                assert!(Instant::now() < deadline, "editor did not reach approval");
                thread::sleep(Duration::from_millis(1));
            };
            fs::write(&path, "written while approval was pending").unwrap();
            service
                .approvals()
                .resolve(&pending.call_id, ApprovalOutcome::AllowOnce);
            let result = worker.join().unwrap();
            assert_eq!(result.error.unwrap().code, expected_error);
            assert_eq!(
                fs::read_to_string(path).unwrap(),
                "written while approval was pending"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    /// 在另一个线程里等审批 pending 出现并决定；主线程同步调用 `handle`。
    fn run_with_decision(
        service: &Arc<FsWriteService>,
        request: &FsWriteRequest,
        decision: ApprovalOutcome,
    ) -> FsWriteResult {
        let broker = service.approvals().clone();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let waiter = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if done_rx.try_recv().is_ok() {
                    return;
                }
                let pending = broker.pending();
                if !pending.is_empty() {
                    for item in pending {
                        broker.resolve(&item.call_id, decision);
                    }
                    return;
                }
                if Instant::now() >= deadline {
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        let result = service.handle(request);
        let _ = done_tx.send(());
        let _ = waiter.join();
        assert!(
            service.approvals().pending().is_empty(),
            "审批记录要清理干净"
        );
        result
    }

    fn error_code(result: &FsWriteResult) -> String {
        result
            .error
            .as_ref()
            .map(|error| error.code.clone())
            .unwrap_or_else(|| panic!("expected an error result, got {result:?}"))
    }

    fn created_versions(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .expect("read workspace")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.contains("dshr-tmp"))
            .collect()
    }

    #[test]
    fn live_permission_revocation_blocks_an_approved_write() {
        let root = temp_root("live-permission");
        let approvals = ApprovalBroker::new();
        let allowed = Arc::new(AtomicBool::new(true));
        let service = Arc::new(
            FsWriteService::open(&root, approvals.clone())
                .unwrap()
                .with_approval_timeout(Duration::from_secs(2))
                .with_live_controls(
                    {
                        let allowed = allowed.clone();
                        move || {
                            if allowed.load(Ordering::Relaxed) {
                                PolicyDecision::Ask
                            } else {
                                PolicyDecision::Deny(
                                    "file write denied in read-only mode".to_owned(),
                                )
                            }
                        }
                    },
                    Arc::new(AtomicBool::new(false)),
                ),
        );
        let write = thread::spawn(move || service.handle(&write_request("blocked.txt", "test")));
        let deadline = Instant::now() + Duration::from_secs(1);
        while approvals.pending().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let pending = approvals.pending();
        assert_eq!(pending.len(), 1);
        allowed.store(false, Ordering::Relaxed);
        approvals.resolve(&pending[0].call_id, ApprovalOutcome::AllowOnce);
        assert_eq!(error_code(&write.join().unwrap()), E_SANDBOX_DENIED);
        assert!(!root.join("blocked.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_approval_continues_pending_and_future_writes_inside_workspace() {
        let root = temp_root("live-no-approval");
        let approvals = ApprovalBroker::new();
        let skip_approval = Arc::new(AtomicBool::new(false));
        let live_setting = skip_approval.clone();
        let service = Arc::new(
            FsWriteService::open(&root, approvals.clone())
                .unwrap()
                .with_approval_timeout(Duration::from_secs(3))
                .with_live_controls(
                    move || {
                        if live_setting.load(Ordering::Relaxed) {
                            PolicyDecision::Allow
                        } else {
                            PolicyDecision::Ask
                        }
                    },
                    Arc::new(AtomicBool::new(false)),
                ),
        );
        let first_write = service.clone();
        let write = thread::spawn(move || first_write.handle(&write_request("notes.txt", "first")));
        let deadline = Instant::now() + Duration::from_secs(2);
        while approvals.pending().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(approvals.pending().len(), 1);
        skip_approval.store(true, Ordering::Relaxed);
        assert!(write.join().unwrap().ok);
        assert!(approvals.pending().is_empty());
        assert_eq!(approvals.take_records()[0].answerer, "permission_mode");

        let update = service.handle(&write_request("notes.txt", "second"));
        assert!(update.ok);
        assert!(update.snapshot.is_some());
        assert_eq!(
            fs::read_to_string(root.join("notes.txt")).unwrap(),
            "second"
        );
        assert_eq!(
            error_code(&service.handle(&write_request("../outside.txt", "blocked"))),
            E_SANDBOX_DENIED,
        );
        assert!(approvals.take_records().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writes_inside_workspace_with_version_and_no_temp_leftovers() {
        let (service, root) = service("write");
        let result = run_with_decision(
            &service,
            &write_request("notes.txt", "hello"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(result.ok, "{result:?}");
        assert_eq!(result.operation.as_deref(), Some("create"));
        assert_eq!(result.bytes, Some(5));
        assert!(result.snapshot.is_none());
        let version = result.version.clone().expect("version");
        assert_eq!(version.matches(':').count(), 1, "version=mtime:size");
        assert_eq!(fs::read_to_string(root.join("notes.txt")).unwrap(), "hello");
        assert!(
            created_versions(&root).is_empty(),
            "临时文件必须被 rename 掉"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn update_keeps_snapshot_and_new_version() {
        let (service, root) = service("snapshot");
        let first = run_with_decision(
            &service,
            &write_request("a.txt", "v1"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(first.ok, "{first:?}");
        let mut second = write_request("a.txt", "v2-much-longer");
        second.expected_version = first.version.clone();
        let result = run_with_decision(&service, &second, ApprovalOutcome::AllowOnce);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.operation.as_deref(), Some("update"));
        let snapshot = result.snapshot.expect("update must snapshot first");
        assert!(
            service.workspace().snapshots_dir().join(&snapshot).exists(),
            "写前快照必须落盘"
        );
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "v2-much-longer"
        );
        assert_ne!(first.version, result.version);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_traversal_and_outside_absolute_paths_without_asking_for_approval() {
        let (service, root) = service("escape");
        for path in ["../outside.txt", "sub/../../outside.txt"] {
            let result = service.handle(&write_request(path, "x"));
            assert_eq!(error_code(&result), E_SANDBOX_DENIED, "path={path}");
        }
        let outside = temp_root("outside-target").join("evil.txt");
        let result = service.handle(&write_request(&outside.to_string_lossy(), "x"));
        assert_eq!(error_code(&result), E_SANDBOX_DENIED);
        assert!(service.approvals().pending().is_empty(), "越界不应进入审批");
        assert!(!root.join("outside.txt").exists());
        assert!(!outside.exists());
        fs::remove_dir_all(&root).ok();
        if let Some(parent) = outside.parent() {
            fs::remove_dir_all(parent).ok();
        }
    }

    #[test]
    fn rejects_workspace_root_mismatch() {
        let (service, root) = service("mismatch");
        let mut request = write_request("a.txt", "x");
        request.workspace_root = Some(if cfg!(windows) {
            "C:\\definitely-not-the-workspace".to_owned()
        } else {
            "/definitely-not-the-workspace".to_owned()
        });
        let result = service.handle(&request);
        assert_eq!(error_code(&result), E_SANDBOX_DENIED);
        assert!(service.approvals().pending().is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stale_version_is_rejected_and_file_untouched() {
        let (service, root) = service("stale");
        let first = run_with_decision(
            &service,
            &write_request("a.txt", "v1"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(first.ok, "{first:?}");
        let mut stale = write_request("a.txt", "clobbered");
        stale.expected_version = Some("1:1".to_owned());
        let result = service.handle(&stale);
        assert_eq!(error_code(&result), E_STALE_VERSION);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
        assert!(
            service.approvals().pending().is_empty(),
            "版本冲突不应进入审批"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn denied_approval_changes_nothing() {
        let (service, root) = service("denied");
        let first = run_with_decision(
            &service,
            &write_request("a.txt", "v1"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(first.ok, "{first:?}");
        let result = run_with_decision(
            &service,
            &write_request("a.txt", "v2"),
            ApprovalOutcome::Deny,
        );
        let error = result.error.expect("denied");
        assert_eq!(error.code, E_SANDBOX_DENIED);
        assert!(error.message.contains("denied"), "{}", error.message);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn oversized_write_is_rejected_before_approval() {
        let (service, root) = service("too-large");
        let big = "x".repeat((dshr_sandbox::MAX_WRITE_BYTES + 1) as usize);
        let result = service.handle(&write_request("big.txt", &big));
        assert_eq!(error_code(&result), E_TOO_LARGE);
        assert!(service.approvals().pending().is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_snapshots_then_removes_and_missing_file_is_structured() {
        let (service, root) = service("delete");
        let created = run_with_decision(
            &service,
            &write_request("gone.txt", "bye"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(created.ok, "{created:?}");
        let mut request = write_request("gone.txt", "");
        request.operation = FsWriteOperation::Delete;
        request.content = None;
        request.tool = "apply_patch".to_owned();
        let deleted = run_with_decision(&service, &request, ApprovalOutcome::AllowOnce);
        assert!(deleted.ok, "{deleted:?}");
        assert_eq!(deleted.operation.as_deref(), Some("delete"));
        assert!(deleted.version.is_none());
        assert!(deleted.snapshot.is_some());
        assert!(!root.join("gone.txt").exists());
        let missing = service.handle(&request);
        assert_eq!(error_code(&missing), E_NOT_FOUND);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn absolute_path_inside_workspace_is_accepted() {
        let (service, root) = service("absolute");
        fs::create_dir_all(root.join("sub")).unwrap();
        let target = root.join("sub").join("abs.txt");
        let result = run_with_decision(
            &service,
            &write_request(&target.to_string_lossy(), "abs"),
            ApprovalOutcome::AllowOnce,
        );
        assert!(result.ok, "{result:?}");
        assert_eq!(fs::read_to_string(target).unwrap(), "abs");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn invalid_request_shapes_are_structured() {
        let (service, root) = service("invalid");
        let mut missing_content = write_request("a.txt", "");
        missing_content.content = None;
        assert_eq!(error_code(&service.handle(&missing_content)), E_REQUEST);
        let mut delete_with_content = write_request("a.txt", "x");
        delete_with_content.operation = FsWriteOperation::Delete;
        assert_eq!(error_code(&service.handle(&delete_with_content)), E_REQUEST);
        assert_eq!(
            error_code(&service.handle(&write_request("  ", "x"))),
            E_REQUEST
        );
        assert_eq!(
            error_code(&service.handle(&write_request("no-such-dir/a.txt", "x"))),
            E_NOT_FOUND
        );
        let payload = json!({ "path": 42 });
        let result = service.handle_payload(&payload);
        assert_eq!(error_code(&result), E_REQUEST);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_target_is_not_treated_as_a_file() {
        let (service, root) = service("dir");
        fs::create_dir_all(root.join("sub")).unwrap();
        let result = service.handle(&write_request("sub", "x"));
        assert_eq!(error_code(&result), E_NOT_REGULAR_FILE);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn workspace_open_failure_is_reported() {
        let root = temp_root("open");
        let blocked = root.join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        let error = FsWriteService::open(&blocked, ApprovalBroker::new());
        assert!(error.is_err(), "把文件当工作区根必须失败");
        fs::remove_dir_all(&root).ok();
    }
}
