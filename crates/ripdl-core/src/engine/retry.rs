use std::time::Duration;

use crate::error::Error;

/// Bounded retry policy with exponential backoff (no jitter yet).
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Retries per segment on top of the first attempt.
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryConfig {
    pub fn backoff(&self, attempt: u32) -> Duration {
        self.base_delay
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max_delay)
    }
}

/// Pure status policy, unit-testable without a live response: 408/429/5xx.
pub fn retryable_status(code: u16) -> bool {
    code == 408 || code == 429 || (500..600).contains(&code)
}

/// Quota/overload replies (host asks the client to slow down).
pub fn rate_limited_status(code: u16) -> bool {
    matches!(code, 429 | 503)
}

/// Only transport-level failures and transient HTTP statuses retry. Failing
/// the whole task on 408/429/5xx punished parallel segments for a temporary
/// condition, e.g. a per-IP quota tripped by eight range requests. Terminal:
/// other 4xx, disk errors, size mismatches.
pub fn is_retryable(e: &Error) -> bool {
    match e {
        Error::Http(h) => match h.status() {
            Some(s) => retryable_status(s.as_u16()),
            None => true,
        },
        Error::ShortSegment { .. } => true,
        Error::StreamStalled(_) => true,
        _ => false,
    }
}

pub fn is_rate_limited(e: &Error) -> bool {
    matches!(e, Error::Http(h) if h.status().is_some_and(|s| rate_limited_status(s.as_u16())))
}

/// The host (or the link to it) is pushing back rather than serving a broken
/// file: a quota reply, a refused/reset connection, a send or read timeout.
/// These clear on their own and deserve the long, jittered leash. A 404, a
/// short segment or a size mismatch never will.
pub fn is_host_pressure(e: &Error) -> bool {
    if is_rate_limited(e) {
        return true;
    }
    // A stalled connection shares the pressure semantics: it clears on its
    // own or not at all, so the segments get the long jittered leash instead
    // of failing the task after the plain budget.
    if matches!(e, Error::StreamStalled(_)) {
        return true;
    }
    matches!(
        e,
        Error::Http(h)
            if h.status().is_none()
                && (h.is_connect() || h.is_timeout() || h.is_request() || h.is_body())
    )
}

/// Retry budget for one segment.
pub fn max_attempts(cfg: &RetryConfig, host_pressure: bool) -> u32 {
    if host_pressure {
        cfg.max_retries.max(10)
    } else {
        cfg.max_retries
    }
}

/// Delay before retrying. `attempt` is 1-based. Other faults keep the
/// configured exponential backoff (0.5 s first); host pressure starts at 2 s,
/// doubles to a 10 s ceiling and carries per-segment jitter, so eight segments
/// never wake in lockstep and re-trip a per-IP quota.
///
/// The ceiling matters more than the attempt count: a host whose quota window
/// is ~10 s (OVH answers `retry-after: 1`) recovers on the 3rd/4th attempt
/// instead of waiting 30 s between tries, and a 10-attempt budget still rides
/// out a minute-long block.
pub fn delay(cfg: &RetryConfig, host_pressure: bool, attempt: u32, salt: u64) -> Duration {
    if host_pressure {
        let base = Duration::from_secs(2)
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(4))
            .min(Duration::from_secs(10));
        // Deterministic spread: 0..~1.5 s, different per segment/attempt.
        let jitter_ms = salt
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(u64::from(attempt) * 0x85EB_CA6B)
            % 1500;
        base + Duration::from_millis(jitter_ms)
    } else {
        cfg.backoff(attempt.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps() {
        let r = RetryConfig::default();
        assert_eq!(r.backoff(0), r.base_delay);
        assert_eq!(r.backoff(1), Duration::from_millis(1000));
        assert_eq!(r.backoff(2), Duration::from_millis(2000));
        assert_eq!(r.backoff(10), r.max_delay);
    }

    #[test]
    fn transient_statuses_retry_but_client_errors_do_not() {
        // Host quota / overload / timeout: transient.
        for code in [408, 429, 500, 502, 503, 504] {
            assert!(retryable_status(code), "{code} should retry");
        }
        // Real client errors: the URL/file is wrong, retrying won't help.
        for code in [400, 401, 403, 404, 410, 416] {
            assert!(!retryable_status(code), "{code} is terminal");
        }
        assert!(rate_limited_status(429));
        assert!(rate_limited_status(503));
        assert!(!rate_limited_status(500));
    }

    #[test]
    fn host_pressure_gets_a_longer_leash_and_jittered_backoff() {
        let r = RetryConfig::default();
        assert_eq!(max_attempts(&r, false), r.max_retries);
        assert!(max_attempts(&r, true) >= 10, "quota waits need room");

        // Transport: 0.5 s first, then 1 s (no jitter).
        assert_eq!(delay(&r, false, 1, 0), Duration::from_millis(500));
        assert_eq!(delay(&r, false, 2, 0), Duration::from_millis(1000));

        // Host pressure: >= 2 s and spread across segments.
        let a = delay(&r, true, 1, 0);
        let b = delay(&r, true, 1, 3);
        assert!(a >= Duration::from_secs(2));
        assert_ne!(a, b, "segments must not retry in lockstep");
        assert!(delay(&r, true, 8, 0) <= Duration::from_secs(11));
    }

    #[test]
    fn only_transport_and_quota_faults_count_as_host_pressure() {
        // A plain task error (size mismatch, bad URL) is not the host asking
        // us to slow down, so it keeps the short leash.
        assert!(!is_host_pressure(&Error::Server("boom".into())));
        assert!(!is_host_pressure(&Error::InvalidUrl("not a url".into())));
    }
}
