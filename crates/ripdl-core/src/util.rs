use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};

/// Serialize `value` as pretty JSON and write it through a temp file +
/// rename, so a crash mid-write never leaves a corrupt file behind.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| Error::State(e.to_string()))?;
    let tmp = path.with_extension("ripdl.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
