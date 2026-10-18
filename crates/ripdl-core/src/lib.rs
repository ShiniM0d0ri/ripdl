//! ripdl-core: the download engine.
//!
//! Pure library, no UI assumptions. Frontends (CLI, daemon, Tauri GUI, browser
//! native-messaging host) all sit on top of this crate, so new features land
//! once here and every frontend gets them.

pub mod bridge;
pub mod categories;
pub mod engine;
pub mod error;
pub mod hunt;
pub mod queue;
pub mod rpc;
pub mod task;
pub mod torrent;
pub mod transport;
pub mod util;

pub use engine::{Bond, Engine, EngineConfig};
pub use error::{Error, Result};
pub use task::TaskState;
pub use util::atomic_write_json;
