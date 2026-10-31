use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("server error: {0}")]
    Server(String),

    /// The server turned the request away with an auth-class status
    /// (401/403/404/410). Structured so the daemon can tell a dead link
    /// (address expired) apart from a broken transfer and park the task
    /// `stale` instead of `failed` - see `retry::is_auth_status`.
    #[error("server returned {0}")]
    HttpStatus(u16),

    /// Upstream library failure (librqbit etc. report via anyhow).
    #[error("{0:#}")]
    Anyhow(#[from] anyhow::Error),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    #[error("download cancelled")]
    Cancelled,

    #[error("downloaded size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("segment {index} ended early: expected {expected} bytes, got {got}")]
    ShortSegment {
        index: usize,
        expected: u64,
        got: u64,
    },

    /// A segment connection went *silent*: alive TCP, zero progress for the
    /// stream-idle watchdog window (a lost source address does exactly this -
    /// every packet dies inside the kernel and the session never terminates).
    /// Retryable: the rollback hands parked bytes back and the redial goes
    /// through whatever link is still alive.
    #[error("stream stalled: no bytes for {0}s")]
    StreamStalled(u64),

    #[error("state file error: {0}")]
    State(String),

    #[error("hunt error: {0}")]
    Hunt(String),

    /// A new download address cannot continue a paused download (different
    /// file, no ranges...). Frontends show the text as-is.
    #[error("refresh failed: {0}")]
    Refresh(String),

    #[error("update check failed: {0}")]
    Update(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
