//! 工具审批：worker 线程阻塞等待 UI 决定，UI 通过轮询拿 pending 列表。
//!
//! 为什么不用 channel 回调：工具执行发生在 Agent 的同步循环里（工作线程），
//! 而审批发生在 UI 线程；用一个独立的 `Mutex + Condvar` 比跨线程 channel 更好
//! 处理超时与"窗口已关闭"的情况。`ChatState` 的大锁不覆盖这里，UI 读 pending 不会卡住。
//!
//! 非交互环境（自检/CI）用 `DSHR_APPROVAL_MODE=allow|deny` 自动决定，不再等待。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::PolicyDecision;

/// 轮询停止信号与当前权限，避免没有手动答复时一直等待。
const APPROVAL_POLL_INTERVAL: Duration = Duration::from_millis(100);

enum ApprovalSource<'a> {
    Tool,
    LivePolicy(LiveApproval<'a>),
    UserOnly,
}

pub struct LiveApproval<'a> {
    pub decide: &'a dyn Fn() -> PolicyDecision,
    pub grant_key: Option<&'a str>,
}

/// 一条等待用户确认的工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    /// 请求时间（Unix 秒），UI 可显示"等待了多久"。
    pub requested_at: u64,
    request_id: u64,
    grant_key: Option<String>,
}

impl PendingApproval {
    pub fn supports_always_allow(&self) -> bool {
        self.grant_key.is_some()
    }
}

/// 审批结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    AllowOnce,
    AllowAlways,
    Deny,
}

impl ApprovalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::AllowAlways => "allow_always",
            Self::Deny => "deny",
        }
    }
}

/// 审批痕迹：一轮结束后由 chat 落成 `approval.asked` / `approval.answered` 事件。
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRecord {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub outcome: ApprovalOutcome,
    pub answerer: String,
}

#[derive(Default)]
struct BrokerState {
    pending: Vec<PendingApproval>,
    decisions: HashMap<String, ApprovalOutcome>,
    records: Vec<ApprovalRecord>,
    next_request_id: u64,
    grants: HashSet<String>,
}

/// 审批 broker：工作线程 `request` 阻塞，UI 线程 `resolve` 唤醒。
pub struct ApprovalBroker {
    state: Mutex<BrokerState>,
    wake: Condvar,
    noninteractive: bool,
}

impl ApprovalBroker {
    pub fn new() -> Arc<Self> {
        Self::with_interaction(true)
    }

    /// Headless runs have no approval answerer.
    pub fn noninteractive() -> Arc<Self> {
        Self::with_interaction(false)
    }

    fn with_interaction(interactive: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BrokerState::default()),
            wake: Condvar::new(),
            noninteractive: !interactive,
        })
    }

    /// 当前等待确认的调用（UI 轮询）。
    pub fn pending(&self) -> Vec<PendingApproval> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .clone()
    }

    /// 工作线程调用：阻塞直到 UI 决定、环境自动决定或超时（超时按拒绝处理）。
    ///
    /// 签名保持不变；内部等价于 [`ApprovalBroker::request_cancellable`] 但不响应取消。
    pub fn request(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
    ) -> ApprovalOutcome {
        self.request_inner(
            call_id,
            tool,
            arguments,
            timeout,
            None,
            ApprovalSource::Tool,
        )
    }

    /// 可取消的审批：等待期间定期检查 `cancel`。
    ///
    /// `cancel` 置位时立刻移除 pending、记一条 `answerer = "cancelled"` 的 Deny
    /// 痕迹并返回 Deny——用户按"停止"后不会继续卡在最多 300s 的审批条上
    /// （M2 review P1-2 / P2-6）。
    pub fn request_cancellable(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> ApprovalOutcome {
        self.request_inner(
            call_id,
            tool,
            arguments,
            timeout,
            Some(cancel),
            ApprovalSource::Tool,
        )
    }

    /// Reevaluate a tool's policy while it waits. A manual answer already submitted
    /// takes priority. The policy callback must not call back into the broker.
    pub fn request_with_policy(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
        cancel: &AtomicBool,
        policy: LiveApproval<'_>,
    ) -> ApprovalOutcome {
        self.request_inner(
            call_id,
            tool,
            arguments,
            timeout,
            Some(cancel),
            ApprovalSource::LivePolicy(policy),
        )
    }

    /// Requires a decision from the interactive user, including in automation environments.
    pub fn request_user_cancellable(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> ApprovalOutcome {
        self.request_inner(
            call_id,
            tool,
            arguments,
            timeout,
            Some(cancel),
            ApprovalSource::UserOnly,
        )
    }

    fn request_inner(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        source: ApprovalSource<'_>,
    ) -> ApprovalOutcome {
        if self.noninteractive {
            self.record(call_id, tool, arguments, ApprovalOutcome::Deny, "headless");
            return ApprovalOutcome::Deny;
        }
        // 非交互模式：自检/CI 不应卡在审批上。
        if let Some(mode) = std::env::var("DSHR_APPROVAL_MODE")
            .ok()
            .filter(|_| !matches!(source, ApprovalSource::UserOnly))
        {
            let outcome = match mode.as_str() {
                "allow" => ApprovalOutcome::AllowOnce,
                "deny" => ApprovalOutcome::Deny,
                _ => ApprovalOutcome::Deny,
            };
            self.record(call_id, tool, arguments, outcome, "auto");
            return outcome;
        }

        let grant_key = match &source {
            ApprovalSource::LivePolicy(policy) => policy.grant_key,
            _ => None,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_request_id += 1;
        let request_id = state.next_request_id;
        state.pending.push(PendingApproval {
            call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            requested_at: unix_secs(),
            request_id,
            grant_key: grant_key.map(str::to_owned),
        });

        let deadline = Instant::now() + timeout;
        let (outcome, answerer) = loop {
            if let Some(outcome) = state.decisions.remove(call_id) {
                break (outcome, "user");
            }
            if cancel.is_some_and(|token| token.load(Ordering::Relaxed)) {
                break (ApprovalOutcome::Deny, "cancelled");
            }
            let now = Instant::now();
            if now >= deadline {
                break (ApprovalOutcome::Deny, "timeout");
            }
            if let ApprovalSource::LivePolicy(policy) = &source {
                match (policy.decide)() {
                    PolicyDecision::Allow => break (ApprovalOutcome::AllowOnce, "permission_mode"),
                    PolicyDecision::Deny(_) => break (ApprovalOutcome::Deny, "permission_mode"),
                    PolicyDecision::Ask => {}
                }
            }
            if grant_key.is_some_and(|key| state.grants.contains(key)) {
                break (ApprovalOutcome::AllowOnce, "tool_grant");
            }
            let wait = (deadline - now).min(APPROVAL_POLL_INTERVAL);
            let (guard, _) = self
                .wake
                .wait_timeout(state, wait)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = guard;
        };
        state.pending.retain(|item| item.call_id != call_id);
        drop(state);
        self.record(call_id, tool, arguments, outcome, answerer);
        outcome
    }

    /// UI 线程调用：允许一次 / 拒绝。
    ///
    /// 只接受**仍在 pending** 的 call_id：过期或跨轮重复的 call_id 不能写入决定，
    /// 否则下一轮同 id 的调用会吃到旧决定被误放行（M2 review P2-6）。
    pub fn resolve(&self, call_id: &str, outcome: ApprovalOutcome) {
        if outcome == ApprovalOutcome::AllowAlways {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.pending.iter().any(|item| item.call_id == call_id) {
            return;
        }
        state.decisions.insert(call_id.to_owned(), outcome);
        self.wake.notify_all();
    }

    /// Only the currently pending, unanswered ordinary request can issue a grant.
    pub fn always_allow_key(&self, pending: &PendingApproval) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.decisions.contains_key(&pending.call_id) || !state.pending.contains(pending) {
            return None;
        }
        pending.grant_key.clone()
    }

    pub fn restore_grants(&self, keys: impl IntoIterator<Item = String>) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .grants
            .extend(keys);
        self.wake.notify_all();
    }

    /// The desktop calls this after the user-selected grant has been saved.
    pub fn resolve_always(&self, pending: &PendingApproval) {
        let Some(key) = pending.grant_key.clone() else {
            return;
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.grants.insert(key);
        if state.pending.contains(pending) {
            state
                .decisions
                .entry(pending.call_id.clone())
                .or_insert(ApprovalOutcome::AllowAlways);
        }
        self.wake.notify_all();
    }

    /// 清空所有 pending（停止生成 / turn 结束时清理），返回被清理的条数。
    ///
    /// 不写 decisions：被取消的调用由 `request_cancellable` 自己在退出路径记
    /// `cancelled` 痕迹，避免"清理"变成一次隐式放行。
    pub fn cancel_pending(&self) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = state.pending.len();
        state.pending.clear();
        drop(state);
        if count > 0 {
            // 唤醒等待者重新检查取消/超时（它们拿不到决定）。
            self.wake.notify_all();
        }
        count
    }

    /// 一轮结束后取走审批痕迹（由 chat 落盘）。
    pub fn take_records(&self) -> Vec<ApprovalRecord> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut state.records)
    }

    fn record(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        outcome: ApprovalOutcome,
        answerer: &str,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.records.push(ApprovalRecord {
            call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            outcome,
            answerer: answerer.to_owned(),
        });
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `DSHR_APPROVAL_MODE` 是进程级环境变量：与超时测试互斥，避免并发互相影响。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_from_another_thread_unblocks_request() {
        let _guard = ENV_LOCK.lock().unwrap();
        let broker = ApprovalBroker::new();
        let requester = broker.clone();
        let handle = std::thread::spawn(move || {
            requester.request(
                "c1",
                "write",
                &json!({ "path": "a.txt" }),
                Duration::from_secs(5),
            )
        });

        // 等 pending 出现，再 resolve。
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let pending = broker.pending();
            if let Some(item) = pending.first() {
                assert_eq!(item.call_id, "c1");
                assert_eq!(item.tool, "write");
                broker.resolve("c1", ApprovalOutcome::AllowOnce);
                break;
            }
            assert!(Instant::now() < deadline, "pending 未出现");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(handle.join().unwrap(), ApprovalOutcome::AllowOnce);
        assert!(broker.pending().is_empty());
        let records = broker.take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, ApprovalOutcome::AllowOnce);
        assert_eq!(records[0].answerer, "user");
    }

    #[test]
    fn timeout_is_treated_as_deny_and_cleans_pending() {
        let _guard = ENV_LOCK.lock().unwrap();
        let broker = ApprovalBroker::new();
        let outcome = broker.request(
            "c-timeout",
            "bash",
            &json!({ "command": "rm -rf /" }),
            Duration::from_millis(30),
        );
        assert_eq!(outcome, ApprovalOutcome::Deny);
        assert!(broker.pending().is_empty());
        let records = broker.take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].answerer, "timeout");
    }

    #[test]
    fn env_mode_short_circuits_allow_and_deny() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("DSHR_APPROVAL_MODE", "allow");
        let broker = ApprovalBroker::new();
        let outcome = broker.request("c2", "edit", &json!({}), Duration::from_millis(10));
        assert_eq!(outcome, ApprovalOutcome::AllowOnce);
        let records = broker.take_records();
        assert_eq!(records[0].answerer, "auto");

        std::env::set_var("DSHR_APPROVAL_MODE", "deny");
        let outcome = broker.request("c3", "bash", &json!({}), Duration::from_millis(10));
        std::env::remove_var("DSHR_APPROVAL_MODE");
        assert_eq!(outcome, ApprovalOutcome::Deny);
    }

    fn wait_until_pending(broker: &ApprovalBroker, call_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if broker.pending().iter().any(|item| item.call_id == call_id) {
                return;
            }
            assert!(Instant::now() < deadline, "pending({call_id}) 未出现");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn pending_tool_reevaluates_while_user_only_approval_keeps_waiting() {
        let _guard = ENV_LOCK.lock().unwrap();
        let broker = ApprovalBroker::new();
        let explicit = broker.clone();
        let user_only = std::thread::spawn(move || {
            explicit.request_user_cancellable(
                "explicit",
                "creator_try_plugin",
                &json!({}),
                Duration::from_secs(5),
                &AtomicBool::new(false),
            )
        });
        wait_until_pending(&broker, "explicit");

        for (decision, expected) in [
            (PolicyDecision::Allow, ApprovalOutcome::AllowOnce),
            (
                PolicyDecision::Deny("read-only".to_owned()),
                ApprovalOutcome::Deny,
            ),
        ] {
            let policy = Arc::new(Mutex::new(PolicyDecision::Ask));
            let live_policy = policy.clone();
            let tool = broker.clone();
            let worker = std::thread::spawn(move || {
                tool.request_with_policy(
                    "ordinary",
                    "write",
                    &json!({}),
                    Duration::from_secs(3),
                    &AtomicBool::new(false),
                    LiveApproval {
                        decide: &|| live_policy.lock().unwrap().clone(),
                        grant_key: None,
                    },
                )
            });
            wait_until_pending(&broker, "ordinary");
            *policy.lock().unwrap() = decision;
            assert_eq!(worker.join().unwrap(), expected);
            let records = broker.take_records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].answerer, "permission_mode");
            let pending = broker.pending();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].call_id, "explicit");
        }

        broker.resolve("explicit", ApprovalOutcome::Deny);
        assert_eq!(user_only.join().unwrap(), ApprovalOutcome::Deny);
        assert_eq!(broker.take_records()[0].answerer, "user");
    }

    #[test]
    fn always_allow_reuses_only_the_saved_key_after_reopening() {
        let _guard = ENV_LOCK.lock().unwrap();
        let broker = ApprovalBroker::new();
        let requester = broker.clone();
        let active = std::thread::spawn(move || {
            requester.request_with_policy(
                "first",
                "write",
                &json!({}),
                Duration::from_secs(3),
                &AtomicBool::new(false),
                LiveApproval {
                    decide: &|| PolicyDecision::Ask,
                    grant_key: Some("session-a:write"),
                },
            )
        });
        wait_until_pending(&broker, "first");
        let pending = broker.pending().pop().unwrap();
        let saved = broker.always_allow_key(&pending).unwrap();
        broker.resolve_always(&pending);
        assert_eq!(active.join().unwrap(), ApprovalOutcome::AllowAlways);
        assert_eq!(broker.take_records()[0].outcome.as_str(), "allow_always");
        assert!(broker.always_allow_key(&pending).is_none());

        let reopened = ApprovalBroker::new();
        reopened.restore_grants([saved]);
        for (key, decision, outcome, answerer) in [
            (
                "session-a:write",
                PolicyDecision::Ask,
                ApprovalOutcome::AllowOnce,
                "tool_grant",
            ),
            (
                "session-a:edit",
                PolicyDecision::Ask,
                ApprovalOutcome::Deny,
                "timeout",
            ),
            (
                "session-b:write",
                PolicyDecision::Ask,
                ApprovalOutcome::Deny,
                "timeout",
            ),
            (
                "session-a:write",
                PolicyDecision::Deny("read-only".to_owned()),
                ApprovalOutcome::Deny,
                "permission_mode",
            ),
        ] {
            assert_eq!(
                reopened.request_with_policy(
                    "next",
                    "write",
                    &json!({}),
                    Duration::from_millis(50),
                    &AtomicBool::new(false),
                    LiveApproval {
                        decide: &|| decision.clone(),
                        grant_key: Some(key)
                    },
                ),
                outcome,
            );
            assert_eq!(reopened.take_records()[0].answerer, answerer);
            assert!(reopened.pending().is_empty());
        }
    }

    /// 停止生成后审批必须快速返回 Deny（不是继续等满 timeout），并清理 pending。
    #[test]
    fn request_cancellable_returns_deny_quickly_after_cancel() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let broker = ApprovalBroker::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let requester = broker.clone();
        let thread_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            requester.request_cancellable(
                "c-cancel",
                "bash",
                &json!({ "command": "sleep 300" }),
                Duration::from_secs(5),
                &thread_cancel,
            )
        });

        wait_until_pending(&broker, "c-cancel");
        let cancelled_at = Instant::now();
        cancel.store(true, Ordering::Relaxed);

        let outcome = handle.join().expect("requester thread");
        let elapsed = cancelled_at.elapsed();
        assert_eq!(outcome, ApprovalOutcome::Deny);
        assert!(
            elapsed < Duration::from_millis(300),
            "取消后应在 100ms 轮询周期内返回，实际 {elapsed:?}"
        );
        assert!(broker.pending().is_empty(), "取消后 pending 必须清空");
        let records = broker.take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].call_id, "c-cancel");
        assert_eq!(records[0].outcome, ApprovalOutcome::Deny);
        assert_eq!(records[0].answerer, "cancelled");
    }

    /// 没有 pending 时 resolve 必须被忽略：不能给后续同 call_id 的调用留"旧决定"。
    #[test]
    fn resolve_unknown_call_id_does_not_leak_a_decision() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let broker = ApprovalBroker::new();

        broker.resolve("ghost", ApprovalOutcome::AllowOnce);
        assert!(broker.pending().is_empty());
        assert!(
            broker.take_records().is_empty(),
            "未知 call_id 不得产生审批记录"
        );

        // 同一个 call_id 真正请求时仍应超时拒绝，而不是吃到上面那条 AllowOnce。
        let outcome = broker.request("ghost", "write", &json!({}), Duration::from_millis(30));
        assert_eq!(outcome, ApprovalOutcome::Deny);
        let records = broker.take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].answerer, "timeout");
    }

    /// 停止生成时清空 pending：返回数量、不写 decisions、等待者走自己的取消路径。
    #[test]
    fn cancel_pending_clears_count_without_writing_decisions() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let broker = ApprovalBroker::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let requester = broker.clone();
        let thread_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            requester.request_cancellable(
                "c-pending",
                "edit",
                &json!({}),
                Duration::from_secs(5),
                &thread_cancel,
            )
        });
        wait_until_pending(&broker, "c-pending");

        assert_eq!(broker.cancel_pending(), 1);
        assert!(broker.pending().is_empty());
        // pending 已空：后续 resolve 被忽略，等待者不会拿到 AllowOnce。
        broker.resolve("c-pending", ApprovalOutcome::AllowOnce);

        // 用取消位解除阻塞（cancel_pending 本身不写决定，不会让调用"被放行"）。
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(handle.join().unwrap(), ApprovalOutcome::Deny);
        let records = broker.take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, ApprovalOutcome::Deny);
        assert_eq!(records[0].answerer, "cancelled");
    }
}
