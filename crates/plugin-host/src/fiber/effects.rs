use super::*;

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
