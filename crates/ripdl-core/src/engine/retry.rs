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

/// Only transport-level failures retry. Status errors (4xx/5xx), disk
/// errors, size mismatches and state corruption are terminal for the segment.
pub fn is_retryable(e: &Error) -> bool {
    match e {
        Error::Http(h) => !h.is_status(),
        Error::ShortSegment { .. } => true,
        _ => false,
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
}
