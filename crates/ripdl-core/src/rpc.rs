use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::task::TaskState;

/// Default daemon RPC port (TCP, localhost only).
pub const DEFAULT_PORT: u16 = 5593;

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
    },
    /// Details for one task's torrent: files/peers/trackers snapshot.
    /// Errors if the task is not a torrent or the session lost it.
    Details {
        id: String,
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
}
