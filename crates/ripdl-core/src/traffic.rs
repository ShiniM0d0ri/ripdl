//! v2 traffic domain: one owner of byte accounting, rates and limits.
//!
//! This module is pure mechanics. It performs no I/O and knows nothing about
//! the daemon, tasks, or frontends. The daemon owns one [`TrafficController`]
//! and hands an `Arc<TaskTraffic>` (or `Arc<PartTraffic>`) to each transfer.
//!
//! Invariants are named in `docs/v2/INVARIANTS.md` and encoded in the tests at
//! the bottom of this file.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Default burst window. A bucket holds at most `rate * burst` tokens, so a
/// short burst is absorbed without letting a 2 s display window read far above
/// the configured cap.
pub const DEFAULT_BURST: Duration = Duration::from_millis(250);
/// Default rate window for tasks and links.
pub const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(2);
/// Sleep slices keep cancellation responsive during a long acquire.
const MAX_SLEEP: Duration = Duration::from_millis(250);

/// A cancelled acquire. Cancellation wins over a pending rate-limit wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("traffic operation cancelled")]
pub struct Cancelled;

/// Monotonic byte counter with compensated rollback.
///
/// A rollback (a failed write whose bytes were counted at read time) moves the
/// counter backwards so the same bytes are not counted twice on retry. The
/// rate window sees the decrease and resets its baseline.
#[derive(Debug, Default)]
pub struct Meter {
    bytes: AtomicU64,
}

impl Meter {
    pub fn add(&self, n: u64) {
        let _ = self
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(n))
            });
    }

    pub fn rollback(&self, n: u64) {
        let _ = self
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(n))
            });
    }

    pub fn load(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.bytes.store(0, Ordering::Relaxed);
    }
}

/// Sliding window of `(instant, cumulative bytes)` samples.
///
/// Rates use **measured** elapsed time. A gap longer than the span starts a
/// fresh baseline, and a byte count that moves backwards resets the window so
/// a rollback or resume adoption never reads as throughput.
#[derive(Debug)]
pub struct RateWindow {
    span: Duration,
    samples: VecDeque<(Instant, u64)>,
    last: Option<u64>,
}

impl RateWindow {
    pub fn new(span: Duration) -> Self {
        Self {
            span,
            samples: VecDeque::new(),
            last: None,
        }
    }

    pub fn push(&mut self, now: Instant, bytes: u64) {
        if let Some(&(t, _)) = self.samples.back() {
            if now < t || now.duration_since(t) > self.span {
                self.samples.clear();
            }
        }
        if self.last.is_some_and(|last| bytes < last) {
            self.samples.clear();
        }
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > self.span {
            self.samples.pop_front();
        }
        self.last = Some(bytes);
    }

    /// Bytes per second across the window, or `None` when the span is too
    /// short or the window was just reset.
    pub fn rate(&self) -> Option<f64> {
        let (&(t0, b0), &(t1, b1)) = (self.samples.front()?, self.samples.back()?);
        let dt = t1.duration_since(t0).as_secs_f64();
        (dt >= 0.5).then(|| (b1.saturating_sub(b0)) as f64 / dt)
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.last = None;
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Token bucket limiter with optional parent limiters.
///
/// Parents are acquired first, so a task limiter with the daemon's global
/// limiter and a host limiter as parents enforces `min(global, host, task)`
/// without any caller knowing the hierarchy.
#[derive(Debug)]
pub struct Limiter {
    rate: RwLock<Option<u64>>,
    burst: Duration,
    state: Mutex<Bucket>,
    parents: RwLock<Vec<Arc<Limiter>>>,
}

impl Limiter {
    pub fn new(rate: Option<u64>) -> Self {
        Self::with_burst(rate, DEFAULT_BURST)
    }

    pub fn with_burst(rate: Option<u64>, burst: Duration) -> Self {
        Self {
            rate: RwLock::new(rate.filter(|&r| r > 0)),
            burst,
            state: Mutex::new(Bucket {
                // A bucket starts full, so a configured burst is available at
                // once (e.g. 250 ms of rate). Zero burst starts empty.
                tokens: rate.map(|r| r as f64 * burst.as_secs_f64()).unwrap_or(0.0),
                last: Instant::now(),
            }),
            parents: RwLock::new(Vec::new()),
        }
    }

    pub fn add_parent(&self, parent: Arc<Limiter>) {
        self.parents.write().unwrap().push(parent);
    }

    pub fn set_rate(&self, rate: Option<u64>) {
        *self.rate.write().unwrap() = rate.filter(|&r| r > 0);
    }

    pub fn rate(&self) -> Option<u64> {
        *self.rate.read().unwrap()
    }

    /// Reserve `n` bytes. `token` makes the wait cancellable; a cancellation
    /// returns [`Cancelled`] within one sleep slice.
    pub async fn acquire(&self, n: u64, token: Option<&CancellationToken>) -> Result<(), Cancelled> {
        if n == 0 {
            return Ok(());
        }
        let parents = self.parents.read().unwrap().clone();
        for parent in parents {
            Box::pin(parent.acquire(n, token)).await?;
        }
        self.acquire_local(n, token).await
    }

    async fn acquire_local(&self, n: u64, token: Option<&CancellationToken>) -> Result<(), Cancelled> {
        let rate = match *self.rate.read().unwrap() {
            Some(r) => r as f64,
            None => return Ok(()),
        };
        let burst_tokens = rate * self.burst.as_secs_f64();
        let mut need = n as f64;

        while need > 0.0 {
            let wait = {
                let mut b = self.state.lock().unwrap();
                let now = Instant::now();
                let dt = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                let available = b.tokens + dt * rate;
                let grant = available.min(need);
                // Grant everything accrued this tick; only the unspent
                // remainder is stored, capped at the burst window. With a
                // zero burst this still paces at `rate` instead of hanging.
                b.tokens = (available - grant).min(burst_tokens);
                if grant > 0.0 {
                    need -= grant;
                }
                (need / rate).max(0.001)
            };
            if need <= 0.0 {
                return Ok(());
            }
            let sleep_for = Duration::from_secs_f64(wait.min(MAX_SLEEP.as_secs_f64()));
            match token {
                Some(t) => {
                    tokio::select! {
                        _ = t.cancelled() => return Err(Cancelled),
                        _ = tokio::time::sleep(sleep_for) => {}
                    }
                }
                None => tokio::time::sleep(sleep_for).await,
            }
        }
        Ok(())
    }
}

/// The kind of transfer, used only for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficKind {
    Http,
    Bundle,
    Torrent,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficState {
    Running,
    Paused,
}

pub type TaskId = String;

/// One task's traffic handle. The daemon replaces `ProgressTrack` with this.
#[derive(Debug)]
pub struct TaskTraffic {
    id: TaskId,
    kind: TrafficKind,
    state: Mutex<TrafficState>,
    run_id: AtomicU64,
    meter: Arc<Meter>,
    rate: Mutex<RateWindow>,
    limiter: Arc<Limiter>,
    parts: RwLock<Vec<Arc<PartTraffic>>>,
    self_handle: Mutex<Option<Arc<TaskTraffic>>>,
}

impl TaskTraffic {
    fn new(id: TaskId, kind: TrafficKind, limiter: Arc<Limiter>) -> Self {
        Self {
            id,
            kind,
            state: Mutex::new(TrafficState::Paused),
            run_id: AtomicU64::new(0),
            meter: Arc::new(Meter::default()),
            rate: Mutex::new(RateWindow::new(DEFAULT_RATE_WINDOW)),
            limiter,
            parts: RwLock::new(Vec::new()),
            self_handle: Mutex::new(None),
        }
    }

    /// Start a new run. A new generation invalidates late samples from the
    /// previous runner, and the rate window starts from a clean baseline.
    pub fn begin_run(&self) -> u64 {
        self.run_id.fetch_add(1, Ordering::SeqCst);
        *self.state.lock().unwrap() = TrafficState::Running;
        self.rate.lock().unwrap().reset();
        self.current_run()
    }

    pub fn pause(&self) {
        *self.state.lock().unwrap() = TrafficState::Paused;
        self.rate.lock().unwrap().reset();
    }

    pub fn current_run(&self) -> u64 {
        self.run_id.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> TrafficState {
        *self.state.lock().unwrap()
    }

    pub fn kind(&self) -> TrafficKind {
        self.kind
    }

    pub fn add_part(&self, name: impl Into<String>) -> Arc<PartTraffic> {
        let part = Arc::new(PartTraffic {
            name: name.into(),
            meter: Arc::new(Meter::default()),
            rate: Mutex::new(RateWindow::new(DEFAULT_RATE_WINDOW)),
            task: self.task_handle(),
        });
        self.parts.write().unwrap().push(Arc::clone(&part));
        part
    }

    fn task_handle(&self) -> Arc<TaskTraffic> {
        // Parts hold an Arc back to the task. The controller creates tasks as
        // Arcs; this helper is only used inside `add_part`, where the caller
        // holds one. It is reconstructed by cloning the outer Arc via a
        // lightweight self-reference stored at construction.
        self.self_handle
            .lock()
            .unwrap()
            .clone()
            .expect("part created on a controller-managed task")
    }

    /// Report durable bytes. Returns `false` for a stale run id.
    pub fn report(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        if run_id != self.current_run() || self.state() != TrafficState::Running {
            return false;
        }
        self.meter.add(bytes);
        self.rate.lock().unwrap().push(now, self.meter.load());
        true
    }

    /// Compensate bytes that were counted but never became durable.
    pub fn rollback(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        if run_id != self.current_run() {
            return false;
        }
        self.meter.rollback(bytes);
        self.rate.lock().unwrap().push(now, self.meter.load());
        true
    }

    pub fn rate_bps(&self) -> Option<f64> {
        if self.state() != TrafficState::Running {
            return None;
        }
        self.rate.lock().unwrap().rate()
    }

    pub fn downloaded(&self) -> u64 {
        self.meter.load()
    }

    pub fn set_limit(&self, rate: Option<u64>) {
        self.limiter.set_rate(rate);
    }

    pub fn limit_bps(&self) -> Option<u64> {
        self.limiter.rate()
    }

    pub fn is_throttled(&self) -> bool {
        self.limiter.rate().is_some()
    }

    pub async fn acquire(&self, n: u64, token: Option<&CancellationToken>) -> Result<(), Cancelled> {
        self.limiter.acquire(n, token).await
    }

    pub fn snapshot(&self) -> TaskTrafficSnapshot {
        TaskTrafficSnapshot {
            id: self.id.clone(),
            kind: self.kind,
            state: self.state(),
            run_id: self.current_run(),
            downloaded: self.downloaded(),
            rate_bps: self.rate_bps(),
            limit_bps: self.limit_bps(),
            throttled: self.is_throttled(),
        }
    }
}

/// Bundle part: has its own meter/rate for display but draws budget from its
/// parent task, so several parts cannot multiply the task or global cap.
#[derive(Debug)]
pub struct PartTraffic {
    name: String,
    meter: Arc<Meter>,
    rate: Mutex<RateWindow>,
    task: Arc<TaskTraffic>,
}

impl PartTraffic {
    pub fn report(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        if !self.task.report(run_id, bytes, now) {
            return false;
        }
        self.meter.add(bytes);
        self.rate.lock().unwrap().push(now, self.meter.load());
        true
    }

    pub fn rollback(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        if !self.task.rollback(run_id, bytes, now) {
            return false;
        }
        self.meter.rollback(bytes);
        self.rate.lock().unwrap().push(now, self.meter.load());
        true
    }

    pub fn rate_bps(&self) -> Option<f64> {
        self.rate.lock().unwrap().rate()
    }

    pub fn downloaded(&self) -> u64 {
        self.meter.load()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn acquire(&self, n: u64, token: Option<&CancellationToken>) -> Result<(), Cancelled> {
        self.task.acquire(n, token).await
    }
}

/// Port implemented by tasks and parts so callers do not care which they hold.
pub trait Traffic: Send + Sync + std::fmt::Debug {
    fn acquire<'a>(
        &'a self,
        n: u64,
        token: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<(), Cancelled>>;

    fn report(&self, run_id: u64, bytes: u64, now: Instant) -> bool;
    fn rollback(&self, run_id: u64, bytes: u64, now: Instant) -> bool;
    fn rate_bps(&self) -> Option<f64>;
    fn downloaded(&self) -> u64;
}

impl Traffic for TaskTraffic {
    fn acquire<'a>(
        &'a self,
        n: u64,
        token: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<(), Cancelled>> {
        Box::pin(TaskTraffic::acquire(self, n, token))
    }
    fn report(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        TaskTraffic::report(self, run_id, bytes, now)
    }
    fn rollback(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        TaskTraffic::rollback(self, run_id, bytes, now)
    }
    fn rate_bps(&self) -> Option<f64> {
        TaskTraffic::rate_bps(self)
    }
    fn downloaded(&self) -> u64 {
        TaskTraffic::downloaded(self)
    }
}

impl Traffic for PartTraffic {
    fn acquire<'a>(
        &'a self,
        n: u64,
        token: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<(), Cancelled>> {
        Box::pin(PartTraffic::acquire(self, n, token))
    }
    fn report(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        PartTraffic::report(self, run_id, bytes, now)
    }
    fn rollback(&self, run_id: u64, bytes: u64, now: Instant) -> bool {
        PartTraffic::rollback(self, run_id, bytes, now)
    }
    fn rate_bps(&self) -> Option<f64> {
        PartTraffic::rate_bps(self)
    }
    fn downloaded(&self) -> u64 {
        PartTraffic::downloaded(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTrafficSnapshot {
    pub id: TaskId,
    pub kind: TrafficKind,
    pub state: TrafficState,
    pub run_id: u64,
    pub downloaded: u64,
    pub rate_bps: Option<f64>,
    pub limit_bps: Option<u64>,
    pub throttled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerSnapshot {
    pub global_limit_bps: Option<u64>,
    pub total_bps: f64,
    pub tasks: Vec<TaskTrafficSnapshot>,
}

/// Owns the global budget and every task's traffic handle. Created once by
/// the daemon and shared with all transfers.
#[derive(Debug)]
pub struct TrafficController {
    global: Arc<Limiter>,
    global_burst: Duration,
    tasks: RwLock<HashMap<TaskId, Arc<TaskTraffic>>>,
    hosts: RwLock<HashMap<String, Arc<Limiter>>>,
}

impl TrafficController {
    pub fn new(global_rate: Option<u64>) -> Arc<Self> {
        Self::with_burst(global_rate, DEFAULT_BURST)
    }

    pub fn with_burst(global_rate: Option<u64>, burst: Duration) -> Arc<Self> {
        Arc::new(Self {
            global: Arc::new(Limiter::with_burst(global_rate, burst)),
            global_burst: burst,
            tasks: RwLock::new(HashMap::new()),
            hosts: RwLock::new(HashMap::new()),
        })
    }

    pub fn global(&self) -> Arc<Limiter> {
        Arc::clone(&self.global)
    }

    fn host_limiter(&self, host: &str) -> Arc<Limiter> {
        let mut hosts = self.hosts.write().unwrap();
        hosts
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(Limiter::with_burst(None, self.global_burst)))
            .clone()
    }

    pub fn register_task(
        &self,
        id: impl Into<TaskId>,
        kind: TrafficKind,
        task_rate: Option<u64>,
        host: Option<&str>,
    ) -> Arc<TaskTraffic> {
        let task_limiter = Arc::new(Limiter::with_burst(task_rate, self.global_burst));
        task_limiter.add_parent(Arc::clone(&self.global));
        if let Some(host) = host {
            task_limiter.add_parent(self.host_limiter(host));
        }

        let task = Arc::new(TaskTraffic::new(id.into(), kind, task_limiter));
        task.bind_self(Arc::clone(&task));
        self.tasks
            .write()
            .unwrap()
            .insert(task.id.clone(), Arc::clone(&task));
        task
    }

    pub fn unregister_task(&self, id: &str) {
        self.tasks.write().unwrap().remove(id);
    }

    pub fn task(&self, id: &str) -> Option<Arc<TaskTraffic>> {
        self.tasks.read().unwrap().get(id).cloned()
    }

    pub fn set_global_rate(&self, rate: Option<u64>) {
        self.global.set_rate(rate);
    }

    pub fn set_task_rate(&self, id: &str, rate: Option<u64>) {
        if let Some(task) = self.task(id) {
            task.set_limit(rate);
        }
    }

    pub fn set_host_rate(&self, host: &str, rate: Option<u64>) {
        self.host_limiter(host).set_rate(rate);
    }

    pub fn snapshot(&self) -> ControllerSnapshot {
        let tasks: Vec<_> = self
            .tasks
            .read()
            .unwrap()
            .values()
            .map(|t| t.snapshot())
            .collect();
        let total_bps = tasks.iter().filter_map(|t| t.rate_bps).sum();
        ControllerSnapshot {
            global_limit_bps: self.global.rate(),
            total_bps,
            tasks,
        }
    }
}

// NOTE: `TaskTraffic` keeps a weak self handle so `add_part` can hand a part an
// Arc<TaskTraffic> without the caller passing it in. It is filled by
// `TrafficController::register_task` below via a private setter.
impl TaskTraffic {
    fn bind_self(&self, handle: Arc<TaskTraffic>) {
        *self.self_handle.lock().unwrap() = Some(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant() -> Instant {
        Instant::now()
    }

    #[tokio::test(start_paused = true)]
    async fn global_cap_two_tasks() {
        let c = TrafficController::with_burst(Some(1000), Duration::ZERO);
        let a = c.register_task("a", TrafficKind::Http, None, None);
        let b = c.register_task("b", TrafficKind::Http, None, None);
        a.begin_run();
        b.begin_run();
        let start = instant();
        tokio::join!(a.acquire(1000, None), b.acquire(1000, None)).0.unwrap();
        let elapsed = start.elapsed().as_secs_f64();
        assert!((1.9..2.2).contains(&elapsed), "elapsed {elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn bundle_parts_share_task_cap() {
        let c = TrafficController::with_burst(None, Duration::ZERO);
        let task = c.register_task("bundle", TrafficKind::Bundle, Some(1000), None);
        task.begin_run();
        let p1 = task.add_part("1");
        let p2 = task.add_part("2");
        let start = instant();
        tokio::join!(p1.acquire(1000, None), p2.acquire(1000, None)).0.unwrap();
        let elapsed = start.elapsed().as_secs_f64();
        assert!((1.9..2.2).contains(&elapsed), "elapsed {elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn torrent_respects_global_cap() {
        let c = TrafficController::with_burst(Some(1000), Duration::ZERO);
        let t = c.register_task("torrent", TrafficKind::Torrent, None, None);
        t.begin_run();
        let start = instant();
        t.acquire(1000, None).await.unwrap();
        assert!(start.elapsed().as_secs_f64() >= 0.9);
    }

    #[tokio::test(start_paused = true)]
    async fn video_respects_global_cap() {
        let c = TrafficController::with_burst(Some(1000), Duration::ZERO);
        let v = c.register_task("video", TrafficKind::Video, None, None);
        v.begin_run();
        let start = instant();
        v.acquire(1000, None).await.unwrap();
        assert!(start.elapsed().as_secs_f64() >= 0.9);
    }

    #[tokio::test(start_paused = true)]
    async fn limit_change_applies_live() {
        let c = TrafficController::with_burst(Some(1000), Duration::ZERO);
        let t = c.register_task("t", TrafficKind::Http, None, None);
        t.begin_run();
        let start = instant();
        t.acquire(1000, None).await.unwrap();
        let first = start.elapsed().as_secs_f64();

        c.set_global_rate(Some(2000));
        let start = instant();
        t.acquire(1000, None).await.unwrap();
        let second = start.elapsed().as_secs_f64();
        assert!(first >= 0.9, "first {first}");
        assert!(second < first * 0.8, "live change ignored: {first} -> {second}");
    }

    #[tokio::test(start_paused = true)]
    async fn paused_task_has_no_live_rate() {
        let c = TrafficController::new(Some(1000));
        let t = c.register_task("t", TrafficKind::Http, None, None);
        let run = t.begin_run();
        let t0 = instant();
        t.report(run, 1000, t0);
        t.report(run, 1000, t0 + Duration::from_secs(2));
        assert!(t.rate_bps().is_some());
        t.pause();
        assert_eq!(t.rate_bps(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_does_not_double_count() {
        let c = TrafficController::new(None);
        let t = c.register_task("t", TrafficKind::Http, None, None);
        let run = t.begin_run();
        let t0 = instant();
        t.report(run, 1000, t0);
        t.report(run, 1000, t0 + Duration::from_secs(1));
        assert_eq!(t.rate_bps(), Some(1000.0));
        t.rollback(run, 1000, t0 + Duration::from_secs(1) + Duration::from_millis(1));
        assert_eq!(t.rate_bps(), None, "a backwards jump resets the window");
    }

    #[tokio::test(start_paused = true)]
    async fn resume_has_no_adoption_spike() {
        let c = TrafficController::new(None);
        let t = c.register_task("t", TrafficKind::Http, None, None);
        let run1 = t.begin_run();
        t.report(run1, 1000, instant());
        t.pause();
        let run2 = t.begin_run();
        let now = instant();
        // librqbit-style adoption: one huge incremental report, then silence.
        t.report(run2, 100 * 1024 * 1024, now);
        assert_eq!(t.rate_bps(), None, "one sample after resume has no span");
        t.report(run2, 0, now + Duration::from_secs(1));
        assert_eq!(t.rate_bps(), Some(0.0), "adoption must not read as speed");
    }

    #[tokio::test(start_paused = true)]
    async fn stale_progress_sample_is_ignored() {
        let c = TrafficController::new(None);
        let t = c.register_task("t", TrafficKind::Http, None, None);
        let old = t.begin_run();
        t.report(old, 500, instant());
        t.pause();
        let current = t.begin_run();
        assert_ne!(old, current);
        assert!(!t.report(old, 5000, instant()), "stale run id must be dropped");
        assert_eq!(t.downloaded(), 500);
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_cancel_latency() {
        let limiter = Limiter::with_burst(Some(10_000), Duration::ZERO);
        let token = CancellationToken::new();
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            t.cancel();
        });
        let start = instant();
        let result = limiter.acquire(100_000, Some(&token)).await;
        assert_eq!(result, Err(Cancelled));
        assert!(start.elapsed() <= Duration::from_millis(300));
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_burst_bound() {
        let limiter = Limiter::with_burst(Some(1000), Duration::from_millis(250));
        let start = instant();
        limiter.acquire(250, None).await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(20), "burst is immediate");
        limiter.acquire(250, None).await.unwrap();
        let elapsed = start.elapsed().as_secs_f64();
        assert!((0.2..0.35).contains(&elapsed), "elapsed {elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn limit_zero_and_invalid() {
        let limiter = Limiter::new(Some(0));
        assert_eq!(limiter.rate(), None, "zero means unlimited");
        let start = instant();
        limiter.acquire(1_000_000, None).await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(20));
        limiter.set_rate(Some(0));
        assert_eq!(limiter.rate(), None);
    }
}
