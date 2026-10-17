use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("server error: {0}")]
    Server(String),

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

    #[error("state file error: {0}")]
    State(String),

    #[error("hunt error: {0}")]
    Hunt(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
