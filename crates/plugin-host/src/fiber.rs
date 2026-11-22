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

mod effects;
mod services;

pub use effects::{DisposeReport, EffectDisposal, EffectOutcome, EffectStack};
pub use services::{ServiceKey, ServiceProvider, ServiceTable};

#[cfg(test)]
mod tests;
