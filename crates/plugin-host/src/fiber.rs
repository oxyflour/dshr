//! CPP-v1 最小 Fiber / Service / Effect 运行时的**自研退路**（PLAN §3.5、ADR-0002）。
//!
//! 本机离线，`jincdream/cordis-rs` 与 `wasmtime` 都无法拉取，因此这一层按
//! spec/cpp-v1.md §4/§5/§6 实现一个**同步、最小但语义正确**的切片：
//!
//! - [`FiberState`]：`Pending / Loading / Active / Failed / Unloading / Disposed` 状态机，
//!   非法转换返回 `E_INTERNAL`（不 panic、不改状态）；
//! - [`Fiber`]：一次装载实例（id / plugin_id / generation / state / effects / 依赖）；
//! - [`ServiceKey`] + [`ServiceTable`]：`<name>/v<major>` 类型擦除服务表
//!   （`Arc<dyn Any + Send + Sync>`），同 key 冲突即 `E_SERVICE_CONFLICT`，撤销幂等；
//! - [`EffectStack`]：按注册序记录、`dispose` 严格 LIFO、幂等；单个 Effect panic/超时
//!   只记录诊断并继续清理（spec §6.3）；
//! - [`FiberHost`]：装载（`Pending → Loading → apply → Active`，失败回滚 Effect 与服务）、
//!   卸载（`→ Unloading → 逆序 Effect → Disposed`）、必需服务撤销时暂停依赖 Fiber、
//!   服务恢复后重新激活。
//!
//! ## 与 cordis-rs 的适配缝隙（替换点）
//!
//! 这一层是**唯一**知道 Fiber 实现的地方（ADR-0002 §隔离要求）。将来接入
//! `jincdream/cordis-rs` 时按下表替换，**`dshr-plugin-api` 与 `chat.rs` 不需要改**：
//!
//! | 本模块 | cordis-rs 概念 | 替换方式 |
//! |---|---|---|
//! | [`FiberState`] / [`Fiber`] | `Fiber` / `FiberState` | 直接把状态机换成 cordis 的 fiber，对外只保留 [`FiberHost`] 方法 |
//! | [`ServiceTable`] | `Service` / `ServiceInject` | 用 cordis 的 service store 实现同一组 `publish/revoke/get` |
//! | [`EffectStack`] | `Effect` / disposer | 用 cordis 的 `effect()` 返回值替换 `Box<dyn FnOnce()>` |
//! | [`FiberHost::load`] | `ctx.plugin(...)` / `apply` | 把 `apply` callback 交给 cordis 的 loader，`FiberContext` 变成 cordis `Context` 的薄包装 |
//! | 依赖暂停/恢复 | `inject` 反向依赖 | cordis 已内建；本模块的 `dependents` 索引可删 |
//!
//! 已知的最小化取舍（后续可补，不在本切片）：
//! 1. 同步装载：没有真正的 `apply` 超时中断，只测量耗时长于阈值即判 `E_APPLY_TIMEOUT`；
//! 2. 依赖暂停后"恢复"只做状态迁移，不重新执行 `apply`（cordis 会重启 Fiber）；
//! 3. 没有 Realm 隔离：一个 [`ServiceTable`] 视为一个 Realm，跨 Realm 可见性留到多 Realm 切片。

#![allow(clippy::module_inception)]

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dshr_plugin_api::{CancellationToken, CapabilitySet, Manifest, PluginError};

/// 单个 Effect 清理的默认超时（spec/cpp-v1.md §6.3）。
pub const EFFECT_DISPOSE_TIMEOUT_MS: u64 = 2_000;
/// `apply` 的默认超时上限（spec/cpp-v1.md §4）。
pub const APPLY_TIMEOUT_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// FiberState
// ---------------------------------------------------------------------------

/// Fiber 生命周期状态（spec/cpp-v1.md §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Failed,
    Unloading,
    Disposed,
}

impl FiberState {
    /// 规范里的 6 个状态（顺序即 §4 图里的出现顺序）。
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Loading,
        Self::Active,
        Self::Failed,
        Self::Unloading,
        Self::Disposed,
    ];

    /// 稳定的小写名字（用于错误信息 / 诊断，不用于协议）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Loading => "Loading",
            Self::Active => "Active",
            Self::Failed => "Failed",
            Self::Unloading => "Unloading",
            Self::Disposed => "Disposed",
        }
    }

    /// `Disposed` 是终态：不可复活，重启必须是新 generation（spec §4 规则）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Disposed)
    }

    /// 是否为"已装载但未就绪"（等待必需服务或退避重试）。
    pub fn is_stopped(self) -> bool {
        matches!(self, Self::Pending | Self::Failed)
    }

    /// 合法转换（spec/cpp-v1.md §4 + §5.2 的依赖暂停）。
    ///
    /// - `Pending -> Loading / Unloading`：装载开始 / 未装载就被清理；
    /// - `Loading -> Active / Failed`：apply 成功 / 失败（含超时、服务缺失）；
    /// - `Active -> Pending / Unloading`：依赖服务撤销（§5.2）/ 卸载；
    /// - `Failed -> Pending / Unloading`：退避重试 / 失败态直接卸载；
    /// - `Unloading -> Disposed`：清理完成；
    /// - `Disposed` 无出边。
    pub fn can_transition_to(self, next: Self) -> bool {
        use FiberState::{Active, Disposed, Failed, Loading, Pending, Unloading};
        matches!(
            (self, next),
            (Pending, Loading)
                | (Pending, Unloading)
                | (Loading, Active)
                | (Loading, Failed)
                | (Active, Pending)
                | (Active, Unloading)
                | (Failed, Pending)
                | (Failed, Unloading)
                | (Unloading, Disposed)
        )
    }
}

impl fmt::Display for FiberState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// ServiceKey / ServiceTable
// ---------------------------------------------------------------------------

/// Service key：`<name>/v<major>`（spec/cpp-v1.md §5.1），如 `tool-registry/v1`。
///
/// 名字部分与 manifest 校验对齐：`[a-z0-9-]+`；major 是十进制数字（不允许前导 `+`/`-`）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceKey {
    raw: String,
    name: String,
    major: u32,
}

impl ServiceKey {
    /// 解析并校验；非法返回 [`PluginError::InvalidServiceKey`]。
    pub fn parse(raw: impl Into<String>) -> Result<Self, PluginError> {
        let raw = raw.into();
        let Some((name, version)) = raw.rsplit_once('/') else {
            return Err(invalid_service_key(&raw, "must be `<name>/v<major>`"));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid_service_key(
                &raw,
                "name part must match `[a-z0-9-]+`",
            ));
        }
        let Some(major_text) = version.strip_prefix('v') else {
            return Err(invalid_service_key(&raw, "version part must be `v<major>`"));
        };
        if major_text.is_empty() || !major_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_service_key(&raw, "major must be decimal digits"));
        }
        let Ok(major) = major_text.parse::<u32>() else {
            return Err(invalid_service_key(&raw, "major does not fit in u32"));
        };
        let name = name.to_owned();
        Ok(Self { raw, name, major })
    }

    /// 名字部分（不含 `/vN`）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// major 版本号。
    pub fn major(&self) -> u32 {
        self.major
    }

    /// 原始文本。
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for ServiceKey {
    type Err = PluginError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

fn invalid_service_key(key: &str, reason: &str) -> PluginError {
    PluginError::InvalidServiceKey {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

/// 服务提供者身份（用于"只回收自己发布的服务"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProvider {
    pub plugin_id: String,
    pub fiber_id: u64,
    pub generation: u64,
}

struct ServiceEntry {
    value: Arc<dyn Any + Send + Sync>,
    provider: ServiceProvider,
}

/// 类型擦除的 Service 表（最小 Realm：一个表 = 一个 Realm）。
///
/// 服务值是 `Arc<dyn Any + Send + Sync>`：发布方与使用方通过具体类型
/// （通常是 trait object，如 `Arc<dyn ToolRegistry>`）downcast。**不使用 unsafe**。
#[derive(Default)]
pub struct ServiceTable {
    services: BTreeMap<ServiceKey, ServiceEntry>,
}

impl fmt::Debug for ServiceTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceTable")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ServiceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 发布服务；同一 key 已有提供者时返回 `E_SERVICE_CONFLICT`（spec §5.2）。
    pub fn publish(
        &mut self,
        key: ServiceKey,
        value: Arc<dyn Any + Send + Sync>,
        provider: ServiceProvider,
    ) -> Result<(), PluginError> {
        if self.services.contains_key(&key) {
            return Err(PluginError::ServiceConflict {
                key: key.as_str().to_owned(),
            });
        }
        self.services.insert(key, ServiceEntry { value, provider });
        Ok(())
    }

    /// 撤销服务；幂等（返回是否真的移除了条目）。
    pub fn revoke(&mut self, key: &ServiceKey) -> bool {
        self.services.remove(key).is_some()
    }

    /// 仅当当前提供者就是给定 Fiber 时才撤销（卸载/回滚用，避免误删别人的服务）。
    pub fn revoke_if_owned(&mut self, key: &ServiceKey, provider: &ServiceProvider) -> bool {
        let owned = self
            .services
            .get(key)
            .is_some_and(|entry| entry.provider == *provider);
        if owned {
            self.services.remove(key);
        }
        owned
    }

    /// 按具体类型取服务；类型不匹配返回 `None`（不 panic）。
    pub fn get<T: Any + Send + Sync>(&self, key: &ServiceKey) -> Option<Arc<T>> {
        let entry = self.services.get(key)?;
        Arc::clone(&entry.value).downcast::<T>().ok()
    }

    /// 类型擦除视图（诊断/转发用）。
    pub fn get_erased(&self, key: &ServiceKey) -> Option<Arc<dyn Any + Send + Sync>> {
        self.services.get(key).map(|entry| Arc::clone(&entry.value))
    }

    /// 当前提供者。
    pub fn provider(&self, key: &ServiceKey) -> Option<&ServiceProvider> {
        self.services.get(key).map(|entry| &entry.provider)
    }

    pub fn contains(&self, key: &ServiceKey) -> bool {
        self.services.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &ServiceKey> {
        self.services.keys()
    }

    pub fn len(&self) -> usize {
        self.services.len()
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }
}

// ---------------------------------------------------------------------------
// EffectStack
// ---------------------------------------------------------------------------

type CleanupFn = Box<dyn FnOnce() + Send + 'static>;

struct Effect {
    id: u64,
    label: String,
    cleanup: Option<CleanupFn>,
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Effect")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// 单个 Effect 的清理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectOutcome {
    /// 正常执行完成。
    Ok,
    /// 清理闭包 panic（会被捕获，不传播给宿主）。
    Panicked(String),
    /// 超过单 Effect 超时（线程已脱离，结果被丢弃；spec §6.3 允许）。
    TimedOut,
}

impl EffectOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn is_timed_out(&self) -> bool {
        matches!(self, Self::TimedOut)
    }
}

/// 一次 Effect 清理的记录项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDisposal {
    pub id: u64,
    pub label: String,
    pub outcome: EffectOutcome,
}

/// `EffectStack::dispose` 的报告；按**实际清理顺序**（LIFO）记录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisposeReport {
    entries: Vec<EffectDisposal>,
}

impl DisposeReport {
    pub fn entries(&self) -> &[EffectDisposal] {
        &self.entries
    }

    pub fn labels(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn all_ok(&self) -> bool {
        self.entries.iter().all(|entry| entry.outcome.is_ok())
    }

    pub fn timed_out(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.outcome.is_timed_out())
            .count()
    }

    pub fn failed(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.outcome, EffectOutcome::Panicked(_)))
            .count()
    }
}

/// 归属于一个 Fiber 的可逆副作用栈（spec/cpp-v1.md §6）。
///
/// - 注册顺序记录在 `Vec` 末尾，`dispose` 从尾部弹出（严格 LIFO）；
/// - `dispose` 幂等：清空后再调用返回空报告、不重复执行；
/// - 每个 Effect 在独立线程执行：panic 被 `catch_unwind` 捕获记为 `Panicked`，
///   超时记为 `TimedOut` 并继续下一个——单个失败**不中断**整体清理。
///
/// 线程不可杀死，这是 std 的边界：超时的清理线程会脱离（detached），其结果被丢弃。
/// 这与 spec §6.3 "记录诊断、继续下一个" 一致；将来换 cordis-rs 时由它的 disposer 接管。
pub struct EffectStack {
    effects: Vec<Effect>,
    next_id: u64,
    timeout: Duration,
    disposed: bool,
}

impl fmt::Debug for EffectStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectStack")
            .field("pending", &self.effects.len())
            .field("timeout_ms", &self.timeout.as_millis())
            .field("disposed", &self.disposed)
            .finish()
    }
}

impl Default for EffectStack {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectStack {
    /// 默认单 Effect 超时 2 s（spec §6.3）。
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_millis(EFFECT_DISPOSE_TIMEOUT_MS))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            effects: Vec::new(),
            next_id: 1,
            timeout,
            disposed: false,
        }
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 登记一个清理动作，返回 Effect id；清理按登记逆序执行。
    pub fn push(
        &mut self,
        label: impl Into<String>,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.effects.push(Effect {
            id,
            label: label.into(),
            cleanup: Some(Box::new(cleanup)),
        });
        id
    }

    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    /// 当前待清理 Effect 的 label，按**注册顺序**（最老的在前）。
    pub fn labels(&self) -> Vec<&str> {
        self.effects
            .iter()
            .map(|effect| effect.label.as_str())
            .collect()
    }

    /// 逆序执行全部清理；幂等。
    pub fn dispose(&mut self) -> DisposeReport {
        let mut report = DisposeReport::default();
        while let Some(mut effect) = self.effects.pop() {
            let cleanup = effect
                .cleanup
                .take()
                .expect("effect cleanup is only taken once");
            let outcome = run_cleanup(cleanup, self.timeout);
            report.entries.push(EffectDisposal {
                id: effect.id,
                label: effect.label,
                outcome,
            });
        }
        self.disposed = true;
        report
    }
}

/// 在独立线程里执行一个清理动作，最多等 `timeout`。
fn run_cleanup(cleanup: CleanupFn, timeout: Duration) -> EffectOutcome {
    let (sender, receiver) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("cpp-effect-cleanup".to_owned())
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(cleanup));
            let _ = sender.send(outcome);
        });
    let handle = match spawned {
        Ok(handle) => handle,
        Err(err) => return EffectOutcome::Panicked(format!("cannot spawn cleanup thread: {err}")),
    };
    match receiver.recv_timeout(timeout) {
        Ok(Ok(())) => {
            let _ = handle.join();
            EffectOutcome::Ok
        }
        Ok(Err(payload)) => {
            let _ = handle.join();
            EffectOutcome::Panicked(panic_message(payload))
        }
        // 超时：线程脱离，结果丢弃；不影响后续 Effect 清理。
        Err(RecvTimeoutError::Timeout) => EffectOutcome::TimedOut,
        Err(RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            EffectOutcome::Panicked("cleanup thread ended without a result".to_owned())
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "effect panicked with a non-string payload".to_owned()
    }
}

// ---------------------------------------------------------------------------
// Fiber
// ---------------------------------------------------------------------------

/// 依赖服务缺失导致的暂停原因（`Active -> Pending`，spec §5.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseReason {
    ServiceMissing(ServiceKey),
}

/// 一个插件实例的生命周期容器（spec/cpp-v1.md §2/§4）。
#[derive(Debug)]
pub struct Fiber {
    id: u64,
    plugin_id: String,
    generation: u64,
    state: FiberState,
    effects: EffectStack,
    required: Vec<ServiceKey>,
    optional: Vec<ServiceKey>,
    published: Vec<ServiceKey>,
    failure: Option<PluginError>,
    pause_reason: Option<PauseReason>,
    resumed_count: u32,
    last_dispose: Option<DisposeReport>,
}

impl Fiber {
    pub fn new(id: u64, plugin_id: impl Into<String>, generation: u64) -> Self {
        Self {
            id,
            plugin_id: plugin_id.into(),
            generation,
            state: FiberState::Pending,
            effects: EffectStack::new(),
            required: Vec::new(),
            optional: Vec::new(),
            published: Vec::new(),
            failure: None,
            pause_reason: None,
            resumed_count: 0,
            last_dispose: None,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// 装载实例编号；卸载/失败后重装即新 generation（spec §2）。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn state(&self) -> FiberState {
        self.state
    }

    pub fn effects(&self) -> &EffectStack {
        &self.effects
    }

    pub fn required_services(&self) -> &[ServiceKey] {
        &self.required
    }

    pub fn optional_services(&self) -> &[ServiceKey] {
        &self.optional
    }

    pub fn published_services(&self) -> &[ServiceKey] {
        &self.published
    }

    pub fn failure(&self) -> Option<&PluginError> {
        self.failure.as_ref()
    }

    pub fn pause_reason(&self) -> Option<&PauseReason> {
        self.pause_reason.as_ref()
    }

    /// 依赖恢复后的重启次数（spec §5.2 上限 3 次）。
    pub fn resumed_count(&self) -> u32 {
        self.resumed_count
    }

    pub fn last_dispose(&self) -> Option<&DisposeReport> {
        self.last_dispose.as_ref()
    }

    /// 状态迁移；非法迁移返回 [`PluginError::InvalidTransition`] 且**不改状态**。
    pub fn transition(&mut self, next: FiberState) -> Result<(), PluginError> {
        if !self.state.can_transition_to(next) {
            return Err(PluginError::InvalidTransition {
                from: self.state.as_str(),
                to: next.as_str(),
            });
        }
        self.state = next;
        Ok(())
    }

    fn set_requirements(&mut self, required: Vec<ServiceKey>, optional: Vec<ServiceKey>) {
        self.required = required;
        self.optional = optional;
    }

    fn set_published(&mut self, published: Vec<ServiceKey>) {
        self.published = published;
    }

    fn set_failure(&mut self, error: PluginError) {
        self.failure = Some(error);
    }

    fn set_pause_reason(&mut self, reason: PauseReason) {
        self.pause_reason = Some(reason);
    }

    fn clear_pause_reason(&mut self) {
        self.pause_reason = None;
    }

    fn bump_resume(&mut self) {
        self.resumed_count += 1;
    }
}

// ---------------------------------------------------------------------------
// FiberSpec / FiberContext
// ---------------------------------------------------------------------------

/// 装载一个 Fiber 所需的静态信息（从 manifest 装配或直接构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiberSpec {
    plugin_id: String,
    required: Vec<ServiceKey>,
    optional: Vec<ServiceKey>,
}

impl FiberSpec {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            required: Vec::new(),
            optional: Vec::new(),
        }
    }

    /// 按 spec/cpp-v1.md §3.3 的 `[inject]` 装配（解析 manifest 的 `required`/`optional`）。
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, PluginError> {
        let mut spec = Self::new(manifest.plugin.id.clone());
        if let Some(inject) = &manifest.inject {
            for raw in &inject.required {
                spec.add_required(raw)?;
            }
            for raw in &inject.optional {
                spec.add_optional(raw)?;
            }
        }
        Ok(spec)
    }

    /// 追加必需服务（解析 `name/vN`，非法返回 `E_MANIFEST`）。
    pub fn add_required(&mut self, raw: &str) -> Result<(), PluginError> {
        self.required.push(ServiceKey::parse(raw)?);
        Ok(())
    }

    /// 追加可选服务。
    pub fn add_optional(&mut self, raw: &str) -> Result<(), PluginError> {
        self.optional.push(ServiceKey::parse(raw)?);
        Ok(())
    }

    pub fn with_required(mut self, key: ServiceKey) -> Self {
        self.required.push(key);
        self
    }

    pub fn with_optional(mut self, key: ServiceKey) -> Self {
        self.optional.push(key);
        self
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn required(&self) -> &[ServiceKey] {
        &self.required
    }

    pub fn optional(&self) -> &[ServiceKey] {
        &self.optional
    }
}

/// `apply` 期间插件能接触到的 Fiber 能力（spec §5/§6，最小切片）。
///
/// 与 [`dshr_plugin_api::PluginContext`] 的关系：`PluginContext` 负责"声明式注册"，
/// 本类型负责"可撤销副作用 + 服务发布"；两者都由宿主在 `apply` 期间持有，
/// `Plugin` trait 本身不变（chat.rs 零改动）。
pub struct FiberContext<'a> {
    plugin_id: &'a str,
    fiber_id: u64,
    generation: u64,
    services: &'a mut ServiceTable,
    effects: &'a mut EffectStack,
    published: &'a mut Vec<ServiceKey>,
    capabilities: &'a CapabilitySet,
    cancellation: CancellationToken,
}

impl FiberContext<'_> {
    pub fn plugin_id(&self) -> &str {
        self.plugin_id
    }

    pub fn fiber_id(&self) -> u64 {
        self.fiber_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 发布服务；本 Fiber 卸载/回滚时自动撤销。
    pub fn publish(
        &mut self,
        key: impl AsRef<str>,
        service: Arc<dyn Any + Send + Sync>,
    ) -> Result<ServiceKey, PluginError> {
        let key = ServiceKey::parse(key.as_ref())?;
        self.publish_keyed(key, service)
    }

    /// 已解析 key 的发布入口；同一 Fiber 重复发布同一 key 即 `E_SERVICE_CONFLICT`。
    pub fn publish_keyed(
        &mut self,
        key: ServiceKey,
        service: Arc<dyn Any + Send + Sync>,
    ) -> Result<ServiceKey, PluginError> {
        if self.published.contains(&key) {
            return Err(PluginError::ServiceConflict {
                key: key.as_str().to_owned(),
            });
        }
        let provider = ServiceProvider {
            plugin_id: self.plugin_id.to_owned(),
            fiber_id: self.fiber_id,
            generation: self.generation,
        };
        self.services.publish(key.clone(), service, provider)?;
        self.published.push(key.clone());
        Ok(key)
    }

    /// 探测（可能缺失的）服务；缺失返回 `None`，不抛错（spec §5.3 optional 语义）。
    pub fn try_get<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        let key = ServiceKey::parse(key).ok()?;
        self.services.get::<T>(&key)
    }

    /// 登记清理动作（Effect）；返回 id，卸载时逆序执行。
    pub fn on_cleanup(
        &mut self,
        label: impl Into<String>,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> u64 {
        self.effects.push(label, cleanup)
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.has(capability)
    }

    /// 声明硬依赖；缺失返回 `E_PERMISSION`（spec §9）。
    pub fn require_capability(&self, capability: &str) -> Result<(), PluginError> {
        self.capabilities.require(capability)
    }

    /// 宿主的取消令牌（工具执行期间可轮询；本切片把它交给插件自己用）。
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

// ---------------------------------------------------------------------------
// FiberHost
// ---------------------------------------------------------------------------

/// 装载成功报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    pub fiber_id: u64,
    pub generation: u64,
    pub state: FiberState,
}

/// 卸载报告（清理结果 + 受影响的依赖 Fiber）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnloadReport {
    pub fiber_id: u64,
    pub generation: u64,
    pub effects: DisposeReport,
    pub revoked_services: Vec<ServiceKey>,
    pub paused_dependents: Vec<String>,
}

/// 最小 Fiber 宿主：服务表 + Fiber 表 + 依赖索引 + 装载/卸载生命周期。
///
/// 与 [`super::PluginHost`] 的分工：`PluginHost` 管 ToolRegistry/面板的原子提交，
/// 本类型管 Fiber 状态机、Service 与 Effect；两者通过 [`PluginHost::load_with_manifest`]
/// 衔接（见 `lib.rs`），`Plugin` trait 不变。
#[derive(Debug)]
pub struct FiberHost {
    services: ServiceTable,
    fibers: BTreeMap<String, Fiber>,
    /// 服务 key -> 依赖它的 plugin id（只记录 `inject.required`）。
    dependents: BTreeMap<ServiceKey, BTreeSet<String>>,
    capabilities: CapabilitySet,
    next_fiber_id: u64,
    next_generation: u64,
    apply_timeout: Duration,
}

impl Default for FiberHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FiberHost {
    pub fn new() -> Self {
        Self::with_capabilities(CapabilitySet::new())
    }

    /// 指定宿主 capability 集合（`apply` 期间通过 `FiberContext` 探测）。
    pub fn with_capabilities(capabilities: CapabilitySet) -> Self {
        Self {
            services: ServiceTable::new(),
            fibers: BTreeMap::new(),
            dependents: BTreeMap::new(),
            capabilities,
            next_fiber_id: 1,
            next_generation: 1,
            apply_timeout: Duration::from_millis(APPLY_TIMEOUT_MS),
        }
    }

    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    pub fn services(&self) -> &ServiceTable {
        &self.services
    }

    /// 设置 `apply` 超时阈值（同步实现只能事后判定，见模块文档）。
    pub fn set_apply_timeout(&mut self, timeout: Duration) {
        self.apply_timeout = timeout;
    }

    pub fn apply_timeout(&self) -> Duration {
        self.apply_timeout
    }

    pub fn fiber(&self, plugin_id: &str) -> Option<&Fiber> {
        self.fibers.get(plugin_id)
    }

    pub fn fiber_state(&self, plugin_id: &str) -> Option<FiberState> {
        self.fibers.get(plugin_id).map(Fiber::state)
    }

    pub fn fibers(&self) -> impl Iterator<Item = &Fiber> {
        self.fibers.values()
    }

    pub fn fiber_count(&self) -> usize {
        self.fibers.len()
    }

    /// 依赖某个服务的 plugin id（只读诊断）。
    pub fn dependent_plugins(&self, key: &ServiceKey) -> Vec<String> {
        self.dependents
            .get(key)
            .map(|plugins| plugins.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 装载一个 Fiber：
    /// `Pending -> Loading -> apply -> Active`；`apply` 失败/超时则回滚 Effect 与服务并进 `Failed`。
    ///
    /// 必需服务缺失时直接进 `Failed` 并返回 `E_SERVICE_MISSING`；服务恢复后可重试。
    pub fn load<F>(&mut self, spec: FiberSpec, apply: F) -> Result<LoadReport, PluginError>
    where
        F: FnOnce(&mut FiberContext<'_>) -> Result<(), PluginError>,
    {
        self.check_loadable(&spec)?;

        let fiber_id = self.next_fiber_id;
        self.next_fiber_id += 1;
        let generation = self.next_generation;
        self.next_generation += 1;

        let mut fiber = Fiber::new(fiber_id, spec.plugin_id.clone(), generation);
        fiber.set_requirements(spec.required.clone(), spec.optional.clone());
        fiber.transition(FiberState::Loading)?;

        // 1) 必需服务必须先就绪（spec §5.3；最小切片不做 10 s 等待）。
        if let Some(missing) = spec
            .required
            .iter()
            .find(|key| !self.services.contains(key))
            .cloned()
        {
            fiber.transition(FiberState::Failed)?;
            let error = PluginError::ServiceMissing {
                key: missing.as_str().to_owned(),
            };
            fiber.set_failure(error.clone());
            self.fibers.insert(spec.plugin_id, fiber);
            return Err(error);
        }

        // 2) 执行 apply 等价 callback；期间可发布服务、登记 Effect、探测 capability。
        let mut effects = EffectStack::new();
        let mut published: Vec<ServiceKey> = Vec::new();
        let cancellation = CancellationToken::new();
        let started = Instant::now();
        let apply_result = {
            let mut ctx = FiberContext {
                plugin_id: &spec.plugin_id,
                fiber_id,
                generation,
                services: &mut self.services,
                effects: &mut effects,
                published: &mut published,
                capabilities: &self.capabilities,
                cancellation: cancellation.clone(),
            };
            apply(&mut ctx)
        };
        let elapsed = started.elapsed();

        // 3) 判定：显式错误优先，其次事后超时判定（同步实现无法中断）。
        let failure = match apply_result {
            Err(error) => Some(error),
            Ok(()) if elapsed > self.apply_timeout => Some(PluginError::ApplyTimeout {
                plugin_id: spec.plugin_id.clone(),
                timeout_ms: u64::try_from(self.apply_timeout.as_millis()).unwrap_or(u64::MAX),
            }),
            Ok(()) => None,
        };

        if let Some(error) = failure {
            // 回滚：先逆序清理 Effect，再撤销本 Fiber 发布的服务，最后落 Failed。
            let report = effects.dispose();
            let provider = ServiceProvider {
                plugin_id: spec.plugin_id.clone(),
                fiber_id,
                generation,
            };
            for key in &published {
                self.services.revoke_if_owned(key, &provider);
            }
            fiber.effects = effects;
            fiber.last_dispose = Some(report);
            fiber.transition(FiberState::Failed)?;
            fiber.set_failure(error.clone());
            self.fibers.insert(spec.plugin_id, fiber);
            return Err(error);
        }

        // 4) 成功：提交 Effect/服务，登记依赖边，并唤醒等待新服务的 Pending Fiber。
        fiber.effects = effects;
        fiber.set_published(published.clone());
        fiber.transition(FiberState::Active)?;
        self.fibers.insert(spec.plugin_id.clone(), fiber);
        for key in &spec.required {
            self.dependents
                .entry(key.clone())
                .or_default()
                .insert(spec.plugin_id.clone());
        }
        for key in &published {
            self.on_service_published(key);
        }

        Ok(LoadReport {
            fiber_id,
            generation,
            state: FiberState::Active,
        })
    }

    /// 从 manifest 的 `[plugin].id` 与 `[inject]` 装配后装载。
    pub fn load_with_manifest<F>(
        &mut self,
        manifest: &Manifest,
        apply: F,
    ) -> Result<LoadReport, PluginError>
    where
        F: FnOnce(&mut FiberContext<'_>) -> Result<(), PluginError>,
    {
        let spec = FiberSpec::from_manifest(manifest)?;
        self.load(spec, apply)
    }

    /// 卸载：`Active/Pending/Failed -> Unloading -> 逆序 Effect -> Disposed`。
    ///
    /// 重复卸载返回 `E_INTERNAL`（[`PluginError::NotLoaded`]），且不会重新执行 Effect。
    pub fn unload(&mut self, plugin_id: &str) -> Result<UnloadReport, PluginError> {
        let FiberHost {
            services,
            fibers,
            dependents,
            ..
        } = self;

        let Some(fiber) = fibers.get_mut(plugin_id) else {
            return Err(PluginError::NotLoaded {
                plugin_id: plugin_id.to_owned(),
            });
        };
        if fiber.state() == FiberState::Disposed {
            return Err(PluginError::NotLoaded {
                plugin_id: plugin_id.to_owned(),
            });
        }
        fiber.transition(FiberState::Unloading)?;

        let mut effects = std::mem::take(&mut fiber.effects);
        let report = effects.dispose();
        fiber.effects = effects;
        fiber.last_dispose = Some(report.clone());
        let fiber_id = fiber.id();
        let generation = fiber.generation();
        let published = std::mem::take(&mut fiber.published);

        // 撤销服务 -> 依赖它的 Fiber 暂停（spec §5.2）。
        let provider = ServiceProvider {
            plugin_id: plugin_id.to_owned(),
            fiber_id,
            generation,
        };
        let mut revoked_services = Vec::new();
        let mut paused_dependents = Vec::new();
        for key in published {
            if services.revoke_if_owned(&key, &provider) {
                revoked_services.push(key.clone());
                paused_dependents.extend(pause_dependents(dependents, fibers, &key));
            }
        }

        // 移除本 Fiber 在依赖图里的入边（对每个 required key 都清一次）。
        for plugins in dependents.values_mut() {
            plugins.remove(plugin_id);
        }
        dependents.retain(|_, plugins| !plugins.is_empty());

        let fiber = fibers
            .get_mut(plugin_id)
            .expect("fiber stays in the map until explicitly removed");
        fiber.transition(FiberState::Disposed)?;

        Ok(UnloadReport {
            fiber_id,
            generation,
            effects: report,
            revoked_services,
            paused_dependents,
        })
    }

    /// 撤销一个服务并通知依赖 Fiber（返回被暂停的 plugin id）。
    pub fn revoke_service(&mut self, key: &ServiceKey) -> Vec<String> {
        self.services.revoke(key);
        self.on_service_revoked(key)
    }

    /// 服务撤销通知：`Active -> Pending`（spec §5.2）。
    pub fn on_service_revoked(&mut self, key: &ServiceKey) -> Vec<String> {
        pause_dependents(&self.dependents, &mut self.fibers, key)
    }

    /// 服务恢复通知：所有必需服务重新就绪的 `Pending` Fiber 回到 `Active`。
    pub fn on_service_published(&mut self, key: &ServiceKey) -> Vec<String> {
        resume_dependents(&self.dependents, &mut self.fibers, &self.services, key)
    }

    fn check_loadable(&self, spec: &FiberSpec) -> Result<(), PluginError> {
        if let Some(fiber) = self.fibers.get(spec.plugin_id()) {
            match fiber.state() {
                FiberState::Active | FiberState::Loading | FiberState::Unloading => {
                    return Err(PluginError::AlreadyLoaded {
                        plugin_id: spec.plugin_id.clone(),
                    });
                }
                // Failed / Disposed / Pending：允许重装，分配新 generation。
                FiberState::Failed | FiberState::Disposed | FiberState::Pending => {}
            }
        }
        Ok(())
    }
}

fn pause_dependents(
    dependents: &BTreeMap<ServiceKey, BTreeSet<String>>,
    fibers: &mut BTreeMap<String, Fiber>,
    key: &ServiceKey,
) -> Vec<String> {
    let Some(plugins) = dependents.get(key) else {
        return Vec::new();
    };
    let mut paused = Vec::new();
    for plugin_id in plugins {
        let Some(fiber) = fibers.get_mut(plugin_id) else {
            continue;
        };
        if fiber.state() != FiberState::Active || fiber.transition(FiberState::Pending).is_err() {
            continue;
        }
        fiber.set_pause_reason(PauseReason::ServiceMissing(key.clone()));
        paused.push(plugin_id.clone());
    }
    paused
}

fn resume_dependents(
    dependents: &BTreeMap<ServiceKey, BTreeSet<String>>,
    fibers: &mut BTreeMap<String, Fiber>,
    services: &ServiceTable,
    key: &ServiceKey,
) -> Vec<String> {
    let Some(plugins) = dependents.get(key) else {
        return Vec::new();
    };
    let mut resumed = Vec::new();
    for plugin_id in plugins {
        let Some(fiber) = fibers.get_mut(plugin_id) else {
            continue;
        };
        if fiber.state() != FiberState::Pending {
            continue;
        }
        if !fiber
            .required_services()
            .iter()
            .all(|required| services.contains(required))
        {
            continue;
        }
        // 最小切片的"自动重启"：Pending -> Loading -> Active。
        // 完整 cordis 会重新执行 apply；这里只做状态迁移（见模块文档的已知取舍）。
        if fiber.transition(FiberState::Loading).is_err()
            || fiber.transition(FiberState::Active).is_err()
        {
            continue;
        }
        fiber.clear_pause_reason();
        fiber.bump_resume();
        resumed.push(plugin_id.clone());
    }
    resumed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use dshr_plugin_api::{capability, Manifest};
    use serde_json::json;

    // ------------------------------------------------------------------
    // 测试辅助
    // ------------------------------------------------------------------

    fn key(raw: &str) -> ServiceKey {
        ServiceKey::parse(raw).expect("测试里的 service key 必须合法")
    }

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn labels(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    fn record(log: &Arc<Mutex<Vec<String>>>, entry: &str) -> impl FnOnce() + Send + 'static {
        let log = Arc::clone(log);
        let entry = entry.to_owned();
        move || log.lock().unwrap().push(entry)
    }

    fn provider(plugin_id: &str, fiber_id: u64, generation: u64) -> ServiceProvider {
        ServiceProvider {
            plugin_id: plugin_id.to_owned(),
            fiber_id,
            generation,
        }
    }

    fn manifest_with_inject(required: &[&str], optional: &[&str]) -> Manifest {
        let value = json!({
            "plugin": {
                "id": "demo.manifest",
                "version": "0.1.0",
                "api": "cordis-portable/v1"
            },
            "runtime": { "kind": "rust-static" },
            "inject": { "required": required, "optional": optional }
        });
        Manifest::from_json_str(&value.to_string()).expect("测试 manifest 必须可解析")
    }

    /// 装载一个只发布某个服务的 provider fiber。
    fn load_provider(host: &mut FiberHost, plugin_id: &str, raw_key: &str) -> LoadReport {
        let raw_key = raw_key.to_owned();
        host.load(FiberSpec::new(plugin_id), move |ctx| {
            ctx.publish(raw_key.as_str(), Arc::new(format!("value-of-{raw_key}")))?;
            Ok(())
        })
        .expect("provider 应装载成功")
    }

    fn dependency_consumer(plugin_id: &str, required: &[&str]) -> FiberSpec {
        let mut spec = FiberSpec::new(plugin_id);
        for raw in required {
            spec.add_required(raw).unwrap();
        }
        spec
    }

    // ------------------------------------------------------------------
    // CPP-FIBER-*：状态机与生命周期
    // ------------------------------------------------------------------

    #[test]
    fn cpp_fiber_001_legal_transition_matrix_matches_spec() {
        use FiberState::*;
        let legal = [
            (Pending, Loading),
            (Pending, Unloading),
            (Loading, Active),
            (Loading, Failed),
            (Active, Pending),
            (Active, Unloading),
            (Failed, Pending),
            (Failed, Unloading),
            (Unloading, Disposed),
        ];
        assert_eq!(legal.len(), 9);
        for (from, to) in legal {
            assert!(from.can_transition_to(to), "{from} -> {to} 应合法");
        }
        for from in FiberState::ALL {
            let edges = FiberState::ALL
                .into_iter()
                .filter(|to| from.can_transition_to(*to))
                .count();
            assert_eq!(
                edges,
                if from == Disposed {
                    0
                } else {
                    legal.iter().filter(|(f, _)| *f == from).count()
                }
            );
        }
    }

    #[test]
    fn cpp_fiber_002_illegal_transitions_are_rejected_without_state_change() {
        use FiberState::*;
        let mut fiber = Fiber::new(7, "demo.state", 1);
        for (from, to) in [
            (Pending, Active),
            (Pending, Disposed),
            (Pending, Pending),
            (Loading, Unloading),
            (Loading, Pending),
            (Active, Loading),
            (Active, Failed),
            (Active, Active),
            (Failed, Active),
            (Unloading, Active),
            (Unloading, Pending),
            (Disposed, Pending),
            (Disposed, Loading),
        ] {
            fiber.state = from;
            let err = fiber.transition(to).expect_err("非法迁移必须报错");
            assert_eq!(
                err,
                PluginError::InvalidTransition {
                    from: from.as_str(),
                    to: to.as_str(),
                }
            );
            assert_eq!(fiber.state(), from, "非法迁移不得改变状态");
        }
    }

    #[test]
    fn cpp_fiber_003_dependency_pause_and_resume_transitions() {
        let mut fiber = Fiber::new(1, "demo.pause", 1);
        fiber.transition(FiberState::Loading).unwrap();
        fiber.transition(FiberState::Active).unwrap();
        fiber.transition(FiberState::Pending).unwrap();
        assert!(fiber.state().is_stopped());
        // 服务恢复后的最小"自动重启"：Pending -> Loading -> Active（不重跑 apply）。
        fiber.transition(FiberState::Loading).unwrap();
        fiber.transition(FiberState::Active).unwrap();
        assert_eq!(fiber.state(), FiberState::Active);
    }

    #[test]
    fn cpp_fiber_004_pending_can_be_unloaded_into_terminal_state() {
        let mut fiber = Fiber::new(1, "demo.never-loaded", 1);
        assert_eq!(fiber.state(), FiberState::Pending);
        fiber.transition(FiberState::Unloading).unwrap();
        fiber.transition(FiberState::Disposed).unwrap();
        assert!(fiber.state().is_terminal());
        assert!(fiber.transition(FiberState::Loading).is_err());
    }

    #[test]
    fn cpp_fiber_005_state_names_are_stable_and_complete() {
        let names: Vec<&str> = FiberState::ALL.iter().map(|state| state.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Pending",
                "Loading",
                "Active",
                "Failed",
                "Unloading",
                "Disposed"
            ]
        );
        assert_eq!(FiberState::ALL.len(), 6);
        assert_eq!(FiberState::Disposed.to_string(), "Disposed");
        assert!(FiberState::Disposed.is_terminal());
        assert!(!FiberState::Active.is_terminal());
    }

    #[test]
    fn cpp_fiber_006_load_succeeds_into_active_with_generation() {
        let mut host = FiberHost::new();
        let report = host
            .load(FiberSpec::new("demo.load"), |_ctx| Ok(()))
            .unwrap();
        assert_eq!(report.state, FiberState::Active);
        assert_eq!(report.fiber_id, 1);
        assert_eq!(report.generation, 1);
        assert_eq!(host.fiber_state("demo.load"), Some(FiberState::Active));
        let fiber = host.fiber("demo.load").unwrap();
        assert_eq!(fiber.plugin_id(), "demo.load");
        assert_eq!(fiber.id(), 1);
        assert_eq!(fiber.generation(), 1);
        assert!(fiber.failure().is_none());
        assert!(fiber.last_dispose().is_none());
    }

    #[test]
    fn cpp_fiber_007_duplicate_load_is_rejected_while_active() {
        let mut host = FiberHost::new();
        host.load(FiberSpec::new("demo.dup"), |_ctx| Ok(()))
            .unwrap();
        let err = host
            .load(FiberSpec::new("demo.dup"), |_ctx| Ok(()))
            .unwrap_err();
        assert_eq!(
            err,
            PluginError::AlreadyLoaded {
                plugin_id: "demo.dup".to_owned()
            }
        );
        assert_eq!(host.fiber_count(), 1);
    }

    #[test]
    fn cpp_fiber_008_apply_failure_marks_failed_and_rolls_back_effects_lifo() {
        let mut host = FiberHost::new();
        let log = log();
        let err = host
            .load(FiberSpec::new("demo.broken"), {
                let log = Arc::clone(&log);
                move |ctx| {
                    ctx.on_cleanup("first", record(&log, "first"));
                    ctx.on_cleanup("second", record(&log, "second"));
                    Err(PluginError::Invalid("boom".to_owned()))
                }
            })
            .unwrap_err();
        assert_eq!(err.code(), "E_INTERNAL");
        assert_eq!(labels(&log), vec!["second", "first"], "失败必须逆序回滚");
        let fiber = host.fiber("demo.broken").unwrap();
        assert_eq!(fiber.state(), FiberState::Failed);
        assert_eq!(fiber.failure(), Some(&err));
        assert_eq!(
            fiber.last_dispose().unwrap().labels(),
            vec!["second", "first"]
        );
        assert_eq!(host.fiber_state("demo.broken"), Some(FiberState::Failed));
    }

    #[test]
    fn cpp_fiber_009_unload_disposes_effects_lifo_and_is_terminal() {
        let mut host = FiberHost::new();
        let log = log();
        host.load(FiberSpec::new("demo.unload"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("a", record(&log, "a"));
                ctx.on_cleanup("b", record(&log, "b"));
                ctx.on_cleanup("c", record(&log, "c"));
                Ok(())
            }
        })
        .unwrap();
        assert!(labels(&log).is_empty(), "Effect 只在卸载/回滚时执行");

        let report = host.unload("demo.unload").unwrap();
        assert_eq!(report.effects.labels(), vec!["c", "b", "a"]);
        assert!(report.effects.all_ok());
        assert_eq!(labels(&log), vec!["c", "b", "a"]);
        assert_eq!(host.fiber_state("demo.unload"), Some(FiberState::Disposed));
        assert_eq!(report.generation, 1);
    }

    #[test]
    fn cpp_fiber_010_second_unload_is_not_loaded_and_does_not_rerun_effects() {
        let mut host = FiberHost::new();
        let log = log();
        host.load(FiberSpec::new("demo.unload-twice"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("only", record(&log, "only"));
                Ok(())
            }
        })
        .unwrap();
        host.unload("demo.unload-twice").unwrap();
        assert_eq!(labels(&log), vec!["only"]);

        let err = host.unload("demo.unload-twice").unwrap_err();
        assert_eq!(
            err,
            PluginError::NotLoaded {
                plugin_id: "demo.unload-twice".to_owned()
            }
        );
        assert_eq!(labels(&log), vec!["only"], "重复卸载不得重跑 Effect");
        assert_eq!(
            host.unload("demo.never-existed").unwrap_err(),
            PluginError::NotLoaded {
                plugin_id: "demo.never-existed".to_owned()
            }
        );
    }

    #[test]
    fn cpp_fiber_011_reload_after_dispose_gets_new_generation() {
        let mut host = FiberHost::new();
        let first = host
            .load(FiberSpec::new("demo.reload"), |_ctx| Ok(()))
            .unwrap();
        host.unload("demo.reload").unwrap();
        let second = host
            .load(FiberSpec::new("demo.reload"), |_ctx| Ok(()))
            .unwrap();
        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(second.fiber_id, first.fiber_id + 1);
        assert_eq!(host.fiber_state("demo.reload"), Some(FiberState::Active));
    }

    #[test]
    fn cpp_fiber_012_missing_required_service_fails_then_retry_succeeds() {
        let mut host = FiberHost::new();
        let spec = dependency_consumer("demo.needs-storage", &["storage/v1"]);
        let err = host.load(spec, |_ctx| Ok(())).unwrap_err();
        assert_eq!(
            err,
            PluginError::ServiceMissing {
                key: "storage/v1".to_owned()
            }
        );
        assert_eq!(err.code(), "E_SERVICE_MISSING");
        assert!(err.is_retryable());
        let fiber = host.fiber("demo.needs-storage").unwrap();
        assert_eq!(fiber.state(), FiberState::Failed);
        assert_eq!(fiber.failure(), Some(&err));
        let failed_generation = fiber.generation();

        // 服务到位后允许重试（新 generation），无需先卸载 Failed Fiber。
        load_provider(&mut host, "demo.storage", "storage/v1");
        let report = host
            .load(
                dependency_consumer("demo.needs-storage", &["storage/v1"]),
                |_ctx| Ok(()),
            )
            .unwrap();
        assert!(report.generation > failed_generation);
        assert_eq!(
            host.fiber_state("demo.needs-storage"),
            Some(FiberState::Active)
        );
    }

    #[test]
    fn cpp_fiber_013_apply_timeout_rolls_back_and_marks_failed() {
        let mut host = FiberHost::new();
        host.set_apply_timeout(Duration::from_millis(0));
        let log = log();
        let err = host
            .load(FiberSpec::new("demo.slow"), {
                let log = Arc::clone(&log);
                move |ctx| {
                    ctx.on_cleanup("rollback", record(&log, "rollback"));
                    thread::sleep(Duration::from_millis(5));
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(err.code(), "E_APPLY_TIMEOUT");
        assert!(err.is_retryable());
        assert_eq!(labels(&log), vec!["rollback"]);
        assert_eq!(host.fiber_state("demo.slow"), Some(FiberState::Failed));
    }

    #[test]
    fn cpp_fiber_014_missing_capability_fails_apply_and_rolls_back() {
        let mut host = FiberHost::new(); // 空 capability 集
        let log = log();
        let err = host
            .load(FiberSpec::new("demo.no-cap"), {
                let log = Arc::clone(&log);
                move |ctx| {
                    ctx.on_cleanup("rollback", record(&log, "rollback"));
                    ctx.require_capability(capability::TOOL_STREAM)?;
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(
            err,
            PluginError::CapabilityMissing {
                capability: capability::TOOL_STREAM.to_owned()
            }
        );
        assert_eq!(err.code(), "E_PERMISSION");
        assert_eq!(labels(&log), vec!["rollback"]);
        assert_eq!(host.fiber_state("demo.no-cap"), Some(FiberState::Failed));
    }

    #[test]
    fn cpp_fiber_015_host_v1_capabilities_allow_apply() {
        let mut host = FiberHost::with_capabilities(CapabilitySet::host_v1());
        let report = host
            .load(FiberSpec::new("demo.with-cap"), |ctx| {
                assert!(ctx.has_capability(capability::UI_CONTRIBUTE));
                ctx.require_capability(capability::UI_CONTRIBUTE)?;
                assert!(!ctx.is_cancelled());
                Ok(())
            })
            .unwrap();
        assert_eq!(report.state, FiberState::Active);
        assert_eq!(host.capabilities().len(), capability::ALL_V1.len());
    }

    #[test]
    fn cpp_fiber_016_load_with_manifest_uses_manifest_id_and_inject() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.storage", "storage/v1");
        let manifest = manifest_with_inject(&["storage/v1"], &["telemetry/v1"]);
        let report = host.load_with_manifest(&manifest, |_ctx| Ok(())).unwrap();
        assert_eq!(report.state, FiberState::Active);
        let fiber = host
            .fiber("demo.manifest")
            .expect("manifest id 即 fiber id");
        assert_eq!(fiber.required_services(), &[key("storage/v1")]);
        assert_eq!(fiber.optional_services(), &[key("telemetry/v1")]);
        assert_eq!(
            host.dependent_plugins(&key("storage/v1")),
            vec!["demo.manifest".to_owned()]
        );
    }

    #[test]
    fn cpp_fiber_017_fiber_spec_from_manifest_validates_service_keys() {
        let manifest = manifest_with_inject(&["storage/v1"], &["optional/v2"]);
        let spec = FiberSpec::from_manifest(&manifest).unwrap();
        assert_eq!(spec.plugin_id(), "demo.manifest");
        assert_eq!(spec.required(), &[key("storage/v1")]);
        assert_eq!(spec.optional(), &[key("optional/v2")]);

        let bad = manifest_with_inject(&["Bad Key"], &[]);
        let err = FiberSpec::from_manifest(&bad).unwrap_err();
        assert_eq!(err.code(), "E_MANIFEST");
        assert!(matches!(err, PluginError::InvalidServiceKey { .. }));
    }

    #[test]
    fn cpp_fiber_018_load_rejects_manifest_service_missing_before_apply_runs() {
        let mut host = FiberHost::new();
        let manifest = manifest_with_inject(&["storage/v1"], &[]);
        let mut applied = false;
        let err = host
            .load_with_manifest(&manifest, |_ctx| {
                applied = true;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code(), "E_SERVICE_MISSING");
        assert!(!applied, "必需服务缺失时不得执行 apply");
    }

    #[test]
    fn cpp_fiber_019_fiber_records_public_accessors() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.provider", "kv/v1");
        let fiber = host.fiber("demo.provider").unwrap();
        assert_eq!(fiber.published_services(), &[key("kv/v1")]);
        assert_eq!(fiber.effects().len(), 0);
        assert_eq!(fiber.resumed_count(), 0);
        assert!(fiber.pause_reason().is_none());
        assert!(host.fiber("nope").is_none());
        assert_eq!(host.fiber_state("nope"), None);
        assert_eq!(host.fibers().count(), 1);
    }

    #[test]
    fn cpp_fiber_020_loading_and_active_fibers_block_reload() {
        let mut host = FiberHost::new();
        host.load(FiberSpec::new("demo.block"), |_ctx| Ok(()))
            .unwrap();
        assert!(matches!(
            host.load(FiberSpec::new("demo.block"), |_ctx| Ok(())),
            Err(PluginError::AlreadyLoaded { .. })
        ));
        // Failed 状态允许重试（见 CPP-FIBER-012），Disposed 允许重装（见 CPP-FIBER-011）。
    }

    #[test]
    fn cpp_fiber_021_apply_can_publish_services_and_inspect_optional_ones() {
        let mut host = FiberHost::new();
        let report = host
            .load(FiberSpec::new("demo.publish"), |ctx| {
                let published = ctx.publish("tool-registry/v1", Arc::new(9u32))?;
                assert_eq!(published.as_str(), "tool-registry/v1");
                assert!(ctx.try_get::<String>("storage/v1").is_none());
                assert_eq!(ctx.fiber_id(), 1);
                assert_eq!(ctx.generation(), 1);
                assert_eq!(ctx.plugin_id(), "demo.publish");
                let _ = ctx.on_cleanup("noop", || {});
                Ok(())
            })
            .unwrap();
        assert_eq!(report.state, FiberState::Active);
        assert!(host.services().contains(&key("tool-registry/v1")));
        assert_eq!(
            host.services().get::<u32>(&key("tool-registry/v1")),
            Some(Arc::new(9))
        );
        assert_eq!(
            host.services().provider(&key("tool-registry/v1")).unwrap(),
            &provider("demo.publish", 1, 1)
        );
    }

    #[test]
    fn cpp_fiber_022_cancellation_token_is_exposed_to_apply() {
        let mut host = FiberHost::new();
        host.load(FiberSpec::new("demo.cancel"), |ctx| {
            assert!(!ctx.cancellation().is_cancelled(), "装载期间未取消");
            ctx.cancellation().cancel();
            assert!(ctx.is_cancelled());
            Ok(())
        })
        .unwrap();
        assert_eq!(host.fiber_state("demo.cancel"), Some(FiberState::Active));
    }

    // ------------------------------------------------------------------
    // CPP-SVC-*：Service key / 服务表 / 依赖暂停恢复
    // ------------------------------------------------------------------

    #[test]
    fn cpp_svc_001_service_key_parses_name_and_major() {
        let parsed = ServiceKey::parse("tool-registry/v12").unwrap();
        assert_eq!(parsed.name(), "tool-registry");
        assert_eq!(parsed.major(), 12);
        assert_eq!(parsed.as_str(), "tool-registry/v12");
        assert_eq!(parsed.to_string(), "tool-registry/v12");
        assert_eq!(
            "storage/v1".parse::<ServiceKey>().unwrap(),
            key("storage/v1")
        );
    }

    #[test]
    fn cpp_svc_002_service_key_rejects_invalid_shapes() {
        for raw in [
            "",
            "storage",
            "storage/v",
            "storage/vX",
            "storage/v-1",
            "Storage/v1",
            "stor age/v1",
            "storage/v1/extra",
            "storage/v99999999999999999999",
        ] {
            let err = ServiceKey::parse(raw).expect_err("非法 key 必须拒绝");
            assert!(
                matches!(err, PluginError::InvalidServiceKey { .. }),
                "`{raw}` 应返回 InvalidServiceKey"
            );
            assert_eq!(err.code(), "E_MANIFEST");
        }
    }

    #[test]
    fn cpp_svc_003_publish_get_and_type_erase() {
        let mut table = ServiceTable::new();
        assert!(table.is_empty());
        table
            .publish(
                key("storage/v1"),
                Arc::new(String::from("persisted")),
                provider("demo.p", 1, 1),
            )
            .unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.contains(&key("storage/v1")));
        assert_eq!(
            table.get::<String>(&key("storage/v1")).as_deref(),
            Some(&String::from("persisted"))
        );
        assert!(table.get::<u32>(&key("storage/v1")).is_none());
        assert!(table.get_erased(&key("storage/v1")).is_some());
        assert!(table.get::<String>(&key("missing/v1")).is_none());
        assert_eq!(
            table.keys().map(ServiceKey::as_str).collect::<Vec<_>>(),
            vec!["storage/v1"]
        );
    }

    #[test]
    fn cpp_svc_004_duplicate_key_is_conflict() {
        let mut table = ServiceTable::new();
        let owner = provider("demo.a", 1, 1);
        table
            .publish(key("storage/v1"), Arc::new(1u32), owner.clone())
            .unwrap();
        let err = table
            .publish(key("storage/v1"), Arc::new(2u32), owner)
            .unwrap_err();
        assert_eq!(
            err,
            PluginError::ServiceConflict {
                key: "storage/v1".to_owned()
            }
        );
        assert_eq!(err.code(), "E_SERVICE_CONFLICT");
        assert!(!err.is_retryable());
        assert_eq!(table.get::<u32>(&key("storage/v1")), Some(Arc::new(1)));
    }

    #[test]
    fn cpp_svc_005_revoke_is_idempotent() {
        let mut table = ServiceTable::new();
        table
            .publish(key("kv/v1"), Arc::new(()), provider("demo.a", 1, 1))
            .unwrap();
        assert!(table.revoke(&key("kv/v1")));
        assert!(!table.revoke(&key("kv/v1")));
        assert!(table.is_empty());
    }

    #[test]
    fn cpp_svc_006_revoke_if_owned_only_removes_matching_provider() {
        let mut table = ServiceTable::new();
        table
            .publish(key("kv/v1"), Arc::new(()), provider("demo.a", 1, 1))
            .unwrap();
        assert!(!table.revoke_if_owned(&key("kv/v1"), &provider("demo.b", 2, 2)));
        assert!(table.contains(&key("kv/v1")));
        assert!(table.revoke_if_owned(&key("kv/v1"), &provider("demo.a", 1, 1)));
        assert!(table.is_empty());
    }

    #[test]
    fn cpp_svc_007_apply_failure_rolls_back_published_services() {
        let mut host = FiberHost::new();
        let err = host
            .load(FiberSpec::new("demo.bad-provider"), |ctx| {
                ctx.publish("kv/v1", Arc::new(1u32))?;
                Err(PluginError::Invalid("boom".to_owned()))
            })
            .unwrap_err();
        assert_eq!(err.code(), "E_INTERNAL");
        assert!(host.services().is_empty(), "失败装载不得留下服务");
        assert_eq!(
            host.fiber_state("demo.bad-provider"),
            Some(FiberState::Failed)
        );
    }

    #[test]
    fn cpp_svc_008_unload_revokes_published_services() {
        let mut host = FiberHost::new();
        host.load(FiberSpec::new("demo.unpublish"), |ctx| {
            ctx.publish("a/v1", Arc::new(1u32))?;
            ctx.publish("b/v1", Arc::new(2u32))?;
            Ok(())
        })
        .unwrap();
        let report = host.unload("demo.unpublish").unwrap();
        assert_eq!(
            report.revoked_services,
            vec![key("a/v1"), key("b/v1")],
            "撤销顺序应与发布顺序一致（服务发布不是 Effect）"
        );
        assert!(host.services().is_empty());
    }

    #[test]
    fn cpp_svc_009_revoke_service_pauses_dependent_fiber() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.store", "storage/v1");
        host.load(
            dependency_consumer("demo.reader", &["storage/v1"]),
            |_ctx| Ok(()),
        )
        .unwrap();
        assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Active));

        let paused = host.revoke_service(&key("storage/v1"));
        assert_eq!(paused, vec!["demo.reader".to_owned()]);
        assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Pending));
        assert_eq!(
            host.fiber("demo.reader").unwrap().pause_reason(),
            Some(&PauseReason::ServiceMissing(key("storage/v1")))
        );
        assert_eq!(
            host.dependent_plugins(&key("storage/v1")),
            vec!["demo.reader".to_owned()]
        );
    }

    #[test]
    fn cpp_svc_010_provider_unload_pauses_and_republish_resumes_dependent() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.store", "storage/v1");
        host.load(
            dependency_consumer("demo.reader", &["storage/v1"]),
            |_ctx| Ok(()),
        )
        .unwrap();

        let report = host.unload("demo.store").unwrap();
        assert_eq!(report.paused_dependents, vec!["demo.reader".to_owned()]);
        assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Pending));

        // 服务恢复 -> 依赖 Fiber 自动回到 Active（最小切片：状态迁移，不重跑 apply）。
        load_provider(&mut host, "demo.store", "storage/v1");
        assert_eq!(host.fiber_state("demo.reader"), Some(FiberState::Active));
        assert_eq!(host.fiber("demo.reader").unwrap().resumed_count(), 1);
        assert!(host.fiber("demo.reader").unwrap().pause_reason().is_none());
    }

    #[test]
    fn cpp_svc_011_resume_requires_all_required_services() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.a", "a/v1");
        load_provider(&mut host, "demo.b", "b/v1");
        host.load(
            dependency_consumer("demo.both", &["a/v1", "b/v1"]),
            |_ctx| Ok(()),
        )
        .unwrap();
        assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Active));

        host.unload("demo.a").unwrap();
        host.unload("demo.b").unwrap();
        assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Pending));

        load_provider(&mut host, "demo.a", "a/v1");
        assert_eq!(
            host.fiber_state("demo.both"),
            Some(FiberState::Pending),
            "只恢复一个服务时不得激活"
        );
        load_provider(&mut host, "demo.b", "b/v1");
        assert_eq!(host.fiber_state("demo.both"), Some(FiberState::Active));
    }

    #[test]
    fn cpp_svc_012_optional_service_missing_does_not_block() {
        let mut host = FiberHost::new();
        let mut spec = FiberSpec::new("demo.optional");
        spec.add_optional("telemetry/v1").unwrap();
        let mut saw_optional = false;
        host.load(spec, |ctx| {
            saw_optional = ctx.try_get::<String>("telemetry/v1").is_none();
            Ok(())
        })
        .unwrap();
        assert!(saw_optional);
        assert_eq!(host.fiber_state("demo.optional"), Some(FiberState::Active));
    }

    #[test]
    fn cpp_svc_013_dependency_edges_are_cleared_on_unload() {
        let mut host = FiberHost::new();
        load_provider(&mut host, "demo.store", "storage/v1");
        host.load(
            dependency_consumer("demo.reader", &["storage/v1"]),
            |_ctx| Ok(()),
        )
        .unwrap();
        assert_eq!(
            host.dependent_plugins(&key("storage/v1")),
            vec!["demo.reader".to_owned()]
        );

        host.unload("demo.reader").unwrap();
        assert!(host.dependent_plugins(&key("storage/v1")).is_empty());
        assert!(host.services().contains(&key("storage/v1")));
    }

    #[test]
    fn cpp_svc_014_same_fiber_cannot_publish_same_key_twice() {
        let mut host = FiberHost::new();
        let err = host
            .load(FiberSpec::new("demo.twice"), |ctx| {
                ctx.publish("kv/v1", Arc::new(1u32))?;
                ctx.publish("kv/v1", Arc::new(2u32))?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.code(), "E_SERVICE_CONFLICT");
        assert!(host.services().is_empty(), "失败装载必须回滚已发布服务");
    }

    // ------------------------------------------------------------------
    // CPP-EFF-*：Effect 栈（LIFO / 幂等 / 失败与超时隔离）
    // ------------------------------------------------------------------

    #[test]
    fn cpp_eff_001_dispose_runs_strictly_lifo() {
        let mut stack = EffectStack::new();
        let log = log();
        stack.push("first", record(&log, "first"));
        stack.push("second", record(&log, "second"));
        stack.push("third", record(&log, "third"));
        assert_eq!(stack.labels(), vec!["first", "second", "third"]);
        assert_eq!(stack.len(), 3);

        let report = stack.dispose();
        assert_eq!(labels(&log), vec!["third", "second", "first"]);
        assert_eq!(report.labels(), vec!["third", "second", "first"]);
        assert!(report.all_ok());
        assert_eq!(report.failed(), 0);
        assert_eq!(report.timed_out(), 0);
    }

    #[test]
    fn cpp_eff_002_dispose_is_idempotent() {
        let mut stack = EffectStack::new();
        let log = log();
        stack.push("only", record(&log, "only"));
        assert!(!stack.is_disposed());
        let first = stack.dispose();
        assert_eq!(first.len(), 1);
        assert!(stack.is_disposed());
        assert!(stack.is_empty());

        let second = stack.dispose();
        assert!(second.is_empty(), "第二次 dispose 不得重复执行");
        assert_eq!(labels(&log), vec!["only"]);
    }

    #[test]
    fn cpp_eff_003_single_panic_does_not_interrupt_cleanup() {
        let mut stack = EffectStack::new();
        let log = log();
        stack.push("before", record(&log, "before"));
        stack.push("panics", || panic!("cleanup exploded"));
        stack.push("after", record(&log, "after"));

        let report = stack.dispose();
        assert_eq!(
            labels(&log),
            vec!["after", "before"],
            "panic 之后的 Effect 仍必须执行"
        );
        assert_eq!(report.labels(), vec!["after", "panics", "before"]);
        assert_eq!(report.failed(), 1);
        assert!(!report.all_ok());
        match &report.entries()[1].outcome {
            EffectOutcome::Panicked(message) => assert!(message.contains("cleanup exploded")),
            other => panic!("预期 Panicked，实际 {other:?}"),
        }
    }

    #[test]
    fn cpp_eff_004_single_timeout_is_recorded_and_cleanup_continues() {
        let mut stack = EffectStack::with_timeout(Duration::from_millis(10));
        let log = log();
        stack.push("slow", {
            let log = Arc::clone(&log);
            move || {
                thread::sleep(Duration::from_millis(250));
                log.lock().unwrap().push("slow".to_owned());
            }
        });
        stack.push("quick", record(&log, "quick"));

        let report = stack.dispose();
        assert_eq!(report.timed_out(), 1);
        assert_eq!(report.labels(), vec!["quick", "slow"]);
        assert_eq!(labels(&log), vec!["quick"], "超时后必须继续清理下一个");
        assert!(report.entries()[1].outcome.is_timed_out());
    }

    #[test]
    fn cpp_eff_005_effect_ids_and_registration_labels_are_recorded() {
        let mut stack = EffectStack::new();
        let first = stack.push("a", || {});
        let second = stack.push("b", || {});
        assert!(second > first);
        assert_eq!(stack.labels(), vec!["a", "b"]);
        let report = stack.dispose();
        assert_eq!(report.entries()[0].id, second);
        assert_eq!(report.entries()[0].label, "b");
        assert_eq!(report.entries()[1].id, first);
    }

    #[test]
    fn cpp_eff_006_empty_dispose_is_ok() {
        let mut stack = EffectStack::new();
        let report = stack.dispose();
        assert!(report.is_empty());
        assert!(report.all_ok());
        assert!(stack.is_disposed());
    }

    #[test]
    fn cpp_eff_007_timeout_is_configurable() {
        let mut stack = EffectStack::with_timeout(Duration::from_millis(50));
        assert_eq!(stack.timeout(), Duration::from_millis(50));
        stack.set_timeout(Duration::from_millis(75));
        assert_eq!(stack.timeout(), Duration::from_millis(75));
        // 默认值来自 spec §6.3。
        assert_eq!(
            EffectStack::new().timeout(),
            Duration::from_millis(EFFECT_DISPOSE_TIMEOUT_MS)
        );
    }

    #[test]
    fn cpp_eff_008_apply_failure_dispose_report_is_kept_on_fiber() {
        let mut host = FiberHost::new();
        let log = log();
        let _ = host
            .load(FiberSpec::new("demo.report"), {
                let log = Arc::clone(&log);
                move |ctx| {
                    ctx.on_cleanup("one", record(&log, "one"));
                    ctx.on_cleanup("two", record(&log, "two"));
                    Err(PluginError::Invalid("rollback".to_owned()))
                }
            })
            .unwrap_err();
        let report = host
            .fiber("demo.report")
            .unwrap()
            .last_dispose()
            .expect("失败装载必须留下回滚报告");
        assert!(report.all_ok());
        assert_eq!(report.labels(), vec!["two", "one"]);
        assert_eq!(labels(&log), vec!["two", "one"]);
    }

    #[test]
    fn cpp_eff_009_unload_report_is_all_ok_for_cooperative_effects() {
        let mut host = FiberHost::new();
        host.load(FiberSpec::new("demo.effects"), |ctx| {
            ctx.on_cleanup("x", || {});
            ctx.on_cleanup("y", || {});
            Ok(())
        })
        .unwrap();
        let report = host.unload("demo.effects").unwrap();
        assert_eq!(report.effects.len(), 2);
        assert!(report.effects.all_ok());
        assert_eq!(report.effects.labels(), vec!["y", "x"]);
    }

    #[test]
    fn cpp_eff_010_effects_of_different_fibers_are_independent() {
        let mut host = FiberHost::new();
        let log = log();
        host.load(FiberSpec::new("demo.one"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("one", record(&log, "one"));
                Ok(())
            }
        })
        .unwrap();
        host.load(FiberSpec::new("demo.two"), {
            let log = Arc::clone(&log);
            move |ctx| {
                ctx.on_cleanup("two", record(&log, "two"));
                Ok(())
            }
        })
        .unwrap();

        host.unload("demo.one").unwrap();
        assert_eq!(labels(&log), vec!["one"], "不得触碰其它 Fiber 的 Effect");
        host.unload("demo.two").unwrap();
        assert_eq!(labels(&log), vec!["one", "two"]);
    }
}
