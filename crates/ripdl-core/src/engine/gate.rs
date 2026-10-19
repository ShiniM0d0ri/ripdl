//! Shared in-flight cap for segment requests.
//!
//! The engine starts one segment per connection (up to `max_segments`). A
//! host that answers those parallel ranges with 429/503 (or starts refusing
//! connections) needs *fewer* connections, not eight retries in lockstep.
//! Each attempt takes a permit; a host-pressure failure halves the cap and a
//! finished attempt doubles it again, so a quota storm degrades to a single
//! stream and then ramps back up once the host is happy.

use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

struct State {
    limit: usize,
    max: usize,
    active: usize,
}

pub struct Gate {
    state: Mutex<State>,
    /// Wakes waiters when a permit frees up or the cap grows.
    notify: Notify,
}

impl Gate {
    pub fn new(max: usize) -> Arc<Self> {
        let max = max.max(1);
        Arc::new(Self {
            state: Mutex::new(State {
                limit: max,
                max,
                active: 0,
            }),
            notify: Notify::new(),
        })
    }

    /// Wait for a free slot. The caller holds the returned permit for one
    /// request attempt and drops it when the attempt ends (success, error or
    /// cancellation), freeing the slot.
    pub async fn acquire(&self) -> Permit<'_> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.state.lock().unwrap();
                if state.active < state.limit {
                    state.active += 1;
                    return Permit { gate: self };
                }
            }
            // Registered before the check above, so a release between the
            // check and this await is not lost.
            notified.await;
        }
    }

    /// Host pushed back: fewer connections in flight from now on.
    pub fn shrink(&self) {
        let mut state = self.state.lock().unwrap();
        state.limit = (state.limit / 2).max(1);
    }

    /// An attempt went through: allow more connections again.
    pub fn grow(&self) {
        let mut state = self.state.lock().unwrap();
        state.limit = (state.limit * 2).min(state.max);
        let grew = state.limit > 0;
        drop(state);
        if grew {
            self.notify.notify_waiters();
        }
    }

    fn release(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.active = state.active.saturating_sub(1);
        }
        // Only one slot freed: one waiter is enough.
        self.notify.notify_one();
    }

    /// Current in-flight cap (diagnostics).
    pub fn limit(&self) -> usize {
        self.state.lock().unwrap().limit
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (state.limit, state.active, state.max)
    }
}

pub struct Permit<'a> {
    gate: &'a Gate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SETTLE: Duration = Duration::from_millis(40);

    #[tokio::test]
    async fn fills_up_to_the_cap_then_waits() {
        let gate = Gate::new(2);
        let a = gate.acquire().await;
        let b = gate.acquire().await;
        assert_eq!(gate.counts(), (2, 2, 2));
        assert!(
            tokio::time::timeout(SETTLE, gate.acquire()).await.is_err(),
            "a third attempt must wait"
        );
        drop(a);
        let c = tokio::time::timeout(SETTLE, gate.acquire())
            .await
            .expect("a freed slot admits the waiter");
        assert_eq!(gate.counts().1, 2);
        drop((b, c));
    }

    #[tokio::test]
    async fn shrink_serializes_then_grow_restores() {
        let gate = Gate::new(4);
        let held: Vec<_> = {
            let mut v = Vec::new();
            for _ in 0..4 {
                v.push(gate.acquire().await);
            }
            v
        };
        gate.shrink();
        gate.shrink(); // 4 -> 2 -> 1
        assert_eq!(gate.counts().0, 1, "cap shrank to one connection");

        // All four are still held; a fifth needs every slot to drain down to
        // the new cap.
        assert!(tokio::time::timeout(SETTLE, gate.acquire()).await.is_err());
        let mut held = held;
        while held.len() > 1 {
            held.pop();
            assert!(
                tokio::time::timeout(SETTLE, gate.acquire()).await.is_err(),
                "still above the shrunken cap"
            );
        }
        held.pop(); // now active == 0 < limit == 1
        let one = tokio::time::timeout(SETTLE, gate.acquire())
            .await
            .expect("one slot available");
        gate.grow();
        assert_eq!(gate.counts().0, 2, "a healthy attempt ramps back up");
        drop(one);
        let second = tokio::time::timeout(SETTLE, gate.acquire())
            .await
            .expect("growth admits another attempt");
        drop(second);
    }

    #[tokio::test]
    async fn grow_never_exceeds_the_initial_cap() {
        let gate = Gate::new(2);
        for _ in 0..5 {
            gate.grow();
        }
        assert_eq!(gate.counts().0, 2);
    }
}
