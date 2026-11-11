//! `ModelProvider` 的重试/退避包装。
//!
//! 只重试**可判定为瞬时**的错误（429 / 5xx / 超时 / 连接被重置等），
//! 并且流式请求必须满足"还没有向 UI 交付任何 delta"才能重试——否则重试会把
//! 已经显示过的文本再吐一遍，用户会看到重复内容。
//!
//! 参数默认对齐官方 DSH 0.2.2-rc.1 的 `@deepseek-ai/dsh-llm` retry-policy：
//! `maxRetries=5`（**总尝试 6 次**）、`initialDelayMs=500`、`maxDelayMs=10000`、
//! `jitterRatio=0.1`（±10% 抖动）。可用 `DSHR_RETRY_ATTEMPTS`（1–10）、
//! `DSHR_RETRY_BASE_MS`、`DSHR_RETRY_MAX_MS`、`DSHR_RETRY_JITTER`（0–1）覆盖。
//! 指数退避：base * 2^(n-1)，封顶 max，再叠加 ±jitter。
//!
//! 与 DSH 的差异：DSH 的 retryableCodes 还包含 `EMPTY_RESPONSE`，dshr 的 provider
//! 目前不产生这个错误码，故 [`is_retryable_error`] 未处理（M3 补）。

#![forbid(unsafe_code)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use dshr_agent_core::{CompletionRequest, ModelProvider, ModelReply};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// 总尝试次数（含首次）；DSH `maxRetries=5` 对应 6。
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// 抖动比例：实际延迟 ∈ `[delay*(1-r), delay*(1+r)]`，再封顶 `max_delay`。
    pub jitter_ratio: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_millis(10_000),
            jitter_ratio: 0.1,
        }
    }
}

impl RetryPolicy {
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Some(attempts) = std::env::var("DSHR_RETRY_ATTEMPTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        {
            policy.max_attempts = attempts.clamp(1, 10);
        }
        if let Some(base) = std::env::var("DSHR_RETRY_BASE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            policy.base_delay = Duration::from_millis(base);
        }
        if let Some(max) = std::env::var("DSHR_RETRY_MAX_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            policy.max_delay = Duration::from_millis(max);
        }
        if let Some(jitter) = std::env::var("DSHR_RETRY_JITTER")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
        {
            policy.jitter_ratio = jitter.clamp(0.0, 1.0);
        }
        policy
    }

    /// 测试用：不 sleep，但保留尝试次数语义。
    pub fn immediate(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.clamp(1, 10),
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            jitter_ratio: 0.0,
        }
    }

    /// 指数退避的确定性值（不含抖动）：`base * 2^(attempt-1)`，封顶 `max_delay`。
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(6));
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }

    /// 在 [`RetryPolicy::delay_for`] 上叠加 ±`jitter_ratio` 抖动。
    ///
    /// `unit` 是 `[0, 1]` 的随机数（测试可注入以断言上下界）。
    pub fn jittered_delay_for(&self, attempt: u32, unit: f64) -> Duration {
        let base = self.delay_for(attempt);
        if base.is_zero() || self.jitter_ratio == 0.0 {
            return base;
        }
        let unit = unit.clamp(0.0, 1.0);
        let factor = 1.0 + (unit * 2.0 - 1.0) * self.jitter_ratio;
        base.mul_f64(factor).min(self.max_delay)
    }
}

/// 无 rand 依赖的伪随机 `[0,1)`：用时钟纳秒的低位，够抖动即可。
fn random_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// 错误字符串是否属于"值得重试"的瞬时错误。
pub fn is_retryable_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "429",
        "rate limit",
        "too many requests",
        "500",
        "502",
        "503",
        "504",
        "timed out",
        "timeout",
        "connection reset",
        "reset by peer",
        "connection refused",
        "connection closed",
        "unexpected eof",
        "temporarily unavailable",
        "overloaded",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

pub struct RetryingProvider {
    inner: Box<dyn ModelProvider>,
    policy: RetryPolicy,
    last_attempts: u32,
}

impl RetryingProvider {
    pub fn new(inner: Box<dyn ModelProvider>, policy: RetryPolicy) -> Self {
        Self {
            inner,
            policy,
            last_attempts: 0,
        }
    }

    /// 最近一次请求实际尝试的次数（含首次）。
    pub fn last_attempts(&self) -> u32 {
        self.last_attempts
    }

    fn sleep_before_retry(&self, attempt: u32) {
        let delay = self.policy.jittered_delay_for(attempt, random_unit());
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }
}

impl ModelProvider for RetryingProvider {
    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        self.inner.cancel_token()
    }

    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.inner.complete(request) {
                Ok(reply) => {
                    self.last_attempts = attempt;
                    return Ok(reply);
                }
                Err(error) => {
                    let retryable = is_retryable_error(&error);
                    if !retryable || attempt >= self.policy.max_attempts {
                        self.last_attempts = attempt;
                        return Err(error);
                    }
                    self.sleep_before_retry(attempt);
                }
            }
        }
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut delivered = false;
            let mut forward = |delta: &str| {
                delivered = true;
                on_delta(delta);
            };
            match self.inner.complete_streaming(request, &mut forward) {
                Ok(reply) => {
                    self.last_attempts = attempt;
                    return Ok(reply);
                }
                Err(error) => {
                    // 已经交付过 delta 就不能重试：会重复显示。
                    let safe_to_retry = !delivered && is_retryable_error(&error);
                    if !safe_to_retry || attempt >= self.policy.max_attempts {
                        self.last_attempts = attempt;
                        return Err(error);
                    }
                    self.sleep_before_retry(attempt);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshr_agent_core::ChatMessage;

    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![ChatMessage::user("hi")],
            tool_specs: Vec::new(),
        }
    }

    struct ScriptedProvider {
        failures: Vec<Option<String>>,
        calls: usize,
        stream_deltas_before_error: bool,
    }

    impl ScriptedProvider {
        fn failing(times: usize, error: &str) -> Self {
            Self {
                failures: (0..times).map(|_| Some(error.to_owned())).collect(),
                calls: 0,
                stream_deltas_before_error: false,
            }
        }

        fn next_result(&mut self) -> Result<ModelReply, String> {
            self.calls += 1;
            match self.failures.get(self.calls - 1).cloned().flatten() {
                Some(error) => Err(error),
                None => Ok(ModelReply {
                    text: String::from("ok"),
                    tool_calls: Vec::new(),
                }),
            }
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn complete(&mut self, _request: &CompletionRequest) -> Result<ModelReply, String> {
            self.next_result()
        }

        fn complete_streaming(
            &mut self,
            _request: &CompletionRequest,
            on_delta: &mut dyn FnMut(&str),
        ) -> Result<ModelReply, String> {
            if self.stream_deltas_before_error {
                on_delta("partial");
            }
            self.next_result()
        }
    }

    #[test]
    fn retries_transient_errors_then_succeeds() {
        let inner = Box::new(ScriptedProvider::failing(2, "HTTP 503 service unavailable"));
        let mut provider = RetryingProvider::new(inner, RetryPolicy::immediate(3));
        let reply = provider.complete(&request()).expect("reply");
        assert_eq!(reply.text, "ok");
        assert_eq!(provider.last_attempts(), 3);
    }

    #[test]
    fn does_not_retry_permanent_errors() {
        let inner = Box::new(ScriptedProvider::failing(1, "HTTP 400 invalid request"));
        let mut provider = RetryingProvider::new(inner, RetryPolicy::immediate(3));
        assert!(provider.complete(&request()).is_err());
        assert_eq!(provider.last_attempts(), 1);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let inner = Box::new(ScriptedProvider::failing(5, "429 rate limit"));
        let mut provider = RetryingProvider::new(inner, RetryPolicy::immediate(3));
        assert!(provider.complete(&request()).is_err());
        assert_eq!(provider.last_attempts(), 3);
    }

    #[test]
    fn streaming_retries_when_nothing_was_delivered() {
        let inner = Box::new(ScriptedProvider::failing(1, "connection reset by peer"));
        let mut provider = RetryingProvider::new(inner, RetryPolicy::immediate(3));
        let mut deltas = Vec::new();
        let reply = provider
            .complete_streaming(&request(), &mut |delta| deltas.push(delta.to_owned()))
            .expect("reply");
        assert_eq!(reply.text, "ok");
        assert!(deltas.is_empty());
        assert_eq!(provider.last_attempts(), 2);
    }

    #[test]
    fn streaming_does_not_retry_after_partial_output() {
        let mut inner = ScriptedProvider::failing(1, "connection reset by peer");
        inner.stream_deltas_before_error = true;
        let mut provider = RetryingProvider::new(Box::new(inner), RetryPolicy::immediate(3));
        let mut deltas = Vec::new();
        assert!(provider
            .complete_streaming(&request(), &mut |delta| deltas.push(delta.to_owned()))
            .is_err());
        assert_eq!(deltas, vec!["partial".to_owned()]);
        assert_eq!(provider.last_attempts(), 1);
    }

    #[test]
    fn defaults_match_dsh_retry_policy() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 6, "DSH maxRetries=5 → 总尝试 6 次");
        assert_eq!(policy.base_delay, Duration::from_millis(500));
        assert_eq!(policy.max_delay, Duration::from_millis(10_000));
        assert_eq!(policy.jitter_ratio, 0.1);
    }

    #[test]
    fn from_env_overrides_defaults_and_clamps() {
        // 本 crate 只有这个测试读写 DSHR_RETRY_*，并行安全；结束后恢复。
        const KEYS: [&str; 4] = [
            "DSHR_RETRY_ATTEMPTS",
            "DSHR_RETRY_BASE_MS",
            "DSHR_RETRY_MAX_MS",
            "DSHR_RETRY_JITTER",
        ];
        let previous: Vec<Option<String>> =
            KEYS.iter().map(|key| std::env::var(key).ok()).collect();
        std::env::set_var("DSHR_RETRY_ATTEMPTS", "4"); // chat --selftest-retry 的口径
        std::env::set_var("DSHR_RETRY_BASE_MS", "100");
        std::env::set_var("DSHR_RETRY_MAX_MS", "1200");
        std::env::set_var("DSHR_RETRY_JITTER", "2.0"); // 越界 → clamp 到 1.0

        let policy = RetryPolicy::from_env();
        assert_eq!(policy.max_attempts, 4);
        assert_eq!(policy.base_delay, Duration::from_millis(100));
        assert_eq!(policy.max_delay, Duration::from_millis(1_200));
        assert_eq!(policy.jitter_ratio, 1.0);

        for (key, value) in KEYS.iter().zip(previous) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn jitter_stays_within_bounds_and_respects_cap() {
        let policy = RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_millis(2_000),
            jitter_ratio: 0.1,
        };
        for attempt in 1..=6 {
            let base = policy.delay_for(attempt);
            for unit in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let delay = policy.jittered_delay_for(attempt, unit);
                assert!(
                    delay <= policy.max_delay,
                    "attempt={attempt} 抖动后不能超过封顶: {delay:?}"
                );
                let lower = base.mul_f64(1.0 - policy.jitter_ratio);
                let upper = base.mul_f64(1.0 + policy.jitter_ratio);
                assert!(
                    delay >= lower && delay <= upper,
                    "attempt={attempt} unit={unit} delay={delay:?} 应在 {lower:?}..={upper:?}"
                );
            }
            // 极端 unit 的边界：0.0 = -jitter，1.0 = +jitter。
            assert_eq!(
                policy.jittered_delay_for(attempt, 0.0),
                base.mul_f64(1.0 - policy.jitter_ratio)
                    .min(policy.max_delay)
            );
            assert_eq!(
                policy.jittered_delay_for(attempt, 1.0),
                base.mul_f64(1.0 + policy.jitter_ratio)
                    .min(policy.max_delay)
            );
        }
        assert_eq!(policy.jittered_delay_for(0, 0.5), Duration::ZERO);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(300),
            jitter_ratio: 0.0,
        };
        assert_eq!(policy.delay_for(0), Duration::ZERO);
        assert_eq!(policy.delay_for(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for(3), Duration::from_millis(300));
        assert_eq!(policy.delay_for(9), Duration::from_millis(300));
    }

    #[test]
    fn error_classification() {
        assert!(is_retryable_error("HTTP 429 Too Many Requests"));
        assert!(is_retryable_error("stream read failed: unexpected eof"));
        assert!(is_retryable_error("connection reset by peer"));
        assert!(!is_retryable_error("HTTP 401 unauthorized"));
        assert!(!is_retryable_error("invalid tool arguments"));
    }
}
