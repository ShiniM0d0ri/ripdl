//! Shared token-bucket speed limiter.
//!
//! One `Limiter` is owned by the whole download (all segments draw from the
//! same budget). Bytes are reserved *as they are read off the wire*, which
//! paces the true consumption rate regardless of how many segments are
//! running. Sleeps are capped in slices so pause/cancel stays responsive.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};

/// Cap single sleeps so cancellation wakes up promptly and non-cancellable
/// callers never stall for multiple seconds inside a slow rate cap.
const MAX_SLEEP: Duration = Duration::from_millis(100);
/// Maximum burst window (250 ms of the configured rate) so idle gaps never
/// accumulate a full second spike above the cap.
const BURST_SECS: f64 = 0.25;

#[derive(Debug)]
pub struct Limiter {
    /// Bytes per second cap, or `None` for unlimited.
    rate: Option<u64>,
    state: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Limiter {
    /// `rate` = bytes per second. `None` disables pacing entirely.
    pub fn new(rate: Option<u64>) -> Self {
        Self {
            rate: rate.filter(|&r| r > 0),
            state: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    /// Reserve `n` bytes, aborting immediately if `token` is cancelled so a
    /// low speed cap never delays pause/cancel.
    pub async fn acquire_with_cancel(&self, n: u64, token: Option<&CancellationToken>) -> Result<()> {
        let Some(rate_u64) = self.rate else {
            return Ok(());
        };
        if n == 0 {
            return Ok(());
        }
        let rate = rate_u64 as f64;
        // 250 ms burst ceiling (floored at 16 KiB when rate allows so small
        // chunks don't spin), never exceeding 1 s of rate.
        let burst_cap = (rate * BURST_SECS).max(16_384.0_f64.min(rate)).min(rate);
        let mut need = n as f64;

        while need > 0.0 {
            if token.is_some_and(CancellationToken::is_cancelled) {
                return Err(Error::Cancelled);
            }
            let wait = {
                let mut b = self.state.lock().unwrap();
                let now = Instant::now();
                let dt = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                let available = (b.tokens + dt * rate).min(burst_cap);
                let grant = available.min(need);
                b.tokens = available - grant;
                if grant > 0.0 {
                    need -= grant;
                }
                // Time to drip the remainder through the cap.
                (need / rate).max(0.001)
            };
            if need <= 0.0 {
                return Ok(());
            }
            let sleep_for = Duration::from_secs_f64(wait.min(MAX_SLEEP.as_secs_f64()));
            match token {
                Some(t) => {
                    tokio::select! {
                        _ = t.cancelled() => return Err(Error::Cancelled),
                        _ = tokio::time::sleep(sleep_for) => {}
                    }
                }
                None => tokio::time::sleep(sleep_for).await,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limiter_cancel_latency() {
        // 1 KiB/s with a 64 KiB acquire would otherwise wait ~64 seconds;
        // cancellation must wake immediately within <300 ms.
        let lim = Limiter::new(Some(1024));
        let token = CancellationToken::new();
        let cancel_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let started = Instant::now();
        let res = lim.acquire_with_cancel(64 * 1024, Some(&token)).await;
        assert!(matches!(res, Err(Error::Cancelled)), "got {res:?}");
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "cancel took too long: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn limiter_burst_bound() {
        // After idling longer than 1s at 100 KiB/s, burst capacity must stay
        // capped at <= 250 ms (~25 KiB), not a full 100 KiB second spike.
        let lim = Limiter::new(Some(100 * 1024));
        tokio::time::sleep(Duration::from_millis(400)).await;
        let started = Instant::now();
        // Requiring 60 KiB after idle must throttle at least ~300 ms for the
        // remaining ~35 KiB above the 25 KiB burst window.
        lim.acquire_with_cancel(60 * 1024, None).await.unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "burst cap too large: elapsed {:?}",
            started.elapsed()
        );
    }
}
