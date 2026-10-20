use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{
    Command, Reply, Request, Response, TaskDetailsSnapshot, TaskInfo, DEFAULT_PORT,
};
use ripdl_core::task::{Progress, TaskKind, TaskState};
use ripdl_core::{atomic_write_json, Engine, EngineConfig};

/// Persisted queue record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskRecord {
    id: String,
    url: String,
    dest_dir: Option<PathBuf>,
    state: TaskState,
    #[serde(default)]
    error: Option<String>,
    /// Extra engine headers (persisted so resume replays them).
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    /// Unix seconds when the task may start (None = immediately).
    #[serde(default)]
    start_at: Option<i64>,
    /// Tool routing (`http` default; `video` = yt-dlp sidecar).
    #[serde(default)]
    kind: TaskKind,
    /// Resolved output file once a tool produced one (completion-time).
    #[serde(default)]
    dest_file: Option<PathBuf>,
    /// Final byte counters, kept so boot-restored records still show a
    /// size/percent in the UI after a daemon restart.
    #[serde(default)]
    downloaded: u64,
    #[serde(default)]
    total: Option<u64>,
    /// Torrent identity + display name, recorded as soon as the engine
    /// manages the torrent (details queries, friendly row titles).
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    torrent_name: Option<String>,
    /// Torrent-only: streaming order (front-to-back piece fetching).
    #[serde(default)]
    sequential: bool,
    /// Torrent-only: per-file selection (None = everything), persisted so
    /// resume/boot-restore replays it.
    #[serde(default)]
    only_files: Option<Vec<usize>>,
    /// Torrent-only: lifetime bytes served to peers. Baseline kept across
    /// restarts; the seeding watcher adds rqbit's per-session counter.
    #[serde(default)]
    uploaded: u64,
    /// Torrent-only: last sampled upload rate (live; cleared at boot).
    #[serde(default)]
    upload_bps: Option<f64>,
    /// Torrent-only: finished and currently serving the swarm.
    #[serde(default)]
    seeding: bool,
    /// Torrent-only: seeding stopped by the user or by a seed limit. Kept
    /// across restarts so a stopped torrent does not start uploading again.
    #[serde(default)]
    seed_paused: bool,
    /// Torrent-only: connected peers (last sample).
    #[serde(default)]
    peers: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct QueueFile {
    tasks: Vec<TaskRecord>,
}

/// Borrowed view for persist(): skips the to_vec() deep copy every save.
#[derive(serde::Serialize)]
struct QueueFileRef<'a> {
    tasks: &'a [TaskRecord],
}

struct TaskRuntime {
    token: CancellationToken,
}

/// One active seeding watcher. The generation makes stale watchers harmless:
/// stop→start can overlap, and only the newest watcher may publish state.
struct SeedWatch {
    token: CancellationToken,
    gen: u64,
}

/// Remove a seeding watcher only if it is still the current generation.
fn forget_seed_watch(
    map: &Mutex<HashMap<String, SeedWatch>>,
    id: &str,
    generation: u64,
) {
    let mut seeders = map.lock().unwrap();
    if seeders.get(id).map(|w| w.gen) == Some(generation) {
        seeders.remove(id);
    }
}

#[derive(Debug)]
struct ProgressTrack {
    last: Progress,
    speed: Option<f64>,
    /// `(when, cumulative bytes)` samples inside the rate window. A sliding
    /// window over real elapsed time beats a per-tick delta: tick jitter and
    /// short bursts (a stalled link catching up) otherwise read as GB/s
    /// spikes followed by hard zeros.
    window: VecDeque<(Instant, u64)>,
}

/// Speed is averaged over this much wall time (>= 2 samples).
const RATE_WINDOW: Duration = Duration::from_secs(2);

struct Daemon {
    port: u16,
    queue_path: PathBuf,
    /// Where the settings file lives (kept so SetConfig can persist).
    config_path: PathBuf,
    /// Settings are swapped from SetConfig and only read elsewhere; a
    /// mutex keeps the swap sound under the many Arc aliases.
    config: Mutex<DaemonConfig>,
    records: Mutex<Vec<TaskRecord>>,
    progress: Mutex<HashMap<String, ProgressTrack>>,
    runtime: Mutex<HashMap<String, TaskRuntime>>,
    /// One token per completed torrent currently being served to the swarm.
    /// Separate from `runtime`: seeding must not hold a download semaphore
    /// slot, and pause/remove need to stop it independently.
    seeders: Mutex<HashMap<String, SeedWatch>>,
    /// Monotonic generation for `seeders` (see `SeedWatch`).
    seed_gen: std::sync::atomic::AtomicU64,
    /// Single torrent session across all torrent tasks; created on first
    /// torrent use (binds DHT/listener only when actually needed). Async
    /// mutex: creation awaits inside the locked section.
    torrents: tokio::sync::Mutex<Option<Arc<ripdl_core::torrent::TorrentSession>>>,
    /// Local addresses the live torrent session binds outgoing peer
    /// connections to (None until the session is created; empty = created
    /// without bonding). `bind_ips` is fixed at creation, so this is the only
    /// truth about whether a `bond_interfaces` change actually applied.
    torrent_bind: std::sync::Mutex<Option<Vec<std::net::IpAddr>>>,
    shutdown: tokio::sync::Notify,
    sem: Arc<tokio::sync::Semaphore>,
    /// Set during an install fetch (concurrent ones would share a .part).
    update_busy: std::sync::atomic::AtomicBool,
}

/// Daemon policy from `<data_dir>/config.json`. Shared type with all
/// frontends (`ripdl_core::rpc::DaemonSettings`); accessors (load/save/
/// defaults) live on the core type so every frontend reads the same file
/// the same way.
type DaemonConfig = ripdl_core::rpc::DaemonSettings;

impl Daemon {
    fn new(queue_path: PathBuf, config_path: PathBuf, config: DaemonConfig) -> Result<Self> {
        let max_concurrent = config.max_concurrent();
        let records = match std::fs::read(&queue_path) {
            Ok(bytes) => serde_json::from_slice::<QueueFile>(&bytes)
                .map(|q| q.tasks)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "queue file unreadable; backing it up, starting clean");
                    let _ = std::fs::rename(&queue_path, queue_path.with_extension("bak"));
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };

        Ok(Self {
            port: DEFAULT_PORT,
            queue_path,
            config_path,
            config: Mutex::new(config),
            records: Mutex::new(records),
            progress: Mutex::new(HashMap::new()),
            runtime: Mutex::new(HashMap::new()),
            seeders: Mutex::new(HashMap::new()),
            seed_gen: std::sync::atomic::AtomicU64::new(0),
            torrents: tokio::sync::Mutex::new(None),
            torrent_bind: std::sync::Mutex::new(None),
            shutdown: tokio::sync::Notify::new(),
            sem: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            update_busy: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// On startup: anything previously Running/Queued is re-queued. The
    /// engine's sidecar resume makes the restart transparent.
    fn boot_resume(self: &Arc<Self>) -> Result<()> {
        let resume_list = {
            let mut changed = false;
            let mut records = self.records.lock().unwrap();
            for r in records.iter_mut() {
                if r.state == TaskState::Running {
                    r.state = TaskState::Queued;
                    changed = true;
                }
                // Live-only fields must not survive a restart: a stale up
                // rate or "seeding" badge would lie until the watcher ticks.
                if r.seeding {
                    r.seeding = false;
                    changed = true;
                }
                if r.upload_bps.take().is_some() {
                    changed = true;
                }
            }
            // Already-clean queue: the on-disk state is identical, skip the
            // unconditional rewrite.
            if !changed {
                records
                    .iter()
                    .filter(|r| r.state == TaskState::Queued)
                    .map(|r| TaskRecord::clone(r))
                    .collect::<Vec<_>>()
            } else {
                let snapshot = records.clone();
                drop(records);
                let _ = self.persist(&snapshot);
                snapshot
                    .into_iter()
                    .filter(|r| r.state == TaskState::Queued)
                    .collect::<Vec<_>>()
            }
        };
        for r in resume_list {
            self.spawn_runner(r);
        }
        Ok(())
    }

    /// Fastresume bitfields live beside the queue, keyed by info hash. No
    /// rqbit session DB is used, so this directory is the only state rqbit
    /// keeps for us.
    fn torrent_state_dir(&self) -> Option<PathBuf> {
        self.queue_path.parent().map(|p| p.join("torrent-state"))
    }

    /// Drop bitfields for torrents the queue no longer knows: nothing else
    /// clears them (rqbit's session store is unused), so a removed task would
    /// leave a file behind forever. Runs at boot, before any session reads.
    fn prune_fastresume(&self) {
        let Some(dir) = self.torrent_state_dir() else {
            return;
        };
        let keep: std::collections::HashSet<String> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| r.info_hash.clone())
            .collect();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut removed = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("bitv") {
                continue;
            }
            let known = path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| keep.contains(stem));
            if !known {
                let _ = std::fs::remove_file(&path);
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(removed, "pruned stale fastresume bitfields");
        }
    }

    /// Re-attach finished/paused torrent tasks to the shared session after a
    /// restart. librqbit keeps no state of its own, so the session starts
    /// empty: without this, `details` reports "torrent session not running"
    /// and nothing seeds until a runner happens to run again.
    async fn boot_restore_torrents(self: &Arc<Self>) {
        let recs: Vec<TaskRecord> = {
            let records = self.records.lock().unwrap();
            records
                .iter()
                .filter(|r| {
                    r.kind == TaskKind::Torrent
                        && r.info_hash.is_some()
                        && matches!(r.state, TaskState::Completed | TaskState::Paused)
                })
                .cloned()
                .collect()
        };
        if recs.is_empty() {
            return;
        }
        let session = match self.ensure_torrent_session().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "torrent restore skipped: session unavailable");
                return;
            }
        };
        for rec in recs {
            let cfg = ripdl_core::torrent::TorrentConfig {
                id: rec.id.clone(),
                payload: rec.url.clone(),
                dest_dir: rec.dest_dir.clone().unwrap_or_else(|| {
                    self.conf()
                        .default_download_dir
                        .clone()
                        .or_else(default_download_dir)
                        .unwrap_or_else(|| PathBuf::from("."))
                }),
                only_files: rec.only_files.clone(),
                sequential: false,
            };
            match session.ensure(&cfg, false).await {
                Ok(_ensured) => {
                    // Paused mid-download torrents would otherwise show 0 B
                    // until the next runner sample: adopt the verified byte
                    // count from the restored handle now (a hard kill never
                    // ran the shutdown merge).
                    let live = session
                        .details(rec.info_hash.as_deref().unwrap_or_default())
                        .ok()
                        .map(|d| (d.downloaded, Some(d.total)));
                    // Re-read after the await: the user may have resumed the
                    // task (or removed it) while the hash-check ran. A runner
                    // now owns a Queued/Running task - leave it live.
                    let current = {
                        let mut records = self.records.lock().unwrap();
                        let Some(r) = records.iter_mut().find(|r| r.id == rec.id) else {
                            continue; // removed during the restore
                        };
                        if let Some((downloaded, total)) = live {
                            r.downloaded = downloaded;
                            r.total = total;
                        }
                        let v = (r.state, r.seed_paused);
                        let snapshot = records.clone();
                        drop(records);
                        let _ = self.persist(&snapshot);
                        v
                    };
                    let (state, seed_paused) = current;
                    if state == TaskState::Completed
                        && self.conf().seed_enabled()
                        && !seed_paused
                    {
                        self.start_seeding(&rec.id);
                    }
                    // Everything else arrived paused (ensure(running=false)),
                    // which keeps a stopped seeder from uploading and a
                    // paused download from resuming on its own, while the
                    // handle stays in the session so details still work.
                }
                Err(e) => tracing::warn!(id = %rec.id, error = %e, "torrent restore failed"),
            }
        }
        tracing::info!("torrent session restored");
    }

    /// Watcher early-exit: only the current generation may clear the flag.
    fn abort_seed_watch(self: &Arc<Self>, id: &str, generation: u64) {
        let current = self
            .seeders
            .lock()
            .unwrap()
            .get(id)
            .map(|w| w.gen == generation)
            == Some(true);
        if !current {
            return;
        }
        let snapshot = {
            let mut records = self.records.lock().unwrap();
            if let Some(r) = records.iter_mut().find(|r| r.id == id) {
                r.seeding = false;
                r.upload_bps = None;
            }
            records.clone()
        };
        let _ = self.persist(&snapshot);
        forget_seed_watch(&self.seeders, id, generation);
    }

    /// Watch one finished torrent and report upload stats into its record.
    /// Deliberately does NOT hold the download semaphore: seeding must never
    /// starve queued downloads. Stops on ratio/time limits, pause, removal
    /// or daemon shutdown.
    fn start_seeding(self: &Arc<Self>, id: &str) {
        if !self.conf().seed_enabled() {
            return;
        }
        let (hash, downloaded, cfg) = {
            let mut records = self.records.lock().unwrap();
            let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                return;
            };
            if r.state != TaskState::Completed || r.kind != TaskKind::Torrent {
                return;
            }
            let Some(hash) = r.info_hash.clone() else { return };
            // A manual (re)start overrides a previous user stop; limits that
            // were already reached are re-checked on the first sample.
            r.seeding = true;
            r.seed_paused = false;
            r.upload_bps = None;
            let cfg = ripdl_core::torrent::TorrentConfig {
                id: r.id.clone(),
                payload: r.url.clone(),
                dest_dir: r.dest_dir.clone().unwrap_or_else(|| {
                    self.conf()
                        .default_download_dir
                        .clone()
                        .or_else(default_download_dir)
                        .unwrap_or_else(|| PathBuf::from("."))
                }),
                only_files: r.only_files.clone(),
                sequential: false,
            };
            (hash, r.downloaded.max(r.total.unwrap_or(0)), cfg)
        };
        let (token, generation) = {
            let mut seeders = self.seeders.lock().unwrap();
            // Replace a winding-down watcher: stop→start can overlap, and the
            // stale one must never clear the new one's state (generation).
            if let Some(old) = seeders.remove(id) {
                old.token.cancel();
            }
            let token = CancellationToken::new();
            let generation = self
                .seed_gen
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            seeders.insert(
                id.to_string(),
                SeedWatch {
                    token: token.clone(),
                    gen: generation,
                },
            );
            (token, generation)
        };

        let this = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            let session = match this.ensure_torrent_session().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "seeding watcher: no session");
                    this.abort_seed_watch(&id, generation);
                    return;
                }
            };
            // Re-enable path: the torrent may be paused (global seeding was
            // off) or absent after a session reset. Ensure it is live again,
            // unless the task was removed while we were spawning.
            {
                let records = this.records.lock().unwrap();
                if !records
                    .iter()
                    .any(|r| r.id == id && r.state == TaskState::Completed)
                {
                    drop(records);
                    this.abort_seed_watch(&id, generation);
                    return;
                }
            }
            if let Err(e) = session.ensure(&cfg, true).await {
                tracing::warn!(error = %e, "seeding watcher: torrent ensure failed");
                this.abort_seed_watch(&id, generation);
                return;
            }
            // librqbit's upload counter is per-session; the record keeps the
            // lifetime total across restarts. Sample the baselines once.
            let base_session = session.seed_stats(&hash).map(|s| s.uploaded).unwrap_or(0);
            let base_lifetime = {
                let records = this.records.lock().unwrap();
                records
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.uploaded)
                    .unwrap_or(0)
            };
            let started = Instant::now();
            let mut last_persist = Instant::now();
            let mut should_pause = false;
            // A torrent restored paused parks itself when its hash check
            // finishes (rqbit latches the add-time start flag), which the
            // watcher has to undo: it owns "seeding" until the token is
            // cancelled. Bounded, so a torrent that cannot go live ends the
            // watch instead of spinning.
            let mut parked_wakes = 0u8;

            loop {
                if token.is_cancelled() {
                    // Pause sets seed_paused; a remove deletes the record.
                    should_pause = this
                        .records
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.seed_paused)
                        .unwrap_or(false);
                    break;
                }
                let Some(stats) = session.seed_stats(&hash) else {
                    break; // forgotten from the session
                };
                if stats.paused && parked_wakes < 5 {
                    parked_wakes = parked_wakes.saturating_add(1);
                    session.unpause_by_hash(&hash).await;
                }
                let uploaded = base_lifetime
                    .saturating_add(stats.uploaded.saturating_sub(base_session));
                let ratio = if downloaded > 0 {
                    uploaded as f64 / downloaded as f64
                } else {
                    0.0
                };
                let ratio_limit = this.conf().seed_ratio();
                let time_limit = this.conf().seed_time_min();
                let elapsed_min = started.elapsed().as_secs_f64() / 60.0;
                let limit_hit = (ratio_limit > 0.0 && ratio >= ratio_limit)
                    || (time_limit > 0 && elapsed_min >= f64::from(time_limit));
                // A restored torrent hash-checks first: `finished` is false
                // for a few ticks, which is not a reason to stop. Only an
                // explicit pause or a reached limit ends the watch.
                let stop = limit_hit || (stats.paused && parked_wakes >= 5);
                {
                    let mut records = this.records.lock().unwrap();
                    let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                        break;
                    };
                    if r.state != TaskState::Completed {
                        should_pause = r.state == TaskState::Paused;
                        break;
                    }
                    r.uploaded = uploaded;
                    r.upload_bps = Some(stats.upload_bps);
                    r.peers = stats.peers;
                    r.seeding = !stop;
                    if stop {
                        r.seed_paused = true;
                    }
                    if last_persist.elapsed() >= Duration::from_secs(15) {
                        let snapshot = records.clone();
                        drop(records);
                        let _ = this.persist(&snapshot);
                        last_persist = Instant::now();
                    }
                }
                if stop {
                    should_pause = true;
                    break;
                }
                tokio::select! {
                    _ = token.cancelled() => {
                        should_pause = this
                            .records
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|r| r.id == id)
                            .map(|r| r.seed_paused)
                            .unwrap_or(false);
                        break;
                    }
                    _ = tokio::time::sleep(if stats.initializing {
                        // A restored torrent is hashing: wake it the moment
                        // the check parks it, not two seconds later.
                        Duration::from_millis(100)
                    } else {
                        Duration::from_secs(2)
                    }) => {}
                }
            }

            // Only the newest watcher may publish the exit state: stop→start
            // can overlap, and a stale watcher must not clear the new one.
            let current = this
                .seeders
                .lock()
                .unwrap()
                .get(&id)
                .map(|w| w.gen == generation)
                .unwrap_or(false);
            if current {
                {
                    let mut records = this.records.lock().unwrap();
                    if let Some(r) = records.iter_mut().find(|r| r.id == id) {
                        r.seeding = false;
                        r.upload_bps = None;
                        if should_pause {
                            r.seed_paused = true;
                        }
                    }
                    let snapshot = records.clone();
                    drop(records);
                    let _ = this.persist(&snapshot);
                }
                if should_pause {
                    session.pause_by_hash(&hash).await;
                }
                forget_seed_watch(&this.seeders, &id, generation);
            }
        });
    }

    fn persist(&self, records: &[TaskRecord]) -> Result<()> {
        atomic_write_json(&self.queue_path, &QueueFileRef { tasks: records })?;
        Ok(())
    }

    /// Accept loops: newline-delimited JSON request/response. On Windows the
    /// daemon also serves a named pipe (locally-isolated, port-free); clients
    /// try the pipe first and fall back to this TCP socket.
    async fn run(self: Arc<Self>) -> Result<()> {
        {
            let daemon = Arc::clone(&self);
            // Windows: named pipe. Unix: domain socket. Both feed the same
            // generic connection handler; TCP below is the universal
            // fallback.
            #[cfg(windows)]
            tokio::spawn(daemon.pipe_accept_loop());
            #[cfg(not(windows))]
            tokio::spawn(daemon.unix_accept_loop());
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))
            .await
            .with_context(|| format!("cannot bind 127.0.0.1:{} (is the daemon already running?)", self.port))?;
        tracing::info!(port = self.port, "ripdl daemon listening");

        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _addr)) => {
                        let daemon = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(e) = daemon.handle_conn(stream).await {
                                tracing::debug!(error = %e, "connection ended");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                },
            }
        }

        // Grace period so in-flight replies can flush, then drain.
        tokio::time::sleep(Duration::from_millis(200)).await;
        for (_, t) in self.runtime.lock().unwrap().drain() {
            t.token.cancel();
        }
        for (_, t) in self.seeders.lock().unwrap().drain() {
            t.token.cancel();
        }
        // Fold the last live byte counts into the records before persisting:
        // otherwise a paused/boot-restored torrent reads 0 B until its next
        // runner sample (the progress map is memory-only). Lock order is
        // records -> progress, matching `set_completed`.
        {
            let mut records = self.records.lock().unwrap();
            let progress = self.progress.lock().unwrap();
            for r in records.iter_mut() {
                if let Some(t) = progress.get(&r.id) {
                    r.downloaded = t.last.downloaded;
                    r.total = t.last.total;
                }
            }
        }
        let snapshot = self.records.lock().unwrap().clone();
        let _ = self.persist(&snapshot);
        tracing::info!("ripdl daemon stopped");
        Ok(())
    }

    #[cfg(not(windows))]
    /// Unix-socket accept loop: the kernel handles connection establishment.
    /// A stale socket file from a crashed daemon is removed before bind.
    /// Failure is non-fatal: TCP keeps serving.
    async fn unix_accept_loop(self: Arc<Self>) {
        use tokio::net::UnixListener;

        let path = match ripdl_core::transport::unix_socket_path() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "unix socket path unavailable; serving TCP only");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "unix socket unavailable; serving TCP only");
                return;
            }
        };
        tracing::info!(path = %path.display(), "ripdl daemon listening");
        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _addr)) => {
                        let daemon = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(e) = daemon.handle_conn(stream).await {
                                tracing::debug!(error = %e, "socket connection ended");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                },
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    /// Named-pipe accept loop: one live instance serving, one reserved for
    /// the next client (the tokio server-handle pattern). Failure here is
    /// non-fatal: the TCP listener keeps serving.
    async fn pipe_accept_loop(self: Arc<Self>) {
        use tokio::net::windows::named_pipe::ServerOptions;

        let first = match ServerOptions::new()
            .first_pipe_instance(true)
            .create(ripdl_core::transport::PIPE_NAME)
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "named pipe unavailable; serving TCP only");
                return;
            }
        };
        let mut next = Some(first);
        loop {
            let Some(cur) = next.take() else { return };
            if let Err(e) = cur.connect().await {
                tracing::warn!(error = %e, "pipe accept failed");
                // Recycle an instance so clients don't see a hole.
                next = ServerOptions::new().create(ripdl_core::transport::PIPE_NAME).ok();
                continue;
            }
            // Reserve the next instance BEFORE serving this connection.
            next = ServerOptions::new()
                .create(ripdl_core::transport::PIPE_NAME)
                .ok();
            let daemon = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = daemon.handle_conn(cur).await {
                    tracing::debug!(error = %e, "pipe connection ended");
                }
            });
        }
    }

    async fn handle_conn(self: Arc<Self>, stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin) -> Result<()> {
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(req) => self.dispatch(req).await,
                Err(e) => Response {
                    id: 0,
                    reply: Reply::Error { message: format!("bad request: {e}") },
                },
            };
            let mut out = serde_json::to_string(&resp).context("serialize response")?;
            out.push('\n');
            write.write_all(out.as_bytes()).await?;
        }
        Ok(())
    }

    async fn dispatch(self: &Arc<Self>, req: Request) -> Response {
        match req.cmd {
            Command::Add { url, dest_dir, headers, start_at, kind, sequential, only_files } => {
                self.cmd_add(req.id, url, dest_dir, headers, start_at, kind, sequential, only_files)
            }
            Command::TorrentPreview { payload } => {
                self.cmd_torrent_preview(req.id, &payload).await
            }
            Command::List => self.cmd_list(req.id),
            Command::Details { id } => self.cmd_details(req.id, &id).await,
            Command::RefreshUrl { id, url, resume } => {
                self.cmd_refresh_url(req.id, &id, &url, resume).await
            }
            Command::UpdateTorrentSelection { id, only_files } => {
                self.cmd_update_selection(req.id, &id, only_files).await
            }
            Command::Pause { id } => self.cmd_pause(req.id, &id).await,
            Command::Resume { id } => self.cmd_resume(req.id, &id),
            Command::Remove { id, delete_files } => {
                self.cmd_remove(req.id, &id, delete_files).await
            }
            Command::Shutdown => {
                self.shutdown.notify_waiters();
                Response { id: req.id, reply: Reply::Ok }
            }
            Command::GetConfig => self.cmd_get_config(req.id),
            Command::SetConfig { settings } => self.cmd_set_config(req.id, settings).await,
            Command::CheckUpdate => self.cmd_check_update(req.id).await,
            Command::UpdateInstall => self.cmd_update_install(req.id, false).await,
        }
    }

    fn cmd_get_config(self: &Arc<Self>, req_id: u64) -> Response {
        Response {
            id: req_id,
            reply: self.config_reply(),
        }
    }

    /// Settings plus the live torrent-session bonding state. Frontends use the
    /// latter because `bond_interfaces` is only read when the shared session is
    /// created: after a toggle the config is already "true" while torrents are
    /// still unbound (and vice versa), and that must be visible, not inferred.
    fn config_reply(self: &Arc<Self>) -> Reply {
        let bind = self.torrent_bind.lock().unwrap().clone();
        Reply::Config {
            settings: self.conf(),
            torrent_session: bind.is_some(),
            torrent_bind: bind
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    /// GitHub probe on behalf of a frontend. Information only.
    async fn cmd_check_update(self: &Arc<Self>, req_id: u64) -> Response {
        match ripdl_core::update::fetch_latest().await {
            Ok(info) => Response {
                id: req_id,
                reply: Reply::Update { info },
            },
            Err(e) => Response {
                id: req_id,
                reply: Reply::Error { message: e.to_string() },
            },
        }
    }

    /// Verified setup download + launch. `silent` = unattended
    /// (`/VERYSILENT`, the opt-in auto path).
    async fn cmd_update_install(self: &Arc<Self>, req_id: u64, silent: bool) -> Response {
        match self.run_update_install(silent).await {
            Ok(()) => Response { id: req_id, reply: Reply::Ok },
            // The full context chain: "installer download failed: sums
            // unreachable: ..." beats a one-line diagnosis that hides the
            // actual cause.
            Err(e) => Response {
                id: req_id,
                reply: Reply::Error { message: format!("{e:#}") },
            },
        }
    }

    async fn run_update_install(self: &Arc<Self>, silent: bool) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        if self
            .update_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            bail!("an update is already being fetched");
        }
        let result = self.run_update_install_inner(silent).await;
        self.update_busy.store(false, Ordering::SeqCst);
        result
    }

    async fn run_update_install_inner(self: &Arc<Self>, silent: bool) -> anyhow::Result<()> {
        // Silent mode must never raise a UAC prompt nobody can answer.
        if silent && !ripdl_core::update::is_elevated() {
            bail!("unattended install needs administrator rights; use Download & Install in the app");
        }
        let info = ripdl_core::update::fetch_latest().await
            .context("update check failed")?;
        if !info.update_available {
            bail!("already on the latest release (v{})", info.current);
        }
        let setup = info.setup.as_ref()
            .context("the new release has no Windows setup.exe attached")?;
        let update_dir = ripdl_core::update::update_dir(
            self.config_path.parent().unwrap_or(&std::env::temp_dir()),
        );
        let path = ripdl_core::update::download_setup(&update_dir, setup, info.sha256.as_ref())
            .await
            .context("installer download failed")?;
        // The installer force-kills us by image name (it can no longer kill
        // our whole tree - Setup is a child of this process). HTTP runners
        // resume from their durable sidecars, but an orphaned yt-dlp would
        // keep appending to the same .part the next daemon re-invokes, so
        // stop the video runners first and leave them paused for the user.
        let video_ids: Vec<String> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.kind == ripdl_core::task::TaskKind::Video)
            .map(|r| r.id.clone())
            .collect();
        for id in video_ids {
            let token = self.runtime.lock().unwrap().get(&id).map(|rt| rt.token.clone());
            if let Some(token) = token {
                tracing::info!(task = %id, "stopping video runner before upgrade");
                token.cancel();
            }
        }
        tracing::info!(path = %path.display(), "launching upgrade installer");
        ripdl_core::update::run_installer(&path, silent)
            .context("failed to launch installer")
    }

    /// Replace the settings wholesale, persist them, and live-apply the
    /// queue-wide knob (concurrency). Per-task knobs (segments, speed) are
    /// read at runner start, so they kick in on the next task.
    async fn cmd_set_config(self: &Arc<Self>, req_id: u64, mut settings: DaemonConfig) -> Response {
        let err = |m: &str| Response {
            id: req_id,
            reply: Reply::Error { message: m.to_string() },
        };
        if settings.max_concurrent == Some(0) {
            return err("max_concurrent must be at least 1");
        }
        if settings.max_segments == Some(0) {
            return err("max_segments must be at least 1");
        }
        if settings.min_segment_size == Some(0) {
            // A zero byte split denominator would kill the planner.
            return err("min_segment_size must be at least 1");
        }
        if let Some(r) = settings.seed_ratio {
            if !r.is_finite() || r < 0.0 {
                return err("seed_ratio must be 0 (no limit) or positive");
            }
        }
        // Frontends may send 0 for "unlimited"; normalize before persisting.
        if settings.max_speed_bytes == Some(0) {
            settings.max_speed_bytes = None;
        }
        // Settings file is the source of truth: only swap the live config
        // after the save succeeded.
        if let Err(e) = settings.save(&self.config_path) {
            return err(&format!("saving config failed: {e}"));
        }
        let old_max = self.conf().max_concurrent();
        let seed_before = self.conf().seed_enabled();
        *self.config.lock().unwrap() = settings;
        let new_max = self.conf().max_concurrent();
        match new_max.checked_sub(old_max) {
            Some(grow) if grow > 0 => self.sem.add_permits(grow),
            Some(_) => {}
            None => {
                // Shrink: own freed permits and forget them (blocks this
                // reply at most until running tasks release; the permits
                // exist only if running < old_max).
                let sem = self.sem.clone();
                let take = (old_max - new_max) as u32;
                tokio::spawn(async move {
                    if let Ok(perm) = sem.acquire_many(take).await {
                        perm.forget();
                    }
                });
            }
        }
        tracing::info!(%new_max, "settings updated");
        // Applying the seeding toggle live: off = stop every watcher and
        // pause the swarm sockets (details keep working); on = start serving
        // finished torrents the user had not explicitly stopped.
        if seed_before != self.conf().seed_enabled() {
            if self.conf().seed_enabled() {
                let ids: Vec<String> = {
                    let records = self.records.lock().unwrap();
                    records
                        .iter()
                        .filter(|r| {
                            r.kind == TaskKind::Torrent
                                && r.state == TaskState::Completed
                                && !r.seed_paused
                        })
                        .map(|r| r.id.clone())
                        .collect()
                };
                for id in ids {
                    self.start_seeding(&id);
                }
            } else {
                let ids: Vec<String> = self.seeders.lock().unwrap().keys().cloned().collect();
                let hashes: Vec<String> = {
                    let records = self.records.lock().unwrap();
                    records
                        .iter()
                        .filter(|r| ids.contains(&r.id))
                        .filter_map(|r| r.info_hash.clone())
                        .collect()
                };
                for id in &ids {
                    if let Some(t) = self.seeders.lock().unwrap().get(id).map(|w| w.token.clone()) {
                        t.cancel();
                    }
                }
                if let Some(session) = self.torrents.lock().await.as_ref() {
                    for hash in hashes {
                        session.pause_by_hash(&hash).await;
                    }
                }
            }
        }
        Response {
            id: req_id,
            reply: self.config_reply(),
        }
    }

    /// Snapshot of the current settings for read paths.
    fn conf(&self) -> DaemonConfig {
        self.config.lock().unwrap().clone()
    }

    /// Engine policy for one HTTP task: daemon-wide limits plus the task's
    /// own headers (which always win over the defaults). `bond` lets a
    /// metadata probe skip the interface scan - it cannot change what the
    /// server answers.
    fn engine_config(
        &self,
        headers: &std::collections::HashMap<String, String>,
        bond: bool,
    ) -> EngineConfig {
        let conf = self.conf();
        EngineConfig {
            max_segments: conf.max_segments(),
            min_segment_size: conf.min_segment_size(),
            max_speed: conf.max_speed(),
            headers: headers.clone(),
            bond: if bond && conf.bond_interfaces() {
                ripdl_core::Bond::Auto
            } else {
                ripdl_core::Bond::Off
            },
            ..EngineConfig::default()
        }
    }

    /// Folders a task may have written its partial file to: the task's own
    /// destination, then the daemon default, then the user's Downloads
    /// (the runner's fallback chain).
    fn sidecar_dirs(&self, rec: &TaskRecord) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(d) = &rec.dest_dir {
            dirs.push(d.clone());
        }
        for d in [self.conf().default_download_dir, default_download_dir()]
            .into_iter()
            .flatten()
        {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        dirs
    }

    /// The daemon's one torrent session, created on demand (DHT/listener
    /// bind once, rooted at the download default). Torrent runners and
    /// preview queries share this same path.
    async fn ensure_torrent_session(
        &self,
    ) -> Result<Arc<ripdl_core::torrent::TorrentSession>, String> {
        let mut guard = self.torrents.lock().await;
        if guard.is_none() {
            let root = self
                .conf()
                .default_download_dir
                .clone()
                .or_else(default_download_dir)
                .unwrap_or_else(|| PathBuf::from("."));
            // Torrents share one session, so bonding is session-wide:
            // read the current uplinks once at creation (daemon restart
            // re-reads; setting changes later only affect HTTP tasks live).
            let bind = if self.conf().bond_interfaces() {
                let ips: Vec<std::net::IpAddr> = ripdl_core::engine::bonded_nics()
                    .into_iter()
                    .map(|n| std::net::IpAddr::V4(n.addr))
                    .collect();
                (!ips.is_empty()).then_some(ips)
            } else {
                None
            };
            let session =
                ripdl_core::torrent::TorrentSession::new(root, bind.clone(), self.torrent_state_dir())
                    .await
                    .map_err(|e| e.to_string())?;
            // Only after creation succeeded: this is what the session really
            // bound, not what the config says.
            *self.torrent_bind.lock().unwrap() = Some(bind.unwrap_or_default());
            *guard = Some(Arc::new(session));
        }
        Ok(Arc::clone(guard.as_ref().unwrap()))
    }

    /// Pre-add torrent preview (Add dialog file list). No task is queued;
    /// magnets may take as long as the swarm needs to yield metadata.
    async fn cmd_torrent_preview(&self, req_id: u64, payload: &str) -> Response {
        let reply: Result<Reply, String> = async {
            let session = self.ensure_torrent_session().await?;
            session
                .preview(payload)
                .await
                .map(|preview| Reply::TorrentPreview { preview })
                .map_err(|e| e.to_string())
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    fn cmd_add(
        self: &Arc<Self>,
        req_id: u64,
        url: String,
        dest_dir: Option<PathBuf>,
        headers: std::collections::HashMap<String, String>,
        start_at: Option<i64>,
        kind: TaskKind,
        sequential: bool,
        only_files: Option<Vec<usize>>,
    ) -> Response {
        // Tasks without an explicit destination land in the user's Downloads
        // folder - never in the daemon's working directory.
        let dest_dir = dest_dir
            .or_else(|| self.conf().default_download_dir.clone())
            .or_else(default_download_dir);
        // Idempotency: the same URL, tool and destination must not
        // double-download into one file (sidecar trample = corrupt output).
        // Re-adding shares the existing task instead.
        let id = uuid::Uuid::new_v4().to_string();
        let rec = TaskRecord {
            id: id.clone(),
            url: url.clone(),
            dest_dir,
            state: TaskState::Queued,
            error: None,
            headers,
            start_at,
            kind,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
            sequential,
            only_files,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
        };
        {
            // One lock scope for the duplicate check AND the push: two
            // concurrent same-URL adds (browser retry / double dialog
            // submit) must not both pass the check and both insert.
            let mut records = self.records.lock().unwrap();
            if kind == TaskKind::Torrent {
                // Torrents additionally dedupe across destinations: the daemon
                // runs ONE rqbit session where a payload maps to a single
                // ManagedTorrent (one output folder), so two same-payload tasks
                // would fight over the same handle - share the existing task.
                if let Some(r) = records
                    .iter()
                    .find(|r| r.kind == TaskKind::Torrent && r.url.trim() == url.trim())
                {
                    return Response {
                        id: req_id,
                        reply: Reply::Added { id: r.id.clone() },
                    };
                }
            }
            if let Some(id) = duplicate_task(&records, &url, &rec.dest_dir, kind) {
                return Response {
                    id: req_id,
                    reply: Reply::Added { id },
                };
            }
            records.push(rec.clone());
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        self.spawn_runner(rec);
        Response { id: req_id, reply: Reply::Added { id } }
    }

    /// Details dispatch: torrents answer with the swarm snapshot, everything
    /// else with the engine's address/segment view. One command so frontends
    /// do not have to know the kind before asking.
    async fn cmd_details(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        let kind = {
            let records = self.records.lock().unwrap();
            records.iter().find(|r| r.id == id).map(|r| r.kind)
        };
        match kind {
            Some(TaskKind::Torrent) => self.cmd_torrent_details(req_id, id).await,
            Some(_) => self.cmd_task_details(req_id, id),
            None => Response {
                id: req_id,
                reply: Reply::Error {
                    message: "task not found".into(),
                },
            },
        }
    }

    /// HTTP/video details: where the task writes, the resume identity and
    /// the byte-range map from the sidecar (the same file resume reads), so
    /// the dialog shows what the engine really planned.
    fn cmd_task_details(&self, req_id: u64, id: &str) -> Response {
        let (rec, live) = {
            // records -> progress is the order cmd_list uses; keep it.
            let records = self.records.lock().unwrap();
            match records.iter().find(|r| r.id == id) {
                Some(r) => {
                    let live = self
                        .progress
                        .lock()
                        .unwrap()
                        .get(id)
                        .map(|t| (t.last.downloaded, t.last.total, t.last.percent));
                    (r.clone(), live)
                }
                None => {
                    return Response {
                        id: req_id,
                        reply: Reply::Error {
                            message: "task not found".into(),
                        },
                    }
                }
            }
        };
        let (dest_file, etag, segments) = match find_sidecar(&rec, &self.sidecar_dirs(&rec)) {
            Some((dest, state)) => (Some(dest), state.etag, state.segments),
            None => (rec.dest_file.clone(), None, Vec::new()),
        };
        let (downloaded, total, percent) = match live {
            Some((d, t, p)) => (d, t, p),
            None => (rec.downloaded, rec.total, None),
        };
        Response {
            id: req_id,
            reply: Reply::TaskDetails {
                details: TaskDetailsSnapshot {
                    id: rec.id,
                    kind: rec.kind,
                    url: rec.url,
                    state: rec.state,
                    dest_file,
                    downloaded,
                    total,
                    percent,
                    etag,
                    error: rec.error,
                    segments,
                },
            },
        }
    }

    /// IDM's "refresh download address": an expiring signed URL can be
    /// replaced for the same file. The paused sidecar is re-pointed (byte
    /// offsets untouched) only after the new address passes the same-file
    /// check; a live runner is paused first so it cannot flush stale state.
    async fn cmd_refresh_url(
        self: &Arc<Self>,
        req_id: u64,
        id: &str,
        url: &str,
        resume: bool,
    ) -> Response {
        let reply: Result<Reply, String> = async {
            let rec = {
                let records = self.records.lock().unwrap();
                match records.iter().find(|r| r.id == id) {
                    Some(r) => r.clone(),
                    None => return Err("task not found".into()),
                }
            };
            if rec.kind != TaskKind::Http {
                return Err(if rec.kind == TaskKind::Torrent {
                    "torrents have no address to refresh; re-add the magnet or .torrent".into()
                } else {
                    "only HTTP downloads can refresh their address".into()
                });
            }
            if rec.state == TaskState::Completed {
                return Err("the download already finished; nothing to refresh".into());
            }

            // A runner holds the URL it was spawned with: stop it and wait
            // for the engine's paused sidecar before swapping the address.
            let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
            if let Some(token) = token {
                token.cancel();
                let deadline = Instant::now() + Duration::from_secs(30);
                while self.runtime.lock().unwrap().contains_key(id) {
                    if Instant::now() > deadline {
                        return Err("the download did not pause in time; try again".into());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }

            // Keep the durable progress when the task already started; a task
            // that never planned has nothing to validate.
            let mut kept = rec.downloaded;
            let mut total = rec.total;
            let mut dest_file = rec.dest_file.clone();
            if let Some((dest, mut state)) = find_sidecar(&rec, &self.sidecar_dirs(&rec)) {
                let engine = Engine::new(self.engine_config(&rec.headers, false))
                    .map_err(|e| e.to_string())?;
                let info = engine.plan(url).await.map_err(|e| e.to_string())?;
                ripdl_core::refresh::check_compatible(&dest, &state, &info)
                    .map_err(|e| e.to_string())?;
                kept = ripdl_core::refresh::kept_bytes(&state);
                total = info.total;
                dest_file = Some(dest.clone());
                ripdl_core::refresh::retarget(&mut state, url, info.etag, info.total);
                ripdl_core::refresh::save(&dest, &state).map_err(|e| e.to_string())?;
            }

            let snapshot = {
                let mut records = self.records.lock().unwrap();
                let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                    return Err("task was removed meanwhile".into());
                };
                r.url = url.to_string();
                r.state = TaskState::Paused;
                r.error = None;
                r.downloaded = kept;
                if total.is_some() {
                    r.total = total;
                }
                if dest_file.is_some() {
                    r.dest_file = dest_file;
                }
                records.clone()
            };
            let _ = self.persist(&snapshot);
            // The old attempt's samples describe the old URL's run.
            self.progress.lock().unwrap().remove(id);

            if resume {
                match self.cmd_resume(req_id, id).reply {
                    Reply::Ok => {}
                    other => return Ok(other),
                }
            }
            Ok(Reply::Refreshed {
                id: id.to_string(),
                kept,
                total,
            })
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    /// Torrent details: match the task's recorded info-hash against the
    /// session's managed torrents and snapshot files/peers/trackers.
    async fn cmd_torrent_details(&self, req_id: u64, id: &str) -> Response {
        let reply: Result<Reply, String> = async {
            let (kind_state, info_hash) = {
                let records = self.records.lock().unwrap();
                let r = records
                    .iter()
                    .find(|r| r.id == id)
                    .ok_or_else(|| "task not found".to_string())?;
                (r.kind, r.info_hash.clone())
            };
            if kind_state != TaskKind::Torrent {
                return Err("task is not a torrent".into());
            }
            let session = {
                let guard = self.torrents.lock().await;
                match guard.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return Err("torrent session not running".into()),
                }
            };
            let hash = info_hash
                .ok_or_else(|| "torrent not yet resolved (no metadata yet?)".to_string())?;
            session
                .details(&hash)
                .map_err(|e| e.to_string())
                .map(|d| Reply::TorrentDetails { details: d })
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    /// Change a torrent task's per-file selection: push it into the live
    /// session (works mid-flight, drives the picker) and persist it into
    /// the record so resume/boot-restore replays the same selection.
    async fn cmd_update_selection(
        self: &Arc<Self>,
        req_id: u64,
        id: &str,
        only_files: Option<Vec<usize>>,
    ) -> Response {
        let reply: Result<Reply, String> = async {
            // Sort + dedup to make the persisted form stable/canonical.
            let canonical = only_files.as_ref().map(|v| {
                let mut v = v.clone();
                v.sort_unstable();
                v.dedup();
                v
            });
            let (record_state, record_hash, snapshot) = {
                let mut records = self.records.lock().unwrap();
                let r = records
                    .iter_mut()
                    .find(|r| r.id == id)
                    .ok_or_else(|| "task not found".to_string())?;
                if r.kind != TaskKind::Torrent {
                    return Err("task is not a torrent".into());
                }
                r.only_files = canonical.clone();
                // One guard pass: mutate AND take the persist snapshot - no
                // second lock acquisition + full-table clone round trip.
                (r.state, r.info_hash.clone(), records.clone())
            };
            let _ = self.persist(&snapshot);
            if record_state == TaskState::Completed {
                // Not in the session (it may even not be running): the
                // recorded selection is enough - the next resume replays it.
                return Ok(Reply::Ok);
            }
            let session = {
                let guard = self.torrents.lock().await;
                match guard.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return Err("torrent session not running".into()),
                }
            };
            let hash = record_hash
                .ok_or_else(|| "torrent not yet resolved (no metadata yet?)".to_string())?;
            session
                .set_selected_files(&hash, canonical)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Reply::Ok)
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    fn cmd_list(&self, req_id: u64) -> Response {
        // The GUI polls this at 2 Hz; no snapshot deep-clone of every record
        // (urls, paths, file vectors) needed - iterate the guard directly.
        // Lock order records -> progress matches set_completed.
        let records = self.records.lock().unwrap();
        let progress = self.progress.lock().unwrap();

        let tasks = records
            .iter()
            .map(|r| {
                 let (downloaded, total, speed_bps, percent, uploaded, upload_bps, verifying) =
                    match progress.get(&r.id) {
                        Some(t) => (
                            t.last.downloaded,
                            t.last.total,
                            t.speed,
                            t.last.percent,
                            t.last.uploaded.max(r.uploaded),
                            r.upload_bps.or(t.last.upload_bps),
                            t.last.verifying,
                        ),
                        None => (
                            r.downloaded,
                            r.total,
                            None,
                            r.total.map(|_| 100.0),
                            r.uploaded,
                            r.upload_bps,
                            false,
                        ),
                    };
                TaskInfo {
                    id: r.id.clone(),
                    url: r.url.clone(),
                    state: r.state,
                    dest_dir: r.dest_dir.clone(),
                    downloaded,
                    total,
                    speed_bps,
                    uploaded,
                    upload_bps,
                    seeding: r.seeding,
                    seeding_stopped: r.seed_paused,
                    peers: r.peers,
                    verifying,
                    error: r.error.clone(),
                    start_at: r.start_at,
                    kind: r.kind,
                    percent,
                    dest_file: r.dest_file.clone(),
                    name: r.torrent_name.clone(),
                    info_hash: r.info_hash.clone(),
                    sequential: r.sequential,
                    only_files: r.only_files.clone(),
                }
            })
            .collect();

        drop(records);
        drop(progress);
        Response { id: req_id, reply: Reply::Tasks { tasks } }
    }

    async fn cmd_pause(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        // A finished torrent may be seeding: stop the watcher and remember
        // the user's choice so the next boot does not resume uploading.
        let seed_token = self
            .seeders
            .lock()
            .unwrap()
            .get(id)
            .map(|w| w.token.clone());
        if let Some(t) = seed_token {
            t.cancel();
            let snapshot = {
                let mut records = self.records.lock().unwrap();
                if let Some(r) = records.iter_mut().find(|r| r.id == id) {
                    r.seeding = false;
                    r.seed_paused = true;
                    r.upload_bps = None;
                }
                records.clone()
            };
            let _ = self.persist(&snapshot);
            self.pause_torrent_in_session(id).await;
            return Response { id: req_id, reply: Reply::Ok };
        }
        let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
        match token {
            Some(t) => {
                t.cancel();
                // The runner pauses the torrent on its way out. Also pause
                // by hash: a torrent can be live in the shared session with
                // no runner (a failed spawn, a restart race), and that leak
                // is exactly what "it uploads while paused" looked like.
                self.pause_torrent_in_session(id).await;
                Response { id: req_id, reply: Reply::Ok }
            }
            None => {
                // Not running: pausing is a no-op success only if already
                // paused (or a finished torrent whose seeding is stopped).
                let parked = self
                    .records
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| {
                        r.id == id
                            && (r.state == TaskState::Paused
                                || (r.state == TaskState::Completed
                                    && r.kind == TaskKind::Torrent))
                    });
                if parked {
                    self.pause_torrent_in_session(id).await;
                    Response { id: req_id, reply: Reply::Ok }
                } else {
                    Response {
                        id: req_id,
                        reply: Reply::Error { message: format!("task {id} not found or not running") },
                    }
                }
            }
        }
    }

    /// Park a torrent task's swarm sockets (best effort; a torrent that was
    /// never added or is already paused is fine). Keeps `details` working
    /// because the handle stays in the session.
    async fn pause_torrent_in_session(&self, id: &str) {
        let hash = {
            let records = self.records.lock().unwrap();
            records
                .iter()
                .find(|r| r.id == id && r.kind == TaskKind::Torrent)
                .and_then(|r| r.info_hash.clone())
        };
        let Some(hash) = hash else { return };
        let session = self.torrents.lock().await.clone();
        if let Some(session) = session {
            session.pause_by_hash(&hash).await;
        }
    }

    fn cmd_resume(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        enum Resume {
            Run(TaskRecord),
            Seed,
        }
        let action = {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) if matches!(r.state, TaskState::Paused | TaskState::Failed) => {
                    r.state = TaskState::Queued;
                    r.error = None;
                    Some(Resume::Run(r.clone()))
                }
                // Completed torrent: "resume" restarts seeding (the row's
                // pause button used it to stop).
                Some(r) if r.state == TaskState::Completed
                    && r.kind == TaskKind::Torrent
                    && r.info_hash.is_some() =>
                {
                    r.seed_paused = false;
                    Some(Resume::Seed)
                }
                _ => None,
            }
        };
        match action {
            Some(Resume::Run(rec)) => {
                let snapshot = self.records.lock().unwrap().clone();
                let _ = self.persist(&snapshot);
                self.spawn_runner(rec);
                Response { id: req_id, reply: Reply::Ok }
            }
            Some(Resume::Seed) => {
                if !self.conf().seed_enabled() {
                    return Response {
                        id: req_id,
                        reply: Reply::Error {
                            message: "seeding is disabled in Settings".into(),
                        },
                    };
                }
                let snapshot = self.records.lock().unwrap().clone();
                let _ = self.persist(&snapshot);
                self.start_seeding(id);
                Response { id: req_id, reply: Reply::Ok }
            }
            None => Response {
                id: req_id,
                reply: Reply::Error {
                    message: format!("task {id} not resumable (missing or running)"),
                },
            },
        }
    }

    async fn cmd_remove(self: &Arc<Self>, req_id: u64, id: &str, delete_files: bool) -> Response {
        let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
        if let Some(t) = token {
            t.cancel();
        }
        if let Some(t) = self.seeders.lock().unwrap().get(id).map(|w| w.token.clone()) {
            t.cancel();
        }
        let evicted = {
            let mut records = self.records.lock().unwrap();
            let Some(pos) = records.iter().position(|r| r.id == id) else {
                return Response { id: req_id, reply: Reply::Ok };
            };
            let rec = records.remove(pos);
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
            rec
        };
        if delete_files {
            // Drain before sweeping: the runner is inside the queue's
            // semaphore and reacts to cancellation, but its pause path may
            // still write ONE final sidecar snapshot. Sweeping first would
            // delete file + sidecar, then the runner would resurrect the
            // sidecar (orphan resume state pointing at deleted bytes).
            // The runner removes itself from `runtime` when finished.
            let mut drained = false;
            for _ in 0..60 {
                if !self.runtime.lock().unwrap().contains_key(id) {
                    drained = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let removed = delete_artifacts(&evicted);
            tracing::info!(id = id, files = removed.len(), drained, "deleted task files");
            if !drained {
                // Runner still winding down (kept its semaphore permit until
                // now): sweep again once it exits so nothing gets re-created.
                let this = Arc::clone(self);
                let rec = evicted.clone();
                tokio::spawn(async move {
                    for _ in 0..120 {
                        if !this.runtime.lock().unwrap().contains_key(&rec.id) {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    delete_artifacts(&rec);
                });
            }
        }
        self.progress.lock().unwrap().remove(id);
        // Release the task's ManagedTorrent from the shared session. Without
        // this a stale handle (old output folder, possibly wiped files)
        // hijacks the next add of the same payload via AlreadyManaged.
        let hash = evicted
            .info_hash
            .clone()
            .or_else(|| {
                if evicted.kind == TaskKind::Torrent {
                    ripdl_core::torrent::magnet_info_hash(&evicted.url)
                } else {
                    None
                }
            });
        if let Some(session) = self.torrents.lock().await.as_ref() {
            if let Some(hash) = hash {
                session.forget_by_hash(&hash).await;
            }
        }
        Response { id: req_id, reply: Reply::Ok }
    }

    /// Spawn the async worker that drives one task from queued to a terminal
    /// state. The engine's cancellation token doubles as the pause switch:
    /// canceling it persists a Paused sidecar, and resume replays it.
    fn spawn_runner(self: &Arc<Self>, rec: TaskRecord) {
        let token = CancellationToken::new();
        self.runtime
            .lock()
            .unwrap()
            .insert(rec.id.clone(), TaskRuntime { token: token.clone() });

        let this = Arc::clone(self);
        tokio::spawn(async move {
            // Every early exit below must drop the runner's runtime entry:
            // paused-while-queued tasks otherwise leave zombie TokenEntries
            // behind, which stalls the `rm --files` drain loop and leaks
            // memory per pause cycle.
            let detach_runtime = || {
                this.runtime.lock().unwrap().remove(&rec.id);
            };
            // Scheduled start: hold Queued until the wall clock arrives.
            // Cancel while waiting -> paused, like any queued task.
            match start_delay(&rec.start_at) {
                Some(delay) => {
                    tokio::select! {
                        _ = token.cancelled() => {
                            detach_runtime();
                            this.set_state(&rec.id, TaskState::Paused, None);
                            return;
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                None => {}
            }

            // Wait for a concurrency slot; pause/remove while queued bails here.
            let _permit = tokio::select! {
                _ = token.cancelled() => {
                    detach_runtime();
                    this.set_state(&rec.id, TaskState::Paused, None);
                    return;
                }
                p = this.sem.clone().acquire_owned() => match p {
                    Ok(p) => p,
                    Err(_) => {
                        detach_runtime();
                        return;
                    }
                },
            };

            // Re-check after acquiring: paused or removed while queued?
            {
                let records = this.records.lock().unwrap();
                match records.iter().find(|r| r.id == rec.id) {
                    Some(r) if r.state == TaskState::Queued => {}
                    _ => {
                        detach_runtime();
                        return;
                    }
                }
            }

            this.set_state(&rec.id, TaskState::Running, None);

            let on_progress_this = Arc::clone(&this);
            let on_progress_id = rec.id.clone();
            let on_progress = move |p| on_progress_this.on_progress(&on_progress_id, p);

            // Route by tool: HTTP gets the multi-segment engine, video gets
            // the yt-dlp sidecar, torrents share one session. All feed the
            // same progress stream and the same pause/cancel semantics; all
            // hand back the produced file/folder (for "open"/"delete").
            let result = match rec.kind {
                TaskKind::Video => {
                    let cfg = ripdl_core::hunt::HuntConfig {
                        id: rec.id.clone(),
                        program: this.conf().ytdlp_path(),
                        url: rec.url.clone(),
                        dest_dir: rec.dest_dir.clone().unwrap_or_default(),
                        headers: rec.headers.clone(),
                    };
                    ripdl_core::hunt::download(cfg, &token, on_progress)
                        .await
                        .map(|out| out.file)
                }
                TaskKind::Torrent => {
                    let session = match this.ensure_torrent_session().await {
                        Ok(s) => s,
                        Err(e) => {
                            detach_runtime();
                            this.set_state(&rec.id, TaskState::Failed, Some(e));
                            return;
                        }
                    };
                    let cfg = ripdl_core::torrent::TorrentConfig {
                        id: rec.id.clone(),
                        payload: rec.url.clone(),
                        dest_dir: rec.dest_dir.clone().unwrap_or_else(|| {
                            this.conf()
                                .default_download_dir
                                .clone()
                                .or_else(default_download_dir)
                                .unwrap_or_else(|| PathBuf::from("."))
                        }),
                        only_files: rec.only_files.clone(),
                        sequential: rec.sequential,
                    };
                    ripdl_core::torrent::download(&session, &cfg, &token, on_progress, {
                        // Identity lands here the moment the engine manages
                        // the torrent; persist it for details queries and
                        // friendly row titles.
                        let this = Arc::clone(&this);
                        let task_id = rec.id.clone();
                        move |meta| {
                            let snapshot = {
                                let mut records = this.records.lock().unwrap();
                                if let Some(r) = records.iter_mut().find(|r| r.id == task_id) {
                                    r.info_hash = Some(meta.info_hash.clone());
                                    r.torrent_name = meta.name.clone();
                                    // Adopt the root early so delete_files
                                    // works on partial torrents too.
                                    if r.dest_file.is_none() {
                                        r.dest_file = Some(meta.root.clone());
                                    }
                                }
                                records.clone()
                            };
                            let _ = this.persist(&snapshot);
                        }
                    })
                    .await
                    .map(|out| Some(out.root))
                }
                TaskKind::Http => {
                    // Per-task engine: daemon policy (config.json) sets the
                    // defaults; task headers live on the client and always
                    // override. Both the probe and every segment carry them.
                    let engine = {
                        let cfg = this.engine_config(&rec.headers, true);
                        match Engine::new(cfg) {
                            Ok(engine) => Arc::new(engine),
                            Err(e) => {
                                detach_runtime();
                                this.set_state(&rec.id, TaskState::Failed, Some(e.to_string()));
                                return;
                            }
                        }
                    };
                    engine
                        .download(&rec.url, rec.dest_dir.clone(), token, on_progress)
                        .await
                        .map(Some)
                }
            };

            this.runtime.lock().unwrap().remove(&rec.id);
            match result {
                Ok(file) => {
                    this.set_completed(&rec.id, file);
                    // Finished torrents keep serving the swarm (the handle
                    // stays in the shared session). The watcher reports
                    // upload stats until a seed limit/pause stops it.
                    if rec.kind == TaskKind::Torrent {
                        this.start_seeding(&rec.id);
                    }
                }
                Err(Error::Cancelled) => this.set_state(&rec.id, TaskState::Paused, None),
                Err(e) => this.set_state(&rec.id, TaskState::Failed, Some(e.to_string())),
            }
        });
    }

    fn set_state(&self, id: &str, state: TaskState, error: Option<String>) {
        {
            let mut records = self.records.lock().unwrap();
            // Fold the last live sample into the record on pause: a torrent
            // has no sidecar, so without this a paused mid-flight task reads
            // "0 B / unknown" until the next daemon boot re-checks it.
            let last = if state == TaskState::Paused {
                self.progress.lock().unwrap().get(id).map(|t| {
                    (t.last.downloaded, t.last.total, t.last.dest_file.clone())
                })
            } else {
                None
            };
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) => {
                    r.state = state;
                    r.error = error;
                    if let Some((downloaded, total, dest_file)) = last {
                        if downloaded > 0 {
                            r.downloaded = downloaded;
                        }
                        if total.is_some() {
                            r.total = total;
                        }
                        if r.dest_file.is_none() {
                            r.dest_file = dest_file;
                        }
                    }
                }
                None => return, // removed meanwhile: never resurrect it
            }
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        if state == TaskState::Running {
            // Fresh run: drop stale speed/progress from any previous attempt.
            self.progress.lock().unwrap().remove(id);
        }
    }

    /// Terminal success with the produced file, remembered for the
    /// "open file" / "delete files" flows (and byte counters for the UI).
    /// Size of a path on disk: files measure directly, directories sum
    /// their contents recursively. None for absent paths (multi-file torrent
    /// roots are folders, where plain `metadata.len()` would report 0).
    fn disk_size(p: &std::path::Path) -> Option<u64> {
        let meta = std::fs::metadata(p).ok()?;
        if !meta.is_dir() {
            return Some(meta.len());
        }
        let mut total = 0u64;
        let mut stack = vec![p.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                match std::fs::metadata(entry.path()) {
                    Ok(m) if m.is_dir() => stack.push(entry.path()),
                    Ok(m) => total += m.len(),
                    Err(_) => {}
                }
            }
        }
        Some(total)
    }

    fn set_completed(&self, id: &str, file: Option<PathBuf>) {
        // The produced path is a parameter, so the (possibly huge) torrent
        // tree walk runs BEFORE any lock is taken - a few-thousand-file
        // root would otherwise block every list/pause/add for the walk.
        let disk = file.as_deref().and_then(Self::disk_size);
        // progress fallback read first: keeps lock order trivial (no
        // records->progress nesting here at all).
        let fallback = self
            .progress
            .lock()
            .unwrap()
            .get(id)
            .map(|t| (t.last.downloaded, t.last.total, t.last.uploaded));
        {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) => {
                    r.state = TaskState::Completed;
                    r.error = None;
                    r.dest_file = file.clone();
                    // Uploads served while the download was still running
                    // are part of the lifetime total too.
                    if let Some((_, _, uploaded)) = fallback {
                        r.uploaded = r.uploaded.max(uploaded);
                    }
                    if r.dest_file.is_some() {
                        match disk {
                            Some(n) => {
                                r.downloaded = n;
                                r.total = Some(n);
                            }
                            // Torrent roots are directories: metadata.len()
                            // is 0 there, so fall back to the last progress
                            // snapshot when the walk fails.
                            None => {
                                if let Some((d, t, _)) = fallback {
                                    r.downloaded = d;
                                    r.total = t;
                                }
                            }
                        }
                    }
                }
                None => return, // removed meanwhile: never resurrect it
            }
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        self.progress.lock().unwrap().remove(id);
    }

    /// Track per-task progress and derive a window-averaged speed.
    fn on_progress(&self, id: &str, p: Progress) {
        // HTTP samples carry the resolved output file from the first tick on:
        // record it so details/open/delete work on partial downloads (older
        // builds only knew the path at completion).
        if let Some(dest) = p.dest_file.clone() {
            self.adopt_dest_file(id, dest);
        }
        let mut map = self.progress.lock().unwrap();
        let now = Instant::now();
        // or_insert_with clones Progress only on the first tick per task;
        // the common path skips the eager clone and the extra hash lookup.
        let entry = map.entry(id.to_string()).or_insert_with(|| ProgressTrack {
            last: p.clone(),
            speed: None,
            window: VecDeque::new(),
        });
        // Resume/recheck hash pass: `downloaded` counts verified bytes and
        // climbs at disk speed, so a derived rate would show hundreds of MB/s
        // and poison the 60 s speed graph. Report no speed and drop the
        // window so the first real sample starts from a clean baseline.
        // Percent-only samples (the yt-dlp sidecar) have no byte rate either.
        if p.verifying || p.percent.is_some() {
            entry.speed = None;
            entry.window.clear();
            entry.last = p;
            return;
        }
        entry.window.push_back((now, p.downloaded));
        // Keep ~2 s of samples (never fewer than two, so a fresh task still
        // gets a span once its second tick lands).
        while entry.window.len() > 2
            && now.duration_since(entry.window[0].0) > RATE_WINDOW
        {
            entry.window.pop_front();
        }
        let rate = match (entry.window.front(), entry.window.back()) {
            (Some(&(t0, b0)), Some(&(t1, b1))) => {
                let dt = t1.duration_since(t0).as_secs_f64();
                // Under half a second the delta is too bursty to display.
                (dt >= 0.5).then(|| ((b1.saturating_sub(b0)) as f64 / dt).clamp(0.0, 1.0e12))
            }
            _ => None,
        };
        // Torrents carry librqbit's own smoothed rate; prefer it (adopted
        // bytes on resume would otherwise look like a burst). HTTP samples
        // leave `speed_bps` None and use the window.
        entry.speed = p.speed_bps.or(rate);
        entry.last = p;
    }

    /// Persist the destination file a tool resolved (once: later samples and
    /// the completion path carry the same value).
    fn adopt_dest_file(&self, id: &str, dest: PathBuf) {
        let snapshot = {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) if r.dest_file.is_none() => {
                    r.dest_file = Some(dest);
                    records.clone()
                }
                _ => return,
            }
        };
        let _ = self.persist(&snapshot);
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Daemon-added tasks without an explicit `-d` land in the user's Downloads
/// folder, falling back to the home directory - never the daemon's cwd.
fn default_download_dir() -> Option<PathBuf> {
    dirs::download_dir().or_else(dirs::home_dir)
}

/// Resume sidecar + partial file for an HTTP task. The recorded destination
/// is checked first; otherwise the task's destination folders are scanned
/// (tasks paused by an older build never recorded `dest_file`). The sidecar's
/// URL must match the task's, so a stale sibling is ignored.
fn find_sidecar(
    rec: &TaskRecord,
    dirs: &[PathBuf],
) -> Option<(PathBuf, ripdl_core::task::TaskStateFile)> {
    if let Some(f) = &rec.dest_file {
        let mut side = f.as_os_str().to_os_string();
        side.push(".ripdl.json");
        if let Some(found) = read_sidecar_for(PathBuf::from(side), &rec.url) {
            return Some(found);
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().ends_with(".ripdl.json") {
                continue;
            }
            if let Some(found) = read_sidecar_for(entry.path(), &rec.url) {
                return Some(found);
            }
        }
    }
    None
}

/// Parse one sidecar and keep it only when it describes `url`; returns the
/// partial file it belongs to.
fn read_sidecar_for(
    sidecar: PathBuf,
    url: &str,
) -> Option<(PathBuf, ripdl_core::task::TaskStateFile)> {
    let bytes = std::fs::read(&sidecar).ok()?;
    let state = serde_json::from_slice::<ripdl_core::task::TaskStateFile>(&bytes).ok()?;
    if state.url != url {
        return None;
    }
    let stem = sidecar
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".ripdl.json"))?;
    Some((sidecar.with_file_name(stem), state))
}

/// Disk artifacts owned by a removed task: the known output file and its
/// engine sidecar, plus any sidecar in the destination whose task URL
/// matches (partial/failed runs never resolved a `dest_file`). Returns the
/// paths it deleted; best effort, never fails the removal.
fn delete_artifacts(rec: &TaskRecord) -> Vec<PathBuf> {
    let mut deleted = Vec::new();
    let remove_file = |p: &std::path::Path, out: &mut Vec<PathBuf>| {
        if std::fs::remove_file(p).is_ok() {
            out.push(p.to_path_buf());
        }
    };
    if let Some(f) = &rec.dest_file {
        // Multi-file torrent roots are directories; single-file torrents and
        // HTTP/video tasks are plain files. Sidecar logic applies to files.
        if f.is_dir() {
            if std::fs::remove_dir_all(f).is_ok() {
                deleted.push(f.to_path_buf());
            }
        } else {
            remove_file(f, &mut deleted);
            let mut sidecar = f.as_os_str().to_os_string();
            sidecar.push(".ripdl.json");
            remove_file(std::path::Path::new(&sidecar), &mut deleted);
        }
    }
    if let Some(dir) = &rec.dest_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".ripdl.json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                let parsed = serde_json::from_slice::<ripdl_core::task::TaskStateFile>(&bytes);
                match parsed {
                    Ok(state) if state.url == rec.url => {
                        let stem_len = name.len() - ".ripdl.json".len();
                        let target = dir.join(&name[..stem_len]);
                        remove_file(&target, &mut deleted);
                        remove_file(&entry.path(), &mut deleted);
                    }
                    _ => {}
                }
            }
        }
    }
    deleted
}

/// Idempotency guard: the same URL, tool and destination must not
/// double-download into one file (sidecar trample = corrupt output).
fn duplicate_task(
    records: &[TaskRecord],
    url: &str,
    dest_dir: &Option<PathBuf>,
    kind: TaskKind,
) -> Option<String> {
    records
        .iter()
        .find(|r| r.kind == kind && &r.url == url && &r.dest_dir == dest_dir)
        .map(|r| r.id.clone())
}

/// How long the runner must hold a task Queued before it may start.
fn start_delay(start_at: &Option<i64>) -> Option<std::time::Duration> {
    let start_at = (*start_at)?;
    let now = now_unix();
    if start_at <= now {
        return None; // due now (or scheduled in the past): start immediately
    }
    Some(std::time::Duration::from_secs((start_at - now) as u64))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, url: &str, dest: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            url: url.to_string(),
            dest_dir: dest.map(PathBuf::from),
            state: TaskState::Completed,
            error: None,
            headers: std::collections::HashMap::new(),
            start_at: None,
            kind: TaskKind::Http,
            sequential: false,
            only_files: None,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
        }
    }

    #[test]
    fn duplicate_found_for_same_url_dest_and_kind() {
        let records = vec![rec("a", "https://x/f.zip", Some("C:\\dl"))];
        assert_eq!(
            duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Http),
            Some("a".into())
        );
        // Different tool does not collide (a video hunt of the same URL is a
        // separate artifact).
        assert!(duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Video).is_none());
    }

    #[test]
    fn no_duplicate_for_different_dest() {
        let records = vec![rec("a", "https://x/f.zip", Some("C:\\dl"))];
        assert!(duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\other")), TaskKind::Http).is_none());
        assert!(duplicate_task(&records, "https://x/g.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Http).is_none());
    }

    #[test]
    fn start_delay_windows() {
        let now = now_unix();
        // No schedule -> start now.
        assert_eq!(start_delay(&None), None);
        // Past-due schedule -> start now.
        assert_eq!(start_delay(&Some(now - 100)), None);
        // Future schedule -> wait the remaining gap.
        assert_eq!(start_delay(&Some(now + 90)), Some(std::time::Duration::from_secs(90)));
    }

    #[test]
    fn config_missing_fields_take_defaults() {
        let cfg: DaemonConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.max_concurrent(), 3);
        assert_eq!(cfg.max_segments(), EngineConfig::default().max_segments);
        assert_eq!(cfg.min_segment_size(), EngineConfig::default().min_segment_size);
        assert_eq!(cfg.max_speed_bytes, None);
        assert_eq!(cfg.default_download_dir, None);
    }

    #[test]
    fn config_partial_file_applies_only_present_fields() {
        let cfg: DaemonConfig = serde_json::from_str(r#"{"max_concurrent": 1}"#).unwrap();
        assert_eq!(cfg.max_concurrent(), 1);
        assert_eq!(cfg.max_segments(), EngineConfig::default().max_segments);

        let cfg: DaemonConfig = serde_json::from_str(
            r#"{"max_segments": 16, "max_speed_bytes": 250000,
                "default_download_dir": "D:/dl"}"#,
        )
        .unwrap();
        assert_eq!(cfg.max_concurrent(), 3);
        assert_eq!(cfg.max_segments(), 16);
        assert_eq!(cfg.max_speed_bytes, Some(250_000));
        assert_eq!(cfg.default_download_dir, Some(PathBuf::from("D:/dl")));
    }

    #[test]
    fn config_garbage_is_rejected_not_panicking() {
        assert!(serde_json::from_str::<DaemonConfig>("nope").is_err());
    }

    #[test]
    fn config_save_roundtrip_and_zero_speed_meaning() {
        let tmp = std::env::temp_dir().join(format!("ripdl-cfgtest-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.json");
        let cfg = DaemonConfig {
            max_concurrent: Some(5),
            max_speed_bytes: Some(0),
            hide_to_tray: Some(false),
            default_download_dir: Some(PathBuf::from("D:\\dl")),
            ..Default::default()
        };
        cfg.save(&path).unwrap();
        let loaded = DaemonConfig::load(&path);
        assert_eq!(loaded, cfg);
        // `0` stays 0 on disk but reads back as "unlimited" for the engine.
        assert_eq!(loaded.max_speed(), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_artifacts_removes_output_sidecar_and_partials() {
        let tmp = std::env::temp_dir().join(format!("ripdl-deltest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = tmp.join("file.zip");
        let partial = tmp.join("other.zip");
        std::fs::write(&out, b"done").unwrap();
        std::fs::write(&partial, b"half").unwrap();
        std::fs::write(
            tmp.join("file.zip.ripdl.json"),
            r#"{"id":"x","url":"https://x/file.zip","etag":null,"total":null,"state":"paused","segments":[]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("other.zip.ripdl.json"),
            r#"{"id":"y","url":"https://x/other.zip","etag":null,"total":null,"state":"paused","segments":[]}"#,
        )
        .unwrap();
        std::fs::write(tmp.join("unrelated.txt"), b"keep").unwrap();

        let rec = TaskRecord {
            id: "t".into(),
            url: "https://x/file.zip".into(),
            dest_dir: Some(tmp.clone()),
            state: TaskState::Paused,
            error: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Http,
            sequential: false,
            only_files: None,
            dest_file: Some(out.clone()),
            downloaded: 4,
            total: Some(4),
            info_hash: None,
            torrent_name: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
        };
        let deleted = delete_artifacts(&rec);

        assert!(!out.exists(), "output file deleted");
        assert!(!tmp.join("file.zip.ripdl.json").exists(), "its sidecar deleted");
        assert!(partial.exists(), "different-url task files kept");
        assert!(tmp.join("other.zip.ripdl.json").exists(), "different-url sidecar kept");
        assert!(tmp.join("unrelated.txt").exists(), "unrelated files kept");
        assert_eq!(deleted.len(), 2, "out + sidecar only");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_artifacts_is_safe_when_nothing_matches() {
        let tmp = std::env::temp_dir().join(format!("ripdl-delsafe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let rec = TaskRecord {
            id: "t".into(),
            url: "https://x/never.seen".into(),
            dest_dir: Some(tmp.clone()),
            state: TaskState::Failed,
            error: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Http,
            sequential: false,
            only_files: None,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
        };
        assert!(delete_artifacts(&rec).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Details/refresh find a partial download's sidecar even when the record
    /// never learned `dest_file` (CLI tasks without `-d`, older builds): the
    /// destination folders are scanned and only a sidecar for the task's own
    /// URL counts; the partial path is derived from the sidecar name.
    #[test]
    fn find_sidecar_scans_folders_and_matches_url() {
        let dir = std::env::temp_dir().join(format!(
            "ripdl-daemon-sidecar-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("f.bin");
        let state = ripdl_core::task::TaskStateFile {
            id: "x".into(),
            url: "https://x/f.bin?token=1".into(),
            etag: Some("\"v1\"".into()),
            total: Some(10),
            state: TaskState::Paused,
            segments: vec![ripdl_core::task::SegmentState {
                index: 0,
                start: 0,
                end: 9,
                done: 4,
            }],
        };
        std::fs::write(
            dir.join("f.bin.ripdl.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();

        let mut rec = rec("t1", "https://x/f.bin?token=1", None);
        let (target, found) = find_sidecar(&rec, &[dir.clone()]).expect("found by scan");
        assert_eq!(target, dest);
        assert_eq!(found.segments[0].done, 4);

        // With dest_file recorded the exact sidecar wins even with no dirs.
        rec.dest_file = Some(dest.clone());
        assert!(find_sidecar(&rec, &[]).is_some());

        // Another URL must not borrow this sidecar (refresh relies on it).
        rec.dest_file = None;
        rec.url = "https://x/f.bin?token=2".into();
        assert!(find_sidecar(&rec, &[dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Boot restore is what makes completed torrents survive a daemon
    // restart: the session is rebuilt from the queue (librqbit keeps no
    // state), so Files/Peers/Trackers stay inspectable and seeding resumes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn boot_restore_keeps_completed_torrents_seeding_and_inspectable() {
        use librqbit::spawn_utils::BlockingSpawner;
        use librqbit::CreateTorrentOptions;

        let tmp = std::env::temp_dir().join(format!(
            "ripdl-daemon-restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let payload_dir = tmp.join("src");
        std::fs::create_dir_all(&payload_dir).unwrap();
        let data = vec![7u8; 400_000];
        let payload = payload_dir.join("payload.bin");
        std::fs::write(&payload, &data).unwrap();

        let spawner = BlockingSpawner::new(2);
        let tor = librqbit::create_torrent(
            &payload,
            CreateTorrentOptions {
                name: Some("payload.bin".into()),
                trackers: Vec::new(),
                piece_length: Some(64 * 1024),
            },
            &spawner,
        )
        .await
        .expect("create torrent");
        let torfile = tmp.join("payload.torrent");
        std::fs::write(&torfile, tor.as_bytes().expect("serialize")).unwrap();

        let daemon = Arc::new(
            Daemon::new(
                tmp.join("queue.json"),
                tmp.join("config.json"),
                DaemonConfig::default(),
            )
            .unwrap(),
        );
        let session = daemon.ensure_torrent_session().await.unwrap();
        let pv = session
            .preview(torfile.to_str().unwrap())
            .await
            .expect("preview local torrent");

        // Simulate the on-disk result of a finished task. ripdl's torrent
        // root is `<dest>/<name>`; librqbit writes the single file inside it.
        let dest = tmp.join("dest");
        let root = dest.join("payload.bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("payload.bin"), &data).unwrap();
        daemon.records.lock().unwrap().push(TaskRecord {
            id: "tor".into(),
            url: torfile.to_string_lossy().into_owned(),
            dest_dir: Some(dest.clone()),
            state: TaskState::Completed,
            error: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Torrent,
            sequential: false,
            only_files: None,
            dest_file: Some(root.clone()),
            downloaded: data.len() as u64,
            total: Some(data.len() as u64),
            info_hash: Some(pv.info_hash.clone()),
            torrent_name: Some("payload.bin".into()),
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
        });

        daemon.boot_restore_torrents().await;
        assert!(
            daemon
                .records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.id == "tor")
                .unwrap()
                .seeding,
            "restored completed torrent starts seeding"
        );

        // Details (files/peers/trackers) work without any runner alive. A
        // restored torrent is added paused, hash-checks, and is then woken by
        // the seeding watcher - so wait for "finished and live", not just
        // "finished".
        let mut details = None;
        for _ in 0..100 {
            if let Ok(d) = session.details(&pv.info_hash) {
                if d.finished && !d.paused {
                    details = Some(d);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let d = details.expect("restored torrent reaches finished state and seeds");
        assert_eq!(d.files.len(), 1, "file list visible after restore");
        assert_eq!(d.total, data.len() as u64);

        // The row's pause/resume pair drives seeding without forgetting the
        // torrent: stop = kept in the session (inspectable), resume = serve.
        let _ = daemon.cmd_pause(1, "tor").await;
        {
            let records = daemon.records.lock().unwrap();
            let r = records.iter().find(|r| r.id == "tor").unwrap();
            assert!(!r.seeding && r.seed_paused, "pause stops seeding");
        }
        let _ = daemon.cmd_resume(2, "tor");
        {
            let records = daemon.records.lock().unwrap();
            let r = records.iter().find(|r| r.id == "tor").unwrap();
            assert!(r.seeding && !r.seed_paused, "resume seeds again");
        }

        for (_, t) in daemon.seeders.lock().unwrap().drain() {
            t.token.cancel();
        }
        session.raw().stop().await;
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

async fn real_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = dirs::data_local_dir()
        .context("cannot resolve local data directory")?
        .join("ripdl");
    std::fs::create_dir_all(&data_dir).context("create data directory")?;
    let queue_path = data_dir.join("queue.json");
    // Stale installers (kept a week as rollback) and half downloads.
    ripdl_core::update::prune_update_dir(&ripdl_core::update::update_dir(&data_dir));

    let config_path = data_dir.join("config.json");
    let config = DaemonConfig::load(&config_path);
    let daemon = Arc::new(Daemon::new(queue_path, config_path, config).context("init daemon")?);
    daemon.boot_resume().context("resume queue")?;
    // Fastresume cache hygiene: drop bitfields for tasks that are gone.
    daemon.prune_fastresume();
    // Re-attach completed/paused torrents in the background: this is what
    // keeps finished torrents seeding and their files/peers/trackers
    // inspectable after a daemon restart (librqbit keeps no state).
    {
        let d = daemon.clone();
        tokio::spawn(async move { d.boot_restore_torrents().await });
    }
    // Opt-in unattended updates; silent on failure (checked again next boot).
    if daemon.conf().update_check() && daemon.conf().update_auto_install() {
        let d = daemon.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = d.run_update_install(true).await {
                tracing::warn!(error = %e, "automatic update failed (retries next launch)");
            }
        });
    }
    daemon.run().await
}

use ripdl_core::Error;
