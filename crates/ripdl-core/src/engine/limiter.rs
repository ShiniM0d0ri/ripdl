//! Shared token-bucket speed limiter.
//!
//! One `Limiter` is owned by the whole download (all segments draw from the
//! same budget). Bytes are reserved *as they are read off the wire*, which
//! paces the true consumption rate regardless of how many segments are
//! running. Sleeps are capped in slices so pause/cancel stays responsive.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cap single sleeps so cancellation is never stuck behind a long wait.
const MAX_SLEEP: Duration = Duration::from_millis(250);

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
            rate,
            state: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    /// Reserve `n` bytes, pacing the caller until the shared budget covers
    /// them. Grants whatever is available each iteration and waits for the
    /// remainder, so any `n` (including chunks larger than the cap) makes
    /// forward progress and terminates.
    pub async fn acquire(&self, n: u64) {
        if self.rate.is_none() || n == 0 {
            return;
        }
        let rate = self.rate.unwrap() as f64;
        let mut need = n as f64;

        while need > 0.0 {
            let wait = {
                let mut b = self.state.lock().unwrap();
                let now = Instant::now();
                let dt = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                // Refill; bucket capacity is 1s of rate so short bursts are
                // absorbed without starving parallel segments.
                b.tokens = (b.tokens + dt * rate).min(rate);
                let grant = b.tokens.min(need);
                if grant > 0.0 {
                    b.tokens -= grant;
                    need -= grant;
                }
                // Time to drip the remainder through the cap.
                (need / rate).max(0.001)
            };
            if need <= 0.0 {
                return;
            }
            // Sleep in bounded slices so pause/cancel stays responsive.
            tokio::time::sleep(Duration::from_secs_f64(wait.min(MAX_SLEEP.as_secs_f64()))).await;
        }
    }
}
