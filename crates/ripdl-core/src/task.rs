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
}

impl Progress {
    pub fn percent(&self) -> Option<f64> {
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
