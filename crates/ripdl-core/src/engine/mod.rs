mod gate;
mod limiter;
mod nic;
mod planner;
pub mod retry;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

pub use nic::{bonded_nics, live_bonded_nics, scan_uplinks, UplinkNic, UplinkScan};
pub use planner::{percent_decode_lossy, PlanInfo};

use crate::error::{Error, Result};
use crate::task::{Progress, SegmentState, TaskState, TaskStateFile};

/// Sentinel end offset for "unknown total size, stream to the end".
const UNBOUNDED: u64 = u64::MAX;

/// Write buffer per segment: syscalls drop ~64x and disks can't stall the
/// network pipeline.
const SEGMENT_BUF: usize = 256 * 1024;

/// How download segments choose their local (source) network interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bond {
    /// OS routing decides for every connection (single-uplink default).
    Off,
    /// Spread segments over discovered interfaces: the OS-default uplink
    /// stays on the unbound client, one bound client per additional
    /// interface. Activates when at least one extra interface exists.
    Auto,
    /// Explicit override: bind every segment to these local addresses
    /// (cycling), no unbound client at all. One address = everything
    /// through that interface. Tests and manual pinning.
    Addresses(Vec<IpAddr>),
}

impl Bond {
    /// `true` when bonding is disabled (plain OS routing).
    pub fn is_off(&self) -> bool {
        matches!(self, Bond::Off)
    }
}

/// One-line state of torrent NIC bonding, for `ripdl config` and the GUI.
///
/// The daemon swaps the shared session's bind list when settings change, so no
/// restart is needed. `session` says whether that session exists yet and `bind`
/// which addresses it actually uses.
pub fn describe_torrent_bond(configured: bool, session: bool, bind: &[IpAddr]) -> String {
    if !configured {
        return "off".to_string();
    }
    if !session {
        return "enabled, applies when the next torrent starts".to_string();
    }
    if bind.is_empty() {
        return "enabled, no uplink bound yet; it follows the network".to_string();
    }
    let nics = bonded_nics();
    let names: Vec<String> = bind
        .iter()
        .map(|ip| {
            nics.iter()
                .find(|n| IpAddr::V4(n.addr) == *ip || n.v6.map(IpAddr::V6) == Some(*ip))
                .map(|n| n.name.clone())
                .unwrap_or_else(|| ip.to_string())
        })
        .collect();
    // One interface contributes up to two bound addresses; name it once.
    let mut unique: Vec<String> = Vec::new();
    for n in names {
        if !unique.contains(&n) {
            unique.push(n);
        }
    }
    format!("active on {}", unique.join(", "))
}

/// Tunables for the download engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Max parallel range segments per download.
    pub max_segments: usize,
    /// Below this size, do not bother splitting into ranges.
    pub min_segment_size: u64,
    pub retry: retry::RetryConfig,
    pub user_agent: String,
    /// Global bytes-per-second cap shared by all segments (None = unlimited).
    pub max_speed: Option<u64>,
    /// Extra request headers sent by probe + every segment (cookies,
    /// referer, auth...). Later entries may replace same-named ones.
    pub headers: std::collections::HashMap<String, String>,
    /// NIC bonding (see [`Bond`]). Default [`Bond::Off`].
    pub bond: Bond,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_segments: 8,
            min_segment_size: 1024 * 1024,
            retry: retry::RetryConfig::default(),
            user_agent: concat!("ripdl/", env!("CARGO_PKG_VERSION")).to_string(),
            max_speed: None,
            headers: std::collections::HashMap::new(),
            bond: Bond::Off,
        }
    }
}

pub struct Engine {
    client: reqwest::Client,
    cfg: EngineConfig,
    limiter: std::sync::Arc<limiter::Limiter>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        let limiter = std::sync::Arc::new(limiter::Limiter::new(cfg.max_speed));
        let client = Self::http_client(&cfg, None)?;
        Ok(Self { client, cfg, limiter })
    }

    /// Build a reqwest client with the engine's identity (user agent,
    /// pass-through headers), optionally pinned to a local interface
    /// address for NIC bonding. All clients are equivalent for requests;
    /// only the socket binding differs.
    fn http_client(cfg: &EngineConfig, local: Option<IpAddr>) -> Result<reqwest::Client> {
        // If the user supplies their own User-Agent header, it must win over
        // the built-in one, so only set the default when they don't.
        let has_user_ua = cfg.headers.keys().any(|k| k.to_ascii_lowercase() == "user-agent");
        let mut builder = reqwest::Client::builder();
        if !has_user_ua {
            builder = builder.user_agent(&cfg.user_agent);
        }
        let mut default_headers = reqwest::header::HeaderMap::new();
        for (k, v) in &cfg.headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                Error::Io(std::io::Error::other(format!("bad header name '{k}': {e}")))
            })?;
            let value = reqwest::header::HeaderValue::from_str(v.as_str()).map_err(|e| {
                Error::Io(std::io::Error::other(format!("bad header value for '{k}': {e}")))
            })?;
            default_headers.insert(name, value);
        }
        builder = builder.default_headers(default_headers);
        if let Some(ip) = local {
            builder = builder.local_address(ip);
        }
        // Dead-connection guards: a half-open server (accepts, then never
        // sends and never FINs) would otherwise pend a segment forever and
        // keep retry/backoff permanently disabled for it. Both timeouts are
        // far above any legitimate read stall.
        builder = builder
            .connect_timeout(Duration::from_secs(15))
            // 15 s of total silence is a dead connection on any real link.
            // 30 s meant a stalled segment kept its share of the file frozen
            // for half a minute before the retry could reconnect.
            .read_timeout(Duration::from_secs(15))
            .tcp_keepalive(Duration::from_secs(15));
        Ok(builder.build()?)
    }

    /// Probe a URL and return the plan without downloading.
    pub async fn plan(&self, url: &str) -> Result<PlanInfo> {
        planner::probe(&self.client, url).await
    }

    /// Download `url` into `dest_dir` (default: current directory).
    ///
    /// `token` cancels the download. Progress is persisted to a
    /// `.ripdl.json` sidecar so an interrupted task can resume.
    pub async fn download(
        &self,
        url: &str,
        dest_dir: Option<PathBuf>,
        token: CancellationToken,
        on_progress: impl FnMut(Progress) + Send + 'static,
    ) -> Result<PathBuf> {
        self.download_named(url, dest_dir, None, token, on_progress)
            .await
    }

    /// Like [`Engine::download`], but the file is written under `filename`
    /// instead of the name the server advertises.
    ///
    /// Bundle parts need this: hosters routinely answer every part with
    /// `attachment; filename=download.zip`, which would otherwise collapse a
    /// whole multi-part set onto one name. The name is still sanitized.
    pub async fn download_named(
        &self,
        url: &str,
        dest_dir: Option<PathBuf>,
        filename: Option<String>,
        token: CancellationToken,
        on_progress: impl FnMut(Progress) + Send + 'static,
    ) -> Result<PathBuf> {
        let info = self.plan(url).await?;

        let dir = match dest_dir {
            Some(d) => d,
            None => std::env::current_dir()?,
        };
        std::fs::create_dir_all(&dir)?;
        let name = filename
            .map(|f| crate::names::sanitize_component(&f, &info.filename))
            .unwrap_or_else(|| info.filename.clone());
        let dest = dir.join(&name);
        let state_path = sidecar_path(&dest);

        let id = uuid::Uuid::new_v4().to_string();

        // Resume when a compatible sidecar exists (same url + etag, not done).
        let (segments, resumed) = match read_sidecar(&state_path)? {
            Some(prev)
                if prev.url == url
                    && prev.etag == info.etag
                    && prev.state != TaskState::Completed =>
            {
                (prev.segments, true)
            }
            _ => (plan_fresh(&info, &self.cfg), false),
        };
        tracing::info!(id = %id, resumed, segments = segments.len(), "planned download");

        // Prepare the destination file (truncate only when starting over).
        {
            let f = if resumed {
                std::fs::OpenOptions::new().create(true).write(true).open(&dest)?
            } else {
                std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&dest)?
            };
            if let Some(total) = info.total {
                f.set_len(total)?;
            }
        }

        let cells: Arc<Mutex<Vec<SegmentState>>> = Arc::new(Mutex::new(segments.clone()));
        let downloaded = Arc::new(AtomicU64::new(segments.iter().map(|s| s.done).sum()));
        let active = Arc::new(AtomicU64::new(segments.len() as u64));
        let finished = Arc::new(AtomicBool::new(false));
        // Shared in-flight cap: 429/503 (or refused connections) halve it, a
        // finished attempt doubles it back, so a quota storm ends up with one
        // connection instead of eight retries tripping the same limit.
        let gate = gate::Gate::new(segments.len());

        // Spawn one task per segment; each writes at its own file offset.
        // NIC bonding (see `Bond`): when active, segments cycle through
        // per-interface clients; a segment whose interface ultimately
        // fails gets one last-ditch attempt over the OS default route.
        let segment_clients = self.bond_clients();

        let mut handles = Vec::with_capacity(segments.len());
        for seg in segments {
            let (client, os_route_fallback) = if segment_clients.is_empty() {
                (self.client.clone(), None)
            } else {
                let (c, is_primary) = &segment_clients[seg.index % segment_clients.len()];
                (
                    c.clone(),
                    (!is_primary).then(|| self.client.clone()),
                )
            };
            let url = url.to_string();
            let etag = info.etag.clone();
            let retry_cfg = self.cfg.retry.clone();
            let dest = dest.clone();
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let token = token.clone();
            let active = active.clone();
            let gate = gate.clone();

            handles.push(tokio::spawn({
                let limiter = self.limiter.clone();
                async move {
                    let mut r = run_segment(
                        &client,
                        &url,
                        etag.as_deref(),
                        seg.clone(),
                        retry_cfg.clone(),
                        cells.clone(),
                        dest.clone(),
                        downloaded.clone(),
                        limiter.clone(),
                        token.clone(),
                        &gate,
                    )
                    .await;
                    if r.is_err() && !matches!(r, Err(Error::Cancelled)) {
                        if let Some(primary) = os_route_fallback {
                            tracing::warn!(
                                index = seg.index,
                                "bonded interface failed; one attempt over the OS default route"
                            );
                            r = run_segment(
                                &primary,
                                &url,
                                etag.as_deref(),
                                seg,
                                retry_cfg,
                                cells,
                                dest,
                                downloaded,
                                limiter,
                                token,
                                &gate,
                            )
                            .await;
                        }
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    r
                }
            }));
        }

        // Task metadata for the sidecar file (kept for the final writes below).
        let meta = TaskStateFile {
            id,
            url: url.to_string(),
            etag: info.etag.clone(),
            total: info.total,
            state: TaskState::Running,
            segments: Vec::new(),
        };

        // Fire-and-forget: this loop exits itself after the final snapshot.
        let _reporter = {
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let active = active.clone();
            let finished = finished.clone();
            let state_path = state_path.clone();
            let meta_task = meta.clone();
            let dest_report = dest.clone();
            let mut on_progress = on_progress;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                // A stall (retry backoff, slow disk) must not queue up a
                // burst of catch-up ticks: each one would sample a tiny dt
                // and read as a huge speed spike.
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // Sentinel != any real value: force one write on the first tick.
                let mut persisted_downloaded = u64::MAX;
                loop {
                    tick.tick().await;
                    let drained = active.load(Ordering::SeqCst) == 0;
                    if drained || finished.load(Ordering::SeqCst) {
                        // Final snapshot with the true final byte count (no
                        // sidecar write here: the caller owns the last state).
                        on_progress(Progress {
                            id: meta_task.id.clone(),
                            downloaded: downloaded.load(Ordering::Relaxed),
                            total: meta_task.total,
                            percent: None,
                            speed_bps: None,
                            dest_file: Some(dest_report.clone()),
                            ..Progress::default()
                        });
                        break;
                    }
                    let downloaded_now = downloaded.load(Ordering::Relaxed);
                    // Skip the disk write when no bytes landed since last tick
                    // (retry backoff, limiter throttle, stalled server):
                    // downloaded is the only sidecar field that moves between
                    // ticks, so an unchanged counter means an unchanged file.
                    if downloaded_now != persisted_downloaded {
                        persisted_downloaded = downloaded_now;
                        let segments = cells.lock().expect("state cells").clone();
                        let file = TaskStateFile {
                            segments,
                            ..meta_task.clone()
                        };
                        // Serialize on this thread (cheap); the fs write+rename
                        // runs on the blocking pool, keeping runtime workers
                        // responsive under antivirus/flush stalls.
                        if let Ok(json) = serde_json::to_vec(&file) {
                            let path = state_path.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                write_sidecar_bytes(&path, json)
                            })
                            .await;
                        }
                    }
                    on_progress(Progress {
                        id: meta_task.id.clone(),
                        downloaded: downloaded_now,
                        total: meta_task.total,
                        percent: None,
                        speed_bps: None,
                        dest_file: Some(dest_report.clone()),
                        ..Progress::default()
                    });
                }
            })
        };

        let results = futures::future::join_all(handles).await;
        // Un-park the reporter so it emits the final snapshot and exits.
        finished.store(true, Ordering::SeqCst);

        // Pick the most meaningful error (real failures over Cancelled).
        let mut first_err: Option<Error> = None;
        for r in results {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let priority = !matches!(e, Error::Cancelled);
                    if first_err.is_none()
                        || (priority && matches!(first_err, Some(Error::Cancelled)))
                    {
                        first_err = Some(e);
                    }
                }
                Err(join_err) => {
                    first_err = Some(Error::Io(std::io::Error::other(join_err.to_string())));
                }
            }
        }

        let persist_paused = |cells: &Arc<Mutex<Vec<SegmentState>>>, meta: &TaskStateFile| {
            let segments = cells.lock().expect("state cells").clone();
            let _ = write_sidecar(
                &state_path,
                &TaskStateFile {
                    segments,
                    state: TaskState::Paused,
                    ..meta.clone()
                },
            );
        };

        if let Some(e) = first_err {
            persist_paused(&cells, &meta);
            return Err(e);
        }

        if let Some(total) = info.total {
            let got = downloaded.load(Ordering::Relaxed);
            if got != total {
                persist_paused(&cells, &meta);
                return Err(Error::SizeMismatch {
                    expected: total,
                    actual: got,
                });
            }
        }

        let _ = std::fs::remove_file(&state_path);
        Ok(dest)
    }

    /// Per-interface segment clients according to `cfg.bond`. Empty result
    /// = every segment uses the unbound primary client (plain mode).
    ///
    /// Binding is best-effort: an interface whose client fails to build is
    /// skipped with a warning, and if *nothing* binds, plain mode wins.
    ///
    /// `Auto` keeps the OS-default interface on the unbound client and
    /// binds one client per additional interface; with fewer than two
    /// discovered paths there is nothing to combine, so this returns empty.
    fn bond_clients(&self) -> Vec<(reqwest::Client, bool)> {
        let mut out: Vec<(reqwest::Client, bool)> = Vec::new();
        match &self.cfg.bond {
            Bond::Off => {}
            Bond::Addresses(addrs) => {
                for ip in addrs {
                    match Self::http_client(&self.cfg, Some(*ip)) {
                        Ok(c) => out.push((c, false)),
                        Err(e) => tracing::warn!(ip = %ip, error = %e, "cannot bind to address"),
                    }
                }
            }
            Bond::Auto => {
                let nics = nic::bonded_nics();
                out.push((self.client.clone(), true));
                for n in nics.iter().filter(|n| !n.is_default_route) {
                    match Self::http_client(&self.cfg, Some(IpAddr::V4(n.addr))) {
                        Ok(c) => {
                            tracing::info!(name = %n.name, addr = %n.addr, "bonding extra uplink");
                            out.push((c, false));
                        }
                        Err(e) => {
                            tracing::warn!(name = %n.name, addr = %n.addr, error = %e, "cannot bind to interface")
                        }
                    }
                }
                let extra = out.len() - 1;
                if extra == 0 {
                    tracing::info!(
                        uplinks = nics.len(),
                        "bonding: no extra interfaces besides the default route; OS routing only"
                    );
                    out.clear();
                }
            }
        }
        out
    }
}

/// Fetch one byte range (or the whole body when `end == UNBOUNDED`) with
/// bounded retries and backoff. A retried segment continues from the byte
/// offset already written; if the server cannot serve ranges, the stream
/// restarts from zero.
///
/// TODO(perf): move sync writes into `spawn_blocking` once we profile.
async fn run_segment(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    initial: SegmentState,
    retry_cfg: retry::RetryConfig,
    cells: Arc<Mutex<Vec<SegmentState>>>,
    dest: PathBuf,
    downloaded: Arc<AtomicU64>,
    limiter: std::sync::Arc<limiter::Limiter>,
    token: CancellationToken,
    gate: &gate::Gate,
) -> Result<()> {
    let mut attempt = 0u32;
    loop {
        // Re-read state: a previous attempt may have advanced it.
        let mut seg = cells.lock().expect("state cells")[initial.index].clone();

        // Server without range support: partial progress is unusable, roll
        // back the counter and restart the stream from zero.
        if seg.end == UNBOUNDED && seg.done > 0 {
            let undone = seg.done;
            downloaded.fetch_sub(undone, Ordering::Relaxed);
            seg.done = 0;
            cells.lock().expect("state cells")[initial.index].done = 0;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&dest)?
                .set_len(0)?;
            tracing::warn!(index = seg.index, "no range support; restarting stream");
        }

        // One in-flight request per permit; cancellation while queued must
        // still win, or pausing a degraded download would hang.
        let permit = tokio::select! {
            _ = token.cancelled() => return Err(Error::Cancelled),
            p = gate.acquire() => p,
        };

        match segment_attempt(
            client, url, etag, &seg, &cells, &dest, &downloaded, &limiter, &token,
        )
        .await
        {
            Ok(()) => {
                drop(permit);
                gate.grow();
                return Ok(());
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e)
                if retry::is_retryable(&e)
                    && attempt < retry::max_attempts(&retry_cfg, retry::is_host_pressure(&e)) =>
            {
                attempt += 1;
                let host_pressure = retry::is_host_pressure(&e);
                let delay = retry::delay(&retry_cfg, host_pressure, attempt, initial.index as u64);
                if host_pressure {
                    // Fewer connections from now on: serial retries have a
                    // chance where eight simultaneous ones re-trip the quota.
                    gate.shrink();
                    tracing::info!(index = seg.index, limit = gate.limit(), "segment concurrency reduced");
                }
                drop(permit);
                tracing::warn!(
                    index = seg.index,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    host_pressure,
                    error = %e,
                    "segment retry"
                );
                tokio::select! {
                    _ = token.cancelled() => return Err(Error::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// One attempt at fetching a segment. Bytes are buffered and written at their
/// absolute file positions via the blocking pool, so slow disks cannot stall
/// the network pipeline; `cells[index].done` advances only for flushed bytes,
/// so a retry (or a crash) resumes from the last durable offset.
#[allow(unused_assignments)] // final-flush stores are intentionally dead
async fn segment_attempt(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    seg: &SegmentState,
    cells: &Arc<Mutex<Vec<SegmentState>>>,
    dest: &Path,
    downloaded: &Arc<AtomicU64>,
    limiter: &limiter::Limiter,
    token: &CancellationToken,
) -> Result<()> {
    let mut file: Option<std::fs::File> = Some(
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(dest)?,
    );

    let mut req = client.get(url);
    if seg.end != UNBOUNDED {
        req = req.header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", seg.start + seg.done, seg.end),
        );
    }
    // Guard against the file changing underneath a resume.
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_MATCH, etag);
    }

    let mut stream = req.send().await?.error_for_status()?.bytes_stream();
    let mut pos = seg.start + seg.done;
    let mut seg_done = seg.done;
    // Ping-pong buffers: one fills from the network while the other is being
    // written on the blocking pool. Network and disk overlap instead of
    // serialize ("read 256 KiB, wait for the disk, repeat").
    let mut bufs: [Vec<u8>; 2] = [
        Vec::with_capacity(SEGMENT_BUF + 1024),
        Vec::with_capacity(SEGMENT_BUF + 1024),
    ];
    let mut cur = 0;
    let mut inflight: Option<tokio::task::JoinHandle<(std::fs::File, Vec<u8>, usize)>> = None;
    // Byte accounting for this attempt: parking a block adds to `counted`,
    // a joined (durable) write adds to `durable_added`. On any non-success
    // exit the un-durable remainder is subtracted again, so a retry that
    // re-downloads from `cells.done` cannot double-count (which used to read
    // as a huge speed spike and could end in SizeMismatch).
    let mut counted: u64 = 0;
    let mut durable_added: u64 = 0;
    macro_rules! rollback {
        () => {
            if counted > durable_added {
                downloaded.fetch_sub(counted - durable_added, Ordering::Relaxed);
            }
        };
    }

    // Flushed bytes advance only when durably on disk (join), but the
    // headline counter moves at park time: a failed write aborts the task
    // anyway, and progress display sidecars resume from the durable
    // cells.done, never from this counter.
    macro_rules! await_write {
        () => {
            if let Some(task) = inflight.take() {
                let (handle, block, flushed) = task.await.expect("blocking pool join");
                if flushed != block.len() {
                    rollback!();
                    return Err(std::io::Error::other("segment file write failed").into());
                }
                // Durable counters move only when the bytes are on disk.
                pos += flushed as u64;
                seg_done += flushed as u64;
                durable_added += flushed as u64;
                cells.lock().expect("state cells")[seg.index].done = seg_done;

                file = Some(handle);
                bufs[1 - cur] = block;
                bufs[1 - cur].clear();
            }
        };
    }

    // Flush the current buffer: join the previous write (it holds the file
    // handle), then park this block on the blocking pool.
    macro_rules! flush {
        () => {
            if !bufs[cur].is_empty() {
                await_write!();
                let mut handle = file.take().expect("segment file handle");
                let block = std::mem::take(&mut bufs[cur]);
                let pos_abs = pos;
                let len = block.len();
                let task = tokio::task::spawn_blocking(move || {
                    use std::io::{Seek, SeekFrom, Write};
                    let mut flushed = 0usize;
                    if handle.seek(SeekFrom::Start(pos_abs)).is_ok()
                        && handle.write_all(&block).is_ok()
                    {
                        flushed = block.len();
                    }
                    (handle, block, flushed)
                });
                inflight = Some(task);
                downloaded.fetch_add(len as u64, Ordering::Relaxed);
                counted += len as u64;
                cur = 1 - cur;
            }
        };
    }

    loop {
        let chunk = tokio::select! {
            _ = token.cancelled() => {
                // Pause/retry with a parked write: join it so the sidecar
                // captures every fully-written block (resume must not
                // re-copy bytes that are already on disk). A failed join
                // here can only leave LESS progress durable - harmless.
                if let Some(task) = inflight.take() {
                    if let Ok((handle, block, flushed)) = task.await {
                        if flushed == block.len() {
                            pos += flushed as u64;
                            seg_done += flushed as u64;
                            durable_added += flushed as u64;
                            cells.lock().expect("state cells")[seg.index].done = seg_done;
                            file = Some(handle);
                            let _ = &file;
                        }
                    }
                }
                rollback!();
                return Err(Error::Cancelled);
            }
            chunk = stream.next() => match chunk {
                Some(Ok(c)) => c,
                Some(Err(e)) => {
                    // Retry resumes from cells.done: give back the bytes that
                    // were parked but never became durable.
                    rollback!();
                    return Err(e.into());
                }
                None => break,
            },
        };

        // Pace consumption: all segments draw from one shared budget.
        limiter.acquire(chunk.len() as u64).await;

        // Flush before an extension that would exceed the preallocated
        // capacity: a realloc here would memcpy the whole ~256 KiB block.
        if bufs[cur].capacity() - bufs[cur].len() < chunk.len() {
            flush!();
        }
        bufs[cur].extend_from_slice(&chunk);
        if bufs[cur].len() >= SEGMENT_BUF {
            flush!();
        }
    }
    flush!();
    // The final block may still be writing: wait for it before reporting the
    // segment done (its counters move in the join).
    await_write!();

    // Server ended the stream early: count as a retryable transport fault.
    if seg.end != UNBOUNDED && seg_done < seg.end + 1 - seg.start {
        return Err(Error::ShortSegment {
            index: seg.index,
            expected: seg.end + 1 - seg.start,
            got: seg_done,
        });
    }
    Ok(())
}

/// Split the file into at most `max_segments` ranges, each at least
/// `min_segment_size`, unless the server cannot serve ranges.
fn plan_fresh(info: &PlanInfo, cfg: &EngineConfig) -> Vec<SegmentState> {
    match (info.accept_ranges, info.total) {
        (true, Some(total)) if total >= cfg.min_segment_size => {
            let n = ((total / cfg.min_segment_size) as usize)
                .min(cfg.max_segments)
                .max(2);
            let chunk = total / n as u64;
            (0..n)
                .map(|i| {
                    let start = i as u64 * chunk;
                    let end = if i == n - 1 { total - 1 } else { start + chunk - 1 };
                    SegmentState { index: i, start, end, done: 0 }
                })
                .collect()
        }
        (true, Some(total)) => vec![SegmentState {
            index: 0,
            start: 0,
            end: total - 1,
            done: 0,
        }],
        _ => vec![SegmentState {
            index: 0,
            start: 0,
            end: UNBOUNDED,
            done: 0,
        }],
    }
}

/// Resume sidecar path for a partial download (`<file>.ripdl.json`).
pub fn sidecar_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".ripdl.json");
    PathBuf::from(s)
}

/// Atomic sidecar write: temp file + rename, so a crash mid-write never
/// leaves a corrupt state file behind. Compact JSON: the sidecar is never
/// read by humans, and resume parses both forms.
fn write_sidecar(path: &Path, file: &TaskStateFile) -> Result<()> {
    let json = serde_json::to_vec(file).map_err(|e| Error::State(e.to_string()))?;
    write_sidecar_bytes(path, json)
}

/// Blocking half: writes prebuilt JSON bytes atomically. Lets the reporter
/// serialize outside the blocking pool and keep runtime threads free.
fn write_sidecar_bytes(path: &Path, json: Vec<u8>) -> Result<()> {
    let tmp = path.with_extension("ripdl.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<Option<TaskStateFile>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(total: Option<u64>, ranges: bool) -> PlanInfo {
        PlanInfo {
            url: "https://example.com/file.zip".parse().unwrap(),
            total,
            accept_ranges: ranges,
            filename: "file.zip".into(),
            etag: None,
        }
    }

    #[test]
    fn small_file_stays_single_segment() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(1024), true), &cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].end, 1023);
    }

    #[test]
    fn large_file_splits_with_contiguous_ranges() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(10 * 1024 * 1024), true), &cfg);
        assert_eq!(plan.len(), 8);
        assert_eq!(plan[0].start, 0);
        assert_eq!(plan[7].end, 10 * 1024 * 1024 - 1);
        for pair in plan.windows(2) {
            assert_eq!(pair[1].start, pair[0].end + 1);
        }
    }

    #[test]
    fn no_ranges_means_single_stream() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(1024 * 1024 * 500), false), &cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].end, UNBOUNDED);
    }

    #[test]
    fn torrent_bond_description_states() {
        let one = [IpAddr::from([10, 0, 0, 2])];
        assert_eq!(describe_torrent_bond(false, false, &[]), "off");
        assert_eq!(describe_torrent_bond(false, true, &[]), "off");
        assert_eq!(describe_torrent_bond(false, true, &one), "off");
        assert!(describe_torrent_bond(true, false, &[]).contains("when the next torrent starts"));
        assert!(describe_torrent_bond(true, true, &[]).contains("follows the network"));
        assert!(describe_torrent_bond(true, true, &one).starts_with("active on"));
    }
}
