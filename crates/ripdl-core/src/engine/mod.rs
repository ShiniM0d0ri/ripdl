mod gate;
mod limiter;
mod nic;
mod planner;
pub mod retry;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

pub use nic::{bonded_nics, live_bonded_nics, scan_uplinks, UplinkNic, UplinkScan};
pub use planner::{percent_decode_lossy, PlanInfo};

use crate::bond;
use crate::error::{Error, Result};
use crate::task::{Progress, SegmentState, TaskState, TaskStateFile};

/// Sentinel end offset for "unknown total size, stream to the end".
const UNBOUNDED: u64 = u64::MAX;

/// Write batching, self-tuned by the throughput the link actually delivers
/// (target ~250 ms of network data per disk write event, bounded): it
/// starts small so the first durability checkpoint arrives fast and the
/// rate estimate has something to measure, then grows with the measured
/// rate and caps at 4 MiB. Each write event is one Defender minifilter
/// interception - batching fewer, bigger writes is the whole disk-toll
/// trick; the OS TCP window, not this buffer, carries the throughput.
/// Interruption cost is bounded: an unflushed batch at a hard kill costs
/// at most its own size of re-download (graceful pauses join their write,
/// so they lose nothing).
const BATCH_MIN: usize = 256 * 1024;
const BATCH_CAP: usize = 4 * 1024 * 1024;

/// Sidecar persistence cadence in steady state: the atomic rename + minifilter
/// rescan cost more than the 200 bytes it writes, so <= 2 s is plenty in
/// steady state; every pause/finish/first-tick flushes immediately.
const SIDECAR_CADENCE: Duration = Duration::from_secs(2);

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
    /// An external, already-live bond authority (typically the daemon's
    /// shared one). When `Some`, the engine saves its known links and
    /// records bytes through it instead of discovering its own set - the
    /// daemon's watcher feeds discovery + sampling, so NIC events and
    /// weight updates apply to every task at once, live. `None` = the
    /// engine builds a private authority at construction (standalone
    /// CLI/tests).
    pub bond_state: Option<Arc<bond::Bond>>,
    /// Stream watchdog: a connection that delivers no bytes for this long
    /// errors as [`Error::StreamStalled`] instead of parking the worker
    /// forever. This happens without a transport fault when a source
    /// address dies under the session (NIC churn) or an intermediary wedges:
    /// the TCP stream stays alive, every packet dies inside the kernel, and
    /// `await read` would wait for a terminator that never comes. Long-slow
    /// links are safe: the window only needs *some* progress. `None`
    /// disables the watchdog (tests only).
    pub stream_idle: Option<std::time::Duration>,
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
            bond_state: None,
            stream_idle: Some(std::time::Duration::from_secs(30)),
        }
    }
}

pub struct Engine {
    client: reqwest::Client,
    cfg: Arc<EngineConfig>,
    limiter: std::sync::Arc<limiter::Limiter>,
    /// HTTP dial authority when bonding is configured (see [`Dialer`]).
    dialer: Option<Arc<Dialer>>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        let limiter = std::sync::Arc::new(limiter::Limiter::new(cfg.max_speed));
        let cfg = Arc::new(cfg);
        let client = Self::http_client(&cfg, None)?;
        let dialer = Dialer::new(cfg.clone(), client.clone()).map(Arc::new);
        Ok(Self {
            client,
            cfg,
            limiter,
            dialer,
        })
    }

    /// The HTTP bond authority, when bonding is configured. The daemon
    /// reaches the same instance through its own `bond_state` handle; this
    /// accessor is for frontends (CLI readouts, tests).
    pub fn bond(&self) -> Option<&Arc<bond::Bond>> {
        self.dialer.as_ref().map(|d| &d.bond)
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
                (upgrade_legacy_layout(&prev, &info, &self.cfg), true)
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
            // Pre-allocate once (planned size known): every later write lands
            // in place - no per-write grow/metadata events, contiguous
            // allocation on NTFS (disk-toll rule C9b). Never truncate a
            // resumed file: only extend when it is shorter than planned.
            let len = f.metadata()?.len();
            if let Some(total) = info.total {
                if len < total {
                    f.set_len(total)?;
                }
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
        // Bonding: every attempt dials through a link picked from the live
        // bond authority (smooth weighted RR over measured rates); a link
        // whose dials fail is skipped on the spot, and when nothing can be
        // picked the OS default route carries the attempt.
        let dialer = self.dialer.clone();

        let mut handles = Vec::with_capacity(segments.len());
        for (rank, seg) in segments.into_iter().enumerate() {
            let url = url.to_string();
            let etag = info.etag.clone();
            let retry_cfg = self.cfg.retry.clone();
            let dest = dest.clone();
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let token = token.clone();
            let active = active.clone();
            let gate = gate.clone();
            let limiter = self.limiter.clone();
            let primary = self.client.clone();
            let dialer = dialer.clone();
            let stream_idle = self.cfg.stream_idle;
            // Ramp-in (the part-start lag spike): every segment used to
            // open its first handshake at the same moment, which read as a
            // hard CPU/AV hitch at every part start. First dials are spaced
            // RAMP_STEP apart; later attempts (and their segments) dial
            // freely. Capped so huge plans never wait more than a few
            // seconds for full parallelism.
            let stagger = RAMP_STEP * (rank as u32).min(RAMP_STAGGER_MAX);

            handles.push(tokio::spawn(async move {
                let r = run_segment(
                    &primary,
                    dialer.clone(),
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
                    stagger,
                    stream_idle,
                )
                .await;
                active.fetch_sub(1, Ordering::SeqCst);
                r
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
                let mut persist_at = tokio::time::Instant::now();
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
                    // Disk-toll rule: the progress callback fires every tick,
                    // but the sidecar (atomic rename + minifilter rescan)
                    // lands at most once per SIDECAR_CADENCE, plus the forced
                    // first write and every state transition (pause/finish).
                    if downloaded_now != persisted_downloaded
                        && tokio::time::Instant::now() >= persist_at
                    {
                        persisted_downloaded = downloaded_now;
                        persist_at = tokio::time::Instant::now() + SIDECAR_CADENCE;
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
}

/// Spacing between a download's first segment dials (ramp-in, see the
/// spawn site in `download_named`).
const RAMP_STEP: Duration = Duration::from_millis(400);
/// Cap on how long the last segment waits for its first dial.
const RAMP_STAGGER_MAX: u32 = 8;

/// One attempt's dial decision: the client to send through and the bond
/// link the connection should be attributed to (`None` for unattributed
/// OS-routed dials - bond off, or a fallback after every link skipped).
struct DialAttempt {
    client: reqwest::Client,
    link: Option<Arc<bond::Link>>,
}

/// The HTTP dial authority: turns [`bond::Bond`] picks into address-bound
/// reqwest clients, one warm pool **per link address** (a reqwest `Client`
/// maintains its own keep-alive connection pool, so established sockets and
/// their TCP congestion windows survive between a link's segment attempts -
/// dialing again only on failure or pool close; `SO_RCVBUF` is never
/// touched so the OS autotuner keeps the throughput).
///
/// Byte attribution happens in the segment attempts (per-chunk `record`);
/// outcomes at dial time (`note_ok`/`note_fail`).
struct Dialer {
    bond: Arc<bond::Bond>,
    /// Identity source (UA, cookies...) for each per-link client.
    cfg: Arc<EngineConfig>,
    /// The unbound client: the OS-default-route dial and the fallback when
    /// every link is skipped.
    primary: reqwest::Client,
    clients: Mutex<HashMap<IpAddr, reqwest::Client>>,
}

impl Dialer {
    /// Built when bonding is configured; `None` in plain mode (`Bond::Off`).
    /// The link set is seeded only when the engine owns the authority - a
    /// daemon-provided one is fed by the daemon's own watcher
    /// (`set_links`/`sample`), which keeps NIC events live across tasks.
    fn new(cfg: Arc<EngineConfig>, primary: reqwest::Client) -> Option<Self> {
        if cfg.bond.is_off() {
            return None;
        }
        let bond = match &cfg.bond_state {
            Some(shared) => Arc::clone(shared),
            None => {
                let links: Vec<bond::LinkInfo> = match &cfg.bond {
                    Bond::Auto => nic::bonded_nics()
                        .iter()
                        .map(|n| bond::LinkInfo {
                            ip: IpAddr::V4(n.addr),
                            name: n.name.clone(),
                            default_route: n.is_default_route,
                        })
                        .collect(),
                    Bond::Addresses(addrs) => addrs
                        .iter()
                        .map(|ip| bond::LinkInfo {
                            ip: *ip,
                            name: ip.to_string(),
                            default_route: false,
                        })
                        .collect(),
                    Bond::Off => unreachable!("checked above"),
                };
                let bond = Arc::new(bond::Bond::new());
                bond.set_links(&links);
                bond
            }
        };
        Some(Self {
            bond,
            cfg,
            primary,
            clients: Mutex::new(HashMap::new()),
        })
    }

    /// Pick a link for one attempt (smooth weighted round-robin; weight-0
    /// links are skipped, all-skipped or no links falls back unattributed).
    /// The OS-default-route link stays unbound - OS routing already directs
    /// it - but its connection still meters so its weight can scale.
    async fn attempt(&self, token: &CancellationToken) -> DialAttempt {
        // The authority may not be populated yet: the daemon discovers and
        // probes uplinks a moment after boot/first enable, and a task that
        // starts inside that window would strand every first dial on the
        // OS route - a worker then streams its whole (often minutes-long)
        // cell unattributed and the bond never engages for it. Give the
        // authority a bounded moment; cancellation wins.
        const PICK_WAIT: Duration = Duration::from_secs(3);
        let mut waited = Duration::ZERO;
        while self.bond.links().is_empty() && waited < PICK_WAIT {
            tokio::select! {
                _ = token.cancelled() => return DialAttempt { client: self.primary.clone(), link: None },
                _ = tokio::time::sleep(Duration::from_millis(200)) => waited += Duration::from_millis(200),
            }
        }
        match self.bond.pick() {
            Some(link) if link.info.default_route => DialAttempt {
                client: self.primary.clone(),
                link: Some(link),
            },
            Some(link) => DialAttempt {
                client: self.client_for(link.info.ip),
                link: Some(link),
            },
            None => DialAttempt {
                client: self.primary.clone(),
                link: None,
            },
        }
    }

    /// The warm client bound to a link address: built once, then pooled.
    /// A `Client`'s internal keep-alive pool carries established sockets
    /// across attempts, so a link's next attempt hops on an existing TCP
    /// window instead of re-handshaking.
    fn client_for(&self, ip: IpAddr) -> reqwest::Client {
        if let Some(pooled) = self.clients.lock().unwrap().get(&ip) {
            return pooled.clone();
        }
        // Built outside the lock (construction does DNS-free but non-trivial
        // work); the entry insert only wins if another attempt got there first.
        match Engine::http_client(&self.cfg, Some(ip)) {
            Ok(built) => {
                let mut clients = self.clients.lock().unwrap();
                clients.entry(ip).or_insert_with(|| built.clone()).clone()
            }
            Err(e) => {
                // Binding can fail when the address left the interface
                // between discovery and dial: best-effort - this attempt
                // goes out over the OS route, attributed to the link, and
                // the dial-failure path marks the link for skipping.
                tracing::warn!(ip = %ip, error = %e, "cannot bind segment client");
                self.primary.clone()
            }
        }
    }
}

/// Fetch one byte range (or the whole body when `end == UNBOUNDED`) with
/// bounded retries and backoff. A retried segment continues from the byte
/// offset already written; if the server cannot serve ranges, the stream
/// restarts from zero.
///
/// When `dialer` is present, every attempt dials through a freshly picked
/// bond link (the retry loop re-picks, so a dead link is left behind on the
/// spot); `stagger` delays the first dial by rank×RAMP_STEP (ramp-in).
async fn run_segment(
    primary: &reqwest::Client,
    dialer: Option<Arc<Dialer>>,
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
    stagger: Duration,
    stream_idle: Option<Duration>,
) -> Result<()> {
    let mut attempt = 0u32;
    // The cell this worker currently owns. On completion it can be split
    // elsewhere: the loop takes what `steal_largest` deals out (work stealing).
    let mut idx = initial.index;
    // The currently owned link's smoothed rate, kept for the size of a steal
    // claim (no measurement yet -> halve).
    let mut rate_bps: Option<f64> = None;
    // Ramp-in: hold this segment's *first* dial back so the part start does
    // not open every handshake at the same instant (G1). Cancellation wins
    // over the wait.
    if stagger > Duration::ZERO {
        tracing::debug!(index = idx, stagger_ms = stagger.as_millis() as u64, "ramp-in stagger");
        tokio::select! {
            _ = token.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep(stagger) => {}
        }
    }
    loop {
        // Re-read state: a previous attempt may have advanced it.
        let mut seg = cells.lock().expect("state cells")[idx].clone();

        // Completed cell: durable before the pause (e.g. serial retries
        // finished some cells before the run was cancelled). Nothing is
        // left to dial - a request here would carry a Range start past its
        // end. The byte counters were initialized from the sidecar.
        if seg.end != UNBOUNDED && seg.done >= seg.end + 1 - seg.start {
            tracing::debug!(index = seg.index, done = seg.done, "cell already durable at resume");
            match steal_largest(&cells, rate_bps) {
                Some(claim) => {
                    idx = claim.index;
                    attempt = 0;
                    tracing::info!(from = seg.index, claimed = claim.index,
                        start = claim.start, bytes = claim.end + 1 - claim.start,
                        "resumed worker already owned a durable cell; stole a claim");
                    continue;
                }
                None => return Ok(()),
            }
        }
        if seg.end == UNBOUNDED && seg.done > 0 {
            let undone = seg.done;
            downloaded.fetch_sub(undone, Ordering::Relaxed);
            seg.done = 0;
            cells.lock().expect("state cells")[idx].done = 0;
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

        // This attempt's dial: a bond pick when bonding is on, or the plain
        // unbound client. The pick re-runs per attempt, so a link that has
        // started to fail is abandoned by the retry loop itself.
        let pick = match dialer.as_ref() {
            Some(d) => Some(d.attempt(&token).await),
            None => None,
        };
        let (client, link) = match pick {
            Some(DialAttempt { client, link }) => (client, link),
            None => (primary.clone(), None),
        };
        // Claim sizing wants a rate ballpark: the link this attempt just
        // picked carries its smoothed EMA (0 before the first sample).
        if let Some(l) = link.as_ref() {
            let s = l.status();
            if s.rate_bps > 0 {
                rate_bps = Some(s.rate_bps as f64);
            }
        }
        let bond_ctx = match (&dialer, link.as_ref()) {
            (Some(d), Some(l)) => Some((d.bond.clone(), Arc::clone(l))),
            _ => None,
        };

        match segment_attempt(
            &client,
            bond_ctx.as_ref().map(|(b, l)| (b.as_ref(), l.as_ref())),
            url,
            etag,
            &seg,
            &cells,
            &dest,
            &downloaded,
            &limiter,
            &token,
            stream_idle,
        )
        .await
        {
            Ok(()) => {
                drop(permit);
                gate.grow();
                // The cell is done. Rather than retire, this worker splits
                // the largest cell still chewing and works the claim
                // (IDM's "freed worker takes half the straggler").
                match steal_largest(&cells, rate_bps) {
                    Some(claim) => {
                        tracing::info!(from = idx, claimed = claim.index,
                            start = claim.start, bytes = claim.end + 1 - claim.start,
                            "freed worker stole the largest remaining cell");
                        idx = claim.index;
                        attempt = 0;
                        continue;
                    }
                    None => return Ok(()),
                }
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e)
                if retry::is_retryable(&e)
                    && attempt < retry::max_attempts(&retry_cfg, retry::is_host_pressure(&e)) =>
            {
                attempt += 1;
                let host_pressure = retry::is_host_pressure(&e);
                let delay = retry::delay(&retry_cfg, host_pressure, attempt, idx as u64);
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

/// Work stealing: a worker whose cell just finished does not retire while
/// some other cell still has a large unclaimed span. It picks the cell with
/// the most bytes left, carves a claim out of the *tail* of its un-flushed
/// span and returns the new cell (the donor keeps working the head, so its
/// flushed prefix and `done` byte stay meaningful; coverage stays contiguous
/// because the carve happens under the same lock that hands it out).
///
/// Claim size: when the stealing worker knowns a link rate it takes
/// `chunks_for(rate)` — about 2 s of that link's throughput, 256 KiB..64 MiB
/// — so a fast link takes small bites often and the donor keeps feeding; a
/// rate-less worker takes the plain half (IDM's split-in-two). Never smaller
/// than [`STEAL_MIN`]: a thinner tail is all overhead (handshake, headers)
/// and never bigger than the donor keeps.
///
/// `Option<usize>`-style semantics: `None` = nothing worth stealing, the
/// caller may retire.
const STEAL_MIN: u64 = 256 * 1024;

/// Split the largest remaining cell in two and hand out its tail.
fn steal_largest(
    cells: &Mutex<Vec<SegmentState>>,
    claim_rate_bps: Option<f64>,
) -> Option<SegmentState> {
    let mut cells = cells.lock().expect("state cells");
    // Donor = the cell with the most un-flushed range. UNBOUNDED cells
    // cannot be split (a second endless stream would fetch the whole body).
    let mut donor: Option<usize> = None;
    let mut span: u64 = 0;
    for (i, c) in cells.iter().enumerate() {
        if c.end == UNBOUNDED {
            continue;
        }
        let left = c.end + 1 - c.start - c.done;
        if left > span {
            span = left;
            donor = Some(i);
        }
    }
    let donor = donor?;
    if span < 2 * STEAL_MIN {
        return None;
    }
    let claim = match claim_rate_bps.filter(|r| *r > 0.0) {
        Some(rate) => bond::chunks_for(rate, span).clamp(STEAL_MIN, span / 2),
        None => span / 2,
    };
    let donor = &mut cells[donor];
    // The claim lives at the far end of the donor's un-flushed span; the
    // donor keeps its flushed prefix plus the span's head.
    let claim_start = donor.start + donor.done + span - claim;
    let claim_end = donor.end;
    donor.end = claim_start - 1;
    let tail = SegmentState {
        index: cells.len(),
        start: claim_start,
        end: claim_end,
        done: 0,
    };
    cells.push(tail.clone());
    Some(tail)
}

/// One attempt at fetching a segment. Bytes are buffered and written at their
/// absolute file positions via the blocking pool, so slow disks cannot stall
/// the network pipeline; `cells[index].done` advances only for flushed bytes,
/// so a retry (or a crash) resumes from the last durable offset.
///
/// `bond_ctx` (present when this attempt dialed through a bond link) meters
/// the connection: `begin/end` around the whole attempt - `in_use` then
/// counts a link's *live* connections (what a status chip shows), not the
/// millisecond the transport send takes - and per-chunk `record` so the
/// link's rate EMA self-measures. `note_ok`/`note_fail` stay at response
/// arrival (a 429/5xx is host throttling, not the uplink).
/// One attempt's link credit, held from dial to stream end so `in_use`
/// reads as "connections live on this link" (a chip's active state), not
/// the instant of the transport send. Drop covers every exit path:
/// pause/cancel, mid-stream transport faults, and the happy flush.
struct BondDial<'a>(Option<(&'a bond::Bond, &'a bond::Link)>);

impl<'a> BondDial<'a> {
    fn begin(ctx: Option<(&'a bond::Bond, &'a bond::Link)>) -> Self {
        if let Some((bond, link)) = &ctx {
            bond.begin(link);
        }
        Self(ctx)
    }
}

impl Drop for BondDial<'_> {
    fn drop(&mut self) {
        if let Some((bond, link)) = self.0 {
            bond.end(link);
        }
    }
}

#[allow(unused_assignments)] // final-flush stores are intentionally dead
async fn segment_attempt(
    client: &reqwest::Client,
    bond_ctx: Option<(&bond::Bond, &bond::Link)>,
    url: &str,
    etag: Option<&str>,
    seg: &SegmentState,
    cells: &Arc<Mutex<Vec<SegmentState>>>,
    dest: &Path,
    downloaded: &Arc<AtomicU64>,
    limiter: &limiter::Limiter,
    token: &CancellationToken,
    stream_idle: Option<Duration>,
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

    // Bond bookkeeping: the connection holds the link credit from its dial
    // until its stream ends (see BondDial below); the response arriving -
    // whatever its status - proves the link works (a 429/5xx is the *host*
    // throttling, not the uplink); a transport failure counts against the
    // link's fail streak. The Drop guard releases the credit on every exit
    // path, including pause/cancel and mid-stream transport faults.
    //
    // The watchdog window covers the send too: a connect an intermediary
    // accepts and then silently never commits parks the worker the same way
    // a silent body would (see the chunk loop).
    //
    // tokio's timer runs on milliseconds, so the "no watchdog" stand-in is
    // years, not Duration::MAX (which panics there).
    const NO_IDLE: Duration = Duration::from_secs(31_536_000);
    let idle = stream_idle.unwrap_or(NO_IDLE);
    let _dial = BondDial::begin(bond_ctx);
    let sent: Result<reqwest::Response> = match tokio::time::timeout(idle, req.send()).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(Error::StreamStalled(idle.as_secs())),
    };
    if let Some((bond, link)) = &bond_ctx {
        match &sent {
            Ok(_) => bond.note_ok(link),
            Err(_) => bond.note_fail(link),
        }
    }
    let mut stream = sent?.error_for_status()?.bytes_stream();
    let mut pos = seg.start + seg.done;
    let mut seg_done = seg.done;
    // Ping-pong buffers: one fills from the network while the other is being
    // written on the blocking pool. Network and disk overlap instead of
    // serialize ("read 256 KiB, wait for the disk, repeat").
    let mut bufs: [Vec<u8>; 2] = [
        Vec::with_capacity(BATCH_MIN + 1024),
        Vec::with_capacity(BATCH_MIN + 1024),
    ];
    // Flush ceiling per cell: the full batch only when the cell is big
    // enough to feed it; a small cell flushes proportionally, so progress
    // counters (and pause granularity) stay honest at any file size. For
    // UNBOUNDED cells the batch cap is the ceiling.
    let cell_len = if seg.end == UNBOUNDED {
        u64::MAX
    } else {
        seg.end + 1 - seg.start
    };
    let cell_cap = ((cell_len / 4).clamp(64 * 1024, BATCH_CAP as u64)) as usize;
    // Self-tuned write batch (see BATCH_MIN): the target starts small -
    // the first durability checkpoint arrives fast and the rate estimate
    // gets something to measure - then tracks the link's throughput
    // once it shows up.
    let started = tokio::time::Instant::now();
    let mut flush_at = BATCH_MIN.min(cell_cap);
    let mut cur = 0;
    let mut inflight: Option<tokio::task::JoinHandle<(std::fs::File, Vec<u8>, usize)>> = None;
    // Byte accounting for this attempt: parking a block adds to `counted`,
    // a joined (durable) write adds to `durable_added`. On any non-success
    // exit the un-durable remainder is subtracted again, so a retry that
    // re-downloads from `cells.done` cannot double-count (which used to read
    // as a huge speed spike and could end in SizeMismatch).
    let mut counted: u64 = 0;
    let mut durable_added: u64 = 0;
    // Set when this cell's tail was stolen while the attempt streamed: the
    // attempt stops at its (now smaller) span and the claim worker fetches
    // the rest.
    let mut tail_stolen = false;
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
                // Steal interaction: another freed worker may have carved
                // this cell's tail out from under this attempt while it
                // streamed (the local `seg` never changes). Credit only
                // what the cell still owns and give the surplus back to
                // the headline counter - the claim worker re-downloads
                // that tail at the same offsets, so the same byte must
                // not be counted twice. When the claim eats everything
                // left, this attempt stops early.
                {
                    let mut state = cells.lock().expect("state cells");
                    let cell = &mut state[seg.index];
                    // UNBOUNDED cells (single stream) can never be overdrawn
                    // and are never carve targets - no clamp needed.
                    let allowed = if cell.end == UNBOUNDED {
                        u64::MAX
                    } else {
                        cell.end.saturating_sub(cell.start) + 1
                    };
                    let credit = allowed.saturating_sub(cell.done).min(flushed as u64);
                    let gave = flushed as u64 - credit;
                    if gave > 0 {
                        downloaded.fetch_sub(gave, Ordering::Relaxed);
                        counted = counted.saturating_sub(gave);
                    }
                    durable_added += credit;
                    cell.done += credit;
                    if cell.done >= allowed {
                        tail_stolen = true;
                    }
                }
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
                            // Same steal clamp as the happy path: a carver
                            // may have shortened this cell mid-attempt.
                            {
                                let mut state = cells.lock().expect("state cells");
                                let cell = &mut state[seg.index];
                                let allowed = if cell.end == UNBOUNDED {
                                    u64::MAX
                                } else {
                                    cell.end.saturating_sub(cell.start) + 1
                                };
                                let credit =
                                    allowed.saturating_sub(cell.done).min(flushed as u64);
                                let gave = flushed as u64 - credit;
                                if gave > 0 {
                                    downloaded.fetch_sub(gave, Ordering::Relaxed);
                                    counted = counted.saturating_sub(gave);
                                }
                                durable_added += credit;
                                cell.done += credit;
                            }
                            file = Some(handle);
                            let _ = &file;
                        }
                    }
                }
                rollback!();
                return Err(Error::Cancelled);
            }
            chunk = tokio::time::timeout(idle, stream.next()) => match chunk {
                // Watchdog wrapped: a connection alive-but-silent (lost
                // source address during NIC churn, a wedged proxy) must
                // error like any transport fault, not park the worker on a
                // read that never returns - one stalled worker froze whole
                // tasks at a partial byte count when the churn test found
                // it. The rollback hands the parked bytes back and the
                // retry redials through whatever link still works.
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(e))) => {
                    // Retry resumes from cells.done: give back the bytes
                    // that were parked but never became durable.
                    rollback!();
                    return Err(e.into());
                }
                Ok(None) => break,
                Err(_) => {
                    rollback!();
                    return Err(Error::StreamStalled(idle.as_secs()));
                }
            },
        };

        // Pace consumption: all segments draw from one shared budget.
        limiter.acquire(chunk.len() as u64).await;
        // Attribute the bytes to the link this connection dialed through;
        // the sampler folds deltas into per-link rate EMAs.
        if let Some((bond, link)) = &bond_ctx {
            bond.record(link, chunk.len() as u64);
        }

        // Retune the batch target from this attempt's own throughput:
        // one disk write event per ~250 ms of network data. Bounded so
        // slow links still batch meaningfully (floor) and the un-durable
        // window plus memory stay sane (cap); the cell cap keeps small
        // cells flushing proportionally.
        let rate = {
            let dt = started.elapsed().as_secs_f64().max(0.05);
            (counted + bufs[cur].len() as u64) as f64 / dt
        };
        flush_at = ((rate * 0.25) as usize).clamp(BATCH_MIN, BATCH_CAP).min(cell_cap);
        // Grow the buffer capacity to hold the evolving batch: an
        // amortized realloc, and the flush-check below still catches
        // per-chunk overflow.
        if bufs[cur].capacity() < flush_at {
            bufs[cur].reserve(flush_at);
        }
        // Flush before an extension that would exceed the capacity:
        // a realloc here would memcpy the whole block.
        if bufs[cur].capacity() - bufs[cur].len() < chunk.len() {
            flush!();
        }
        bufs[cur].extend_from_slice(&chunk);
        if bufs[cur].len() >= flush_at {
            flush!();
        }
        if tail_stolen {
            // A freed worker carved this cell's tail off mid-attempt: the
            // attempt's span is empty from here on. Flush the remainder so
            // the pre-cut prefix lands durable (the join clamps the credit,
            // the surplus of the tail is given back), then leave - the claim
            // worker fetches everything past the cut.
            flush!();
            await_write!();
            return Ok(());
        }
    }
    flush!();
    // The final block may still be writing: wait for it before reporting the
    // segment done (its counters move in the join).
    await_write!();

    // Server ended the stream early: count as a retryable transport fault.
    // (A stolen tail also leaves the raw `seg_done` short of the stale
    // local range on purpose - the claim owns the rest.)
    if seg.end != UNBOUNDED && !tail_stolen && seg_done < seg.end + 1 - seg.start {
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

/// One-cell UNBOUNDED layout, as the pre-behavioral-probe daemons stored
/// whenever a bare HEAD hid `accept-ranges`.
fn legacy_unbounded(prev: &TaskStateFile) -> bool {
    prev.segments.len() == 1 && prev.segments[0].end == UNBOUNDED
}

/// Resume a legacy sidecar against what the fresh probe now proves. If the
/// old layout is UNBOUNDED single-stream but the server clearly serves
/// ranges, upgrade to a fresh ranged plan and carry the durable prefix into
/// its first cell: those bytes were written contiguously from offset 0, so
/// the first cell resumes mid-stream and the rest re-download their (already
/// written, identical) spans in parallel. Replaying the old layout would
/// re-download the whole tail on one connection forever.
fn upgrade_legacy_layout(prev: &TaskStateFile, info: &PlanInfo, cfg: &EngineConfig) -> Vec<SegmentState> {
    if !legacy_unbounded(prev) {
        return prev.segments.clone();
    }
    let ranged_now =
        matches!((info.accept_ranges, info.total), (true, Some(total)) if total >= cfg.min_segment_size);
    if !ranged_now {
        return prev.segments.clone();
    }
    let mut fresh = plan_fresh(info, cfg);
    let carried = prev
        .segments
        .first()
        .map(|s| s.done)
        .unwrap_or(0)
        .min(fresh[0].end + 1 - fresh[0].start);
    fresh[0].done = carried;
    tracing::info!(
        carried,
        cells = fresh.len(),
        "legacy UNBOUNDED sidecar re-ranged into cells"
    );
    fresh
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
