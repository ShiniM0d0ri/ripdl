//! ripdl-core: the download engine.
//!
//! Pure library, no UI assumptions. Frontends (CLI, daemon, Tauri GUI, browser
//! native-messaging host) all sit on top of this crate, so new features land
//! once here and every frontend gets them.

pub mod categories;
pub mod engine;
pub mod error;
pub mod queue;
pub mod task;

pub use engine::{Engine, EngineConfig};
pub use error::{Error, Result};
pub use task::TaskState;
