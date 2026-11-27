use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::error::{Error, Result};

fn temp_sibling(path: &Path) -> PathBuf {
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), seq));
    PathBuf::from(name)
}

/// Write raw bytes through a unique temp file + rename, so concurrent writers
/// or crashes mid-write never collide or leave corrupt files behind.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = temp_sibling(path);
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Serialize `value` as pretty JSON and write it through a unique temp file +
/// rename, so crashes or concurrent saves never corrupt the target file.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| Error::State(e.to_string()))?;
    atomic_write(path, &json)
}
