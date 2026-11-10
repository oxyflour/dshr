//! `ModelProvider` 的重试/退避包装。
//!
//! 只重试**可判定为瞬时**的错误（429 / 5xx / 超时 / 连接被重置等），
//! 并且流式请求必须满足"还没有向 UI 交付任何 delta"才能重试——否则重试会把
//! 已经显示过的文本再吐一遍，用户会看到重复内容。
//!
//! 参数：`DSHR_RETRY_ATTEMPTS`（1–10，默认 3）、`DSHR_RETRY_BASE_MS`（默认 200）、
//! `DSHR_RETRY_MAX_MS`（默认 2000）。指数退避：base * 2^(n-1)，封顶 max。

#![forbid(unsafe_code)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use dshr_agent_core::{CompletionRequest, ModelProvider, ModelReply};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_millis(2_000),
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
        policy
    }

    /// 测试用：不 sleep，但保留尝试次数语义。
    pub fn immediate(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.clamp(1, 10),
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(6));
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }
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
        let delay = self.policy.delay_for(attempt);
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
    fn backoff_grows_and_is_capped() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(300),
        };
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
