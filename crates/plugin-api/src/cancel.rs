//! 协作式取消令牌（spec/cpp-v1.md §7.4）。
//!
//! 设计约束：`plugin-api` **不引入 tokio/async 运行时**，所以这里只用 `std`：
//!
//! - 状态是 `Arc<AtomicBool>`，可跨线程克隆、无 `unsafe`；
//! - [`CancellationToken::cancelled`] 返回一个手写 [`Future`]，宿主/插件可以把它
//!   `select` 进自己的等待逻辑（Waker 注册表在 `cancel` 时统一唤醒）；
//! - [`CancellationToken::wait_timeout`] 是不需要执行器时的同步退路（内部用
//!   `Condvar`，不忙等）。
//!
//! ## 与 cordis-rs 的适配缝隙
//!
//! `cancelled()` 是纯 `std::future::Future`，与具体执行器解耦。将来接入 cordis-rs
//! 或任何 async 运行时（甚至 GPUI 的执行器）时，只需要在宿主侧给它一个 executor；
//! 插件作者看到的 API 不变。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// 在多个任务之间共享的取消令牌；`clone` 共享同一状态。
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    /// 已注册的 Waker；`cancel` 时整体唤醒并清空。
    waiters: Mutex<Vec<Waker>>,
    /// 与 `waiters` 共用同一把锁，供同步 `wait_timeout` 等待。
    bell: Condvar,
}

impl CancellationToken {
    /// 新建未取消的令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 触发取消。返回 `true` 表示本次调用完成了 `未取消 -> 已取消` 的状态迁移；
    /// 重复调用幂等并返回 `false`。
    pub fn cancel(&self) -> bool {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return false;
        }
        let waiters = {
            let mut guard = self.lock_waiters();
            std::mem::take(&mut *guard)
        };
        for waker in waiters {
            waker.wake();
        }
        self.inner.bell.notify_all();
        true
    }

    /// 是否已取消（任意线程、任意时刻可读）。
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 可被 `await` 的取消通知；已取消时第一次 poll 即为 `Ready`。
    pub fn cancelled(&self) -> Cancelled {
        Cancelled {
            token: self.clone(),
            registered: None,
        }
    }

    /// 同步等待取消，最多 `timeout`。返回是否已取消（超时返回 `false`）。
    ///
    /// 不引入执行器也能用；内部 `Condvar` 等待，取消时被立即唤醒。
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock_waiters();
        loop {
            if self.is_cancelled() {
                return true;
            }
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            let (next, _timed_out) = self
                .inner
                .bell
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next;
            if self.is_cancelled() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// 当前注册的等待者数量（诊断/测试用；`cancel` 后归零）。
    pub fn waiter_count(&self) -> usize {
        self.lock_waiters().len()
    }

    fn lock_waiters(&self) -> std::sync::MutexGuard<'_, Vec<Waker>> {
        self.inner
            .waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 注册 Waker（按 `will_wake` 去重）。
    fn register_waiter(&self, waker: &Waker) {
        if self.is_cancelled() {
            return;
        }
        let mut guard = self.lock_waiters();
        if !guard.iter().any(|existing| existing.will_wake(waker)) {
            guard.push(waker.clone());
        }
    }

    fn remove_waiter(&self, waker: &Waker) {
        let mut guard = self.lock_waiters();
        guard.retain(|existing| !existing.will_wake(waker));
    }
}

/// [`CancellationToken::cancelled`] 返回的 Future；只产出 `()`。
#[derive(Debug)]
pub struct Cancelled {
    token: CancellationToken,
    registered: Option<Waker>,
}

impl Future for Cancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if this.token.is_cancelled() {
            return Poll::Ready(());
        }
        let waker = cx.waker();
        this.token.register_waiter(waker);
        if let Some(previous) = &this.registered {
            if !previous.will_wake(waker) {
                this.registered = Some(waker.clone());
            }
        } else {
            this.registered = Some(waker.clone());
        }
        // 注册与检查之间可能刚好被取消：再读一次，避免丢掉这次唤醒。
        if this.token.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for Cancelled {
    fn drop(&mut self) {
        if let Some(waker) = self.registered.take() {
            if !self.token.is_cancelled() {
                self.token.remove_waiter(&waker);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn poll_once(future: &mut Cancelled) -> Poll<()> {
        // `Waker::noop()` 是 std 的稳定 API（rustc 1.85+），避免手写 unsafe RawWaker。
        let mut cx = Context::from_waker(Waker::noop());
        Pin::new(future).poll(&mut cx)
    }

    // CPP-FIBER-111..116：取消令牌语义。
    #[test]
    fn cpp_fiber_111_new_token_is_not_cancelled_and_cancel_is_idempotent() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        assert!(token.cancel());
        assert!(token.is_cancelled());
        assert!(!token.cancel(), "重复 cancel 必须幂等返回 false");
    }

    #[test]
    fn cpp_fiber_112_clones_share_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled(), "clone 取消后原令牌必须可见");
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cpp_fiber_113_cancelled_future_pends_then_completes() {
        let token = CancellationToken::new();
        let mut future = token.cancelled();
        assert_eq!(poll_once(&mut future), Poll::Pending);
        assert_eq!(token.waiter_count(), 1);
        token.cancel();
        assert_eq!(poll_once(&mut future), Poll::Ready(()));
        assert_eq!(token.waiter_count(), 0, "cancel 必须清空等待者");
    }

    #[test]
    fn cpp_fiber_114_dropping_future_unregisters_waiter() {
        let token = CancellationToken::new();
        {
            let mut future = token.cancelled();
            assert_eq!(poll_once(&mut future), Poll::Pending);
            assert_eq!(token.waiter_count(), 1);
        }
        assert_eq!(
            token.waiter_count(),
            0,
            "drop 未完成的 future 必须注销 waker"
        );
    }

    #[test]
    fn cpp_fiber_115_wait_timeout_returns_false_without_cancel() {
        let token = CancellationToken::new();
        assert!(!token.wait_timeout(Duration::from_millis(10)));
    }

    #[test]
    fn cpp_fiber_116_wait_timeout_is_woken_by_cancel_from_other_thread() {
        let token = CancellationToken::new();
        let canceller = token.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            canceller.cancel();
        });
        assert!(
            token.wait_timeout(Duration::from_secs(5)),
            "取消后 wait_timeout 必须立即返回 true"
        );
        handle.join().unwrap();

        // 已取消的令牌再等待也立即返回 true。
        assert!(token.wait_timeout(Duration::from_millis(1)));
    }
}
