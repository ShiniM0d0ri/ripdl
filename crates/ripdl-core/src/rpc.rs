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
    Added { id: String },
    Tasks { tasks: Vec<TaskInfo> },
    Ok,
    Error { message: String },
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
}
