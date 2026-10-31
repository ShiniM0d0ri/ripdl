use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{
    BundlePart, Command, DuplicatePolicy, ExtensionStatus, LanInfo, LanSendInfo, ProposalOutcome,
    Reply, Request, Response, TaskDetailsSnapshot, TaskInfo,
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
    /// Why a `stale` task's address died (`auth` | `size` | `name` |
    /// `login`). Cleared when the task runs again or its address is
    /// refreshed.
    #[serde(default)]
    stale_reason: Option<String>,
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
    /// Fixed output name for a "download again" copy (see `free_copy_name`).
    /// None = the engine plans the name from the server's headers.
    #[serde(default)]
    output_name: Option<String>,
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
    /// Bundle-only: folder name (`<dest>/<name>` root and the row title).
    #[serde(default)]
    bundle_name: Option<String>,
    /// Bundle-only: the parts in part-number order. Each part carries its
    /// own state/bytes, so a pause keeps every finished part finished.
    #[serde(default)]
    parts: Vec<BundlePart>,
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
    /// spikes followed by hard zeros. Torrents use a longer span
    /// (`TORRENT_RATE_WINDOW`) plus the adoption guard.
    window: VecDeque<(Instant, u64)>,
    /// When the last sample landed (elapsed time for the adoption guard).
    at: Instant,
}

/// Speed is averaged over this much wall time (>= 2 samples).
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// Torrents get a longer span: pieces land in bursts, and the row used to
/// flap between 0 and peak on every 2 s tick.
const TORRENT_RATE_WINDOW: Duration = Duration::from_secs(4);
/// Above this a byte jump in one tick cannot be network traffic (200 MiB/s
/// per tick ≈ 1.6 Gbps), it is librqbit adopting data already on disk.
const MAX_PLAUSIBLE_BPS: f64 = 200.0 * 1024.0 * 1024.0;
/// A stalled task (tool rate ~0) that suddenly gains this much in one tick is
/// a resume adoption, not a burst.
const ADOPTION_JUMP: u64 = 32 * 1024 * 1024;

/// Is this `downloaded` jump a resume/restart adoption rather than traffic?
/// librqbit reports 0 first and then counts the bytes already on disk, which
/// a byte delta would read as multi-GB/s (seen: 943 MiB/s on the row).
fn adoption_jump(prev_bytes: u64, prev_tool_bps: Option<f64>, new_bytes: u64, dt: Duration) -> bool {
    let jump = new_bytes.saturating_sub(prev_bytes);
    let secs = dt.as_secs_f64().max(0.001);
    let stalled = prev_tool_bps.unwrap_or(0.0) < 1024.0;
    jump as f64 / secs > MAX_PLAUSIBLE_BPS || (stalled && jump > ADOPTION_JUMP)
}

/// Turn measured per-uplink rates into connection weights. Relative to the
/// fastest link, 1..=10; a link under 5% of the best is quarantined (0) so new
/// peer connections stop landing on a dying tether. With no meaningful traffic
/// yet (a quiet swarm) everything stays even: 1.
/// Adapter names kept out of bonding (config `bond_exclude`): exact,
/// case-insensitive match against discovery's names, so a Settings checkbox
/// and a typed CLI value agree. Pure for tests.
fn bond_allowed(excludes: &[String], name: &str) -> bool {
    let n = name.trim().to_lowercase();
    !excludes.iter().any(|x| x.trim().to_lowercase() == n)
}

fn weights_from_rates(rates: &[(std::net::IpAddr, f64)]) -> Vec<(std::net::IpAddr, u32)> {
    let best = rates.iter().map(|(_, r)| *r).fold(0.0, f64::max);
    rates
        .iter()
        .map(|(ip, rate)| {
            if best < 1024.0 {
                return (*ip, 1);
            }
            if *rate < best * 0.05 {
                return (*ip, 0);
            }
            (*ip, ((10.0 * rate / best).round() as u32).clamp(1, 10))
        })
        .collect()
}


/// Push one `(now, cumulative bytes)` sample, dropping what fell out of the
/// window. A gap longer than the window (pause, suspend, a recheck) starts a
/// fresh baseline instead of reporting a rate across dead time.
fn push_rate_sample(
    window: &mut VecDeque<(Instant, u64)>,
    now: Instant,
    bytes: u64,
    span: Duration,
) {
    if let Some(&(t, _)) = window.back() {
        if now.duration_since(t) > span {
            window.clear();
        }
    }
    window.push_back((now, bytes));
    while window.len() > 2 && now.duration_since(window[0].0) > span {
        window.pop_front();
    }
}

/// Bytes moved across the window. Under half a second the delta is too
/// bursty to display.
fn window_rate(window: &VecDeque<(Instant, u64)>) -> Option<f64> {
    let (&(t0, b0), &(t1, b1)) = (window.front()?, window.back()?);
    let dt = t1.duration_since(t0).as_secs_f64();
    (dt >= 0.5).then(|| ((b1.saturating_sub(b0)) as f64 / dt).clamp(0.0, 1.0e12))
}

struct Daemon {
    port: u16,
    /// Per-start secret every RPC connection must present first. Written to
    /// `rpc.token` only after the TCP endpoint is ours (see `run`).
    rpc_token: String,
    queue_path: PathBuf,
    /// Where the settings file lives (kept so SetConfig can persist).
    config_path: PathBuf,
    extension_status_path: PathBuf,
    extension_status: Mutex<ExtensionStatus>,
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
    /// Uplinks whose path actually works (probed, not just discovered), plus
    /// the ones discovery found but reachability rejected. The latter is
    /// display-only, but it is what makes "my tether is configured and
    /// ignored" visible instead of mysterious.
    uplinks: std::sync::Mutex<Vec<ripdl_core::engine::UplinkNic>>,
    uplinks_dead: std::sync::Mutex<Vec<ripdl_core::engine::UplinkNic>>,
    /// Consecutive failed reachability probes per address. One dropped
    /// handshake is a phone call or a roam, not an outage: a link is only
    /// evicted on the second failure in a row and comes back on the first
    /// success.
    uplink_failures: std::sync::Mutex<HashMap<std::net::IpAddr, u8>>,
    /// Last per-uplink byte totals (for rate sampling) and the smoothed rates.
    bind_sample: std::sync::Mutex<Option<(Instant, Vec<(std::net::IpAddr, u64)>)>>,
    bind_rates: std::sync::Mutex<Vec<(std::net::IpAddr, f64)>>,
    bind_weights: std::sync::Mutex<Vec<(std::net::IpAddr, u32)>>,
    /// When the weights were last reset so a quarantined link can retry.
    bind_explore: std::sync::Mutex<Instant>,
    /// The HTTP engine's bond authority, created once at boot and shared with
    /// every task engine through `EngineConfig.bond_state`; this watcher feeds
    /// it (`feed_http_bond`), so NIC events and weight shifts apply to all
    /// downloads live, the same way the torrent session's bind set follows the
    /// machine here.
    bond: std::sync::Arc<ripdl_core::bond::Bond>,
    /// Latest `sample()` snapshot for `Reply::Config.bond_links` (plain RPC
    /// mirror; the authority itself is not serializable-lockable cheaply).
    bond_links: std::sync::Mutex<Vec<ripdl_core::rpc::BondLink>>,
    shutdown: tokio::sync::Notify,
    sem: Arc<tokio::sync::Semaphore>,
    /// Set during an install fetch (concurrent ones would share a .part).
    update_busy: std::sync::atomic::AtomicBool,
    /// LAN drop box listener. Dropped (tasks aborted) whenever receiving is
    /// turned off, re-ported or re-pointed at another folder.
    lan: tokio::sync::Mutex<Option<ripdl_core::lan::LanServer>>,
    /// Outgoing LAN copies, newest last. Volatile: a LAN copy is not a queue
    /// task, so a daemon restart simply forgets finished ones.
    lan_sends: Mutex<Vec<Arc<Mutex<LanSendState>>>>,
    /// Browser handoffs by the extension's proposal id (see `cmd_propose`).
    proposals: Mutex<HashMap<String, Proposal>>,
}

/// A browser handoff, keyed by the extension's proposal id.
struct Proposal {
    at: Instant,
    state: ProposalState,
}

enum ProposalState {
    /// Probe running; no task yet.
    Pending,
    /// Queued for this proposal: an abandon removes that task.
    Taken(String),
    /// An abandon arrived first (or while probing): a late propose is refused.
    Abandoned,
}

/// Probe budget for one handoff; the extension gives up after about 3 s.
const PROPOSAL_PROBE: Duration = Duration::from_secs(2);
/// How long a handoff keeps its state, so a late abandon still finds it.
const PROPOSAL_TTL: Duration = Duration::from_secs(60);

/// Live state of one outgoing LAN copy.
struct LanSendState {
    id: String,
    name: String,
    peer: String,
    path: PathBuf,
    total: u64,
    sent: u64,
    done: bool,
    error: Option<String>,
}

/// Daemon policy from `<data_dir>/config.json`. Shared type with all
/// frontends (`ripdl_core::rpc::DaemonSettings`); accessors (load/save/
/// defaults) live on the core type so every frontend reads the same file
/// the same way.
type DaemonConfig = ripdl_core::rpc::DaemonSettings;

/// Why a confirmed link death parks a task, beyond plain auth answers.
enum StaleCause {
    Auth,
    Changed { new: u64, old: u64 },
}

impl Daemon {
    fn new(queue_path: PathBuf, config_path: PathBuf, config: DaemonConfig) -> Result<Self> {
        let max_concurrent = config.max_concurrent();
        let extension_status_path = config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("extension-status.json");
        let extension_status = std::fs::read(&extension_status_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
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
            port: ripdl_core::transport::rpc_port(),
            rpc_token: ripdl_core::transport::generate_token(),
            queue_path,
            config_path,
            extension_status_path,
            extension_status: Mutex::new(extension_status),
            config: Mutex::new(config),
            records: Mutex::new(records),
            progress: Mutex::new(HashMap::new()),
            runtime: Mutex::new(HashMap::new()),
            seeders: Mutex::new(HashMap::new()),
            seed_gen: std::sync::atomic::AtomicU64::new(0),
            torrents: tokio::sync::Mutex::new(None),
            torrent_bind: std::sync::Mutex::new(None),
    uplinks: std::sync::Mutex::new(Vec::new()),
    uplinks_dead: std::sync::Mutex::new(Vec::new()),
    uplink_failures: std::sync::Mutex::new(HashMap::new()),
    bind_sample: std::sync::Mutex::new(None),
    bind_rates: std::sync::Mutex::new(Vec::new()),
    bind_weights: std::sync::Mutex::new(Vec::new()),
    bind_explore: std::sync::Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
            bond: std::sync::Arc::new(ripdl_core::bond::Bond::new()),
            bond_links: std::sync::Mutex::new(Vec::new()),
            sem: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            update_busy: std::sync::atomic::AtomicBool::new(false),
            lan: tokio::sync::Mutex::new(None),
            lan_sends: Mutex::new(Vec::new()),
            proposals: Mutex::new(HashMap::new()),
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
                // A bundle part that was mid-flight is re-run by the bundle
                // coordinator: leaving it "running" would misreport it as
                // live until the first sample.
                for part in r.parts.iter_mut() {
                    if part.state == TaskState::Running {
                        part.state = TaskState::Queued;
                        changed = true;
                    }
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
                            // A just-restored handle reports 0 bytes and the
                            // whole-torrent total until its check lands: never
                            // overwrite a real byte count with that (it made a
                            // paused 2.9 GB task read "0 B / 114 GiB").
                            if downloaded > 0 {
                                r.downloaded = downloaded;
                            }
                            // With a per-file selection the record's total is
                            // the selected bytes; librqbit reports all files.
                            if r.only_files.is_none() {
                                r.total = total;
                            }
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
        // TCP is the single-instance gate: a second daemon fails here before
        // it can touch the pipe, the unix socket file or the token file.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))
            .await
            .with_context(|| format!("cannot bind 127.0.0.1:{} (is the daemon already running?)", self.port))?;
        ripdl_core::transport::write_rpc_token(&self.rpc_token)
            .context("cannot write the daemon token (rpc.token)")?;
        {
            let daemon = Arc::clone(&self);
            // Windows: named pipe. Unix: domain socket. Both feed the same
            // authenticated connection handler; TCP is the universal fallback.
            #[cfg(windows)]
            tokio::spawn(daemon.pipe_accept_loop());
            #[cfg(not(windows))]
            tokio::spawn(daemon.unix_accept_loop());
        }
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
                    // A paused/restored torrent may carry a 0-byte sample
                    // from before its check; keep the last real count. And a
                    // selection fixes the total (the tool reports all files).
                    if t.last.downloaded > 0 {
                        r.downloaded = t.last.downloaded;
                    }
                    if r.only_files.is_none() {
                        r.total = t.last.total;
                    }
                }
            }
        }
        let snapshot = self.records.lock().unwrap().clone();
        let _ = self.persist(&snapshot);
        ripdl_core::transport::clear_rpc_token(&self.rpc_token);
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

        let pipe = ripdl_core::transport::pipe_name();
        let first = match ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
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
                next = ServerOptions::new().create(&pipe).ok();
                continue;
            }
            // Reserve the next instance BEFORE serving this connection.
            next = ServerOptions::new().create(&pipe).ok();
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

        // Every connection opens with the daemon token (see
        // `transport::auth_line`). Anything else gets one refusal with id 0
        // and is closed before any command is read.
        let authorized = match lines.next_line().await? {
            Some(first) => ripdl_core::transport::auth_matches(first.trim(), &self.rpc_token),
            None => false,
        };
        if !authorized {
            let refusal = Response {
                id: 0,
                reply: Reply::Error { message: "unauthorized".to_string() },
            };
            let mut out = serde_json::to_string(&refusal).context("serialize response")?;
            out.push('\n');
            write.write_all(out.as_bytes()).await?;
            return Ok(());
        }

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(req) => self.dispatch(req).await,
                Err(e) => {
                    // Recover the caller's id when the JSON parses as a request
                    // envelope (an unknown command variant lands here). Without
                    // it the reply is uncorrelatable and a caller cannot tell
                    // "unsupported command" from "crossed reply".
                    let id = serde_json::from_str::<serde_json::Value>(&line)
                        .ok()
                        .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
                        .unwrap_or(0);
                    Response {
                        id,
                        reply: Reply::Error { message: format!("bad request: {e}") },
                    }
                }
            };
            let mut out = serde_json::to_string(&resp).context("serialize response")?;
            out.push('\n');
            write.write_all(out.as_bytes()).await?;
        }
        Ok(())
    }

    async fn dispatch(self: &Arc<Self>, req: Request) -> Response {
        match req.cmd {
            Command::Add { url, dest_dir, headers, start_at, kind, sequential, only_files, on_duplicate } => {
                self.cmd_add(req.id, url, dest_dir, headers, start_at, kind, sequential, only_files, on_duplicate)
            }
            Command::TorrentPreview { payload } => {
                self.cmd_torrent_preview(req.id, &payload).await
            }
            Command::AddBundle { name, dest_dir, parts, start_at } => {
                self.cmd_add_bundle(req.id, name, dest_dir, parts, start_at)
            }
            Command::PreviewBundle { links } => self.cmd_preview_bundle(req.id, links),
            Command::List => self.cmd_list(req.id),
            Command::Details { id } => self.cmd_details(req.id, &id).await,
            Command::RefreshUrl { id, url, resume, part } => {
                self.cmd_refresh_url(req.id, &id, &url, resume, part).await
            }
            Command::AddBundleParts { id, parts } => {
                self.cmd_add_bundle_parts(req.id, &id, parts).await
            }
            Command::UpdateTorrentSelection { id, only_files } => {
                self.cmd_update_selection(req.id, &id, only_files).await
            }
            Command::Pause { id } => self.cmd_pause(req.id, &id).await,
            Command::Resume { id } => self.cmd_resume(req.id, &id),
            Command::Open { id, folder } => self.cmd_open(req.id, &id, folder),
            Command::Remove { id, delete_files } => {
                self.cmd_remove(req.id, &id, delete_files).await
            }
            Command::Propose { proposal, url, headers, expected_bytes } => {
                self.cmd_propose(req.id, proposal, url, headers, expected_bytes).await
            }
            Command::Abandon { proposal } => self.cmd_abandon(req.id, &proposal).await,
            Command::Shutdown => {
                self.shutdown.notify_waiters();
                Response { id: req.id, reply: Reply::Ok }
            }
            Command::GetConfig => self.cmd_get_config(req.id),
            Command::SetConfig { settings } => self.cmd_set_config(req.id, settings).await,
            Command::ExtensionHello { version } => self.cmd_extension_hello(req.id, version),
            Command::CheckUpdate => self.cmd_check_update(req.id).await,
            Command::UpdateInstall => self.cmd_update_install(req.id, false).await,
            Command::LanInfo => self.cmd_lan_info(req.id).await,
            Command::LanPeers => self.cmd_lan_peers(req.id).await,
            Command::LanSend { url, code, path } => {
                self.cmd_lan_send(req.id, url, code, path).await
            }
            Command::LanSends => self.cmd_lan_sends(req.id),
            Command::LanSendForget { id } => self.cmd_lan_forget(req.id, &id),
            Command::LanNewCode => self.cmd_lan_new_code(req.id).await,
        }
    }

    // -----------------------------------------------------------------
    // LAN transfer (send/receive files on the same network)
    // -----------------------------------------------------------------

    /// Receive folder: explicit setting, else `<Downloads>/ripdl-inbox`,
    /// else under the data directory when the shell has no Downloads.
    fn lan_dir(&self) -> PathBuf {
        if let Some(dir) = self.conf().lan_dir {
            return dir;
        }
        dirs::download_dir()
            .map(|d| d.join("ripdl-inbox"))
            .unwrap_or_else(|| {
                self.queue_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("lan-inbox")
            })
    }

    /// Name the other device sees ("DESKTOP-XXXX" by default).
    fn device_name() -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "ripdl".to_string())
    }

    /// (Re)start or stop the drop box so it matches the live settings.
    /// Called at boot, from SetConfig and from LanNewCode.
    async fn apply_lan(self: &Arc<Self>) {
        // Stop first: the port may be changing and a listener cannot share
        // it. Dropping the server aborts its accept/discovery tasks.
        self.lan.lock().await.take();
        let conf = self.conf();
        if !conf.lan_enabled() {
            return;
        }
        let dir = self.lan_dir();
        let code = match conf.lan_code.clone().filter(|c| !c.is_empty()) {
            Some(code) => code,
            None => {
                let code = ripdl_core::lan::generate_code();
                {
                    let mut cfg = self.config.lock().unwrap();
                    cfg.lan_code = Some(code.clone());
                    let snapshot = cfg.clone();
                    drop(cfg);
                    if let Err(e) = snapshot.save(&self.config_path) {
                        tracing::warn!(error = %e, "cannot persist the LAN code");
                    }
                }
                code
            }
        };
        let cfg = ripdl_core::lan::LanConfig {
            host: std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: conf.lan_port(),
            dir,
            code,
            open: conf.lan_open(),
            name: Self::device_name(),
        };
        // A just-dropped listener releases its port on the next scheduler
        // tick; retry briefly instead of losing the feature to that race.
        let mut last_err = None;
        for attempt in 0..10 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            match ripdl_core::lan::LanServer::start(cfg.clone()).await {
                Ok(server) => {
                    tracing::info!(
                        url = server.url(),
                        dir = %cfg.dir.display(),
                        open = cfg.open,
                        "lan receiver listening"
                    );
                    *self.lan.lock().await = Some(server);
                    return;
                }
                Err(e) => last_err = Some(e),
            }
        }
        tracing::warn!(
            error = %last_err.map(|e| e.to_string()).unwrap_or_default(),
            port = cfg.port,
            "lan receiver could not start (port in use?)"
        );
    }

    async fn cmd_lan_info(self: &Arc<Self>, req_id: u64) -> Response {
        let conf = self.conf();
        let dir = self.lan_dir();
        let (running, url, port) = {
            let server = self.lan.lock().await;
            match server.as_ref() {
                Some(s) => (true, s.url().to_string(), s.port()),
                None => (false, String::new(), conf.lan_port()),
            }
        };
        let files = ripdl_core::lan::files(&dir).await;
        Response {
            id: req_id,
            reply: Reply::Lan {
                info: LanInfo {
                    enabled: conf.lan_enabled(),
                    running,
                    url,
                    port,
                    code: conf.lan_code.clone().unwrap_or_default(),
                    open: conf.lan_open(),
                    dir,
                    files,
                },
            },
        }
    }

    async fn cmd_lan_peers(self: &Arc<Self>, req_id: u64) -> Response {
        let peers = ripdl_core::lan::discover(Duration::from_millis(1200)).await;
        Response {
            id: req_id,
            reply: Reply::LanPeers { peers },
        }
    }

    async fn cmd_lan_send(
        self: &Arc<Self>,
        req_id: u64,
        url: String,
        code: String,
        path: PathBuf,
    ) -> Response {
        let err = |m: String| Response {
            id: req_id,
            reply: Reply::Error { message: m },
        };
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            return err("empty receiver address".into());
        }
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_file() => m,
            Ok(_) => return err(format!("not a file: {}", path.display())),
            Err(e) => return err(format!("cannot read {}: {e}", path.display())),
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let id = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(Mutex::new(LanSendState {
            id: id.clone(),
            name,
            peer: url.clone(),
            path: path.clone(),
            total: meta.len(),
            sent: 0,
            done: false,
            error: None,
        }));
        {
            let mut sends = self.lan_sends.lock().unwrap();
            sends.push(state.clone());
            // The list is a progress view, not a log: keep the last 20.
            let excess = sends.len().saturating_sub(20);
            sends.drain(..excess);
        }
        let st = state.clone();
        tokio::spawn(async move {
            let cfg = ripdl_core::lan::SendConfig { url, code, path };
            let progress = {
                let st = st.clone();
                move |sent, total| {
                    if let Ok(mut s) = st.lock() {
                        s.sent = sent;
                        s.total = total;
                    }
                }
            };
            let result = ripdl_core::lan::send_file(&cfg, progress).await;
            if let Ok(mut s) = st.lock() {
                s.done = true;
                match result {
                    Ok(stored) => s.name = stored,
                    Err(e) => s.error = Some(e.to_string()),
                }
            }
        });
        Response {
            id: req_id,
            reply: Reply::LanSend { id },
        }
    }

    fn cmd_lan_sends(&self, req_id: u64) -> Response {
        let sends = self
            .lan_sends
            .lock()
            .unwrap()
            .iter()
            .filter_map(|s| s.lock().ok().map(|s| LanSendInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                peer: s.peer.clone(),
                path: s.path.clone(),
                total: s.total,
                sent: s.sent,
                done: s.done,
                error: s.error.clone(),
            }))
            .collect();
        Response {
            id: req_id,
            reply: Reply::LanSends { sends },
        }
    }

    fn cmd_lan_forget(&self, req_id: u64, id: &str) -> Response {
        self.lan_sends.lock().unwrap().retain(|s| {
            s.lock().map(|s| s.id != id).unwrap_or(false)
        });
        Response {
            id: req_id,
            reply: Reply::Ok,
        }
    }

    async fn cmd_lan_new_code(self: &Arc<Self>, req_id: u64) -> Response {
        let code = ripdl_core::lan::generate_code();
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.lan_code = Some(code);
            let snapshot = cfg.clone();
            drop(cfg);
            if let Err(e) = snapshot.save(&self.config_path) {
                return Response {
                    id: req_id,
                    reply: Reply::Error {
                        message: format!("saving config failed: {e}"),
                    },
                };
            }
        }
        self.apply_lan().await;
        self.cmd_lan_info(req_id).await
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
            torrent_rates: self
                .bind_rates
                .lock()
                .unwrap()
                .iter()
                .map(|(ip, rate)| (ip.to_string(), *rate))
                .collect(),
            torrent_unreachable: self
                .uplinks_dead
                .lock()
                .unwrap()
                .iter()
                .map(|n| format!("{} ({})", n.name, n.addr))
                .collect(),
            bond_links: self.bond_links.lock().unwrap().clone(),
            bundles: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
            extension: self.extension_status.lock().unwrap().clone(),
        }
    }

    fn cmd_extension_hello(&self, req_id: u64, version: String) -> Response {
        let version = version.trim();
        if version.is_empty() || version.len() > 64 {
            return Response {
                id: req_id,
                reply: Reply::Error {
                    message: "extension version must be 1-64 characters".into(),
                },
            };
        }
        let last_seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let status = ExtensionStatus {
            version: version.to_string(),
            last_seen,
        };
        if let Err(e) = atomic_write_json(&self.extension_status_path, &status) {
            return Response {
                id: req_id,
                reply: Reply::Error {
                    message: format!("saving extension status failed: {e}"),
                },
            };
        }
        *self.extension_status.lock().unwrap() = status;
        Response {
            id: req_id,
            reply: Reply::Ok,
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
        // Both the installer and its checksum, or no install at all.
        let (setup, sums) = ripdl_core::update::plan_install(&info)?;
        let update_dir = ripdl_core::update::update_dir(
            self.config_path.parent().unwrap_or(&std::env::temp_dir()),
        );
        let verified = ripdl_core::update::download_setup(&update_dir, setup, sums)
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
        // Re-check the bytes just before launch. The held handle shares read
        // access only, so the file cannot be replaced while it is open.
        let _held = ripdl_core::update::open_verified(&verified.path, &verified.sha256)
            .context("installer check before launch failed")?;
        tracing::info!(path = %verified.path.display(), "launching upgrade installer");
        ripdl_core::update::run_installer(&verified.path, silent)
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
        let lan_before = (
            self.conf().lan_enabled(),
            self.conf().lan_port(),
            self.lan_dir(),
            self.conf().lan_open(),
            self.conf().lan_code.clone(),
        );
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
        let lan_after = (
            self.conf().lan_enabled(),
            self.conf().lan_port(),
            self.lan_dir(),
            self.conf().lan_open(),
            self.conf().lan_code.clone(),
        );
        if lan_before != lan_after {
            self.apply_lan().await;
        }
        // Bonding applies to the live torrent session immediately: uplink
        // lists are hot-swappable now, so saving the toggle must not wait for
        // a daemon restart (HTTP tasks already read the setting per download).
        self.refresh_uplinks().await;
        self.sync_torrent_binding().await;
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
            // The one bond authority the watcher feeds; every HTTP task dials
            // from this same link set, so measured weights and skips are
            // shared across tasks instead of re-discovered per engine.
            bond_state: Some(self.bond.clone()),
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
            // Torrents share one session. Bonding is session-wide, but the
            // address list is hot-swappable (`sync_torrent_binding`), so a
            // boot-time race with Wi-Fi DHCP heals instead of sticking.
            let bind = if self.conf().bond_interfaces() {
                let ips = self.uplink_addrs();
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
            let session = Arc::new(session);
            *guard = Some(session);
        }
        Ok(Arc::clone(guard.as_ref().unwrap()))
    }

    /// Bindable addresses of the uplinks that are *proven* to work right now:
    /// one IPv4 per interface plus its global IPv6 when it has one, so v6
    /// peers are bonded and metered too instead of taking the OS route.
    fn uplink_addrs(&self) -> Vec<std::net::IpAddr> {
        let excludes = self.conf().bond_excludes();
        let mut out: Vec<std::net::IpAddr> = Vec::new();
        for n in self.uplinks.lock().unwrap().iter() {
            if !bond_allowed(&excludes, &n.name) {
                continue;
            }
            out.push(std::net::IpAddr::V4(n.addr));
            if let Some(v6) = n.v6 {
                out.push(std::net::IpAddr::V6(v6));
            }
        }
        out
    }

    /// Re-check the machine's uplinks: discovery plus a live reachability
    /// handshake. A discovered interface is not an uplink - an unplugged USB
    /// tether keeps its address *and* its DHCP gateway, and binding peer
    /// dials to a path that drops packets is what made bonding slower than a
    /// single link. A 400 Mbps phone tether is a real uplink too, though, and
    /// it drops out during a call: evict on two consecutive failures, restore
    /// on the first success, so neither a blip nor a dead path wins.
    async fn refresh_uplinks(&self) {
        let scan = ripdl_core::engine::scan_uplinks().await;
        let prev: Vec<ripdl_core::engine::UplinkNic> = self.uplinks.lock().unwrap().clone();
        let mut live = Vec::new();
        let mut dead = Vec::new();
        {
            let mut failures = self.uplink_failures.lock().unwrap();
            for nic in scan.live {
                failures.remove(&std::net::IpAddr::V4(nic.addr));
                live.push(nic);
            }
            for nic in scan.dead {
                let addr = std::net::IpAddr::V4(nic.addr);
                let strikes = failures.entry(addr).or_insert(0);
                *strikes = strikes.saturating_add(1);
                let was_live = prev.iter().any(|p| p.addr == nic.addr);
                if was_live && *strikes < 2 {
                    tracing::info!(
                        name = %nic.name,
                        addr = %nic.addr,
                        "bonding: uplink missed a probe, keeping it until the next one"
                    );
                    live.push(nic);
                } else {
                    if was_live {
                        tracing::info!(
                            name = %nic.name,
                            addr = %nic.addr,
                            "bonding: uplink is down, dropping it until it answers"
                        );
                    }
                    dead.push(nic);
                }
            }
            // Forget history for interfaces that vanished from discovery.
            let known: Vec<std::net::IpAddr> =
                live.iter().chain(dead.iter()).map(|n| std::net::IpAddr::V4(n.addr)).collect();
            failures.retain(|addr, _| known.contains(addr));
        }
        *self.uplinks.lock().unwrap() = live;
        *self.uplinks_dead.lock().unwrap() = dead;
    }

    /// Keep the shared torrent session's bonded uplinks in step with the
    /// machine. `bind_ips` used to be frozen when the session was created, so
    /// a session built at boot (restore runs immediately) before Wi-Fi
    /// finished DHCP - or before a tether was plugged in - kept that stale
    /// uplink set forever. That is the "I have two connections but torrents
    /// only use one" report. Swapping is live: new peer connections rotate
    /// over the new set, existing peers keep their source address.
    async fn sync_torrent_binding(&self) {
        let Some(session) = self.torrents.lock().await.clone() else {
            return;
        };
        let mut want: Vec<std::net::IpAddr> = if self.conf().bond_interfaces() {
            self.uplink_addrs()
        } else {
            Vec::new()
        };
        want.sort();
        let mut current = session.bind_ips();
        current.sort();
        if current == want {
            return;
        }
        session.set_bind_ips(want.clone());
        *self.torrent_bind.lock().unwrap() = Some(want.clone());
        tracing::info!(uplinks = ?want, "torrent bonding follows the current uplinks");
    }

    /// Uplinks appear and disappear while the daemon runs (Wi-Fi re-associates,
    /// a phone gets plugged in - or loses its data during a call and comes
    /// back). Probe every tick so a returning link is used within 5 s; two
    /// consecutive failures are needed to evict one, so a blip costs nothing.
    async fn watch_bonding(self: Arc<Self>) {
        let mut ticks = 0u64;
        loop {
            self.refresh_uplinks().await;
            self.sync_torrent_binding().await;
            self.apply_bind_weights().await;
            self.feed_http_bond().await;
            if ticks % 3 == 0 {
                self.reconcile_torrent_records().await;
            }
            ticks += 1;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Feed the HTTP engine's bond authority: the link set follows the proved
    /// uplinks (same truth the torrent binding uses) and the sampler folds
    /// each link's metered bytes into its rate EMA + weights every tick. The
    /// task engines dial from this instance, so a link that just left (or got
    /// fast) is acted on at the next engine pick - no restart, no re-probe.
    ///
    /// Bonding off: keep the link set empty, so a pick never lands on a
    /// stale link after the toggle; engine `bond` config is Off anyway.
    async fn feed_http_bond(&self) {
        let dt = Duration::from_secs(5).as_secs_f64();
        if !self.conf().bond_interfaces() {
            self.bond.set_links(&[]);
            *self.bond_links.lock().unwrap() = Vec::new();
            return;
        }
        let links: Vec<ripdl_core::bond::LinkInfo> = {
            let excludes = self.conf().bond_excludes();
            self.uplinks
                .lock()
                .unwrap()
                .iter()
                .filter(|n| bond_allowed(&excludes, &n.name))
                .map(|n| ripdl_core::bond::LinkInfo {
                    ip: std::net::IpAddr::V4(n.addr),
                    name: n.name.clone(),
                    default_route: n.is_default_route,
                })
                .collect()
        };
        self.bond.set_links(&links);
        let status = self.bond.sample(dt);
        *self.bond_links.lock().unwrap() = status
            .iter()
            .map(|s| ripdl_core::rpc::BondLink {
                name: s.name.clone(),
                ip: s.ip.to_string(),
                default_route: s.default_route,
                rate_bps: s.rate_bps,
                weight: s.weight,
                in_use: s.in_use,
                fails: s.fails,
            })
            .collect();
    }

    /// Weight new peer connections by what each bonded uplink actually
    /// delivers. A 50/50 split across two very different paths parks half the
    /// swarm on the slower one, which is the "bonding is slower than one
    /// connection" report. Measured from the bytes librqbit reads per bound
    /// connection (`bind_totals`), so this costs no probe traffic.
    ///
    /// Delivered bytes - not bytes per connection - on purpose: per-connection
    /// speed here mostly reflects *which* peers a link happened to get (this
    /// swarm is IPv6-heavy while its v4 peers are 95% dead), so dividing by
    /// connection count zeroes whole addresses and starves the fast link.
    /// Aggregate bytes are self-correcting: a saturated link stops growing
    /// while an idle fast one keeps scaling. A link under 5% of the best is
    /// quarantined, and every minute the weights reset to 1 for one tick so a
    /// recovered - or newly wired - link can compete.
    async fn apply_bind_weights(&self) {
        let Some(session) = self.torrents.lock().await.clone() else {
            return;
        };
        let now = Instant::now();
        let totals = session.bind_totals();
        if totals.is_empty() {
            return;
        }
        let mut prev = self.bind_sample.lock().unwrap();
        let Some((t0, before)) = prev.clone() else {
            *prev = Some((now, totals));
            return;
        };
        // A new session (daemon restart, session rebuild) restarts every
        // counter at 0: rebaseline instead of reading a bogus negative delta.
        if totals
            .iter()
            .any(|(ip, total)| before.iter().any(|(i, b)| i == ip && total < b))
        {
            *prev = Some((now, totals));
            return;
        }
        let dt = now.duration_since(t0).as_secs_f64().max(0.1);
        *prev = Some((now, totals.clone()));

        let mut rates = self.bind_rates.lock().unwrap();
        for (ip, total) in &totals {
            let old = before
                .iter()
                .find(|(i, _)| i == ip)
                .map(|(_, b)| *b)
                .unwrap_or(*total);
            let sample = total.saturating_sub(old) as f64 / dt;
            // Ease in: one 5 s window is noisy on a phone link.
            let ema = match rates.iter().find(|(i, _)| i == ip) {
                Some((_, r)) => r * 0.6 + sample * 0.4,
                None => sample,
            };
            if let Some(entry) = rates.iter_mut().find(|(i, _)| i == ip) {
                entry.1 = ema;
            } else {
                rates.push((*ip, ema));
            }
        }
        let mut weights = weights_from_rates(&rates);
        if self.bind_explore.lock().unwrap().elapsed() > Duration::from_secs(60) {
            *self.bind_explore.lock().unwrap() = Instant::now();
            weights = weights.iter().map(|(ip, _)| (*ip, 1)).collect();
        }
        session.set_bind_weights(weights.clone());
        let changed = *self.bind_weights.lock().unwrap() != weights;
        if changed {
            *self.bind_weights.lock().unwrap() = weights.clone();
            tracing::info!(
                rates = ?rates.iter().map(|(i, r)| (i.to_string(), (r / 1024.0).round())).collect::<Vec<_>>(),
                weights = ?weights.iter().map(|(i, w)| (i.to_string(), *w)).collect::<Vec<_>>(),
                "bonding weights updated from measured uplink throughput"
            );
        }
    }

    /// A paused torrent has no runner, so nothing folds its real byte count
    /// back into the queue: after a restart it used to sit at "0 B / 114 GiB"
    /// (the pre-check snapshot) until the user resumed it. The session is the
    /// source of truth while it is alive - adopt it on the same slow tick.
    async fn reconcile_torrent_records(&self) {
        let Some(session) = self.torrents.lock().await.clone() else {
            return;
        };
        let targets: Vec<(String, String, bool)> = {
            let records = self.records.lock().unwrap();
            records
                .iter()
                .filter(|r| {
                    r.kind == TaskKind::Torrent
                        && matches!(r.state, TaskState::Paused | TaskState::Completed)
                })
                .filter_map(|r| {
                    r.info_hash
                        .clone()
                        .map(|h| (r.id.clone(), h, r.only_files.is_some()))
                })
                .collect()
        };
        let mut changed = false;
        for (id, hash, selected) in targets {
            let Ok(d) = session.details(&hash) else { continue };
            let want_total = if selected {
                // A selection fixes the total, but the session reports the
                // whole-torrent byte count until its check has run: accept its
                // number only when it is plausible (<= every file's size).
                let all: u64 = d.files.iter().map(|f| f.size).sum();
                let picked: u64 = d.files.iter().filter(|f| f.included).map(|f| f.size).sum();
                if d.total > 0 && all > 0 && d.total <= all {
                    d.total
                } else {
                    picked
                }
            } else {
                d.total
            };
            let mut records = self.records.lock().unwrap();
            let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                continue;
            };
            // 0 means "not checked yet" (a restore starts there): keep what
            // the queue already knows instead of zeroing real progress.
            if d.downloaded > 0 && r.downloaded != d.downloaded {
                r.downloaded = d.downloaded;
                changed = true;
            }
            if want_total > 0 && r.total != Some(want_total) {
                r.total = Some(want_total);
                changed = true;
            }
        }
        if changed {
            let snapshot = self.records.lock().unwrap().clone();
            let _ = self.persist(&snapshot);
        }
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
        on_duplicate: DuplicatePolicy,
    ) -> Response {
        let reply = match self.enqueue(
            url,
            dest_dir,
            headers,
            start_at,
            kind,
            sequential,
            only_files,
            on_duplicate,
        ) {
            Ok(Enqueued::New(id)) => Reply::Added { id },
            Ok(Enqueued::Existing(id)) if on_duplicate == DuplicatePolicy::Report => {
                match self.task_info_for(&id) {
                    Some(task) => Reply::Duplicate { task },
                    None => Reply::Error {
                        message: "the existing download was removed; add it again".to_string(),
                    },
                }
            }
            Ok(Enqueued::Existing(id)) => Reply::Added { id },
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    /// Queue one HTTP, video, torrent or copy task. What an identical existing
    /// download means is up to `policy` (see `DuplicatePolicy`).
    fn enqueue(
        self: &Arc<Self>,
        url: String,
        dest_dir: Option<PathBuf>,
        headers: std::collections::HashMap<String, String>,
        start_at: Option<i64>,
        kind: TaskKind,
        sequential: bool,
        only_files: Option<Vec<usize>>,
        policy: DuplicatePolicy,
    ) -> Result<Enqueued, String> {
        // Tasks without an explicit destination land in the user's Downloads
        // folder - never in the daemon's working directory.
        let dest_dir = dest_dir
            .or_else(|| self.conf().default_download_dir.clone())
            .or_else(default_download_dir);
        // Idempotency: the same URL, tool and destination must not
        // double-download into one file (sidecar trample = corrupt output).
        // Re-adding shares the existing task instead.
        let id = uuid::Uuid::new_v4().to_string();
        let mut rec = TaskRecord {
            id: id.clone(),
            url: url.clone(),
            dest_dir,
            state: TaskState::Queued,
            error: None,
            stale_reason: None,
            headers,
            start_at,
            kind,
            dest_file: None,
            output_name: None,
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
            bundle_name: None,
            parts: Vec::new(),
        };
        {
            // One lock scope for the duplicate check AND the push: two
            // concurrent same-URL adds (browser retry / double dialog
            // submit) must not both pass the check and both insert.
            let mut records = self.records.lock().unwrap();
            if policy == DuplicatePolicy::Copy {
                // A copy is a second file beside the first, so it never meets
                // the duplicate check. It gets a name nothing else uses.
                rec.output_name = Some(copy_output_name(&records, &url, &rec.dest_dir, kind)?);
            } else {
                if kind == TaskKind::Torrent {
                    // Torrents additionally dedupe across destinations: the daemon
                    // runs ONE rqbit session where a payload maps to a single
                    // ManagedTorrent (one output folder), so two same-payload tasks
                    // would fight over the same handle - share the existing task.
                    if let Some(r) = records
                        .iter()
                        .find(|r| r.kind == TaskKind::Torrent && r.url.trim() == url.trim())
                    {
                        return Ok(Enqueued::Existing(r.id.clone()));
                    }
                }
                if let Some(existing) = duplicate_task(&records, &url, &rec.dest_dir, kind) {
                    return Ok(Enqueued::Existing(existing));
                }
            }
            records.push(rec.clone());
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        self.spawn_runner(rec);
        Ok(Enqueued::New(id))
    }

    /// The current view of one task, for the `Duplicate` answers. Lock order
    /// records -> progress, as in `cmd_list`.
    fn task_info_for(&self, id: &str) -> Option<TaskInfo> {
        let records = self.records.lock().unwrap();
        let progress = self.progress.lock().unwrap();
        records.iter().find(|r| r.id == id).map(|r| task_info_of(r, &progress))
    }

    /// Open a task's file, or reveal it in its folder (`Command::Open`). Only
    /// the path the task itself recorded is ever handed to the shell.
    fn cmd_open(&self, req_id: u64, id: &str, folder: bool) -> Response {
        let file = {
            let records = self.records.lock().unwrap();
            records.iter().find(|r| r.id == id).map(|r| r.dest_file.clone())
        };
        let reply = match file {
            None => Reply::Error { message: format!("no task {id}") },
            Some(None) => Reply::Error { message: "this download has no file yet".to_string() },
            Some(Some(path)) if !path.exists() => Reply::Error {
                message: format!("the file is gone: {}", path.display()),
            },
            Some(Some(path)) => match open_in_shell(&path, folder) {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error { message: format!("cannot open {}: {e}", path.display()) },
            },
        };
        Response { id: req_id, reply }
    }

    /// Browser handoff, step 1 (`Command::Propose`). The probe carries the
    /// browser's headers and the size it saw, so a link that answers
    /// differently for ripdl (cookie-gated, another file) is refused before
    /// the browser's copy is cancelled. Only `new` means "take it"; `duplicate`
    /// asks the user first (see `Reply::Proposed`).
    async fn cmd_propose(
        self: &Arc<Self>,
        req_id: u64,
        proposal: String,
        url: String,
        headers: std::collections::HashMap<String, String>,
        expected_bytes: Option<u64>,
    ) -> Response {
        // The claim covers the probe. A duplicate id is refused without
        // touching the claim that already owns it.
        let claimed = begin_proposal(&mut self.proposals.lock().unwrap(), &proposal, Instant::now());
        if let Err(why) = claimed {
            return refused(req_id, why);
        }
        let total = match self.probe_total(&url, &headers).await {
            Ok(total) => total,
            Err(why) => return self.release_refused(req_id, &proposal, why),
        };
        if let (Some(expected), Some(total)) = (expected_bytes, total) {
            if expected != total {
                let why = format!("size differs: browser saw {expected} bytes, server says {total}");
                return self.release_refused(req_id, &proposal, why);
            }
        }
        let mut map = self.proposals.lock().unwrap();
        // An abandon that arrived during the probe wins: the browser kept its copy.
        if matches!(map.get(&proposal).map(|p| &p.state), Some(ProposalState::Abandoned)) {
            map.remove(&proposal);
            return refused(req_id, "abandoned while probing");
        }
        // A duplicate is an answer, not a refusal: the extension shows the
        // choice, and the browser keeps its copy until the user decides.
        match self.enqueue(url, None, headers, None, TaskKind::Http, false, None, DuplicatePolicy::Report) {
            Ok(Enqueued::New(id)) => {
                let state = ProposalState::Taken(id.clone());
                map.insert(proposal, Proposal { at: Instant::now(), state });
                Response {
                    id: req_id,
                    reply: Reply::Proposed {
                        outcome: ProposalOutcome::New,
                        id: Some(id),
                        reason: None,
                        task: None,
                    },
                }
            }
            Ok(Enqueued::Existing(id)) => {
                // Not queued by this proposal: an abandon must not remove it.
                map.remove(&proposal);
                let task = self.task_info_for(&id);
                Response {
                    id: req_id,
                    reply: Reply::Proposed {
                        outcome: ProposalOutcome::Duplicate,
                        id: Some(id),
                        reason: None,
                        task,
                    },
                }
            }
            Err(why) => {
                map.remove(&proposal);
                refused(req_id, why)
            }
        }
    }

    /// The size `url` reports with the browser's headers, or why it could not
    /// say in time. Bounded by `PROPOSAL_PROBE`.
    async fn probe_total(
        &self,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
    ) -> Result<Option<u64>, String> {
        let engine = Engine::new(self.engine_config(headers, false)).map_err(|e| e.to_string())?;
        match tokio::time::timeout(PROPOSAL_PROBE, engine.plan(url)).await {
            Ok(Ok(plan)) => Ok(plan.total),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the server did not answer in time".to_string()),
        }
    }

    /// A `refused` answer that also releases the claim on `proposal`.
    fn release_refused(&self, req_id: u64, proposal: &str, why: String) -> Response {
        self.proposals.lock().unwrap().remove(proposal);
        refused(req_id, why)
    }

    /// Browser handoff, step 2 (`Command::Abandon`). The browser kept its copy,
    /// so the task a `new` propose queued goes, files included. An abandon that
    /// arrives first leaves a tombstone that refuses the late propose.
    async fn cmd_abandon(self: &Arc<Self>, req_id: u64, proposal: &str) -> Response {
        let taken = abandon_proposal(&mut self.proposals.lock().unwrap(), proposal, Instant::now());
        if let Some(id) = taken {
            let _ = self.cmd_remove(req_id, &id, true).await;
        }
        Response { id: req_id, reply: Reply::Ok }
    }

    /// Queue one bundle: every link becomes a part of a single task that owns
    /// `<dest>/<name>/`, the way a torrent owns its root. The caller normally
    /// groups first (`PreviewBundle`); when it does not, the same grouping
    /// runs here so a bare RPC call still lands in a sane folder.
    fn cmd_add_bundle(
        self: &Arc<Self>,
        req_id: u64,
        name: Option<String>,
        dest_dir: Option<PathBuf>,
        parts: Vec<BundlePart>,
        start_at: Option<i64>,
    ) -> Response {
        let dest_dir = dest_dir
            .or_else(|| self.conf().default_download_dir.clone())
            .or_else(default_download_dir);
        let links: Vec<ripdl_core::bundle::Link> = parts
            .iter()
            .map(|p| ripdl_core::bundle::Link {
                url: p.url.clone(),
                filename: p.filename.clone(),
            })
            .collect();
        let group = match name {
            Some(name) => ripdl_core::bundle::make_group(&name, &links),
            None => match ripdl_core::bundle::group(&links).into_iter().next() {
                Some(group) => group,
                None => {
                    return Response {
                        id: req_id,
                        reply: Reply::Error {
                            message: "no usable links in the bundle".into(),
                        },
                    }
                }
            },
        };
        if group.parts.is_empty() {
            return Response {
                id: req_id,
                reply: Reply::Error {
                    message: "no usable links in the bundle".into(),
                },
            };
        }
        // Headers travel per part; grouping only decides order and names.
        let headers_of = |url: &str| -> std::collections::HashMap<String, String> {
            parts
                .iter()
                .find(|p| p.url == url)
                .map(|p| p.headers.clone())
                .unwrap_or_default()
        };
        let record_parts: Vec<BundlePart> = group
            .parts
            .iter()
            .map(|p| BundlePart {
                url: p.url.clone(),
                filename: Some(p.filename.clone()),
                headers: headers_of(&p.url),
                state: TaskState::Queued,
                downloaded: 0,
                total: None,
                seq: p.seq,
                error: None,
                attempted: false,
            })
            .collect();

        let id = uuid::Uuid::new_v4().to_string();
        let rec = TaskRecord {
            id: id.clone(),
            // The first part keeps `url` meaningful for the paths that still
            // read it (refresh, delete sweep, logs).
            url: record_parts[0].url.clone(),
            dest_dir,
            state: TaskState::Queued,
            error: None,
            stale_reason: None,
            headers: Default::default(),
            start_at,
            kind: TaskKind::Bundle,
            dest_file: None,
            output_name: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
            sequential: false,
            only_files: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
            bundle_name: Some(group.name.clone()),
            parts: record_parts,
        };
        let sorted_urls = |parts: &[BundlePart]| -> Vec<String> {
            let mut urls: Vec<String> = parts.iter().map(|p| p.url.clone()).collect();
            urls.sort();
            urls.dedup();
            urls
        };
        let want = sorted_urls(&rec.parts);
        {
            let mut records = self.records.lock().unwrap();
            // Idempotency: the same set of part URLs is the same task.
            if let Some(existing) = records.iter().find(|r| {
                r.kind == TaskKind::Bundle && sorted_urls(&r.parts) == want
            }) {
                return Response {
                    id: req_id,
                    reply: Reply::Added {
                        id: existing.id.clone(),
                    },
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

    /// Append more links to an existing bundle ("I forgot part 7"). New
    /// links must group under the bundle's own name and carry a part number;
    /// URLs already present are skipped. The bundle then restarts (a runner
    /// holds the parts it was spawned with), which resumes paused, failed
    /// AND completed bundles - a completed task picks the new part up at
    /// once, finished parts never re-download.
    async fn cmd_add_bundle_parts(
        self: &Arc<Self>,
        req_id: u64,
        id: &str,
        parts: Vec<BundlePart>,
    ) -> Response {
        let reply: Result<Reply, String> = async {
            let bundle_name = {
                let records = self.records.lock().unwrap();
                let Some(rec) = records.iter().find(|r| r.id == id) else {
                    return Err("task not found".into());
                };
                if rec.kind != TaskKind::Bundle {
                    return Err("task is not a bundle".into());
                }
                rec.bundle_name
                    .clone()
                    .ok_or_else(|| "bundle has no name".to_string())?
            };

            // Every new link must prove it belongs: its filename groups under
            // the bundle's name and carries a part number. A stray cover file
            // or foreign set is refused with the reason, not silently added.
            let links: Vec<ripdl_core::bundle::Link> = parts
                .iter()
                .map(|p| ripdl_core::bundle::Link {
                    url: p.url.clone(),
                    filename: p.filename.clone(),
                })
                .collect();
            let group = ripdl_core::bundle::group(&links)
                .into_iter()
                .find(|g| g.name == bundle_name)
                .ok_or_else(|| {
                    format!(
                        "none of the links names a part of \"{bundle_name}\" (they group elsewhere or carry no part number)"
                    )
                })?;
            if group.parts.iter().any(|p| p.seq.is_none()) {
                return Err("a link without a part number cannot join a bundle".into());
            }

            // Headers travel per part, same as AddBundle.
            let headers_of = |url: &str| -> std::collections::HashMap<String, String> {
                parts
                    .iter()
                    .find(|p| p.url == url)
                    .map(|p| p.headers.clone())
                    .unwrap_or_default()
            };

            // URLs the bundle already carries are skipped, not errors.
            let existing: Vec<String> = {
                let records = self.records.lock().unwrap();
                records
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.parts.iter().map(|p| p.url.clone()).collect())
                    .unwrap_or_default()
            };
            let fresh: Vec<_> = group
                .parts
                .iter()
                .filter(|p| !existing.iter().any(|old| old == &p.url))
                .collect();
            if fresh.is_empty() {
                return Ok(Reply::BundlePartsAdded { id: id.to_string(), added: 0 });
            }

            // A runner holds the parts it was spawned with: stop it before
            // mutating the record, then restart below.
            let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
            if let Some(token) = token {
                token.cancel();
                let deadline = Instant::now() + Duration::from_secs(30);
                while self.runtime.lock().unwrap().contains_key(id) {
                    if Instant::now() > deadline {
                        return Err("the bundle did not pause in time; try again".into());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }

            let mut added = 0usize;
            let snapshot = {
                let mut records = self.records.lock().unwrap();
                let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                    return Err("task was removed meanwhile".into());
                };
                for p in &fresh {
                    if r.parts.iter().any(|old| old.url == p.url) {
                        continue; // raced in meanwhile: still idempotent
                    }
                    added += 1;
                    r.parts.push(BundlePart {
                        url: p.url.clone(),
                        filename: Some(p.filename.clone()),
                        headers: headers_of(&p.url),
                        state: TaskState::Queued,
                        downloaded: 0,
                        total: None,
                        seq: p.seq,
                        error: None,
                        attempted: false,
                    });
                }
                r.parts.sort_by_key(|p| p.seq.unwrap_or(u32::MAX));
                if added > 0 {
                    r.state = TaskState::Queued;
                    r.error = None;
                }
                let (downloaded, total) = Self::bundle_totals(&r.parts);
                r.downloaded = downloaded;
                r.total = total;
                records.clone()
            };
            let _ = self.persist(&snapshot);
            self.progress.lock().unwrap().remove(id);

            if added > 0 {
                self.spawn_runner(
                    self.records
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|r| r.id == id)
                        .cloned()
                        .ok_or_else(|| "task was removed meanwhile".to_string())?,
                );
            }
            Ok(Reply::BundlePartsAdded { id: id.to_string(), added })
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    /// Group a selection without writing anything: the extension's offer chip
    /// and `ripdl bundle preview` show this instead of guessing the rules.
    fn cmd_preview_bundle(&self, req_id: u64, links: Vec<BundlePart>) -> Response {
        let links: Vec<ripdl_core::bundle::Link> = links
            .iter()
            .map(|p| ripdl_core::bundle::Link {
                url: p.url.clone(),
                filename: p.filename.clone(),
            })
            .collect();
        Response {
            id: req_id,
            reply: Reply::BundlePreview {
                groups: ripdl_core::bundle::group(&links),
            },
        }
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
        let is_bundle = rec.kind == TaskKind::Bundle;
        let (dest_file, etag, segments) = if is_bundle {
            // Parts have their own sidecars inside the root; the row's file
            // is the folder itself.
            (rec.dest_file.clone(), None, Vec::new())
        } else {
            match find_sidecar(&rec, &self.sidecar_dirs(&rec)) {
                Some((dest, state)) => (Some(dest), state.etag, state.segments),
                None => (rec.dest_file.clone(), None, Vec::new()),
            }
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
                    stale_reason: rec.stale_reason,
                    segments,
                    parts: if is_bundle { rec.parts } else { Vec::new() },
                    bundle_name: rec.bundle_name,
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
        part: Option<u32>,
    ) -> Response {
        if part.is_some() {
            return self.cmd_refresh_part(req_id, id, url, resume, part.unwrap()).await;
        }
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
                r.stale_reason = None;
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
                part: None,
            })
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    /// Refresh ONE part of a bundle: swap that part's address while keeping
    /// its on-disk bytes. The whole bundle pauses for the swap (a runner
    /// holds the parts it was spawned with) and resumes afterwards; parts
    /// that already finished stay finished.
    async fn cmd_refresh_part(
        self: &Arc<Self>,
        req_id: u64,
        id: &str,
        url: &str,
        resume: bool,
        seq: u32,
    ) -> Response {
        let reply: Result<Reply, String> = async {
            let (rec, index) = {
                let records = self.records.lock().unwrap();
                let Some(rec) = records.iter().find(|r| r.id == id) else {
                    return Err("task not found".into());
                };
                if rec.kind != TaskKind::Bundle {
                    return Err("--part only applies to a bundle task".into());
                }
                let index = rec
                    .parts
                    .iter()
                    .position(|p| p.seq == Some(seq))
                    .ok_or_else(|| format!("no part {seq} in this bundle"))?;
                (rec.clone(), index)
            };
            if rec.parts[index].state == TaskState::Completed {
                return Err(format!("part {seq} already finished; nothing to refresh"));
            }

            // Stop the bundle's runner so no part is mid-flight during the
            // swap, then wait for it to land its sidecars.
            let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
            if let Some(token) = token {
                token.cancel();
                let deadline = Instant::now() + Duration::from_secs(30);
                while self.runtime.lock().unwrap().contains_key(id) {
                    if Instant::now() > deadline {
                        return Err("the bundle did not pause in time; try again".into());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }

            // A part that never started has no sidecar to validate: the swap
            // is pure bookkeeping. One that did gets the same compatibility
            // gate as a whole task (same file name + size, byte ranges).
            let part = &rec.parts[index];
            let filename = part
                .filename
                .clone()
                .unwrap_or_else(|| ripdl_core::bundle::Link::new(part.url.as_str()).part_filename());
            let mut kept = part.downloaded;
            let mut total = part.total;
            if let Some(root) = &rec.dest_file {
                let sidecar = root.join(format!("{filename}.ripdl.json"));
                if let Some((dest, mut state)) = read_sidecar_for(sidecar, &part.url) {
                    let engine = Engine::new(self.engine_config(&part.headers, false))
                        .map_err(|e| e.to_string())?;
                    let info = engine.plan(url).await.map_err(|e| e.to_string())?;
                    ripdl_core::refresh::check_compatible(&dest, &state, &info)
                        .map_err(|e| e.to_string())?;
                    kept = ripdl_core::refresh::kept_bytes(&state);
                    total = info.total;
                    ripdl_core::refresh::retarget(&mut state, url, info.etag, info.total);
                    ripdl_core::refresh::save(&dest, &state).map_err(|e| e.to_string())?;
                }
            }

            let snapshot = {
                let mut records = self.records.lock().unwrap();
                let Some(r) = records.iter_mut().find(|r| r.id == id) else {
                    return Err("task was removed meanwhile".into());
                };
                let Some(part) = r.parts.get_mut(index) else {
                    return Err("part was removed meanwhile".into());
                };
                part.url = url.to_string();
                part.state = TaskState::Paused;
                part.error = None;
                part.downloaded = kept;
                if total.is_some() {
                    part.total = total;
                }
                let (downloaded, total) = Self::bundle_totals(&r.parts);
                r.downloaded = downloaded;
                r.total = total;
                r.state = TaskState::Paused;
                r.error = None;
                records.clone()
            };
            let _ = self.persist(&snapshot);
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
                part: Some(seq),
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
            .map(|r| task_info_of(r, &progress))
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
                Some(r) if matches!(r.state, TaskState::Paused | TaskState::Failed | TaskState::Stale) => {
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

    /// Drive a bundle: one engine run per part, each taking a slot from the
    /// daemon-wide pool, all writing into the task's own folder.
    ///
    /// `Ok(Some(root))` = every part finished; `Ok(None)` = cancelled (or the
    /// record went away); `Err` = a part failed - the other parts still ran
    /// to the end.
    async fn run_bundle(
        self: &Arc<Self>,
        rec: TaskRecord,
        token: CancellationToken,
    ) -> std::result::Result<Option<PathBuf>, String> {
        // Re-check after the scheduled-start wait: paused/removed while queued?
        {
            let records = self.records.lock().unwrap();
            match records.iter().find(|r| r.id == rec.id) {
                Some(r) if r.state == TaskState::Queued => {}
                _ => return Ok(None),
            }
        }
        // Root: reuse the folder recorded on an earlier run (a resume must keep
        // writing into the same one), else claim a free name now.
        let root = match rec.dest_file.clone() {
            Some(root) => root,
            None => {
                let dir = rec.dest_dir.clone().unwrap_or_else(|| PathBuf::from("."));
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let name = rec.bundle_name.clone().unwrap_or_else(|| "bundle".into());
                let root = ripdl_core::bundle::free_dir(&dir, &name);
                // Persist the root before the first byte: delete_files and the
                // row's "Save to" must work on a partial bundle too.
                let snapshot = {
                    let mut records = self.records.lock().unwrap();
                    match records.iter_mut().find(|r| r.id == rec.id) {
                        Some(r) => r.dest_file = Some(root.clone()),
                        None => return Ok(None),
                    }
                    records.clone()
                };
                let _ = self.persist(&snapshot);
                root
            }
        };
        std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        self.set_state(&rec.id, TaskState::Running, None);

        // G2: the row's bar is summed part bytes / summed part totals, and
        // the all-known rule (deliberate: a half-known sum paints a
        // percentage that drops as the rest are probed) needs every size
        // first. Parts used to learn theirs only when their own runner
        // started, so with a few download slots the bar stayed empty until
        // completions made slots - and probes - trickle through. One probe
        // wave upfront paints the bar from the first real tick.
        self.probe_part_totals(&rec.id).await;

        let mut workers = tokio::task::JoinSet::new();
        for index in 0..rec.parts.len() {
            let this = Arc::clone(self);
            let part_token = token.child_token();
            let root = root.clone();
            let id = rec.id.clone();
            workers.spawn(async move { this.run_bundle_part(&id, index, &root, part_token).await });
        }
        let mut failure: Option<String> = None;
        while let Some(joined) = workers.join_next().await {
            let outcome = match joined {
                Ok(outcome) => outcome,
                Err(e) => Err(format!("part worker stopped: {e}")),
            };
            if let Err(e) = outcome {
                if failure.is_none() {
                    failure = Some(e);
                }
            }
        }
        // Cancellation wins over a part error: a paused bundle is not failed.
        if token.is_cancelled() {
            return Ok(None);
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(Some(root)),
        }
    }

    /// One part of a bundle: take a normal download slot, run the engine with
    /// this part's own name and headers, publish its state.
    /// Probe the sizes of this bundle's not-yet-known parts, all at once.
    /// The probe is the same `engine.plan` a part's own runner does; it can
    /// only move `total` fields, never bytes. A failed probe leaves the part
    /// unknown - its runner probes again anyway, the aggregate just keeps
    /// the all-known rule a little longer.
    async fn probe_part_totals(self: &Arc<Self>, id: &str) {
        let parts: Vec<(usize, String, std::collections::HashMap<String, String>)> = {
            let records = self.records.lock().unwrap();
            let Some(rec) = records.iter().find(|r| r.id == id) else {
                return;
            };
            rec.parts
                .iter()
                .enumerate()
                // Only parts no runner has ever taken: a Failed/Paused part
                // that re-runs gets probed by its own runner on resume (its
                // probed flag stays set), so the wave would double-count on
                // a request-counting host.
                .filter(|(_, p)| p.total.is_none() && !p.attempted)
                .map(|(i, p)| (i, p.url.clone(), p.headers.clone()))
                .collect()
        };
        // Computation only: the plan cannot change what the server answers.
        let mut probes = tokio::task::JoinSet::new();
        for (index, url, headers) in parts {
            let this = Arc::clone(self);
            probes.spawn(async move {
                let info = match Engine::new(this.engine_config(&headers, false)) {
                    Ok(engine) => match engine.plan(&url).await {
                        Ok(info) => info.total,
                        Err(_) => None,
                    },
                    Err(_) => None,
                };
                (index, info)
            });
        }
        while let Some(joined) = probes.join_next().await {
            if let Ok((index, Some(total))) = joined {
                self.set_part_total(id, index, total);
            }
        }
    }

    /// Record one part's probed size and re-publish the bundle aggregate
    /// (the row's "x / total" bytes and bar are sums, so they unblock as
    /// soon as the last unknown total lands).
    fn set_part_total(&self, id: &str, index: usize, total: u64) {
        let (downloaded, known_total) = {
            let mut records = self.records.lock().unwrap();
            let Some(rec) = records.iter_mut().find(|r| r.id == id) else {
                return;
            };
            let Some(part) = rec.parts.get_mut(index) else {
                return;
            };
            if part.total.is_some() {
                return;
            }
            part.total = Some(total);
            let (downloaded, total) = Self::bundle_totals(&rec.parts);
            rec.downloaded = downloaded;
            rec.total = total;
            (rec.downloaded, rec.total)
        };
        // Mirror into the live sample so the row shows the total from its
        // next repaint, not only after the first part's progress tick. The
        // byte count is left alone: it moves with the running parts.
        let mut progress = self.progress.lock().unwrap();
        progress.entry(id.to_string()).or_insert_with(|| ProgressTrack {
            last: Progress {
                id: id.to_string(),
                downloaded,
                total: known_total,
                ..Progress::default()
            },
            speed: None,
            window: std::collections::VecDeque::new(),
            at: Instant::now(),
        });
        if let Some(entry) = progress.get_mut(id) {
            if known_total.is_some() {
                entry.last.total = known_total;
            }
        }
    }

    async fn run_bundle_part(
        self: &Arc<Self>,
        id: &str,
        index: usize,
        root: &std::path::Path,
        token: CancellationToken,
    ) -> std::result::Result<(), String> {
        let (url, filename, headers, already) = {
            let records = self.records.lock().unwrap();
            let Some(rec) = records.iter().find(|r| r.id == id) else {
                return Ok(());
            };
            let Some(part) = rec.parts.get(index) else {
                return Ok(());
            };
            (
                part.url.clone(),
                part.filename.clone().unwrap_or_default(),
                part.headers.clone(),
                part.state == TaskState::Completed,
            )
        };
        // Finished parts stay finished across a resume (their sidecar keeps
        // the bytes either way).
        if already {
            return Ok(());
        }
        let filename = if filename.is_empty() {
            ripdl_core::bundle::Link::new(url.as_str()).part_filename()
        } else {
            filename
        };

        // The slot comes from the same semaphore as every other transfer.
        let permit = tokio::select! {
            _ = token.cancelled() => {
                self.set_part_state(id, index, TaskState::Paused, None, None);
                return Ok(());
            }
            p = self.sem.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => return Ok(()),
            },
        };
        if token.is_cancelled() {
            drop(permit);
            self.set_part_state(id, index, TaskState::Paused, None, None);
            return Ok(());
        }

        self.set_part_state(id, index, TaskState::Running, None, None);
        let engine = match Engine::new(self.engine_config(&headers, true)) {
            Ok(engine) => engine,
            Err(e) => {
                let msg = e.to_string();
                self.set_part_state(id, index, TaskState::Failed, Some(msg.clone()), None);
                return Err(format!("{filename}: {msg}"));
            }
        };
        let this = Arc::clone(self);
        let task_id = id.to_string();
        let on_progress = move |p: Progress| this.on_part_progress(&task_id, index, p);
        let result = engine
            .download_named(
                &url,
                Some(root.to_path_buf()),
                Some(filename.clone()),
                token.clone(),
                on_progress,
            )
            .await;
        drop(permit);
        match result {
            Ok(path) => {
                // The file itself is the authority on a finished part: the
                // engine's progress reporter is detached and can deliver an
                // early (0-byte) or late sample after this point.
                let bytes = std::fs::metadata(&path).map(|m| m.len()).ok();
                self.set_part_state(id, index, TaskState::Completed, None, bytes);
                Ok(())
            }
            Err(Error::Cancelled) => {
                self.set_part_state(id, index, TaskState::Paused, None, None);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_part_state(id, index, TaskState::Failed, Some(msg.clone()), None);
                Err(format!("{filename}: {msg}"))
            }
        }
    }

    /// Fold one part's sample into the record and publish the bundle's
    /// aggregate, so the row shows one rate and one percent for the whole set.
    fn on_part_progress(&self, id: &str, index: usize, p: Progress) {
        let aggregate = {
            let mut records = self.records.lock().unwrap();
            let Some(rec) = records.iter_mut().find(|r| r.id == id) else {
                return;
            };
            let Some(part) = rec.parts.get_mut(index) else {
                return;
            };
            // Only a part that is still running may move: a settled part owns
            // its final byte count, and the detached reporter can deliver an
            // early 0-byte sample or a late final one.
            if part.state != TaskState::Running {
                return;
            }
            // Nothing known yet (URL still being probed): publishing a 0-byte
            // sample would retire the row's "connecting" state early and leave
            // it reading "running 0 B" through the probe.
            if p.downloaded == 0 && p.total.is_none() {
                return;
            }
            part.downloaded = p.downloaded;
            if p.total.is_some() {
                part.total = p.total;
            }
            let (downloaded, total) = Self::bundle_totals(&rec.parts);
            rec.downloaded = downloaded;
            rec.total = total;
            Progress {
                id: id.to_string(),
                downloaded,
                total,
                dest_file: rec.dest_file.clone(),
                ..Progress::default()
            }
        };
        self.on_progress(id, aggregate);
    }

    /// Per-part state change, persisted: a paused bundle must come back with
    /// the parts it already finished. `bytes` is the finished size of a
    /// completed part (None leaves the sampled byte count alone).
    fn set_part_state(
        &self,
        id: &str,
        index: usize,
        state: TaskState,
        error: Option<String>,
        bytes: Option<u64>,
    ) {
        let snapshot = {
            let mut records = self.records.lock().unwrap();
            let Some(rec) = records.iter_mut().find(|r| r.id == id) else {
                return;
            };
            let Some(part) = rec.parts.get_mut(index) else {
                return;
            };
            part.state = state;
            part.error = error;
            // A runner taking the part counts as "its engine has probed this
            // URL at least once" - the upfront probe wave must not repeat it.
            if state == TaskState::Running {
                part.attempted = true;
            }
            if let Some(bytes) = bytes {
                part.downloaded = bytes;
                part.total = Some(bytes);
            }
            // Keep the task's own counters in step with its parts.
            let (downloaded, total) = Self::bundle_totals(&rec.parts);
            rec.downloaded = downloaded;
            rec.total = total;
            records.clone()
        };
        let _ = self.persist(&snapshot);
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

            // A bundle owns no slot of its own: its parts each take one, so
            // `max_concurrent` keeps meaning "transfers in flight".
            if rec.kind == TaskKind::Bundle {
                let result = this.run_bundle(rec.clone(), token.clone()).await;
                detach_runtime();
                match result {
                    Ok(Some(root)) => this.set_completed(&rec.id, Some(root)),
                    // Cancelled while queued or running: land on Paused the
                    // same way the single-task path does.
                    Ok(None) => this.set_state(&rec.id, TaskState::Paused, None),
                    Err(e) => this.set_state(&rec.id, TaskState::Failed, Some(e)),
                }
                return;
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
                // Bundles are driven by `run_bundle` before this point: their
                // parts take the slots, not the task.
                TaskKind::Bundle => unreachable!("bundle handled by run_bundle"),
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
                        .download_named(
                            &rec.url,
                            rec.dest_dir.clone(),
                            rec.output_name.clone(),
                            token,
                            on_progress,
                        )
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
                Err(e) => this.park_stale_or_fail(&rec, &e).await,
            }
        });
    }

    /// Park a task whose address died, or fail it the ordinary way.
    ///
    /// An auth-class answer (401/403/404/410, surfaced structurally by the
    /// engine as `Error::HttpStatus`) on a task that already holds bytes
    /// reads as "the signed link expired": the transfer is fine, the address
    /// is not. One fresh probe confirms it - a CDN's one-off 403 must not
    /// park a healthy task - and a changed file size parks the task too,
    /// because an etag mismatch would otherwise restart the download from
    /// zero and destroy the kept bytes. Everything else fails as before.
    async fn park_stale_or_fail(self: &Arc<Self>, rec: &TaskRecord, e: &Error) {
        let auth = ripdl_core::engine::retry::is_auth(e);
        // Only plain HTTP downloads refresh their address; video tasks
        // re-resolve through yt-dlp and torrents have no address to rot.
        if rec.kind != TaskKind::Http || !auth {
            self.set_state(&rec.id, TaskState::Failed, Some(e.to_string()));
            return;
        }
        // A task with no bytes has no link to lose: a fresh add hitting 403
        // is just a bad or expired URL. The live sample counts too - a first
        // run may have pulled gigabytes before the host turned the remaining
        // ranges away.
        let bytes_held = rec.downloaded > 0
            || self
                .progress
                .lock()
                .unwrap()
                .get(&rec.id)
                .is_some_and(|t| t.last.downloaded > 0);
        if !bytes_held {
            self.set_state(&rec.id, TaskState::Failed, Some(e.to_string()));
            return;
        }
        let verdict = match Engine::new(self.engine_config(&rec.headers, false)) {
            Ok(engine) => match engine.plan(&rec.url).await {
                Err(e2) if ripdl_core::engine::retry::is_auth(&e2) => {
                    // Confirmed dead: the address answers auth-class again.
                    Some(StaleCause::Auth)
                }
                Ok(info) => match (info.total, rec.total) {
                    (Some(new), Some(old)) if new != old => Some(StaleCause::Changed { new, old }),
                    _ => None,
                },
                Err(_) => None,
            },
            Err(_) => None,
        };
        match verdict {
            Some(StaleCause::Auth) => {
                self.set_stale(&rec.id, "auth", Some(e.to_string()));
            }
            Some(StaleCause::Changed { new, old }) => {
                self.set_stale(
                    &rec.id,
                    "size",
                    Some(format!(
                        "the address now serves {new} bytes, but the download has {old} - the file changed on the server"
                    )),
                );
            }
            // The probe recovered (or could not answer): keep the ordinary
            // failure; the user resumes when ready.
            None => self.set_state(&rec.id, TaskState::Failed, Some(e.to_string())),
        }
    }

    /// Park a task `stale`: bytes kept, no runner, no auto-retry, survives
    /// restarts. Same record folding as Paused plus the reason.
    fn set_stale(&self, id: &str, reason: &str, error: Option<String>) {
        self.set_state_reason(id, TaskState::Stale, error, Some(reason.to_string()));
    }

    fn set_state(&self, id: &str, state: TaskState, error: Option<String>) {
        self.set_state_reason(id, state, error, None);
    }

    fn set_state_reason(
        &self,
        id: &str,
        state: TaskState,
        error: Option<String>,
        reason: Option<String>,
    ) {
        {
            let mut records = self.records.lock().unwrap();
            // Fold the last live sample into the record on pause: a torrent
            // has no sidecar, so without this a paused mid-flight task reads
            // "0 B / unknown" until the next daemon boot re-checks it.
            let last = if matches!(state, TaskState::Paused | TaskState::Stale) {
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
                    match reason {
                        Some(reason) => r.stale_reason = Some(reason),
                        None if state == TaskState::Running => r.stale_reason = None,
                        None => {}
                    }
                    if let Some((downloaded, total, dest_file)) = last {
                        if downloaded > 0 {
                            r.downloaded = downloaded;
                        }
                        if total.is_some() && r.only_files.is_none() {
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

    /// The bundle's own byte counters: the sum of its parts, and a total only
    /// once every part knows its size (a half-known sum paints a percentage
    /// that drops as the rest are probed).
    fn bundle_totals(parts: &[BundlePart]) -> (u64, Option<u64>) {
        let downloaded = parts.iter().map(|p| p.downloaded).sum();
        let total = parts
            .iter()
            .all(|p| p.total.is_some())
            .then(|| parts.iter().filter_map(|p| p.total).sum());
        (downloaded, total)
    }

    /// Size of a path on disk: files measure directly, directories sum
    /// their contents recursively. None for absent paths (multi-file torrent
    /// roots are folders, where plain `metadata.len()` would report 0).
    ///
    /// Resume sidecars (`.ripdl.json`) are skipped: they are bookkeeping, not
    /// payload, and an in-flight buffered write can land one *after* a
    /// finished part removed its own - counting it would make a bundle's
    /// reported size depend on that race.
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
                let name = entry.file_name();
                if name.to_string_lossy().ends_with(".ripdl.json") {
                    continue;
                }
                match std::fs::metadata(entry.path()) {
                    Ok(m) if m.is_dir() => stack.push(entry.path()),
                    Ok(m) => total += m.len(),
                    Err(_) => {}
                }
            }
        }
        Some(total)
    }

    /// Terminal success with the produced file, remembered for the
    /// "open file" / "delete files" flows (and byte counters for the UI).
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
                    if r.kind == TaskKind::Bundle {
                        // Part records own the durable payload byte counts.
                        // Walking the bundle directory here can race a late
                        // sidecar write and count bookkeeping as downloaded
                        // payload.
                        let (downloaded, total) = Self::bundle_totals(&r.parts);
                        r.downloaded = downloaded;
                        r.total = total;
                    } else if r.dest_file.is_some() {
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
        // Kind lookup happens before the progress lock (adopt_dest_file above
        // already took the records lock): nesting the two in the opposite
        // order would invite a deadlock.
        let is_torrent = self
            .records
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.id == id && r.kind == TaskKind::Torrent);
        let mut map = self.progress.lock().unwrap();
        let now = Instant::now();
        // or_insert_with clones Progress only on the first tick per task;
        // the common path skips the eager clone and the extra hash lookup.
        let entry = map.entry(id.to_string()).or_insert_with(|| ProgressTrack {
            last: p.clone(),
            speed: None,
            window: VecDeque::new(),
            at: now,
        });
        // Resume/recheck hash pass: `downloaded` counts verified bytes and
        // climbs at disk speed, so a derived rate would show hundreds of MB/s
        // and poison the 60 s speed graph. Report no speed and drop the
        // window so the first real sample starts from a clean baseline.
        // Percent-only samples (the yt-dlp sidecar) have no byte rate either.
        if p.verifying || p.percent.is_some() {
            entry.speed = None;
            entry.window.clear();
            entry.at = now;
            entry.last = p;
            return;
        }
        if is_torrent {
            // Byte-based like HTTP (the tool's own rate is an estimate that
            // keeps reporting when nothing arrives - measured 0-600 KiB/s
            // across 30 s with `downloaded` frozen), over a longer span, with
            // an adoption guard: a resume counts data already on disk in one
            // jump, and that must not read as throughput.
            if adoption_jump(
                entry.last.downloaded,
                entry.last.speed_bps,
                p.downloaded,
                now.duration_since(entry.at),
            ) {
                entry.window.clear();
            }
            push_rate_sample(&mut entry.window, now, p.downloaded, TORRENT_RATE_WINDOW);
            entry.speed = window_rate(&entry.window);
        } else {
            push_rate_sample(&mut entry.window, now, p.downloaded, RATE_WINDOW);
            let rate = window_rate(&entry.window);
            // HTTP samples carry the engine's own windowed rate; the derived
            // one is the fallback.
            entry.speed = p.speed_bps.or(rate);
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            tracing::debug!(
                id,
                torrent = is_torrent,
                downloaded = p.downloaded,
                shown_bps = entry.speed,
                tool_bps = p.speed_bps,
                "progress sample"
            );
        }
        entry.at = now;
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

/// What `Daemon::enqueue` did with a request.
enum Enqueued {
    /// A new task was queued under this id.
    New(String),
    /// The same download already exists under this id; nothing was queued.
    Existing(String),
}

/// A `refused` answer to a browser handoff: the browser keeps its download.
fn refused(req_id: u64, why: impl Into<String>) -> Response {
    Response {
        id: req_id,
        reply: Reply::Proposed {
            outcome: ProposalOutcome::Refused,
            id: None,
            reason: Some(why.into()),
            task: None,
        },
    }
}

/// Claim a proposal id for its probe. A tombstone left by an earlier abandon
/// is consumed and refuses the propose; a live id cannot be claimed twice.
fn begin_proposal(
    map: &mut HashMap<String, Proposal>,
    key: &str,
    now: Instant,
) -> Result<(), String> {
    prune_proposals(map, now);
    match map.get(key).map(|p| matches!(p.state, ProposalState::Abandoned)) {
        Some(true) => {
            map.remove(key);
            Err("abandoned before it arrived".to_string())
        }
        Some(false) => Err("duplicate proposal id".to_string()),
        None => {
            map.insert(key.to_string(), Proposal { at: now, state: ProposalState::Pending });
            Ok(())
        }
    }
}

/// Settle an abandon: returns the task the proposal queued, if it queued one.
/// Anything else leaves a tombstone so a propose not yet seen is refused.
fn abandon_proposal(map: &mut HashMap<String, Proposal>, key: &str, now: Instant) -> Option<String> {
    prune_proposals(map, now);
    match map.remove(key) {
        Some(Proposal { state: ProposalState::Taken(id), .. }) => Some(id),
        _ => {
            map.insert(key.to_string(), Proposal { at: now, state: ProposalState::Abandoned });
            None
        }
    }
}

fn prune_proposals(map: &mut HashMap<String, Proposal>, now: Instant) {
    map.retain(|_, p| now.saturating_duration_since(p.at) < PROPOSAL_TTL);
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

/// The file name for a "download again" copy of the direct download `url` in
/// `dest_dir`: the original's file name, suffixed until nothing uses it. A name
/// is taken while a file or its resume sidecar sits on disk, or while a task
/// owns it, so a copy never shares bytes or a sidecar with another task.
fn copy_output_name(
    records: &[TaskRecord],
    url: &str,
    dest_dir: &Option<PathBuf>,
    kind: TaskKind,
) -> Result<String, String> {
    if kind != TaskKind::Http {
        return Err("download again is only offered for direct downloads".to_string());
    }
    let original = records
        .iter()
        .filter(|r| r.kind == kind && r.url == url && &r.dest_dir == dest_dir)
        .find_map(|r| r.dest_file.as_ref()?.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| "this download has no file yet, so there is nothing to copy".to_string())?;
    let dir = dest_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    (0..10_000u32)
        .map(|n| ripdl_core::names::suffixed(&original, n))
        .find(|name| {
            let dest = dir.join(name);
            !dest.exists()
                && !ripdl_core::engine::sidecar_path(&dest).exists()
                && !records.iter().any(|r| {
                    r.dest_dir.as_ref() == Some(&dir)
                        && (r.output_name.as_deref() == Some(name.as_str())
                            || r.dest_file.as_deref() == Some(dest.as_path()))
                })
        })
        .ok_or_else(|| "no free file name next to the existing download".to_string())
}

/// The view of one record that every frontend shows (`Reply::Tasks`,
/// `Reply::Duplicate`). The caller holds the `progress` lock.
fn task_info_of(r: &TaskRecord, progress: &HashMap<String, ProgressTrack>) -> TaskInfo {
    // A parked task keeps its last sample for resume bookkeeping, but
    // reporting it as a live rate prints bandwidth that is not moving (and
    // inflates a frontend's total-speed strip).
    let live = matches!(r.state, TaskState::Running | TaskState::Queued);
    // No sample yet on a live task: the engine is still probing the URL and
    // planning segments (HTTP/video only - a torrent has its own metadata
    // state).
    let probing = live && r.kind != TaskKind::Torrent && !progress.contains_key(&r.id);
    // Bundle rows: how many parts are done and how many the numbering proves
    // exist (0 = the selection does not say).
    let (parts_done, parts_total, parts_missing) = if r.kind == TaskKind::Bundle {
        let seqs: Vec<Option<u32>> = r.parts.iter().map(|p| p.seq).collect();
        let (_, total, missing) = ripdl_core::bundle::numbering(&seqs);
        let done = r
            .parts
            .iter()
            .filter(|p| p.state == TaskState::Completed)
            .count() as u32;
        (done, total.unwrap_or(0), missing)
    } else {
        (0, 0, Vec::new())
    };
    let (downloaded, total, speed_bps, percent, uploaded, upload_bps, verifying) =
        match progress.get(&r.id) {
            Some(t) => (
                t.last.downloaded,
                t.last.total,
                if live { t.speed } else { None },
                t.last.percent,
                t.last.uploaded.max(r.uploaded),
                r.upload_bps.or(t.last.upload_bps),
                t.last.verifying,
            ),
            None => (
                r.downloaded,
                r.total,
                None,
                // No live sample: never claim 100%. A paused task has a total
                // too (it used to read "paused 100%").
                None,
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
        probing,
        error: r.error.clone(),
        stale_reason: r.stale_reason.clone(),
        start_at: r.start_at,
        kind: r.kind,
        percent,
        dest_file: r.dest_file.clone(),
        name: r.bundle_name.clone().or_else(|| r.torrent_name.clone()),
        info_hash: r.info_hash.clone(),
        sequential: r.sequential,
        only_files: r.only_files.clone(),
        parts_done,
        parts_total,
        parts_missing,
    }
}

/// Hand `path` to the desktop shell: the file itself, or its folder with the
/// file selected. The process is spawned, not waited for.
#[cfg(windows)]
fn open_in_shell(path: &Path, folder: bool) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    if folder {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().map(|_| ())
}

#[cfg(not(windows))]
fn open_in_shell(path: &Path, folder: bool) -> std::io::Result<()> {
    let target = if folder { path.parent().unwrap_or(path) } else { path };
    std::process::Command::new("xdg-open").arg(target).spawn().map(|_| ())
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

    /// A daemon whose queue and downloads live in a fresh temp folder.
    fn proposal_daemon(tag: &str) -> (Arc<Daemon>, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("ripdl-proposal-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ripdl_core::rpc::DaemonSettings {
            default_download_dir: Some(dir.clone()),
            ..Default::default()
        };
        let daemon = Daemon::new(dir.join("queue.json"), dir.join("config.json"), config).unwrap();
        (Arc::new(daemon), dir)
    }

    /// Loopback server for handoff probes: HEAD reports `len` bytes, while the
    /// body request never answers, so a queued task stays live for the test.
    async fn proposal_server(len: u64) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match sock.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    if head.starts_with(b"HEAD ") {
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        );
                        let _ = sock.write_all(answer.as_bytes()).await;
                    } else {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                });
            }
        });
        format!("http://{addr}/file.bin")
    }

    fn proposed(resp: Response) -> (ProposalOutcome, Option<String>, Option<String>) {
        match resp.reply {
            Reply::Proposed { outcome, id, reason, .. } => (outcome, id, reason),
            other => panic!("expected a proposed reply, got {other:?}"),
        }
    }

    fn has_task(daemon: &Daemon, id: &str) -> bool {
        daemon.records.lock().unwrap().iter().any(|r| r.id == id)
    }

    #[tokio::test]
    async fn propose_queues_once_and_abandon_drops_only_its_own_task() {
        let (daemon, dir) = proposal_daemon("own");
        let url = proposal_server(4096).await;

        let (outcome, task, _) =
            proposed(daemon.cmd_propose(1, "dl-a".into(), url.clone(), HashMap::new(), Some(4096)).await);
        assert_eq!(outcome, ProposalOutcome::New);
        let task = task.expect("new task id");
        assert!(has_task(&daemon, &task));

        // The same link proposed again is a duplicate: the answer names the
        // queued task so the browser can ask the user, and owns nothing.
        match daemon.cmd_propose(2, "dl-b".into(), url.clone(), HashMap::new(), None).await.reply {
            Reply::Proposed { outcome, id, task: Some(info), .. } => {
                assert_eq!(outcome, ProposalOutcome::Duplicate);
                assert_eq!(id.as_deref(), Some(task.as_str()));
                assert_eq!(info.id, task);
            }
            other => panic!("expected a duplicate answer with its task, got {other:?}"),
        }

        daemon.cmd_abandon(3, "dl-b").await;
        assert!(has_task(&daemon, &task), "abandon of a shared task must not remove it");

        daemon.cmd_abandon(4, "dl-a").await;
        assert!(!has_task(&daemon, &task), "abandon removes the task its proposal queued");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn propose_refuses_what_the_browser_did_not_see() {
        let (daemon, dir) = proposal_daemon("refuse");
        let url = proposal_server(4096).await;

        let (outcome, id, reason) =
            proposed(daemon.cmd_propose(1, "dl-size".into(), url, HashMap::new(), Some(1000)).await);
        assert_eq!(outcome, ProposalOutcome::Refused);
        assert!(id.is_none());
        assert!(reason.unwrap_or_default().contains("size"));

        // Nothing listens on this port: the link is refused, not queued.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let (outcome, _, _) = proposed(
            daemon
                .cmd_propose(2, "dl-dead".into(), format!("http://{closed}/x.bin"), HashMap::new(), None)
                .await,
        );
        assert_eq!(outcome, ProposalOutcome::Refused);
        assert!(daemon.records.lock().unwrap().is_empty());
        assert!(daemon.proposals.lock().unwrap().is_empty(), "refusals release their claim");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn abandon_that_arrives_first_refuses_the_late_propose() {
        let (daemon, dir) = proposal_daemon("tombstone");
        let url = proposal_server(4096).await;

        daemon.cmd_abandon(1, "dl-late").await;
        let (outcome, id, _) =
            proposed(daemon.cmd_propose(2, "dl-late".into(), url, HashMap::new(), None).await);
        assert_eq!(outcome, ProposalOutcome::Refused);
        assert!(id.is_none());
        assert!(daemon.records.lock().unwrap().is_empty());
        assert!(daemon.proposals.lock().unwrap().is_empty(), "the tombstone is consumed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rpc_refuses_a_connection_without_the_token() {
        let (daemon, dir) = proposal_daemon("auth-refuse");
        let (mut client, server) = tokio::io::duplex(4096);
        let serve = tokio::spawn(daemon.clone().handle_conn(server));
        // A wrong token is refused, and the list request behind it never runs.
        client
            .write_all(b"{\"auth\":\"not-the-token\"}\n{\"id\":5,\"cmd\":{\"type\":\"list\"}}\n")
            .await
            .unwrap();
        let mut lines = BufReader::new(client).lines();
        let refusal: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(refusal["id"], 0);
        assert_eq!(refusal["reply"]["type"], "error");
        assert_eq!(refusal["reply"]["message"], "unauthorized");
        assert!(lines.next_line().await.unwrap().is_none(), "the connection is closed");
        serve.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rpc_serves_commands_after_the_token() {
        let (daemon, dir) = proposal_daemon("auth-ok");
        let token = daemon.rpc_token.clone();
        let (mut client, server) = tokio::io::duplex(4096);
        let serve = tokio::spawn(daemon.clone().handle_conn(server));
        let hello = format!("{{\"auth\":\"{token}\"}}\n{{\"id\":5,\"cmd\":{{\"type\":\"list\"}}}}\n");
        client.write_all(hello.as_bytes()).await.unwrap();
        let mut lines = BufReader::new(client).lines();
        let reply: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply["id"], 5);
        assert_eq!(reply["reply"]["type"], "tasks");
        drop(lines);
        serve.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn open_refuses_an_unknown_task() {
        let (daemon, dir) = proposal_daemon("open");
        match daemon.cmd_open(1, "nope", false).reply {
            Reply::Error { message } => assert!(message.contains("no task"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn copy_output_name_takes_the_first_free_suffix() {
        let dir = std::env::temp_dir().join(format!("ripdl-copyname-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.zip"), b"done").unwrap();
        let url = "https://x/file.zip";
        let dest = Some(dir.clone());
        let mut done = rec("t1", url, dir.to_str());
        done.dest_file = Some(dir.join("file.zip"));
        let first = ripdl_core::names::suffixed("file.zip", 1);
        let second = ripdl_core::names::suffixed("file.zip", 2);

        // The original is on disk, so the copy takes the first suffix that is free.
        assert_eq!(copy_output_name(&[done.clone()], url, &dest, TaskKind::Http), Ok(first.clone()));

        // A name another task already owns is skipped too.
        let mut owner = rec("t2", url, dir.to_str());
        owner.output_name = Some(first);
        assert_eq!(copy_output_name(&[done.clone(), owner], url, &dest, TaskKind::Http), Ok(second));

        // Only direct downloads have a copy, and only once a file exists.
        assert!(copy_output_name(&[done.clone()], url, &dest, TaskKind::Torrent).is_err());
        assert!(copy_output_name(&[], url, &dest, TaskKind::Http).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn rec(id: &str, url: &str, dest: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            url: url.to_string(),
            dest_dir: dest.map(PathBuf::from),
            output_name: None,
            state: TaskState::Completed,
            error: None,
            stale_reason: None,
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
            bundle_name: None,
            parts: Vec::new(),
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
    fn extension_hello_persists_and_is_reported_after_restart() {
        let tmp = std::env::temp_dir().join(format!(
            "ripdl-extension-status-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let queue = tmp.join("queue.json");
        let config = tmp.join("config.json");
        let daemon = Arc::new(
            Daemon::new(queue.clone(), config.clone(), DaemonConfig::default()).unwrap(),
        );
        let response = daemon.cmd_extension_hello(1, "0.5.0".into());
        assert!(matches!(response.reply, Reply::Ok));
        match daemon.config_reply() {
            Reply::Config { extension, .. } => {
                assert_eq!(extension.version, "0.5.0");
                assert!(extension.last_seen > 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }

        let restored = Arc::new(
            Daemon::new(queue, config, DaemonConfig::default()).unwrap(),
        );
        match restored.config_reply() {
            Reply::Config { extension, .. } => {
                assert_eq!(extension.version, "0.5.0");
                assert!(extension.last_seen > 0);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(tmp);
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
            stale_reason: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Http,
            sequential: false,
            only_files: None,
            dest_file: Some(out.clone()),
            output_name: None,
            downloaded: 4,
            total: Some(4),
            info_hash: None,
            torrent_name: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
            bundle_name: None,
            parts: Vec::new(),
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
            output_name: None,
            dest_dir: Some(tmp.clone()),
            state: TaskState::Failed,
            error: None,
            stale_reason: None,
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
            bundle_name: None,
            parts: Vec::new(),
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

    /// Offline HTTP fixture for the bundle tests: `/f<N>` serves 64 KiB with a
    /// real 206 for Range requests, `/missing` always answers 404, `/flaky`
    /// answers 404 until the test flips its flag. Per-path GET counts prove a
    /// finished part is never fetched twice.
    struct Fixture {
        base: String,
        flaky_ok: Arc<std::sync::atomic::AtomicBool>,
        gets: Arc<Mutex<HashMap<String, usize>>>,
    }

    async fn spawn_fixture() -> Fixture {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

        const SIZE: usize = 64 * 1024;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let flaky_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gets: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
        let (flag, counts) = (Arc::clone(&flaky_ok), Arc::clone(&gets));

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (flag, counts) = (Arc::clone(&flag), Arc::clone(&counts));
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let mut lines = head.lines();
                    let request = lines.next().unwrap_or_default().to_string();
                    let mut words = request.split_whitespace();
                    let method = words.next().unwrap_or("GET").to_string();
                    let path = words.next().unwrap_or("/").to_string();
                    let range = lines.find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        lower
                            .strip_prefix("range:")
                            .map(|v| v.trim().to_string())
                    });

                    if method == "GET" {
                        *counts.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
                    }
                    let blocked = path.contains("missing")
                        || (path.contains("flaky")
                            && !flag.load(std::sync::atomic::Ordering::SeqCst));
                    if blocked {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }

                    let body: Vec<u8> = if path.starts_with("/f") {
                        (0..SIZE).map(|i| (i % 251) as u8).collect()
                    } else {
                        Vec::new()
                    };
                    let (status, start, end) = match range.as_deref() {
                        Some(r) => {
                            let spec = r.trim_start_matches("bytes=");
                            let (a, b) = spec.split_once('-').unwrap_or(("0", ""));
                            let start: usize = a.trim().parse().unwrap_or(0);
                            let end: usize = if b.trim().is_empty() {
                                SIZE.saturating_sub(1)
                            } else {
                                b.trim().parse().unwrap_or(SIZE - 1)
                            };
                            ("206 Partial Content", start.min(SIZE), end.min(SIZE.saturating_sub(1)))
                        }
                        None => ("200 OK", 0, SIZE.saturating_sub(1)),
                    };
                    let slice = if body.is_empty() {
                        &body[..]
                    } else {
                        &body[start..=end]
                    };
                    let headers = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{SIZE}\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                    let _ = sock.write_all(headers.as_bytes()).await;
                    if method != "HEAD" {
                        let _ = sock.write_all(slice).await;
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });

        Fixture {
            base: format!("http://{addr}"),
            flaky_ok,
            gets,
        }
    }

    fn bundle_parts(base: &str, names: &[&str]) -> Vec<BundlePart> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| BundlePart {
                url: format!("{base}/f{}", i + 1),
                filename: Some((*name).to_string()),
                headers: Default::default(),
                state: TaskState::Queued,
                downloaded: 0,
                total: None,
                seq: None,
                error: None,
                attempted: false,
            })
            .collect()
    }

    /// Poll the queue until the task reaches `want` (or panic after `timeout`).
    async fn wait_state(daemon: &Daemon, id: &str, want: TaskState) -> TaskRecord {
        for _ in 0..500 {
            {
                let records = daemon.records.lock().unwrap();
                if let Some(rec) = records.iter().find(|r| r.id == id) {
                    if rec.state == want {
                        return rec.clone();
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let records = daemon.records.lock().unwrap();
        let seen = records
            .iter()
            .find(|r| r.id == id)
            .map(|r| format!("{:?} ({:?})", r.state, r.error))
            .unwrap_or_else(|| "missing".into());
        panic!("timed out waiting for {want:?}, saw {seen}");
    }

    fn temp_bundle_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ripdl-bundle-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bundle_parts_share_one_folder_and_finish_together() {
        let fx = spawn_fixture().await;
        let tmp = temp_bundle_dir("ok");
        let dest = tmp.join("dest");
        let daemon = Arc::new(
            Daemon::new(
                tmp.join("queue.json"),
                tmp.join("config.json"),
                DaemonConfig::default(),
            )
            .unwrap(),
        );

        let parts = bundle_parts(
            &fx.base,
            &["x.part01.rar", "x.part02.rar", "x.part03.rar"],
        );
        let id = match daemon.cmd_add_bundle(1, None, Some(dest.clone()), parts, None).reply {
            Reply::Added { id } => id,
            other => panic!("unexpected reply: {other:?}"),
        };

        let rec = wait_state(&daemon, &id, TaskState::Completed).await;
        assert_eq!(rec.bundle_name.as_deref(), Some("x"));
        assert_eq!(rec.parts.len(), 3);
        assert!(rec.parts.iter().all(|p| p.state == TaskState::Completed));
        // One folder, the part names the page gave, not the server's.
        let root = dest.join("x");
        assert!(root.is_dir(), "bundle root missing: {}", root.display());
        let listing: Vec<(String, u64)> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| {
                let entry = e.ok()?;
                let len = entry.metadata().ok()?.len();
                Some((entry.file_name().to_string_lossy().into_owned(), len))
            })
            .collect();
        let parts_seen: Vec<_> = rec
            .parts
            .iter()
            .map(|p| (p.filename.clone(), p.downloaded, p.total, p.state))
            .collect();
        assert_eq!(
            rec.downloaded,
            3 * 64 * 1024,
            "parts={parts_seen:?} listing={listing:?}"
        );
        assert_eq!(rec.total, Some(3 * 64 * 1024));
        for name in ["x.part01.rar", "x.part02.rar", "x.part03.rar"] {
            let bytes = std::fs::read(root.join(name)).expect("part on disk");
            assert_eq!(bytes.len(), 64 * 1024);
        }
        // Nothing but the parts (a resume sidecar may linger: the engine's
        // reporter writes one from a detached task and can land it after a
        // finished part removed its own - it is bookkeeping, never payload).
        let payload: Vec<&String> = listing
            .iter()
            .map(|(name, _)| name)
            .filter(|name| !name.ends_with(".ripdl.json"))
            .collect();
        assert_eq!(payload.len(), 3, "no extra payload files: {payload:?}");

        // The row and the details dialog carry the same facts.
        match daemon.cmd_list(2).reply {
            Reply::Tasks { tasks } => {
                let info = tasks.iter().find(|t| t.id == id).expect("row");
                assert_eq!(info.kind, TaskKind::Bundle);
                assert_eq!(info.parts_done, 3);
                assert_eq!(info.parts_total, 3);
                assert!(info.parts_missing.is_empty());
                assert_eq!(info.name.as_deref(), Some("x"));
                assert_eq!(info.dest_file.as_deref(), Some(root.as_path()));
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        match daemon.cmd_task_details(3, &id).reply {
            Reply::TaskDetails { details } => {
                assert_eq!(details.parts.len(), 3);
                assert_eq!(details.bundle_name.as_deref(), Some("x"));
                assert_eq!(details.dest_file.as_deref(), Some(root.as_path()));
                assert!(details.parts.iter().all(|p| p.total == Some(64 * 1024)));
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_part_fails_the_bundle_and_resume_only_refetches_it() {
        let fx = spawn_fixture().await;
        let tmp = temp_bundle_dir("resume");
        let dest = tmp.join("dest");
        let daemon = Arc::new(
            Daemon::new(
                tmp.join("queue.json"),
                tmp.join("config.json"),
                DaemonConfig::default(),
            )
            .unwrap(),
        );

        let part = |url: String, name: &str| BundlePart {
            url,
            filename: Some(name.to_string()),
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        };
        let parts = vec![
            part(format!("{}/f1", fx.base), "x.part01.rar"),
            part(format!("{}/flaky", fx.base), "x.part02.rar"),
            part(format!("{}/f3", fx.base), "x.part03.rar"),
        ];
        let id = match daemon.cmd_add_bundle(1, None, Some(dest.clone()), parts, None).reply {
            Reply::Added { id } => id,
            other => panic!("unexpected reply: {other:?}"),
        };

        let rec = wait_state(&daemon, &id, TaskState::Failed).await;
        assert_eq!(rec.parts[1].state, TaskState::Failed);
        assert_eq!(rec.parts[0].state, TaskState::Completed);
        assert_eq!(rec.parts[2].state, TaskState::Completed);
        let root = dest.join("x");
        assert!(root.join("x.part01.rar").exists());
        assert!(root.join("x.part03.rar").exists());
        assert!(!root.join("x.part02.rar").exists());

        // The two good parts were fetched exactly once.
        assert_eq!(fx.gets.lock().unwrap().get("/f1").copied(), Some(1));
        assert_eq!(fx.gets.lock().unwrap().get("/f3").copied(), Some(1));

        fx.flaky_ok.store(true, std::sync::atomic::Ordering::SeqCst);
        match daemon.cmd_resume(4, &id).reply {
            Reply::Ok => {}
            other => panic!("unexpected reply: {other:?}"),
        }
        let rec = wait_state(&daemon, &id, TaskState::Completed).await;
        assert!(rec.parts.iter().all(|p| p.state == TaskState::Completed));
        assert!(root.join("x.part02.rar").exists());
        // Resume only fetched what was missing: no part was downloaded twice.
        // For /flaky the count is 3, not 2: the *add-time* wave probes every
        // never-run part, and on a 404 host the planner falls back from HEAD
        // to a `bytes=0-0` GET (counted). The resume itself fetched it once.
        assert_eq!(fx.gets.lock().unwrap().get("/f1").copied(), Some(1));
        assert_eq!(fx.gets.lock().unwrap().get("/f3").copied(), Some(1));
        assert_eq!(fx.gets.lock().unwrap().get("/flaky").copied(), Some(3));
        assert_eq!(rec.downloaded, 3 * 64 * 1024);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refresh_one_part_swaps_only_that_address() {
        let fx = spawn_fixture().await;
        let tmp = temp_bundle_dir("refresh-part");
        let dest = tmp.join("dest");
        let daemon = Arc::new(
            Daemon::new(
                tmp.join("queue.json"),
                tmp.join("config.json"),
                DaemonConfig::default(),
            )
            .unwrap(),
        );

        let part = |url: String, name: &str| BundlePart {
            url,
            filename: Some(name.to_string()),
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        };
        let parts = vec![
            part(format!("{}/f1", fx.base), "x.part01.rar"),
            part(format!("{}/flaky", fx.base), "x.part02.rar"),
            part(format!("{}/f3", fx.base), "x.part03.rar"),
        ];
        let id = match daemon.cmd_add_bundle(1, None, Some(dest.clone()), parts, None).reply {
            Reply::Added { id } => id,
            other => panic!("unexpected reply: {other:?}"),
        };
        wait_state(&daemon, &id, TaskState::Failed).await;

        // An expired link for part 2 only: the fixture serves the same bytes
        // at every /fN, so /f9 is the "fresh" address of the same file.
        match daemon
            .cmd_refresh_url(2, &id, &format!("{}/f9", fx.base), true, Some(2))
            .await
            .reply
        {
            Reply::Refreshed { part: Some(2), .. } => {}
            other => panic!("unexpected reply: {other:?}"),
        }
        let rec = wait_state(&daemon, &id, TaskState::Completed).await;
        assert_eq!(rec.parts[1].url, format!("{}/f9", fx.base), "only part 2 moved");
        assert_eq!(rec.parts[0].url, format!("{}/f1", fx.base));
        assert!(rec.parts.iter().all(|p| p.state == TaskState::Completed));
        let root = dest.join("x");
        assert!(root.join("x.part02.rar").exists());
        // The flaky address never delivered: the fresh one fetched exactly once.
        assert_eq!(fx.gets.lock().unwrap().get("/f9").copied(), Some(1));
        assert_eq!(rec.downloaded, 3 * 64 * 1024);

        // A finished part refuses a refresh: its bytes are final.
        match daemon
            .cmd_refresh_url(3, &id, &format!("{}/f8", fx.base), false, Some(1))
            .await
            .reply
        {
            Reply::Error { message } => assert!(message.contains("already finished"), "{message}"),
            other => panic!("unexpected reply: {other:?}"),
        }
        // So does an unknown part number.
        match daemon
            .cmd_refresh_url(4, &id, &format!("{}/f8", fx.base), false, Some(9))
            .await
            .reply
        {
            Reply::Error { message } => assert!(message.contains("no part 9"), "{message}"),
            other => panic!("unexpected reply: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn add_parts_fills_a_completed_bundle_and_skips_duplicates() {
        let fx = spawn_fixture().await;
        let tmp = temp_bundle_dir("add-parts");
        let dest = tmp.join("dest");
        let daemon = Arc::new(
            Daemon::new(
                tmp.join("queue.json"),
                tmp.join("config.json"),
                DaemonConfig::default(),
            )
            .unwrap(),
        );

        let part = |url: String, name: &str| BundlePart {
            url,
            filename: Some(name.to_string()),
            headers: Default::default(),
            state: TaskState::Queued,
            downloaded: 0,
            total: None,
            seq: None,
            error: None,
            attempted: false,
        };
        let parts = vec![
            part(format!("{}/f1", fx.base), "x.part01.rar"),
            part(format!("{}/f2", fx.base), "x.part02.rar"),
        ];
        let id = match daemon.cmd_add_bundle(1, None, Some(dest.clone()), parts, None).reply {
            Reply::Added { id } => id,
            other => panic!("unexpected reply: {other:?}"),
        };
        wait_state(&daemon, &id, TaskState::Completed).await;
        let root = dest.join("x");
        assert!(root.join("x.part01.rar").exists());

        // The forgotten part 3 joins and the completed bundle picks it up.
        let more = vec![part(format!("{}/f3", fx.base), "x.part03.rar")];
        match daemon.cmd_add_bundle_parts(2, &id, more).await.reply {
            Reply::BundlePartsAdded { added: 1, .. } => {}
            other => panic!("unexpected reply: {other:?}"),
        }
        let rec = wait_state(&daemon, &id, TaskState::Completed).await;
        assert_eq!(rec.parts.len(), 3);
        assert!(rec.parts.iter().all(|p| p.state == TaskState::Completed));
        // Parts stay in part-number order for the details table.
        let seqs: Vec<Option<u32>> = rec.parts.iter().map(|p| p.seq).collect();
        assert_eq!(seqs, vec![Some(1), Some(2), Some(3)]);
        assert!(root.join("x.part03.rar").exists());
        assert_eq!(rec.downloaded, 3 * 64 * 1024);

        // The same link again is a no-op that leaves the state alone.
        let dup = vec![part(format!("{}/f1", fx.base), "x.part01.rar")];
        match daemon.cmd_add_bundle_parts(3, &id, dup).await.reply {
            Reply::BundlePartsAdded { added: 0, .. } => {}
            other => panic!("unexpected reply: {other:?}"),
        }
        let rec = {
            let records = daemon.records.lock().unwrap();
            records.iter().find(|r| r.id == id).cloned().unwrap()
        };
        assert_eq!(rec.state, TaskState::Completed, "duplicate add must not restart");
        assert_eq!(rec.parts.len(), 3);

        // A link from a different set is refused with the reason.
        let foreign = vec![part(format!("{}/f4", fx.base), "y.part01.rar")];
        match daemon.cmd_add_bundle_parts(4, &id, foreign).await.reply {
            Reply::Error { message } => assert!(message.contains('y'), "{message}"),
            other => panic!("unexpected reply: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
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
            stale_reason: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Torrent,
            sequential: false,
            only_files: None,
            dest_file: Some(root.clone()),
            output_name: None,
            downloaded: data.len() as u64,
            total: Some(data.len() as u64),
            info_hash: Some(pv.info_hash.clone()),
            torrent_name: Some("payload.bin".into()),
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seed_paused: false,
            peers: 0,
            bundle_name: None,
            parts: Vec::new(),
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

    // A resume makes librqbit jump from 0 to "everything already on disk" in
    // one tick; the row used to read 943 MiB/s because of it.
    #[test]
    fn adoption_jump_ignores_a_resume() {
        let dt = Duration::from_secs(1);
        assert!(adoption_jump(0, Some(0.0), 2_252_154_753, dt), "resume adoption");
        assert!(adoption_jump(0, None, 2_252_154_753, dt), "no tool rate yet");
        // A stalled task that suddenly gains 40 MiB in a tick.
        assert!(adoption_jump(1_000, Some(0.0), 42_000_000, dt));
    }

    #[test]
    fn bond_exclude_matches_a_settings_checkbox_and_typed_cli_value() {
        let excludes = vec!["tether".to_string(), "ethernet 2".to_string()];
        assert!(bond_allowed(&excludes, "Wi-Fi"), "unlisted uplink stays in");
        assert!(bond_allowed(&excludes, "  Wi-Fi  "), "discovery names trim");
        assert!(!bond_allowed(&excludes, "Ethernet 2"), "case-insensitive exact");
        assert!(!bond_allowed(&excludes, "TETHER"), "the checkbox is lowercase");
        assert!(bond_allowed(&excludes, "Ethernet 20"), "prefix must not match");
        assert!(bond_allowed(&[], "anything"), "empty list excludes nothing");
        assert!(!bond_allowed(&excludes, " tether "), "typed CLI values trim");
    }

    #[test]
    fn adoption_jump_keeps_real_traffic() {        let dt = Duration::from_secs(1);
        // 40 MiB/s on a fast link, tool agrees.
        assert!(!adoption_jump(0, Some(40.0 * 1024.0 * 1024.0), 41_943_040, dt));
        // Steady swarm traffic.
        assert!(!adoption_jump(1_000_000_000, Some(3_000_000.0), 1_003_000_000, dt));
    }

    #[test]
    fn torrent_window_averages_piece_bursts() {
        let t0 = Instant::now();
        let mut w = VecDeque::new();
        // 0, 4 MiB, 4 MiB, 8 MiB over 1 s ticks (pieces land in clumps).
        for (i, bytes) in [0u64, 4, 4, 8].iter().enumerate() {
            push_rate_sample(
                &mut w,
                t0 + Duration::from_secs(i as u64),
                bytes * 1024 * 1024,
                TORRENT_RATE_WINDOW,
            );
        }
        let r = window_rate(&w).expect("windowed rate");
        // 8 MiB over 3 s, not 4 MiB/s and not 0.
        assert!((r - (8.0 * 1024.0 * 1024.0 / 3.0)).abs() < 2_000.0, "rate {r}");
    }

    // HTTP tasks keep the sliding window (bytes moved / elapsed).
    #[test]
    fn rate_window_needs_two_samples() {
        let t0 = Instant::now();
        let mut w = VecDeque::new();
        push_rate_sample(&mut w, t0, 1_000, RATE_WINDOW);
        assert_eq!(window_rate(&w), None, "one sample has no span");

        push_rate_sample(&mut w, t0 + Duration::from_secs(2), 3_000, RATE_WINDOW);
        let r = window_rate(&w).expect("two samples over 2 s");
        assert!((r - 1_000.0).abs() < 1.0, "rate {r}");
    }

    #[test]
    fn rate_window_restarts_after_a_gap() {
        let t0 = Instant::now();
        let mut w = VecDeque::new();
        push_rate_sample(&mut w, t0, 0, RATE_WINDOW);
        push_rate_sample(&mut w, t0 + Duration::from_secs(2), 2_000, RATE_WINDOW);
        // 60 s pause: the stale sample must not stretch the jump over a minute.
        push_rate_sample(&mut w, t0 + Duration::from_secs(62), 5_000, RATE_WINDOW);
        assert_eq!(w.len(), 1, "gap starts a fresh baseline");
        assert_eq!(window_rate(&w), None);

        push_rate_sample(&mut w, t0 + Duration::from_secs(64), 7_000, RATE_WINDOW);
        let r = window_rate(&w).expect("rate after the gap");
        assert!((r - 1_000.0).abs() < 1.0, "rate {r}");
    }

    #[test]
    fn rate_window_stays_bounded() {
        let t0 = Instant::now();
        let mut w = VecDeque::new();
        for i in 0..12u64 {
            push_rate_sample(&mut w, t0 + Duration::from_secs(i * 2), i * 1_000, RATE_WINDOW);
        }
        assert!(w.len() <= 2, "2 s window keeps at most two samples, got {}", w.len());
    }

    // The "bonding is slower than one connection" report: 50/50 across a fast
    // link and a dying one parks half the swarm on the dead path.
    #[test]
    fn weights_follow_measured_throughput() {
        use std::net::IpAddr;
        let wifi: IpAddr = "192.168.1.8".parse().unwrap();
        let tether: IpAddr = "10.118.70.127".parse().unwrap();

        // 7 MB/s vs 2 MB/s: about 10 vs 3.
        let w = weights_from_rates(&[(wifi, 7.0e6), (tether, 2.0e6)]);
        assert_eq!(w, vec![(wifi, 10), (tether, 3)]);

        // Tether collapsed to 20 KB/s: quarantined.
        let w = weights_from_rates(&[(wifi, 7.0e6), (tether, 2.0e4)]);
        assert_eq!(w, vec![(wifi, 10), (tether, 0)]);

        // Quiet swarm (<1 KiB/s everywhere): stay even instead of quarantining
        // every uplink.
        let w = weights_from_rates(&[(wifi, 500.0), (tether, 0.0)]);
        assert_eq!(w, vec![(wifi, 1), (tether, 1)]);
    }
    /// A dead link parks the task `stale`, not `failed`: after the host turns an
    /// in-progress HTTP task away with 403, the daemon confirms with one probe
    /// and parks it with reason `auth` - bytes and sidecar intact, no runner.
    /// A refreshed address (or the link coming back) resumes the same bytes to
    /// completion.
    #[tokio::test]
    async fn dead_link_parks_task_stale_and_refresh_recovers() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const TOTAL: usize = 8 * 1024 * 1024;
        let forbidden = Arc::new(AtomicBool::new(false));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let flag = Arc::clone(&forbidden);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let forbidden = Arc::clone(&flag);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match sock.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let text = String::from_utf8_lossy(&head);
                    if forbidden.load(Ordering::SeqCst) {
                        let _ = sock
                            .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                    let range = text
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("range: bytes=")
                                .and_then(|s| s.split_once('-'))
                                .and_then(|(a, _)| a.trim().parse::<u64>().ok())
                        });
                    if text.starts_with("HEAD") {
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\netag: \"v1\"\r\ncontent-length: {TOTAL}\r\naccept-ranges: bytes\r\nconnection: close\r\n\r\n"
                        );
                        let _ = sock.write_all(answer.as_bytes()).await;
                        return;
                    }
                    let start = match range {
                        Some(a) => a,
                        None => 0,
                    };
                    // Paced in 64 KiB blocks so the test can pause
                    // mid-flight deterministically (a full-speed 8 MiB would
                    // complete before the pause command lands).
                    let mut sent = start;
                    while sent < TOTAL as u64 {
                        let end = (sent + 64 * 1024).min(TOTAL as u64);
                        let chunk: Vec<u8> = (sent..end).map(|i| (i % 251) as u8).collect();
                        if sent == start {
                            let head = format!(
                                "HTTP/1.1 206 Partial Content\r\netag: \"v1\"\r\ncontent-range: bytes {start}-{}-{TOTAL}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                TOTAL as u64 - 1,
                                TOTAL as u64 - start
                            );
                            let _ = sock.write_all(head.as_bytes()).await;
                        }
                        let _ = sock.write_all(&chunk).await;
                        tokio::time::sleep(Duration::from_millis(15)).await;
                        sent = end;
                    }
                });
            }
        });

        let (daemon, dir) = proposal_daemon("stale");
        let url = format!("http://{addr}/file.bin?t=1");
        let resp = daemon.cmd_add(
            1,
            url.clone(),
            Some(dir.clone()),
            HashMap::new(),
            None,
            TaskKind::Http,
            false,
            None,
            DuplicatePolicy::Report,
        );
        let id = match resp.reply {
            Reply::Added { id } => id,
            other => panic!("expected Added, got {other:?}"),
        };

        // Wait for real progress, then pause: the sidecar holds bytes.
        let mut progress = 0u64;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(t) = daemon.task_info_for(&id) {
                progress = t.downloaded;
                if progress > 0 {
                    break;
                }
            }
        }
        assert!(progress > 0, "download never started");
        daemon.cmd_pause(2, &id).await;
        let mut paused = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if daemon
                .records
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.id == id && r.state == TaskState::Paused)
            {
                paused = true;
                break;
            }
        }
        assert!(paused, "task never paused");

        // The link dies. Resuming must park the task stale(auth), not failed.
        forbidden.store(true, Ordering::SeqCst);
        daemon.cmd_resume(3, &id);
        let mut stale = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let hit = daemon.records.lock().unwrap().iter().find(|r| r.id == id).map(|r| {
                (r.state == TaskState::Stale, r.stale_reason.clone(), r.error.clone())
            });
            if let Some((true, Some(reason), Some(err))) = hit {
                assert_eq!(reason, "auth");
                assert!(err.contains("403"), "error text: {err}");
                stale = true;
                break;
            }
            // A generic failure here would mean classification did not run.
            assert!(
                !daemon
                    .records
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.id == id && r.state == TaskState::Failed),
                "dead link must park stale, not failed"
            );
        }
        assert!(stale, "task never reached stale");

        // The link comes back (fresh token): refresh the address, resume, done.
        forbidden.store(false, Ordering::SeqCst);
        let fresh = format!("http://{addr}/file.bin?t=2");
        let resp = daemon.cmd_refresh_url(4, &id, &fresh, false, None).await;
        assert!(
            matches!(resp.reply, Reply::Refreshed { .. }),
            "refresh failed: {:?}",
            resp.reply
        );
        daemon.cmd_resume(5, &id);
        let mut done = false;
        for _ in 0..400 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if daemon
                .records
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.id == id && r.state == TaskState::Completed)
            {
                done = true;
                break;
            }
        }
        assert!(done, "task never completed after the refresh");

        // The bytes are the server's, complete and uncorrupted.
        let dest = dir.join("file.bin");
        let data = std::fs::read(&dest).expect("output file");
        assert_eq!(data.len(), TOTAL);
        for (i, b) in data.iter().enumerate() {
            assert_eq!(*b, (i as u64 % 251) as u8, "byte {i} corrupted");
        }
        assert!(
            !dir.join("file.bin.ripdl.json").exists(),
            "sidecar must be gone on completion"
        );
    }
}

async fn real_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = ripdl_core::transport::app_data_dir()
        .context("cannot resolve local data directory")?;
    std::fs::create_dir_all(&data_dir).context("create data directory")?;
    let queue_path = data_dir.join("queue.json");
    // Stale installers (kept a week as rollback) and half downloads.
    ripdl_core::update::prune_update_dir(&ripdl_core::update::update_dir(&data_dir));

    let config_path = data_dir.join("config.json");
    let config = DaemonConfig::load(&config_path);
    let daemon = Arc::new(Daemon::new(queue_path, config_path, config).context("init daemon")?);
    daemon.boot_resume().context("resume queue")?;
    // LAN drop box: bind only when the user turned receiving on. A port
    // clash logs a warning and leaves the rest of the daemon working.
    daemon.apply_lan().await;
    // Fastresume cache hygiene: drop bitfields for tasks that are gone.
    daemon.prune_fastresume();
    // Re-attach completed/paused torrents in the background: this is what
    // keeps finished torrents seeding and their files/peers/trackers
    // inspectable after a daemon restart (librqbit keeps no state).
    {
        let d = daemon.clone();
        tokio::spawn(async move { d.boot_restore_torrents().await });
    }
    // Bonding follows the machine from here on (an uplink may show up after
    // the session was created, e.g. Wi-Fi finishing DHCP at boot).
    {
        let d = daemon.clone();
        tokio::spawn(async move { d.watch_bonding().await });
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
