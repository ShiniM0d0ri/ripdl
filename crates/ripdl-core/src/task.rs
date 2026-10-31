use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lifecycle of a single download task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    /// The bytes on disk are fine but the address is not (an expired signed
    /// link answers 401/403/404/410). Not `failed`: nothing about the
    /// transfer is broken - the user re-grabs a fresh link and refreshes the
    /// task's address (see `crate::refresh`). Parked like Paused: no runner,
    /// no auto-retry, survives a daemon restart as-is.
    Stale,
}

/// The tool that performs the task: plain HTTP range requests, the
/// `yt-dlp` sidecar for video/stream pages, or the local torrent engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Http,
    /// Video/stream page handled by the yt-dlp sidecar (hunt mode).
    Video,
    /// BitTorrent payload (magnet / .torrent) via the torrent engine.
    Torrent,
    /// Multi-part direct links that own one folder (`<dest>/<name>/`).
    Bundle,
}

/// Persisted progress for one byte-range segment.
///
/// `end` is inclusive and an absolute file offset. The sentinel `u64::MAX`
/// means "no known end" (server does not advertise a size; single stream).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentState {
    pub index: usize,
    /// First byte of the range (inclusive, absolute file offset).
    pub start: u64,
    /// Last byte of the range (inclusive, absolute file offset).
    pub end: u64,
    /// Bytes already written for this segment.
    pub done: u64,
}

/// Sidecar file persisted next to a partial download, enabling resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateFile {
    pub id: String,
    pub url: String,
    pub etag: Option<String>,
    pub total: Option<u64>,
    pub state: TaskState,
    pub segments: Vec<SegmentState>,
}

/// Progress snapshot emitted by the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub id: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// Tool-level completion percent for tasks whose bytes are unknown up
    /// front (video tasks: parsed from yt-dlp's progress line).
    #[serde(default)]
    pub percent: Option<f64>,
    /// Bytes per second, sampled by tools that can estimate it (HTTP
    /// segments and torrent swarms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<f64>,
    /// Torrent-only: lifetime bytes served to peers (this run + restores).
    #[serde(default)]
    pub uploaded: u64,
    /// Torrent-only: current upload rate (bytes/s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_bps: Option<f64>,
    /// Torrent-only: every wanted piece is on disk and the swarm is being
    /// served. The daemon maps this to the "seeding" row label.
    #[serde(default)]
    pub seeding: bool,
    /// Torrent-only: the engine is hashing on-disk data (a resume/recheck)
    /// instead of downloading. `downloaded` then climbs at *disk* speed, so
    /// the daemon must not derive a rate from it; frontends show "verifying".
    #[serde(default)]
    pub verifying: bool,
    /// Output file the tool resolved for this run (the HTTP engine knows it
    /// as soon as the URL is probed). Lets the daemon record partial paths,
    /// so details/open/delete work before the task completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_file: Option<PathBuf>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            id: String::new(),
            downloaded: 0,
            total: None,
            percent: None,
            speed_bps: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            verifying: false,
            dest_file: None,
        }
    }
}

impl Progress {
    pub fn percent(&self) -> Option<f64> {
        if let Some(p) = self.percent {
            return Some(p);
        }
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.downloaded as f64 / t as f64) * 100.0)
    }
}

/// In-memory description of a download task.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub url: String,
    pub dest: PathBuf,
    pub state: TaskState,
}
