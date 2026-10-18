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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// GUI preference (the daemon just persists it): check GitHub for a
    /// newer release when the app starts. Default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_check: Option<bool>,
    /// When a startup check finds a newer release AND this is on: download
    /// the SHA256-verified Windows setup.exe and run it unattended
    /// (`/VERYSILENT`); only the UAC prompt shows. Default off — new
    /// releases install on one explicit click unless the user opts in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_auto_install: Option<bool>,
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

    /// Startup update checks on/off (default on).
    pub fn update_check(&self) -> bool {
        self.update_check.unwrap_or(true)
    }

    /// Startup update checks also install automatically (default off).
    pub fn update_auto_install(&self) -> bool {
        self.update_auto_install.unwrap_or(false)
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
        if patch.update_check.is_some() {
            self.update_check = patch.update_check;
        }
        if patch.update_auto_install.is_some() {
            self.update_auto_install = patch.update_auto_install;
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
    },
    /// Details for one task's torrent: files/peers/trackers snapshot.
    /// Errors if the task is not a torrent or the session lost it.
    Details {
        id: String,
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
    Remove {
        id: String,
        /// Also delete the downloaded file(s) from disk (default: keep them).
        #[serde(default)]
        delete_files: bool,
    },
    /// Current daemon-wide settings (config.json contents).
    GetConfig,
    /// Ask GitHub whether a newer release exists than the running build.
    /// One direct api.github.com GET; never automatic, no telemetry.
    CheckUpdate,
    /// Download the latest release's Windows setup.exe (SHA256-verified
    /// against the release's SHA256SUMS.txt) and run it, upgrading in
    /// place. Only reached via an explicit click or the
    /// `update_auto_install` opt-in setting.
    UpdateInstall,
    /// Replace the daemon settings wholesale (omitted fields = default).
    /// Persisted to `<data_dir>/config.json`; `max_concurrent` applies
    /// live, the rest take effect on the next task start.
    SetConfig {
        settings: DaemonSettings,
    },
    Shutdown,
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
    Tasks {
        tasks: Vec<TaskInfo>,
    },
    TorrentDetails {
        details: crate::torrent::TorrentDetailsSnapshot,
    },
    /// Current daemon-wide settings.
    Config {
        settings: DaemonSettings,
    },
    /// Result of a `CheckUpdate` probe.
    Update {
        info: crate::update::UpdateInfo,
    },
    Ok,
    Error {
        message: String,
    },
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
}
