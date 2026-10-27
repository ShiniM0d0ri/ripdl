use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::task::TaskState;

/// Default daemon RPC port (TCP, localhost only).
pub const DEFAULT_PORT: u16 = 5593;

/// Daemon-wide settings (`<data_dir>/config.json`). Every field is optional
/// so any partial file works: absent fields fall back to built-in defaults.
/// Shared with all frontends: CLI and GUI both send/receive this verbatim;
/// the daemon persists it back unchanged (so frontend-only fields like
/// `hide_to_tray` round-trip too).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaemonSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_segments: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_segment_size: Option<u64>,
    /// Bytes per second shared cap; `None` = uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_speed_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_download_dir: Option<PathBuf>,
    /// Where the yt-dlp sidecar lives for video tasks. Bare names resolve
    /// through `PATH`; unset means the literal name `yt-dlp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ytdlp_path: Option<String>,
    /// GUI preference (the daemon just persists it): closing the window
    /// minimizes to the tray instead of exiting. Default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_to_tray: Option<bool>,
    /// NIC bonding: spread each download's segments across every uplink
    /// interface (Wi-Fi + Ethernet + tethering...), combining their
    /// bandwidth. Default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bond_interfaces: Option<bool>,
    /// Adapter names kept OUT of bonding (case-insensitive exact match),
    /// e.g. a metered phone tether that must never carry download traffic.
    /// An empty list = every working uplink participates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bond_exclude: Option<Vec<String>>,
    /// Check GitHub for updates at GUI startup. Default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_check: Option<bool>,
    /// With `update_check`: install found updates unattended
    /// (`/VERYSILENT`; only the UAC prompt shows). Default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_auto_install: Option<bool>,
    /// Serve finished torrents back to the swarm. Default on (a torrent
    /// client that never uploads is banned by most private trackers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_enabled: Option<bool>,
    /// Stop seeding once uploaded/downloaded reaches this ratio. `0` means
    /// no ratio limit. Default 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_ratio: Option<f64>,
    /// Stop seeding after this many minutes (`0` = unlimited). Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_time_min: Option<u32>,
    /// Accept files from other devices on the local network (the LAN drop
    /// box). Default off: a network-facing listener is an explicit opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_enabled: Option<bool>,
    /// TCP port for the drop box (default 8463).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_port: Option<u16>,
    /// Receive folder (default `<Downloads>/ripdl-inbox`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_dir: Option<PathBuf>,
    /// Trusted-network mode: accept uploads without the pairing code.
    /// Default off (the code is what keeps a cafe Wi-Fi out of the inbox).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_open: Option<bool>,
    /// Pairing code for this device. Generated on first enable and kept so
    /// paired devices keep working across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_code: Option<String>,
    /// Optional path to a manually loaded extension copy. The app-managed
    /// extension under the per-user data directory is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_dir: Option<PathBuf>,
}

impl DaemonSettings {
    /// Missing file = defaults; unreadable file = warn + defaults (never
    /// rewritten, so a typo doesn't clobber the user's edits).
    /// Lives here (not in the daemon) so frontends can read the same file.
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read(path) {
            Ok(mut bytes) => {
                // Editors (Notepad!) save UTF-8 with BOM; serde rejects it and
                // the whole user config would silently reset to defaults.
                if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                    bytes.drain(..3);
                }
                serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    eprintln!("warn: ignoring invalid config {}: {e}", path.display());
                    Self::default()
                })
            }
            Err(_) => Self::default(),
        }
    }

    /// Persist the settings (pretty JSON through `atomic_write_json`).
    pub fn save(&self, path: &std::path::Path) -> crate::error::Result<()> {
        crate::atomic_write_json(path, self)
    }

    /// Queue-wide concurrent-download cap (built-in default 3).
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent.unwrap_or(3).max(1)
    }

    pub fn max_segments(&self) -> usize {
        self.max_segments
            .unwrap_or(crate::engine::EngineConfig::default().max_segments)
    }

    pub fn min_segment_size(&self) -> u64 {
        self.min_segment_size
            .unwrap_or(crate::engine::EngineConfig::default().min_segment_size)
    }

    /// Bytes/second cap; `0` in the file means "unlimited" (a zero
    /// token-bucket rate would stall the download forever).
    pub fn max_speed(&self) -> Option<u64> {
        self.max_speed_bytes.filter(|&b| b > 0)
    }

    /// The sidecar entry point for video tasks: explicit config path or
    /// PATH fallback.
    pub fn ytdlp_path(&self) -> String {
        self.ytdlp_path
            .clone()
            .unwrap_or_else(|| "yt-dlp".to_string())
    }

    /// NIC bonding on/off for new tasks (default off).
    pub fn bond_interfaces(&self) -> bool {
        self.bond_interfaces.unwrap_or(false)
    }

    /// Adapter names kept out of bonding, normalized for an exact
    /// case-insensitive match against discovery's names.
    pub fn bond_excludes(&self) -> Vec<String> {
        self.bond_exclude
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// Startup update checks on/off (default on).
    pub fn update_check(&self) -> bool {
        self.update_check.unwrap_or(true)
    }

    /// Startup update checks also install automatically (default off).
    pub fn update_auto_install(&self) -> bool {
        self.update_auto_install.unwrap_or(false)
    }

    /// Seeding on/off (default on).
    pub fn seed_enabled(&self) -> bool {
        self.seed_enabled.unwrap_or(true)
    }

    /// Stop at this share ratio; `0` disables the ratio limit (default 1.0).
    pub fn seed_ratio(&self) -> f64 {
        self.seed_ratio.unwrap_or(1.0).max(0.0)
    }

    /// Stop after this many minutes; `0` = no time limit (default 0).
    pub fn seed_time_min(&self) -> u32 {
        self.seed_time_min.unwrap_or(0)
    }

    /// LAN receive on/off (default off).
    pub fn lan_enabled(&self) -> bool {
        self.lan_enabled.unwrap_or(false)
    }

    /// Drop-box port (default 8463).
    pub fn lan_port(&self) -> u16 {
        self.lan_port.unwrap_or(crate::lan::DEFAULT_PORT)
    }

    /// Trusted-network mode (no code; default off).
    pub fn lan_open(&self) -> bool {
        self.lan_open.unwrap_or(false)
    }

    /// Apply a partial update: every `Some(x)` in `patch` wins; `None`
    /// leaves the current value untouched. Useful for CLI-style
    /// single-key edits on top of the live values.
    pub fn apply(&mut self, patch: Self) {
        if patch.max_concurrent.is_some() {
            self.max_concurrent = patch.max_concurrent;
        }
        if patch.max_segments.is_some() {
            self.max_segments = patch.max_segments;
        }
        if patch.min_segment_size.is_some() {
            self.min_segment_size = patch.min_segment_size;
        }
        if patch.max_speed_bytes.is_some() {
            self.max_speed_bytes = patch.max_speed_bytes;
        }
        if patch.default_download_dir.is_some() {
            self.default_download_dir = patch.default_download_dir;
        }
        if patch.ytdlp_path.is_some() {
            self.ytdlp_path = patch.ytdlp_path;
        }
        if patch.hide_to_tray.is_some() {
            self.hide_to_tray = patch.hide_to_tray;
        }
        if patch.bond_interfaces.is_some() {
            self.bond_interfaces = patch.bond_interfaces;
        }
        if patch.bond_exclude.is_some() {
            self.bond_exclude = patch.bond_exclude;
        }
        if patch.update_check.is_some() {
            self.update_check = patch.update_check;
        }
        if patch.update_auto_install.is_some() {
            self.update_auto_install = patch.update_auto_install;
        }
        if patch.seed_enabled.is_some() {
            self.seed_enabled = patch.seed_enabled;
        }
        if patch.seed_ratio.is_some() {
            self.seed_ratio = patch.seed_ratio;
        }
        if patch.seed_time_min.is_some() {
            self.seed_time_min = patch.seed_time_min;
        }
        if patch.lan_enabled.is_some() {
            self.lan_enabled = patch.lan_enabled;
        }
        if patch.lan_port.is_some() {
            self.lan_port = patch.lan_port;
        }
        if patch.lan_dir.is_some() {
            self.lan_dir = patch.lan_dir;
        }
        if patch.lan_open.is_some() {
            self.lan_open = patch.lan_open;
        }
        if patch.lan_code.is_some() {
            self.lan_code = patch.lan_code;
        }
    }
}

/// A client request. One JSON object per line on the connection.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub cmd: Command,
}

/// Commands the daemon understands. Frontends (CLI, GUI, extension host)
/// all speak this, so new commands land once here and everywhere benefits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Add {
        url: String,
        #[serde(default)]
        dest_dir: Option<PathBuf>,
        /// Extra request headers for the engine (cookies, referer...).
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
        /// Unix seconds when the task may start (None = immediately).
        /// The runner holds the task Queued until then.
        #[serde(default)]
        start_at: Option<i64>,
        /// Tool routing: `http` (default) or `video` (yt-dlp sidecar).
        #[serde(default)]
        kind: crate::task::TaskKind,
        /// Torrent-only: fetch pieces front-to-back (streaming order).
        #[serde(default)]
        sequential: bool,
        /// Torrent-only: fetch only these file indices (None = everything).
        /// Comes from the Add dialog's pre-add file checklist.
        #[serde(default)]
        only_files: Option<Vec<usize>>,
        /// What happens when this download already exists (same URL, tool and
        /// destination). Default `share`: the existing task's id comes back.
        #[serde(default)]
        on_duplicate: DuplicatePolicy,
    },
    /// Queue several direct links as **one** task that owns a folder
    /// (`<dest>/<name>/<parts...>`), the way a torrent owns its root. The
    /// daemon groups nothing here: the caller decides (usually after
    /// `PreviewBundle`).
    AddBundle {
        /// Folder / display name. Derived from the first part when omitted.
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        dest_dir: Option<PathBuf>,
        parts: Vec<BundlePart>,
        #[serde(default)]
        start_at: Option<i64>,
    },
    /// Group a selection of links **without** queueing anything: which links
    /// belong together, what the folder would be called, and which part
    /// numbers the numbering proves missing.
    PreviewBundle {
        links: Vec<BundlePart>,
    },
    /// Pre-add torrent preview: resolve metadata + file list WITHOUT
    /// queueing anything. May take a while for magnets (swarm metadata
    /// fetch, same as qBittorrent's "Retrieving metadata").
    TorrentPreview {
        payload: String,
    },
    /// Details for one task: torrents get files/peers/trackers
    /// (`Reply::TorrentDetails`), HTTP/video tasks get addresses, the output
    /// file and the byte-range segments (`Reply::TaskDetails`).
    Details {
        id: String,
    },
    /// Point a paused/failed HTTP task at a fresh URL (expired signed link)
    /// while keeping the bytes already on disk. `resume` restarts it right
    /// away. Rejects a new address that serves a different file.
    /// For a bundle task, `part` selects ONE part by its part number (the
    /// seq the filename proves): the whole bundle pauses briefly, that
    /// part's address swaps, and the bundle resumes.
    RefreshUrl {
        id: String,
        url: String,
        #[serde(default)]
        resume: bool,
        #[serde(default)]
        part: Option<u32>,
    },
    /// Append more links to an existing bundle task ("I forgot part 7").
    /// New links must group under the bundle's own name; URLs already in
    /// the bundle are skipped. The bundle resumes afterwards so a
    /// completed task picks the missing part up immediately.
    AddBundleParts {
        id: String,
        parts: Vec<BundlePart>,
    },
    /// Change a torrent task's per-file selection. `None` selects every
    /// file; works live and while paused. The choice is persisted and
    /// survives daemon restarts and resumes.
    UpdateTorrentSelection {
        id: String,
        only_files: Option<Vec<usize>>,
    },
    List,
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    /// Open a task's file in the default app, or (`folder`) reveal it in its
    /// folder. Only the path the daemon recorded for the task is ever used.
    Open {
        id: String,
        #[serde(default)]
        folder: bool,
    },
    Remove {
        id: String,
        /// Also delete the downloaded file(s) from disk (default: keep them).
        #[serde(default)]
        delete_files: bool,
    },
    /// Browser handoff, step 1: probe `url` with the browser's headers and
    /// queue it when the answer matches what the browser saw (`expected_bytes`
    /// when known). `Reply::Proposed` `new` queued it; `duplicate` means the
    /// same download already exists (nothing queued, the existing task comes
    /// back) and the caller asks the user. Both let the browser copy go;
    /// `refused` does not. `proposal` is the caller's id for this decision.
    Propose {
        proposal: String,
        url: String,
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
        #[serde(default)]
        expected_bytes: Option<u64>,
    },
    /// Browser handoff, step 2: the browser kept its copy after all (deadline
    /// passed, or its cancel failed). Drops the task a `new` propose queued.
    Abandon {
        proposal: String,
    },
    /// Current daemon-wide settings (config.json contents).
    GetConfig,
    /// Query GitHub for a newer release.
    CheckUpdate,
    /// SHA256-verified setup download + unattended (or visible) install.
    UpdateInstall,
    /// Replace the daemon settings wholesale (omitted fields = default).
    /// Persisted to `<data_dir>/config.json`; `max_concurrent` applies
    /// live, the rest take effect on the next task start.
    SetConfig {
        settings: DaemonSettings,
    },
    /// Extension service-worker wake heartbeat. Recorded by the daemon for
    /// the Settings browser-install status; no page or browsing data is sent.
    ExtensionHello {
        version: String,
    },
    /// Receiving state: URL, pairing code, inbox path and its contents.
    LanInfo,
    /// Ask the LAN who is receiving (UDP broadcast, ~1 s).
    LanPeers,
    /// Copy one local file to another ripdl (or browser) on this network.
    /// Returns a send id; progress is polled with `LanSends`.
    LanSend {
        url: String,
        #[serde(default)]
        code: String,
        path: PathBuf,
    },
    /// In-flight and recent outgoing LAN copies.
    LanSends,
    /// Forget one finished send entry.
    LanSendForget {
        id: String,
    },
    /// Rotate this device's pairing code (paired devices must re-enter it).
    LanNewCode,
    Shutdown,
}

/// How `Add` treats a download that already exists (same URL, tool and
/// destination directory).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// Answer `added` with the existing task's id (idempotent retries).
    #[default]
    Share,
    /// Queue nothing and answer `duplicate` with the existing task, so the
    /// caller can ask the user what to do.
    Report,
    /// Queue another copy anyway ("download again"). Torrents never copy.
    Copy,
}

/// Verdict on a browser handoff (`Reply::Proposed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOutcome {
    New,
    Duplicate,
    Refused,
}

/// A daemon reply. One JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub reply: Reply,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Added {
        id: String,
    },
    /// Answer to `Propose`: `new` (queued now), `duplicate` (the same download
    /// already exists as `task`; nothing new was queued), or `refused`
    /// (leave it to the browser; `reason` says why).
    Proposed {
        outcome: ProposalOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The existing task, for a `duplicate` verdict.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskInfo>,
    },
    /// Answer to `Add` with `on_duplicate: report`: the same download already
    /// exists and nothing was queued. `task` is its current state.
    Duplicate {
        task: TaskInfo,
    },
    Tasks {
        tasks: Vec<TaskInfo>,
    },
    TorrentDetails {
        details: crate::torrent::TorrentDetailsSnapshot,
    },
    /// Result of a `Details` for a non-torrent task.
    TaskDetails {
        details: TaskDetailsSnapshot,
    },
    /// Result of a `RefreshUrl`: how much on-disk progress the swap kept.
    Refreshed {
        id: String,
        /// Durable bytes already on disk (kept, not re-downloaded).
        #[serde(default)]
        kept: u64,
        #[serde(default)]
        total: Option<u64>,
        /// Bundle-only: the part number that was refreshed.
        #[serde(default)]
        part: Option<u32>,
    },
    /// Result of `AddBundleParts`: how many links actually joined the
    /// bundle (URLs already present are skipped).
    BundlePartsAdded {
        id: String,
        #[serde(default)]
        added: usize,
    },
    /// Result of a `TorrentPreview`.
    TorrentPreview {
        preview: crate::torrent::TorrentPreviewSnapshot,
    },
    /// Result of a `PreviewBundle`: the groups a selection would become.
    BundlePreview {
        groups: Vec<crate::bundle::Group>,
    },
    /// Current daemon-wide settings.
    Config {
        settings: DaemonSettings,
        /// True when the shared torrent session already exists. Its bonding
        /// (`bind_ips`) is fixed when the session is created, so a changed
        /// `bond_interfaces` only takes effect after a daemon restart; this
        /// flag (with `torrent_bind`) lets frontends say so instead of
        /// silently showing a stale config value.
        #[serde(default)]
        torrent_session: bool,
        /// Local addresses the live session actually binds outgoing peer
        /// connections to (empty when the session is not bonded).
        #[serde(default)]
        torrent_bind: Vec<String>,
        /// Measured download rate per bound uplink (address, bytes/s) from the
        /// bytes peer connections actually read. Lets frontends say why an
        /// uplink is being avoided (a dying tether) instead of guessing.
        #[serde(default)]
        torrent_rates: Vec<(String, f64)>,
        /// `"name (addr)"` of interfaces that look like uplinks but failed the
        /// daemon's reachability check, so a configured-but-dead tether is
        /// explained rather than silently ignored.
        #[serde(default)]
        torrent_unreachable: Vec<String>,
        /// The HTTP engine's live bond: per uplink, its measured rate EMA,
        /// current weight, in-use connection count and dial-fail streak. The
        /// daemon feeds it (`set_links` + `sample`) every 5 s and every HTTP
        /// task picks dials from it, so this is the same truth the segments
        /// act on. Empty when bonding is off or no older daemon feeds it.
        #[serde(default)]
        bond_links: Vec<BondLink>,
        /// Capability flag: this daemon understands `AddBundle`/`PreviewBundle`
        /// (multi-part tasks). Frontends check it instead of probing with an
        /// unknown command - an older daemon simply omits the field, which
        /// deserializes as `false` and routes links to the plain-add path.
        #[serde(default)]
        bundles: bool,
        /// The daemon's own version (`CARGO_PKG_VERSION`).
        version: String,
        /// Last browser-extension service-worker heartbeat, persisted by the
        /// daemon. `last_seen == 0` means the extension has never checked in.
        extension: ExtensionStatus,
    },
    /// Result of a `CheckUpdate`.
    Update {
        info: crate::update::UpdateInfo,
    },
    /// Receiver status + inbox listing.
    Lan {
        info: LanInfo,
    },
    /// Receivers discovered on the local network.
    LanPeers {
        peers: Vec<crate::lan::LanPeer>,
    },
    /// Accepted a LAN copy; poll progress with `LanSends`.
    LanSend {
        id: String,
    },
    /// Outgoing LAN copies (newest last).
    LanSends {
        sends: Vec<LanSendInfo>,
    },
    Ok,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStatus {
    pub version: String,
    pub last_seen: i64,
}

/// One live HTTP-bond uplink as the daemon publishes it (a deserializable
/// mirror of `bond::LinkStatus`, whose `IpAddr` RPC convention here is a
/// string like every other address field).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BondLink {
    pub name: String,
    pub ip: String,
    pub default_route: bool,
    /// Smoothed measured rate, bytes/s (idle links keep the last EMA).
    pub rate_bps: u64,
    /// Relative preference of new dials (0 = quarantined).
    pub weight: u32,
    /// Connections currently using this link.
    pub in_use: u32,
    /// Consecutive dial failures (0 = healthy).
    pub fails: u32,
}

/// Receiver status shown by every frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanInfo {
    /// The setting (may be on while `running` is false after a port clash).
    pub enabled: bool,
    pub running: bool,
    /// Base URL other devices should use (empty while not running).
    #[serde(default)]
    pub url: String,
    pub port: u16,
    /// Pairing code for this device (empty when none was generated yet).
    #[serde(default)]
    pub code: String,
    /// Trusted-network mode: no code required.
    #[serde(default)]
    pub open: bool,
    /// Receive folder (created on first enable).
    #[serde(default)]
    pub dir: PathBuf,
    /// Files currently in the receive folder, newest first.
    #[serde(default)]
    pub files: Vec<crate::lan::LanFile>,
}

/// One outgoing LAN copy (volatile: LAN sends are not queue tasks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanSendInfo {
    pub id: String,
    /// File name (updated to the name the receiver stored it under).
    pub name: String,
    /// Receiver base URL.
    pub peer: String,
    pub path: PathBuf,
    pub total: u64,
    pub sent: u64,
    pub done: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Serializable view of one task for frontends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub url: String,
    pub state: TaskState,
    #[serde(default)]
    pub dest_dir: Option<PathBuf>,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub speed_bps: Option<f64>,
    /// Torrent-only: lifetime bytes served to peers.
    #[serde(default)]
    pub uploaded: u64,
    #[serde(default)]
    pub upload_bps: Option<f64>,
    /// Torrent-only: finished and currently serving the swarm.
    #[serde(default)]
    pub seeding: bool,
    /// Torrent-only: seeding was stopped by the user or a limit (don't
    /// auto-resume on the next boot).
    #[serde(default)]
    pub seeding_stopped: bool,
    /// Torrent-only: connected peers (last sample).
    #[serde(default)]
    pub peers: u32,
    /// Torrent-only: hashing on-disk data (resume/recheck) rather than
    /// downloading; speed is suppressed while true.
    #[serde(default)]
    pub verifying: bool,
    /// Running, but no bytes have been reported yet: the engine is still
    /// probing the URL / planning segments. Frontends show "connecting" so a
    /// few seconds of setup does not read as a stalled download.
    #[serde(default)]
    pub probing: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Unix seconds when the task may start (None = immediately).
    #[serde(default)]
    pub start_at: Option<i64>,
    /// Tool routing (`http` default; `video` = yt-dlp sidecar).
    #[serde(default)]
    pub kind: crate::task::TaskKind,
    /// Percent for tasks whose byte totals are unknown (video).
    #[serde(default)]
    pub percent: Option<f64>,
    /// Resolved output file after completion (for open/delete actions).
    #[serde(default)]
    pub dest_file: Option<PathBuf>,
    /// Torrent display name once known (the payload may be a magnet with no
    /// meaningful text; frontends show this instead of the URL).
    #[serde(default)]
    pub name: Option<String>,
    /// Torrent identity (recorded as soon as the engine manages it).
    #[serde(default)]
    pub info_hash: Option<String>,
    /// Torrent-only: pieces arrive in streaming (front-to-back) order.
    #[serde(default)]
    pub sequential: bool,
    /// Torrent-only: current per-file selection (None = everything).
    #[serde(default)]
    pub only_files: Option<Vec<usize>>,
    /// Bundle-only: parts finished and parts the numbering proves exist.
    /// `0` means unknown (a selection that does not start at part 1).
    #[serde(default)]
    pub parts_done: u32,
    #[serde(default)]
    pub parts_total: u32,
    /// Bundle-only: part numbers missing from the set.
    #[serde(default)]
    pub parts_missing: Vec<u32>,
}

/// One part of a bundle.
///
/// Frontends fill `url`/`filename`/`headers` and leave the rest defaulted;
/// the daemon writes the progress fields as the part runs, so the same type
/// travels as a request, lives in `queue.json`, and comes back in details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlePart {
    pub url: String,
    /// Filename the page knows (defaults to the URL tail).
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub state: crate::task::TaskState,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub total: Option<u64>,
    /// Part number the filename proves (None when it carries no numbering).
    #[serde(default)]
    pub seq: Option<u32>,
    #[serde(default)]
    pub error: Option<String>,
    /// A runner has taken this part to its engine at least once. Lets the
    /// `run_bundle` upfront probe wave target exactly the never-run parts:
    /// a reset-to-queued Failed part gets probed by its own runner on resume
    /// (a second probe would double-count on request-counting hosts) while
    /// fresh parts get their sizes before any slot frees up.
    #[serde(default)]
    pub attempted: bool,
}

/// One task's engine view behind the details dialog: where it writes, the
/// resume identity, and the byte-range segment map (HTTP/video tasks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetailsSnapshot {
    pub id: String,
    pub kind: crate::task::TaskKind,
    pub url: String,
    pub state: TaskState,
    /// Destination file (partial runs know it once the URL is probed).
    #[serde(default)]
    pub dest_file: Option<PathBuf>,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub total: Option<u64>,
    /// Tool-level percent for tasks without byte totals (video/yt-dlp).
    #[serde(default)]
    pub percent: Option<f64>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Segments from the resume sidecar (empty when the engine never
    /// planned, when the task finished, or for the yt-dlp sidecar).
    #[serde(default)]
    pub segments: Vec<crate::task::SegmentState>,
    /// Bundle-only: the parts, in part-number order (empty for other kinds).
    #[serde(default)]
    pub parts: Vec<BundlePart>,
    /// Bundle-only: folder that holds the parts (the task's own root).
    #[serde(default)]
    pub bundle_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_commands_parse_from_the_extension() {
        let propose: Request = serde_json::from_str(
            r#"{"id":7,"cmd":{"type":"propose","proposal":"dl-1","url":"https://files.example/a.zip","headers":{"Referer":"https://files.example/"},"expected_bytes":42}}"#,
        )
        .expect("propose parses");
        assert!(matches!(
            propose.cmd,
            Command::Propose {
                expected_bytes: Some(42),
                ..
            }
        ));

        // The extension sends null when the browser did not know the size.
        let no_size: Request = serde_json::from_str(
            r#"{"id":8,"cmd":{"type":"propose","proposal":"dl-2","url":"https://files.example/b.zip","expected_bytes":null}}"#,
        )
        .expect("null-size propose parses");
        assert!(matches!(
            no_size.cmd,
            Command::Propose {
                expected_bytes: None,
                ..
            }
        ));

        let abandon: Request =
            serde_json::from_str(r#"{"id":9,"cmd":{"type":"abandon","proposal":"dl-1"}}"#)
                .expect("abandon parses");
        assert!(matches!(abandon.cmd, Command::Abandon { ref proposal } if proposal == "dl-1"));

        let reply: Reply =
            serde_json::from_str(r#"{"type":"proposed","outcome":"duplicate","id":"t1"}"#)
                .expect("proposed parses");
        assert!(matches!(
            reply,
            Reply::Proposed { outcome: ProposalOutcome::Duplicate, id: Some(ref id), reason: None, task: None } if id == "t1"
        ));
    }

    #[test]
    fn duplicate_policy_defaults_to_share_and_parses() {
        let plain: Request = serde_json::from_str(
            r#"{"id":1,"cmd":{"type":"add","url":"https://files.example/a.zip"}}"#,
        )
        .expect("add parses");
        assert!(matches!(
            plain.cmd,
            Command::Add {
                on_duplicate: DuplicatePolicy::Share,
                ..
            }
        ));

        for (word, want) in [
            ("report", DuplicatePolicy::Report),
            ("copy", DuplicatePolicy::Copy),
        ] {
            let line = format!(
                r#"{{"id":2,"cmd":{{"type":"add","url":"https://files.example/a.zip","on_duplicate":"{word}"}}}}"#
            );
            let req: Request = serde_json::from_str(&line).expect("policy parses");
            assert!(matches!(req.cmd, Command::Add { on_duplicate, .. } if on_duplicate == want));
        }

        let open: Request = serde_json::from_str(r#"{"id":3,"cmd":{"type":"open","id":"t1"}}"#)
            .expect("open parses");
        assert!(matches!(open.cmd, Command::Open { ref id, folder: false } if id == "t1"));
    }

    #[test]
    fn bond_exclude_round_trips_and_normalizes() {
        let mut cfg = DaemonSettings::default();
        // Absent = every uplink participates.
        assert!(cfg.bond_excludes().is_empty());
        cfg.bond_exclude = Some(vec![
            "  Tether ".to_string(),
            "Wi-Fi".to_string(),
            String::new(),
        ]);
        // Trimmed, lowercased, empty names dropped; exact case-insensitive
        // match against discovery's names is what the daemon filters with.
        assert_eq!(cfg.bond_excludes(), vec!["tether", "wi-fi"]);
        // A patch replaces (an explicit empty list clears the exclusion).
        let mut live = cfg.clone();
        live.apply(DaemonSettings {
            bond_exclude: Some(Vec::new()),
            ..DaemonSettings::default()
        });
        assert!(live.bond_excludes().is_empty());
        // Absent on the wire stays out of the JSON (older frontends unaware).
        let json = serde_json::to_string(&DaemonSettings::default()).expect("serialize");
        assert!(!json.contains("bond_exclude"));
    }

    #[test]
    fn config_reply_carries_extension_status() {
        let reply: Reply = serde_json::from_str(
            r#"{"type":"config","settings":{"max_segments":4},"version":"0.4.1","extension":{"version":"0.4.1","last_seen":42}}"#,
        )
        .expect("config reply parses");
        match reply {
            Reply::Config {
                settings,
                torrent_session,
                torrent_bind,
                torrent_rates,
                torrent_unreachable,
                bond_links,
                bundles,
                version,
                extension,
            } => {
                assert_eq!(settings.max_segments, Some(4));
                assert_eq!(version, "0.4.1");
                assert_eq!(extension.version, "0.4.1");
                assert_eq!(extension.last_seen, 42);
                assert!(!torrent_session);
                assert!(torrent_bind.is_empty());
                assert!(torrent_rates.is_empty());
                assert!(torrent_unreachable.is_empty());
                // A pre-bond daemon omits the field entirely and reads empty.
                assert!(bond_links.is_empty());
                // A pre-bundle daemon answers the same way, so the extension
                // reads this as "no bundle support" instead of probing.
                assert!(!bundles);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn config_reply_round_trips_the_live_bond_state() {
        let reply = Reply::Config {
            settings: DaemonSettings::default(),
            torrent_session: true,
            torrent_bind: vec!["10.0.0.2".to_string()],
            torrent_rates: vec![("10.0.0.2".to_string(), 7.0e6)],
            torrent_unreachable: vec!["Ethernet 2 (10.99.0.5)".to_string()],
            bond_links: vec![BondLink {
                name: "Ethernet 2".to_string(),
                ip: "10.99.0.5".to_string(),
                default_route: true,
                rate_bps: 7_340_032,
                weight: 10,
                in_use: 4,
                fails: 0,
            }],
            bundles: true,
            version: "0.4.1".to_string(),
            extension: ExtensionStatus {
                version: "0.4.1".to_string(),
                last_seen: 42,
            },
        };
        let json = serde_json::to_string(&reply).expect("serialize");
        match serde_json::from_str::<Reply>(&json).expect("parse") {
            Reply::Config {
                torrent_session,
                torrent_bind,
                torrent_rates,
                torrent_unreachable,
                bond_links,
                ..
            } => {
                assert!(torrent_session);
                assert_eq!(torrent_bind, vec!["10.0.0.2".to_string()]);
                assert_eq!(torrent_rates, vec![("10.0.0.2".to_string(), 7.0e6)]);
                assert_eq!(
                    torrent_unreachable,
                    vec!["Ethernet 2 (10.99.0.5)".to_string()]
                );
                assert_eq!(
                    bond_links,
                    vec![BondLink {
                        name: "Ethernet 2".to_string(),
                        ip: "10.99.0.5".to_string(),
                        default_route: true,
                        rate_bps: 7_340_032,
                        weight: 10,
                        in_use: 4,
                        fails: 0,
                    }]
                );
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }
}
