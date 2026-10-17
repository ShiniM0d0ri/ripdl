use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lifecycle of a single download task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// The tool that performs the task: plain HTTP range requests, or the
/// `yt-dlp` sidecar for video/stream pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Http,
    /// Video/stream page handled by the yt-dlp sidecar (hunt mode).
    Video,
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
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            id: String::new(),
            downloaded: 0,
            total: None,
            percent: None,
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
