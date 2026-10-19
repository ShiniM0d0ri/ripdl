//! Torrent tasks via `librqbit` (Apache-2.0) - the P3 transport.
//!
//! The daemon owns one session process-wide (DHT + listener); each torrent
//! task is a `ManagedTorrent` inside it. Progress samples the same
//! `Progress` stream the HTTP engine feeds, so the queue/UI layers cannot
//! tell the transports apart. Pause = librqbit pause (piece state survives
//! on disk), resume = re-ensure + unpause; the queue record payload
//! (magnet/.torrent) is the source of truth - no rqbit-side persistence.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, TorrentStats,
    TorrentStatsState,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::task::Progress;

/// One torrent task, in the same shape as `Task`/`HuntConfig`.
#[derive(Debug, Clone)]
pub struct TorrentConfig {
    pub id: String,
    /// magnet URI / info-hash / .torrent local path / .torrent http(s) URL.
    pub payload: String,
    pub dest_dir: PathBuf,
    /// Only fetch these file indices (None = everything).
    pub only_files: Option<Vec<usize>>,
    /// Sequential mode: a background drain keeps an open stream walking the
    /// files in order, which makes librqbit's picker prioritize pieces at
    /// the read position (usable playback before the download completes).
    pub sequential: bool,
}

/// Disk artifacts of a finished torrent: the torrent's root (the file
/// itself for single-file payloads, `<dest_dir>/<name>` for multi-file
/// ones, per rqbit's own layout) plus its identity.
#[derive(Debug, Clone)]
pub struct TorrentOutput {
    pub root: PathBuf,
    pub info_hash: String,
}

/// Result of `ensure`: the managed handle plus the guaranteed
/// task-owned artifact root on disk.
pub struct Ensured {
    pub handle: Arc<ManagedTorrent>,
    /// `<dest_dir>/<display name>`: single-file torrents put their file
    /// right there, multi-file ones fill the folder.
    pub root: PathBuf,
}

/// Identity facts surfaced as soon as the torrent is managed (works even
/// before metadata arrives: the info-hash is known from the payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentMeta {
    pub info_hash: String,
    /// Display name if known at this point (magnet `dn=`, metainfo parse,
    /// or library metadata once fetched).
    pub name: Option<String>,
    /// Task-owned artifact root computed for this task.
    pub root: PathBuf,
}

/// One row of the Files tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentFileSnapshot {
    /// Relative path inside the task root (joined components).
    pub name: String,
    pub size: u64,
    /// Bytes verified on disk for this file.
    pub downloaded: u64,
    /// Is this file part of the current `only_files` selection?
    pub included: bool,
}

/// One row of the Peers tab (rqbit exposes counters per peer, not
/// instantaneous dw/up speeds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentPeerSnapshot {
    pub addr: String,
    pub client: Option<String>,
    /// rqbit peer state name ("Live", "Connecting", ...).
    pub state: String,
    pub downloaded: u64,
    pub uploaded: u64,
}

/// Everything the details dialog / `details` CLI command needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentDetailsSnapshot {
    pub info_hash: String,
    pub name: Option<String>,
    pub root: Option<PathBuf>,
    pub total: u64,
    pub downloaded: u64,
    pub finished: bool,
    pub paused: bool,
    /// Lifetime bytes served to peers (rqbit's counter for this session).
    #[serde(default)]
    pub uploaded: u64,
    #[serde(default)]
    pub upload_speed_bps: f64,
    #[serde(default)]
    pub download_speed_bps: f64,
    /// Peers currently connected (rqbit "live" peer count).
    #[serde(default)]
    pub peers_connected: u32,
    /// Finished and not paused: this torrent is being served to the swarm.
    #[serde(default)]
    pub seeding: bool,
    pub files: Vec<TorrentFileSnapshot>,
    pub peers: Vec<TorrentPeerSnapshot>,
    pub trackers: Vec<String>,
}

/// Lightweight live sample for the daemon's seeding watcher (cheaper than
/// the full details snapshot: no file walk, no peer table).
#[derive(Debug, Clone, Copy, Default)]
pub struct TorrentSeedStats {
    /// rqbit's uploaded counter since this session added the torrent.
    pub uploaded: u64,
    pub upload_bps: f64,
    pub download_bps: f64,
    pub peers: u32,
    pub finished: bool,
    pub paused: bool,
}

/// One row of the pre-add file list (qB-style file picking before the
/// torrent is queued).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentPreviewFile {
    /// Relative path inside the task root.
    pub name: String,
    pub size: u64,
}

/// The Add-dialog preview: identity + full file list of a torrent payload,
/// resolved *before* the task exists. `.torrent` payloads parse offline in
/// milliseconds; magnets resolve over the swarm (the same wait qBittorrent
/// shows as "Retrieving metadata"), so callers should surface an
/// in-flight state and tolerate long / failing waits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentPreviewSnapshot {
    pub info_hash: String,
    pub name: Option<String>,
    pub files: Vec<TorrentPreviewFile>,
    pub total: u64,
}

/// Does a user-supplied payload look like a torrent (auto-detection for
/// `add`, GUI and bridge routing)? Pure string/path heuristics by design:
/// no network, no file reads.
pub fn looks_like_torrent_payload(payload: &str) -> bool {
    let p = payload.trim();
    if p.starts_with("magnet:") {
        return true;
    }
    if !p.contains("://") && PathBuf::from(p).exists() {
        return true;
    }
    // .torrent by URL extension (query/fragment stripped)
    let bare = p.split(['?', '#']).next().unwrap_or(p);
    bare.to_ascii_lowercase().ends_with(".torrent")
}

/// On-disk metainfo sniff: a legal .torrent always starts with `d<digit>:`.
pub fn is_torrent_file_bytes(b: &[u8]) -> bool {
    b.first() == Some(&b'd') && b.len() >= 2 && b[1].is_ascii_digit()
}

/// Resolve a payload to an `AddTorrent` plus the torrent's display name
/// when it can be known up front (parsed metainfo, or a magnet's `dn=`).
/// http(s) .torrent URLs are fetched here so every byte-backed payload
/// yields a name - rqbit never has to fetch them itself.
///
/// Recording the name before adding matters: rqbit writes files straight
/// into the output folder, so ripdl always sets `sub_folder` to keep each
/// task's artifacts in a dedicated, deletable directory.
async fn resolve_payload(payload: &str) -> Result<(AddTorrent<'static>, Option<String>)> {
    let p = payload.trim();
    if p.starts_with("magnet:") {
        return Ok((AddTorrent::from_url(p.to_string()), magnet_display_name(p)));
    }
    let bytes = if p.starts_with("http://") || p.starts_with("https://") {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let resp = client.get(p).send().await?.error_for_status()?;
        resp.bytes().await?.to_vec()
    } else {
        let path = PathBuf::from(p);
        if !path.is_file() {
            return Err(Error::InvalidUrl(payload.to_string()));
        }
        std::fs::read(&path)?
    };
    let name = metainfo_name(&bytes);
    Ok((AddTorrent::from_bytes(Bytes::from(bytes)), name))
}

/// Torrent name from metainfo bytes (None on any parse trouble; the
/// fallback sub-folder keeps the task self-contained regardless).
fn metainfo_name(bytes: &[u8]) -> Option<String> {
    let parsed = librqbit::torrent_from_bytes(bytes).ok()?;
    let name = parsed.info.data.name.as_ref()?;
    std::str::from_utf8(name.as_ref())
        .ok()
        .map(str::to_string)
}

/// Info-hash from a magnet URI's `xt=urn:btih:` parameter (hex or base32
/// encoded, v1 only). Lets callers identify a payload before the session
/// resolves metadata.
pub fn magnet_info_hash(payload: &str) -> Option<String> {
    if !payload.starts_with("magnet:") {
        return None;
    }
    for pair in payload.split('&') {
        let core = pair.trim().strip_prefix("magnet:?").unwrap_or(pair.trim());
        let Some(xt) = core.strip_prefix("xt=urn:btih:") else {
            continue;
        };
        if xt.len() == 40 && xt.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(xt.to_ascii_lowercase());
        }
        // rfct base32 variant: 32 chars, decode -> 20 bytes -> hex
        if xt.len() == 32 && xt.chars().all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit())
        {
            return base32_to_hex(xt);
        }
    }
    None
}

/// RFC 4648 base32 (case-insensitive, no padding) to lowercase hex.
fn base32_to_hex(s: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let upper = s.to_ascii_uppercase();
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(40);
    for c in upper.bytes() {
        let idx = ALPHABET.iter().position(|a| *a == c)?;
        acc = (acc << 5) | idx as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if out.len() != 20 {
        return None;
    }
    Some(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Magnet `dn=` display name, percent-decoded and sanitized for use as a
/// folder name. A display name, not a guarantee - but good enough for the
/// dedicated sub-folder (and usually identical to the real name).
fn magnet_display_name(payload: &str) -> Option<String> {
    for pair in payload.split('&').skip(1) {
        if let Some(v) = pair.strip_prefix("dn=") {
            let decoded = percent_decode(v);
            return Some(sanitize_component(&decoded));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => {
                let hex = b[i + 1..i + 3]
                    .iter()
                    .all(|c| c.is_ascii_hexdigit())
                    .then(|| {
                        u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16)
                            .unwrap()
                    });
                if let Some(byte) = hex {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Path-component hygiene for display names coming from the wild.
fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim().to_string();
    if trimmed.is_empty() {
        s.to_string()
    } else {
        trimmed
    }
}

/// Process-wide handle over the daemon's single torrent session.
pub struct TorrentSession {
    session: Arc<Session>,
}

impl TorrentSession {
    /// Start the session (explicit random ports; no rqbit-side persistence
    /// - the queue record is the source of truth, and separate sessions
    /// must never collide on one DHT/resource).
    ///
    /// `bind_ips`: when set (bonding on), every outgoing peer connection is
    /// bound to one of these local addresses round-robin (see
    /// `SessionOptions::bind_ips` in the vendored librqbit). The listener and
    /// DHT stay OS-routed; uTP, once TCP loses the race, also stays
    /// OS-routed — bonding redistributes the TCP peer load.
    pub async fn new(default_output_folder: PathBuf, bind: Option<Vec<IpAddr>>) -> Result<Self> {
        let opts = librqbit::SessionOptions {
            bind_ips: bind,
            dht: Some(librqbit::DhtSessionConfig {
                bootstrap_addrs: None,
                port: Some(0),          // ephemeral: never collide
                persistence: None,      // our records own state
            }),
            listen: Some(librqbit::ListenerOptions {
                listen_addr: "[::]:0".parse().unwrap(),
                ..librqbit::ListenerOptions::default()
            }),
            ..librqbit::SessionOptions::default()
        };
        Ok(Self {
            session: Session::new_with_opts(default_output_folder, opts).await?,
        })
    }

    /// Inner session (peers/trackers/files for the future Details RPC).
    pub fn raw(&self) -> &Arc<Session> {
        &self.session
    }

    /// Make sure the torrent is managed and running. Idempotent per
    /// info-hash: an already-managed (paused) torrent is unpaused; a fresh
    /// one is added with this task's destination and file selection.
    ///
    /// Always returns the task-owned root: `<dest_dir>/<display name>` (or
    /// `<dest_dir>/ripdl-torrent-<id>` when no name is known yet). rqbit
    /// writes files directly into its output folder, so ripdl sets
    /// `sub_folder` itself to keep artifacts contained and deletable.
    pub async fn ensure(&self, cfg: &TorrentConfig) -> Result<Ensured> {
        let (add, name) = resolve_payload(&cfg.payload).await?;
        let sub = match name {
            Some(n) => sanitize_component(&n),
            None => format!("ripdl-torrent-{}", cfg.id),
        };
        let root = cfg.dest_dir.join(&sub);
        // rqbit takes either output_folder (absolute) or sub_folder
        // (relative to the session default), never both - pass the
        // combined task-owned root directly.
        let mut opts = AddTorrentOptions {
            output_folder: Some(root.to_string_lossy().into_owned()),
            // Reuse files already on disk (resume/boot-restore/partial data):
            // with the default (false) rqbit refuses to open existing files
            // and the task fails with "error creating a new file". Overwrite
            // + piece hash-check is the correct resume semantic.
            overwrite: true,
            ..AddTorrentOptions::default()
        };
        if let Some(files) = &cfg.only_files {
            opts.only_files = Some(files.clone());
        }
        match self.session.add_torrent(add, Some(opts)).await? {
            AddTorrentResponse::AlreadyManaged(_, h) => {
                if h.is_paused() {
                    self.session.unpause(&h).await?;
                }
                Ok(Ensured { handle: h, root })
            }
            AddTorrentResponse::Added(_, h) => Ok(Ensured { handle: h, root }),
            AddTorrentResponse::ListOnly(_) => Err(Error::State(
                "torrent add unexpectedly returned list-only".into(),
            )),
        }
    }

    /// Drop a torrent from the session (files stay on disk; the daemon's
    /// delete_files path owns disk cleanup).
    pub async fn forget(&self, h: &Arc<ManagedTorrent>) {
        let _ = self
            .session
            .delete(librqbit::api::TorrentIdOrHash::Id(h.id()), false)
            .await;
    }

    /// Drop a torrent from the session (files stay on disk; the daemon's
    /// delete_files path owns disk cleanup). Needed when a task is removed:
    /// a stale managed handle would otherwise hijack the next add of the
    /// same payload (`AlreadyManaged` keeps the OLD output folder).
    pub async fn forget_by_hash(&self, info_hash_hex: &str) {
        let Some(handle) = self.find(info_hash_hex) else { return };
        let _ = self
            .session
            .delete(librqbit::api::TorrentIdOrHash::Id(handle.id()), false)
            .await;
    }

    /// librqbit pause: verified piece state survives on disk, unpause resumes.
    pub async fn pause(&self, h: &Arc<ManagedTorrent>) {
        let _ = self.session.pause(h).await;
    }

    /// Change which files the torrent fetches. `None` selects everything.
    /// Works live and while paused; drives the picker, not existing data.
    pub async fn set_selected_files(
        &self,
        info_hash_hex: &str,
        files: Option<Vec<usize>>,
    ) -> Result<()> {
        // Resolve "select all" through the torrent's metadata file count
        // (metadata missing = nothing to expand into; fail loudly rather
        // than silently deselecting everything).
        let handle = self.find(info_hash_hex)
            .ok_or_else(|| Error::State(format!("no managed torrent with hash {info_hash_hex}")))?;
        let wanted = match files {
            None => handle
                .with_metadata(|meta| (0..meta.file_infos.len()).collect::<Vec<usize>>())
                .map_err(|_| {
                    Error::State("torrent metadata not resolved yet".into())
                })?,
            Some(v) => v,
        };
        let set: std::collections::HashSet<usize> = wanted.into_iter().collect();
        self.session.update_only_files(&handle, &set).await?;
        Ok(())
    }

    fn find(&self, info_hash_hex: &str) -> Option<Arc<ManagedTorrent>> {
        self.session.with_torrents(|torrents| {
            torrents
                .map(|(_, t)| Arc::clone(t))
                .find(|t| t.info_hash().as_string() == info_hash_hex)
        })
    }

    /// Live swarm sample for the seeding watcher: rqbit's session upload
    /// counter, instantaneous up/down rates and the connected peer count.
    /// `None` when the torrent is not (or no longer) managed.
    pub fn seed_stats(&self, info_hash_hex: &str) -> Option<TorrentSeedStats> {
        let handle = self.find(info_hash_hex)?;
        let st = handle.stats();
        let live = st.live.as_ref();
        let paused = matches!(st.state, TorrentStatsState::Paused);
        Some(TorrentSeedStats {
            uploaded: st.uploaded_bytes,
            upload_bps: live.map(|l| l.upload_speed.as_bytes() as f64).unwrap_or(0.0),
            download_bps: live
                .map(|l| l.download_speed.as_bytes() as f64)
                .unwrap_or(0.0),
            peers: live.map(|l| l.snapshot.peer_stats.live).unwrap_or(0),
            finished: st.finished,
            paused,
        })
    }

    /// Pause by info-hash (boot restore and the seeding watcher act on
    /// hashes, while the download loop holds a handle).
    pub async fn pause_by_hash(&self, info_hash_hex: &str) {
        if let Some(h) = self.find(info_hash_hex) {
            self.pause(&h).await;
        }
    }

    /// Snapshot hash of every managed torrent, keyed by info-hash. Frontends
    /// match these against their records.
    pub fn list_torrents(&self) -> Vec<String> {
        self.session
            .with_torrents(|torrents| torrents.map(|(_, t)| t.info_hash().as_string()).collect())
    }

    /// Full details for one managed torrent, found by info-hash.
    pub fn details(&self, info_hash_hex: &str) -> Result<TorrentDetailsSnapshot> {
        // Details come straight off the library: TorrentStats for totals,
        // per-file details from the metainfo, per-peer counters via Api.
        let found = self
            .session
            .with_torrents(|torrents| {
                torrents
                    .map(|(_, t)| Arc::clone(t))
                    .find(|t| t.info_hash().as_string() == info_hash_hex)
            });
        let handle = found.ok_or_else(|| {
            Error::State(format!("no managed torrent with hash {info_hash_hex}"))
        })?;

        let st = handle.stats();
        let live = st.live.as_ref();
        let paused = matches!(st.state, TorrentStatsState::Paused);
        let mut snapshot = TorrentDetailsSnapshot {
            info_hash: handle.info_hash().as_string(),
            name: handle.name(),
            root: Some(handle.output_folder().to_path_buf()),
            total: st.total_bytes,
            downloaded: st.progress_bytes,
            finished: st.finished,
            paused,
            uploaded: st.uploaded_bytes,
            upload_speed_bps: live.map(|l| l.upload_speed.as_bytes() as f64).unwrap_or(0.0),
            download_speed_bps: live
                .map(|l| l.download_speed.as_bytes() as f64)
                .unwrap_or(0.0),
            peers_connected: live.map(|l| l.snapshot.peer_stats.live).unwrap_or(0),
            seeding: st.finished && !paused,
            files: Vec::new(),
            peers: Vec::new(),
            trackers: {
                let mut v: Vec<String> = handle
                    .shared()
                    .trackers
                    .iter()
                    .map(|u| u.to_string())
                    .collect();
                v.sort();
                v
            },
        };

        // Files + per-file progress (file_progress lines up with the
        // metadata file order).
        let wanted = handle.only_files();
        if let Ok(files) = handle.with_metadata(|meta| {
            meta.info
                .iter_file_details()
                .enumerate()
                .map(|(idx, d)| TorrentFileSnapshot {
                    name: d.filename.to_string(),
                    size: d.len,
                    downloaded: st.file_progress.get(idx).copied().unwrap_or(0),
                    included: wanted.as_deref().map(|o| o.contains(&idx)).unwrap_or(true),
                })
                .collect::<Vec<_>>()
        }) {
            snapshot.files = files;
        }

        // Peers: grab state + counters from the library Api (the filter
        // type lives in a private module; Default::default() names nothing).
        let api = librqbit::Api::new(self.session.clone(), None);
        if let Ok(snap) = api.api_peer_stats(
            librqbit::api::TorrentIdOrHash::Hash(handle.info_hash()),
            Default::default(),
        ) {
            let mut peers: Vec<TorrentPeerSnapshot> = snap
                .peers
                .into_iter()
                .map(|(addr, p)| TorrentPeerSnapshot {
                    addr,
                    client: p.client_name,
                    state: p.state.to_string(),
                    downloaded: p.counters.fetched_bytes,
                    uploaded: p.counters.uploaded_bytes,
                })
                .collect();
            peers.sort_by(|a, b| b.downloaded.cmp(&a.downloaded));
            snapshot.peers = peers;
        }

        Ok(snapshot)
    }

    /// qB-style pre-add preview: resolve a payload's metadata and file list
    /// WITHOUT managing the torrent (rqbit `list_only`). `.torrent` payloads
    /// (disk or URL) parse offline in milliseconds; magnets spend as long
    /// as the swarm needs - the same wait qBittorrent shows in its add
    /// dialog. The session is reusable: list-only adds never touch the
    /// managed db, so previewing can't corrupt or hijack real tasks.
    pub async fn preview(&self, payload: &str) -> Result<TorrentPreviewSnapshot> {
        let (add, name) = resolve_payload(payload).await?;
        let opts = AddTorrentOptions {
            list_only: true,
            ..AddTorrentOptions::default()
        };
        match self.session.add_torrent(add, Some(opts)).await? {
            AddTorrentResponse::ListOnly(list) => {
                let files: Vec<TorrentPreviewFile> = list
                    .info
                    .iter_file_details()
                    .map(|d| TorrentPreviewFile {
                        name: d.filename.to_string(),
                        size: d.len,
                    })
                    .collect();
                let total = files.iter().map(|f| f.size).sum();
                Ok(TorrentPreviewSnapshot {
                    info_hash: list.info_hash.as_string(),
                    name,
                    files,
                    total,
                })
            }
            _ => Err(Error::State(
                "list-only torrent add unexpectedly managed a torrent".into(),
            )),
        }
    }
}

/// Full run loop for one task: ensure -> sample stats every second ->
/// cancel-aware pause. Mirrors `engine::download`'s contract exactly.
///
/// `on_meta` fires once right after the torrent is managed with its
/// identity (info-hash, display name, artifact root) so callers can record
/// it into the task record while the download is still running.
pub async fn download(
    session: &TorrentSession,
    cfg: &TorrentConfig,
    token: &CancellationToken,
    mut on_progress: impl FnMut(Progress) + Send,
    mut on_meta: impl FnMut(TorrentMeta) + Send,
) -> Result<TorrentOutput> {
    if token.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let ensured = session.ensure(cfg).await?;
    on_meta(TorrentMeta {
        info_hash: ensured.handle.info_hash().as_string(),
        name: ensured.handle.name(),
        root: ensured.root.clone(),
    });

    // Sequential mode: keep a drains task alive for the whole run; it is
    // aborted wherever this loop exits (finish, cancel, error).
    let _drain = if cfg.sequential {
        Some(AbortOnDrop(tokio::spawn(sequential_drain(
            ensured.handle.clone(),
            token.clone(),
        ))))
    } else {
        None
    };

    loop {
        if token.is_cancelled() {
            // piece state survives on disk; unpause will resume cheaply
            let _ = session.pause(&ensured.handle).await;
            return Err(Error::Cancelled);
        }
        let stats = ensured.handle.stats();
        let phase = classify(&stats);
        if let TorrentPhaseKind::Failed(e) = &phase {
            return Err(Error::State(e.clone()));
        }
        if stats.finished {
            emit(&cfg.id, &stats, &mut on_progress);
            return Ok(TorrentOutput {
                root: ensured.root,
                info_hash: ensured.handle.info_hash().as_string(),
            });
        }
        emit(&cfg.id, &stats, &mut on_progress);
        // While only metadata fetching is happening (no bytes yet) poll a
        // bit faster so progress emits regularly but not aggressively.
        let tick = if matches!(phase, TorrentPhaseKind::Fetching) { 500 } else { 1000 };
        tokio::time::sleep(Duration::from_millis(tick)).await;
    }
}

enum TorrentPhaseKind {
    Fetching,
    Active,
    Failed(String),
}

/// Aborts the sequential drain task whenever the download loop exits - the
/// drain has no natural end while files are still wanted and would linger.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Sequential download: walk the selected files in metadata order, one
/// open librqbit stream at a time, reading through to EOF. The open stream
/// registers the read position as the picker's priority, so pieces arrive
/// front-to-back instead of rarest-first - the file head becomes playable
/// long before the whole task completes. The FileStream read itself blocks
/// on piece availability (waker fires when a piece lands), so the drain
/// self-throttles to the actual download speed and never re-downloads:
/// pieces already on disk are plain disk reads.
async fn sequential_drain(handle: Arc<ManagedTorrent>, token: CancellationToken) {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 256 * 1024];
    'outer: loop {
        if token.is_cancelled() {
            return;
        }
        // Metadata first: magnets spend their opening stretch without it.
        let ids = match handle.with_metadata(|meta| {
            let n = meta.file_infos.len();
            match handle.only_files() {
                Some(wanted) => wanted,
                None => (0..n).collect(),
            }
        }) {
            Ok(ids) => ids,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        for id in ids {
            if token.is_cancelled() {
                return;
            }
            let stream = match handle.clone().stream(id).await {
                Ok(s) => s,
                Err(_) => {
                    // Torrent dropped from the session (remove/stop) - the
                    // caller's loop will exit on its own; stop draining.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue 'outer;
                }
            };
            // Pin (rerouted reads need it; also sidesteps any Unpin
            // requirement since librqbit doesn't export the type).
            tokio::pin!(stream);
            loop {
                if token.is_cancelled() {
                    return;
                }
                tokio::select! {
                    _ = token.cancelled() => {
                        return; // FileStream:Drop removes the stream's priority
                    }
                    read = stream.read(&mut buf) => match read {
                        Ok(0) => break, // EOF: next file
                        Ok(_) => {}
                        Err(_) => break, // stream error: retry the walk
                    },
                }
            }
        }
    }
}

fn classify(st: &TorrentStats) -> TorrentPhaseKind {
    match st.state {
        TorrentStatsState::Error => {
            TorrentPhaseKind::Failed(st.error.clone().unwrap_or_else(|| "torrent failed".into()))
        }
        TorrentStatsState::Initializing { .. } if st.progress_bytes == 0 && !st.finished => {
            // No metadata yet: magnet jobs spend time here. `Initializing`
            // is also readiness-checking when bytes exist - approximated by
            // looking at whether we have any identifying name.
            match handle_name_guess(st) {
                true => TorrentPhaseKind::Active,
                false => TorrentPhaseKind::Fetching,
            }
        }
        _ => TorrentPhaseKind::Active,
    }
}

/// A stats sample that needs the torrent name can't have it from here;
/// rqbit exposes it on the handle - approximation: metadata presence is
/// signaled by total_bytes > 0 for magnets (none until info arrives).
fn handle_name_guess(st: &TorrentStats) -> bool {
    st.total_bytes > 0
}

fn emit(id: &str, st: &TorrentStats, on_progress: &mut impl FnMut(Progress)) {
    let speed = st.live.as_ref().map(|l| l.download_speed.as_bytes() as f64);
    let upload = st.live.as_ref().map(|l| l.upload_speed.as_bytes() as f64);
    // percent left to None: bytes + total drive the bar, and the daemon's
    // percent branch would otherwise clear its derived speed.
    on_progress(Progress {
        id: id.to_string(),
        downloaded: st.progress_bytes,
        total: Some(st.total_bytes),
        percent: None,
        speed_bps: speed,
        uploaded: st.uploaded_bytes,
        upload_bps: upload,
        // A finished torrent is already being served while the loop emits
        // its final sample; the daemon's seeding watcher takes over after.
        seeding: st.finished,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- offline integration: local seeder <-> our download loop ----------
    //
    // Creates a real .torrent, seeds it through an in-process rqbit session
    // and leeches it with `download()` on a second session - loopback only,
    // no public network. Proves ensure/sample/finish/pause plumbing.

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn loopback_seed_leech_roundtrip() {
        use librqbit::spawn_utils::BlockingSpawner;
        use librqbit::CreateTorrentOptions;

        let tmp = std::env::temp_dir().join(format!(
            "ripdl-tortest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Payload: 2 MiB patterned so corruption is detectable.
        let data_path = tmp.join("payload.bin");
        let mut data = Vec::with_capacity(2 * 1024 * 1024);
        let count = (2 * 1024 * 1024) / 8;
        for i in 0..count {
            data.extend_from_slice(&(i as u64).wrapping_mul(0x0123_4567_89ab_cdef).to_le_bytes());
        }
        std::fs::write(&data_path, &data).unwrap();

        // Metainfo.
        let spawner = BlockingSpawner::new(2);
        let tor = librqbit::create_torrent(
            &data_path,
            CreateTorrentOptions {
                name: Some("payload.bin".into()),
                trackers: Vec::new(),
                piece_length: Some(256 * 1024),
            },
            &spawner,
        )
        .await
        .expect("create torrent");
        let bytes: Vec<u8> = tor.as_bytes().expect("serialize").to_vec();
        let torfile = tmp.join("payload.bin.torrent");
        std::fs::write(&torfile, &bytes).unwrap();

        // Seeder (raw rqbit session; ripdl's own torrent engine is the
        // client under test - not the seeder). Same explicit-port hygiene
        // as the real engine to keep two sessions out of each other's way.
        let seed_opts = librqbit::SessionOptions {
            dht: Some(librqbit::DhtSessionConfig {
                bootstrap_addrs: None,
                port: Some(0),
                persistence: None,
            }),
            listen: Some(librqbit::ListenerOptions {
                listen_addr: "[::]:0".parse().unwrap(),
                ..librqbit::ListenerOptions::default()
            }),
            ..librqbit::SessionOptions::default()
        };
        let seed_session = Session::new_with_opts(tmp.join("seed-default"), seed_opts)
            .await
            .unwrap();
        seed_session
            .create_and_serve_torrent(
                &data_path,
                CreateTorrentOptions {
                    name: Some("payload.bin".into()),
                    trackers: Vec::new(),
                    piece_length: Some(256 * 1024),
                },
            )
            .await
            .expect("seed");

        // Leech: the real ripdl path (ensure -> sample -> finished), with a
        // bonded source-IP list of just the loopback IP — every outgoing
        // peer connection then binds to 127.0.0.1 before connecting, which
        // exercises the vendored librqbit bind_ips plumbing end-to-end.
        let leech = TorrentSession::new(
            tmp.join("leech-default"),
            Some(vec!["127.0.0.1".parse::<IpAddr>().unwrap()]),
        )
        .await
        .unwrap();
        let dest = tmp.join("leech");
        let cfg = TorrentConfig {
            id: "tor-1".into(),
            payload: torfile.to_string_lossy().into_owned(),
            dest_dir: dest.clone(),
            only_files: None,
            // Sequential mode on: exercises the streaming drain's piece
            // priority alongside the normal sampler loop.
            sequential: true,
        };
        let token = CancellationToken::new();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let seen2 = seen.clone();
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            crate::torrent::download(&leech, &cfg, &token, move |p| {
                use std::sync::atomic::Ordering::Relaxed;
                seen2.store(p.downloaded, Relaxed);
            }, |meta| {
                assert_eq!(meta.root.file_name().and_then(|n| n.to_str()), Some("payload.bin"));
                assert_eq!(meta.info_hash.len(), 40, "hex info-hash");
            }),
        )
        .await
        .expect("download finished in time")
        .expect("download ok");

        // The output root holds the file with identical contents.
        let produced = out.root;
        assert!(produced.is_file() || produced.is_dir(), "root exists: {produced:?}");
        let got = if produced.is_file() {
            std::fs::read(&produced).unwrap()
        } else {
            std::fs::read(produced.join("payload.bin")).unwrap()
        };
        assert_eq!(got, data, "payload round-tripped identically");
        assert!(
            seen.load(std::sync::atomic::Ordering::Relaxed) >= data.len() as u64,
            "progress reported live bytes"
        );

        // After completion the handle must stay in the session: this is what
        // powers post-download Files/Peers details and seeding after a
        // daemon restart (boot restore re-adds it).
        let stats = leech
            .seed_stats(&out.info_hash)
            .expect("finished torrent stays managed");
        assert!(stats.finished, "leech is finished and seedable");
        assert!(!stats.paused, "finished torrents are not auto-paused");
        let details = leech.details(&out.info_hash).expect("details survive completion");
        assert_eq!(details.total, data.len() as u64);
        assert_eq!(details.files.len(), 1, "file list still available");
        assert!(details.seeding, "details report the seeding phase");

        seed_session.stop().await;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Pre-add preview: a local .torrent resolves offline (no swarm), with
    // the full file list and never touching the managed db.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_local_torrent_offline() {
        use librqbit::spawn_utils::BlockingSpawner;
        use librqbit::CreateTorrentOptions;

        let tmp = std::env::temp_dir().join(format!(
            "ripdl-prevtest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Two files inside one folder = multi-file metainfo.
        let d = tmp.join("dir");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.bin"), vec![1u8; 1_000_000]).unwrap();
        std::fs::write(d.join("b.bin"), vec![2u8; 500_000]).unwrap();

        let spawner = BlockingSpawner::new(2);
        let tor = librqbit::create_torrent(
            &d,
            CreateTorrentOptions {
                name: Some("dir".into()),
                trackers: Vec::new(),
                piece_length: Some(16 * 1024),
            },
            &spawner,
        )
        .await
        .expect("create torrent");
        let bytes: Vec<u8> = tor.as_bytes().expect("serialize").to_vec();
        let torfile = tmp.join("dir.torrent");
        std::fs::write(&torfile, &bytes).unwrap();

        let session = TorrentSession::new(tmp.join("unused"), None).await.unwrap();
        let pv = session
            .preview(torfile.to_str().unwrap())
            .await
            .expect("preview");
        assert_eq!(pv.files.len(), 2);
        assert_eq!(pv.total, 1_500_000);
        assert_eq!(pv.info_hash.len(), 40);
        assert!(pv.name.as_deref() == Some("dir"), "name: {:?}", pv.name);
        let names: Vec<String> = pv
            .files
            .iter()
            .map(|f| f.name.replace('\\', "/"))
            .collect();
        assert!(
            names.iter().any(|n| n.contains("a.bin")) && names.iter().any(|n| n.contains("b.bin")),
            "file paths: {names:?}"
        );

        // Preview must not have managed anything.
        assert!(
            session.list_torrents().is_empty(),
            "list-only add leaked a managed torrent"
        );

        // TorrentSession has no explicit stop; drop is fine for a test.
        drop(session);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    // Pre-add selection: only_files=[1] must keep file 0 empty on disk
    // (zero bytes) while file 1 round-trips fully. Loopback-only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn loopback_only_files_restriction() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info,librqbit=debug"))
            .with_test_writer()
            .try_init()
            .ok();
        use librqbit::spawn_utils::BlockingSpawner;
        use librqbit::CreateTorrentOptions;

        let tmp = std::env::temp_dir().join(format!(
            "ripdl-onlyfiles-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Two 512 KiB patterned files. Patterns (not constant fills!) make
        // the info-hash unique per run: a constant fill would put the same
        // fixture on the public DHT for every CI machine ever, and foreign
        // peers for that fixture interfere with the loopback exchange.
        let d = tmp.join("fpair");
        std::fs::create_dir_all(&d).unwrap();
        let pattern = |fill: u8, seed: u8| {
            let mut v = Vec::with_capacity(512 * 1024);
            for i in 0..512 * 1024 {
                v.push(fill ^ (seed.wrapping_add((i * 31 % 251) as u8)));
            }
            v
        };
        let data0 = pattern(0xAA, 7);
        let data1 = pattern(0xBB, 13);
        std::fs::write(d.join("f0.bin"), &data0).unwrap();
        std::fs::write(d.join("f1.bin"), &data1).unwrap();

        let spawner = BlockingSpawner::new(2);
        let tor = librqbit::create_torrent(
            &d,
            CreateTorrentOptions {
                name: Some("fpair".into()),
                trackers: Vec::new(),
                piece_length: Some(64 * 1024),
            },
            &spawner,
        )
        .await
        .expect("create torrent");
        let bytes: Vec<u8> = tor.as_bytes().expect("serialize").to_vec();
        let torfile = tmp.join("fpair.torrent");
        std::fs::write(&torfile, &bytes).unwrap();

        // File order in the metainfo follows the filesystem's directory
        // order (create_torrent doesn't sort), which differs between
        // Windows and Linux — resolve the index of f1.bin from the
        // metainfo instead of assuming it.
        let parsed = librqbit::torrent_from_bytes(&bytes).expect("parse metainfo");
        let want = parsed
            .info
            .data
            .validate()
            .expect("validate metainfo")
            .iter_file_details()
            .enumerate()
            .find(|(_, d)| d.filename.to_string().ends_with("f1.bin"))
            .map(|(i, _)| i)
            .expect("f1.bin present in metainfo");

        // Seeder as a plain rqbit session (exact loopback hygiene like the
        // roundtrip test below).
        let seed_opts = librqbit::SessionOptions {
            dht: Some(librqbit::DhtSessionConfig {
                bootstrap_addrs: None,
                port: Some(0),
                persistence: None,
            }),
            listen: Some(librqbit::ListenerOptions {
                listen_addr: "[::]:0".parse().unwrap(),
                ..librqbit::ListenerOptions::default()
            }),
            ..librqbit::SessionOptions::default()
        };
        let seed_session = Session::new_with_opts(tmp.join("seed-default"), seed_opts).await.unwrap();
        seed_session
            .create_and_serve_torrent(
                &d,
                CreateTorrentOptions {
                    name: Some("fpair".into()),
                    trackers: Vec::new(),
                    piece_length: Some(64 * 1024),
                },
            )
            .await
            .expect("seed");

        // Leech with only file 1 selected: file 0 must stay zero-filled.
        let leech = TorrentSession::new(
            tmp.join("leech-default"),
            Some(vec!["127.0.0.1".parse::<IpAddr>().unwrap()]),
        )
        .await
        .unwrap();
        let dest = tmp.join("leech");
        let cfg = TorrentConfig {
            id: "only-1".into(),
            payload: torfile.to_string_lossy().into_owned(),
            dest_dir: dest.clone(),
            only_files: Some(vec![want]),
            sequential: false,
        };
        let token = CancellationToken::new();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let seen2 = seen.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(90),
            crate::torrent::download(&leech, &cfg, &token, move |p| {
                use std::sync::atomic::Ordering::Relaxed;
                seen2.store(p.downloaded, Relaxed);
            }, |_| {}),
        )
        .await
        .expect("download finished in time")
        .expect("download ok");

        let root = dest.join("fpair");
        assert_eq!(std::fs::read(root.join("f1.bin")).unwrap(), data1, "selected file round-trips");
        // Unselected file: rqbit doesn't even materialize it — absent or
        // empty is success, anything non-zero means the picker leaked.
        let f0 = std::fs::read(root.join("f0.bin")).unwrap_or_default();
        assert!(
            f0.iter().all(|b| *b == 0),
            "unselected file must stay untouched, nonzero bytes: {}",
            f0.iter().filter(|b| **b != 0).count()
        );
        std::fs::remove_dir_all(&tmp).unwrap();
        let _ = (f0, seen, seed_session);
    }

    #[test]
    fn magnet_info_hash_hex_and_base32() {
        assert_eq!(
            magnet_info_hash(
                "magnet:?xt=urn:btih:08ADA5A7A6183AAE1E09D831DF6748D566095A10&dn=Sintel"
            )
            .as_deref(),
            Some("08ada5a7a6183aae1e09d831df6748d566095a10")
        );
        // Same hash, rfct base32 encoding (Sintel's, via standard
        // RFC 4648 MSB-first chunking).
        let got = magnet_info_hash("magnet:?xt=urn:btih:BCW2LJ5GDA5K4HQJ3AY56Z2I2VTASWQQ")
            .expect("base32 variant must decode");
        assert_eq!(got.len(), 40, "len {}", got.len());
        assert_eq!(
            got.as_bytes(),
            "08ada5a7a6183aae1e09d831df6748d566095a10".as_bytes(),
            "base32 decode bytes: {:?}",
            got.as_bytes()
        );
        // dn-only / other-param variants must not trip the xt search.
        assert_eq!(
            magnet_info_hash("magnet:?dn=x&xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10")
                .as_deref(),
            Some("08ada5a7a6183aae1e09d831df6748d566095a10")
        );
        assert_eq!(magnet_info_hash("magnet:?dn=only-dn"), None);
        assert_eq!(magnet_info_hash("https://x/f.zip"), None);
    }

    #[test]
    fn magnet_payloads_detected() {
        assert!(looks_like_torrent_payload(
            "magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862"
        ));
        assert!(looks_like_torrent_payload("  magnet:?xt=urn:btih:X"));
        assert!(!looks_like_torrent_payload(
            "https://example.com/film.mp4"
        ));
    }

    #[test]
    fn torrent_urls_detected() {
        assert!(looks_like_torrent_payload(
            "https://example.com/download/ubuntu.torrent"
        ));
        assert!(looks_like_torrent_payload(
            "https://example.com/download/ubuntu.torrent?token=x"
        ));
        assert!(!looks_like_torrent_payload("https://example.com/file.ZIP"));
    }

    #[test]
    fn local_paths_by_extension_or_existence() {
        // `.torrent` extension wins even for paths not on disk (string
        // heuristic by design - no fs reads during routing).
        assert!(looks_like_torrent_payload(
            "C:\\definitely\\not\\a\\real\\file.torrent"
        ));
        // A plain local path that exists is a torrent; one that doesn't
        // exist and has no torrent extension stays HTTP.
        let tmp = std::env::temp_dir().join("ripdl-payload-exists.bin");
        std::fs::write(&tmp, b"hi").unwrap();
        assert!(looks_like_torrent_payload(tmp.to_str().unwrap()));
        std::fs::remove_file(&tmp).unwrap();
        assert!(!looks_like_torrent_payload(
            "C:\\definitely\\not\\a\\real\\file.zip"
        ));
    }

    #[test]
    fn bencode_sniff() {
        assert!(is_torrent_file_bytes(b"d4:infod6:lengthi3eeyee"));
        assert!(!is_torrent_file_bytes(b"PK\x03\x04"));
        assert!(is_torrent_file_bytes(b"d10:created-by3:xxx")); // binary-safe
        assert!(!is_torrent_file_bytes(b""));
    }

    #[tokio::test]
    async fn resolve_routes_payloads() {
        let m = resolve_payload("magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862&dn=Sintel")
            .await
            .unwrap();
        assert_eq!(m.1.as_deref(), Some("Sintel"));
        let not_torrent = resolve_payload("D:\\no\\such\\file.zip").await;
        assert!(matches!(not_torrent, Err(Error::InvalidUrl(_))));
    }

    #[test]
    fn percent_decode_and_sanitize() {
        assert_eq!(percent_decode("Sintel%20%28movie%29"), "Sintel (movie)");
        assert_eq!(
            sanitize_component("bad//name:with*chars?"),
            "bad__name_with_chars_"
        );
        assert_eq!(sanitize_component("  ..weird..  "), "weird");
    }
}
