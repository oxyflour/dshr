//! 工具审批：worker 线程阻塞等待 UI 决定，UI 通过轮询拿 pending 列表。
//!
//! 为什么不用 channel 回调：工具执行发生在 Agent 的同步循环里（工作线程），
//! 而审批发生在 UI 线程；用一个独立的 `Mutex + Condvar` 比跨线程 channel 更好
//! 处理超时与"窗口已关闭"的情况。`ChatState` 的大锁不覆盖这里，UI 读 pending 不会卡住。
//!
//! 非交互环境（自检/CI）用 `DSHR_APPROVAL_MODE=allow|deny` 自动决定，不再等待。

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// 一条等待用户确认的工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingApproval {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    /// 请求时间（Unix 秒），UI 可显示"等待了多久"。
    pub requested_at: u64,
}

/// 审批结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    AllowOnce,
    Deny,
}

impl ApprovalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
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
}

/// 审批 broker：工作线程 `request` 阻塞，UI 线程 `resolve` 唤醒。
pub struct ApprovalBroker {
    state: Mutex<BrokerState>,
    wake: Condvar,
}

impl ApprovalBroker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BrokerState::default()),
            wake: Condvar::new(),
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
    pub fn request(
        &self,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
    ) -> ApprovalOutcome {
        // 非交互模式：自检/CI 不应卡在审批上。
        if let Ok(mode) = std::env::var("DSHR_APPROVAL_MODE") {
            let outcome = match mode.as_str() {
                "allow" => ApprovalOutcome::AllowOnce,
                "deny" => ApprovalOutcome::Deny,
                _ => ApprovalOutcome::Deny,
            };
            self.record(call_id, tool, arguments, outcome, "auto");
            return outcome;
        }

        let entry = PendingApproval {
            call_id: call_id.to_owned(),
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            requested_at: unix_secs(),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending.push(entry);

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(outcome) = state.decisions.remove(call_id) {
                state.pending.retain(|item| item.call_id != call_id);
                drop(state);
                self.record(call_id, tool, arguments, outcome, "user");
                return outcome;
            }
            let now = Instant::now();
            if now >= deadline {
                state.pending.retain(|item| item.call_id != call_id);
                drop(state);
                self.record(call_id, tool, arguments, ApprovalOutcome::Deny, "timeout");
                return ApprovalOutcome::Deny;
            }
            let (guard, _) = self
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = guard;
        }
    }

    /// UI 线程调用：允许一次 / 拒绝。
    pub fn resolve(&self, call_id: &str, outcome: ApprovalOutcome) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.decisions.insert(call_id.to_owned(), outcome);
        self.wake.notify_all();
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
}
