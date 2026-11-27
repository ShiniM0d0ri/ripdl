//! Refresh download address: point a paused HTTP download at a fresh URL
//! (signed links expire after minutes) while keeping the bytes already on
//! disk - the IDM "refresh download address" flow.
//!
//! The engine only resumes when the sidecar's `url` and `etag` match the next
//! probe, so a refresh rewrites both. Validation is deliberately strict: the
//! saved byte offsets are only meaningful if the new address serves the same
//! file (same size and name) and still supports byte ranges. Anything else
//! would silently splice two different files together.

use std::path::Path;

use crate::engine::{sidecar_path, PlanInfo};
use crate::error::{Error, Result};
use crate::task::{TaskState, TaskStateFile};
use crate::util::atomic_write_json;

/// Persisted end offset of an unbounded (no-range) segment.
const UNBOUNDED: u64 = u64::MAX;

/// Read the resume sidecar for `dest` (`None` when the task never planned).
pub fn load(dest: &Path) -> Result<Option<TaskStateFile>> {
    match std::fs::read(sidecar_path(dest)) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Persist `state` beside `dest` (atomic swap, same layout the engine reads).
/// Re-applies Windows hidden attribute after atomic rename replaces the inode.
pub fn save(dest: &Path, state: &TaskStateFile) -> Result<()> {
    let path = sidecar_path(dest);
    atomic_write_json(&path, state)?;
    crate::engine::hide_on_windows(&path);
    Ok(())
}

/// Bytes already durable on disk that a refresh keeps.
pub fn kept_bytes(state: &TaskStateFile) -> u64 {
    state.segments.iter().map(|s| s.done).sum()
}

/// What a compatible-but-imperfect adopt warns about. The bytes stay; the
/// user just learns what might not line up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshCheck {
    /// None on a clean adopt.
    pub warning: Option<String>,
}

impl RefreshCheck {
    #[allow(dead_code)] // phase 3 (extension matching) consumes the clean adopt
    fn clean() -> Self {
        Self { warning: None }
    }
}

/// Reject a new address that cannot continue the paused download.
///
/// `prev` is the sidecar of the download being retargeted, `info` the probe
/// of the new address, `dest` the partial file on disk. A clean adopt answers
/// `Ok(RefreshCheck::clean())`; an imperfect one (the server advertises a
/// different file version) answers `Ok` with a warning - the bytes are kept,
/// but the user should know.
pub fn check_compatible(
    dest: &Path,
    prev: &TaskStateFile,
    info: &PlanInfo,
) -> Result<RefreshCheck> {
    let old_total = prev
        .total
        .ok_or_else(|| Error::Refresh("the paused download never learned the file size".into()))?;
    let new_total = info
        .total
        .ok_or_else(|| Error::Refresh("the new address does not report a file size".into()))?;
    if old_total != new_total {
        return Err(Error::Refresh(format!(
            "the new address serves {new_total} bytes, but the paused download expects {old_total} - not the same file"
        )));
    }
    if !info.accept_ranges {
        return Err(Error::Refresh(
            "the new address does not support byte ranges, so the download cannot continue where it stopped".into(),
        ));
    }
    if prev.segments.iter().any(|s| s.end == UNBOUNDED) {
        return Err(Error::Refresh(
            "the paused download has no range state (its server did not support ranges), so it cannot continue".into(),
        ));
    }
    let mut spans: Vec<(u64, u64)> = Vec::with_capacity(prev.segments.len());
    for s in &prev.segments {
        if s.start > s.end || s.end >= old_total {
            return Err(Error::Refresh(format!(
                "corrupt resume sidecar: segment {} ({}..={}) falls outside file size {old_total}",
                s.index, s.start, s.end
            )));
        }
        let span = s.end - s.start + 1;
        if s.done > span {
            return Err(Error::Refresh(format!(
                "corrupt resume sidecar: segment {} recorded {} done bytes for a {span}-byte span",
                s.index, s.done
            )));
        }
        spans.push((s.start, s.end));
    }
    spans.sort_unstable();
    for pair in spans.windows(2) {
        if pair[0].1 >= pair[1].0 {
            return Err(Error::Refresh(
                "corrupt resume sidecar: overlapping byte ranges".into(),
            ));
        }
    }
    let kept = kept_bytes(prev);
    if kept > old_total {
        return Err(Error::Refresh(format!(
            "corrupt resume sidecar: kept bytes ({kept}) exceed file size ({old_total})"
        )));
    }
    if let Some(name) = dest.file_name() {
        let want = name.to_string_lossy();
        if !same_name(&want, &info.filename) {
            return Err(Error::Refresh(format!(
                "the new address saves as {:?}, but the paused download is {:?} - point it at the same file name",
                info.filename, want
            )));
        }
    }
    // Different ETag means the server serves a different file revision even if
    // name and size match: never splice bytes across two versions.
    if let (Some(old), Some(new)) = (&prev.etag, &info.etag) {
        if old != new {
            return Err(Error::Refresh(
                "the new address serves a different file version (ETag changed) - start a fresh download instead of keeping mismatched bytes"
                    .into(),
            ));
        }
    }
    Ok(RefreshCheck::clean())
}

/// Point the paused state at a new address: segments and their offsets are
/// untouched, only the identity the engine resumes against changes.
pub fn retarget(state: &mut TaskStateFile, url: &str, etag: Option<String>, total: Option<u64>) {
    state.url = url.to_string();
    state.etag = etag;
    if total.is_some() {
        state.total = total;
    }
    state.state = TaskState::Paused;
}

fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::SegmentState;

    fn info(total: Option<u64>, ranges: bool, name: &str) -> PlanInfo {
        PlanInfo {
            url: "https://cdn.example.com/f.bin?token=new".parse().unwrap(),
            total,
            accept_ranges: ranges,
            filename: name.to_string(),
            etag: Some("\"next\"".to_string()),
        }
    }

    fn state() -> TaskStateFile {
        TaskStateFile {
            id: "t1".into(),
            url: "https://cdn.example.com/f.bin?token=old".into(),
            etag: Some("\"v1\"".into()),
            total: Some(4096),
            state: TaskState::Paused,
            segments: vec![
                SegmentState {
                    index: 0,
                    start: 0,
                    end: 2047,
                    done: 1024,
                },
                SegmentState {
                    index: 1,
                    start: 2048,
                    end: 4095,
                    done: 2048,
                },
            ],
        }
    }

    fn dest(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from("C:/dl").join(name)
    }

    #[test]
    fn refresh_etag_mismatch_rejected() {
        let d = dest("f.bin");
        let err =
            check_compatible(&d, &state(), &info(Some(4096), true, "f.bin")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ETag") || msg.contains("different file version"), "{msg}");
    }

    #[test]
    fn refresh_validates_sidecar_bounds() {
        let d = dest("f.bin");
        let mut prev = state();
        prev.etag = Some("\"next\"".into());

        let mut bad_end = prev.clone();
        bad_end.segments[1].end = 4096;
        assert!(check_compatible(&d, &bad_end, &info(Some(4096), true, "f.bin")).is_err());

        let mut bad_done = prev.clone();
        bad_done.segments[0].done = 2049;
        assert!(check_compatible(&d, &bad_done, &info(Some(4096), true, "f.bin")).is_err());

        let mut overlap = prev.clone();
        overlap.segments[1].start = 2047;
        assert!(check_compatible(&d, &overlap, &info(Some(4096), true, "f.bin")).is_err());
    }

    #[test]
    fn refresh_sidecar_stays_hidden() {
        let dir = std::env::temp_dir().join(format!("ripdl-refresh-hide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.join("f.bin");
        let st = state();
        save(&d, &st).unwrap();
        save(&d, &st).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let attrs = std::fs::metadata(sidecar_path(&d)).unwrap().file_attributes();
            assert_ne!(attrs & 0x2, 0, "refresh sidecar must retain FILE_ATTRIBUTE_HIDDEN");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_etag_adopts_clean() {
        let d = dest("f.bin");
        let mut prev = state();
        prev.etag = Some("\"next\"".into());
        let check =
            check_compatible(&d, &prev, &info(Some(4096), true, "f.bin")).expect("compatible");
        assert_eq!(check.warning, None);
    }

    #[test]
    fn different_size_is_rejected_with_both_numbers() {
        let d = dest("f.bin");
        let err = check_compatible(&d, &state(), &info(Some(8192), true, "f.bin")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("8192") && msg.contains("4096"), "{msg}");
    }

    #[test]
    fn different_file_name_is_rejected() {
        let d = dest("f.bin");
        let err = check_compatible(&d, &state(), &info(Some(4096), true, "other.bin")).unwrap_err();
        assert!(err.to_string().contains("other.bin"), "{err}");
    }

    #[test]
    fn no_range_support_is_rejected() {
        let d = dest("f.bin");
        let err = check_compatible(&d, &state(), &info(Some(4096), false, "f.bin")).unwrap_err();
        assert!(err.to_string().contains("byte ranges"), "{err}");
    }

    #[test]
    fn unbounded_segments_are_rejected() {
        let d = dest("f.bin");
        let mut st = state();
        st.segments = vec![SegmentState {
            index: 0,
            start: 0,
            end: UNBOUNDED,
            done: 512,
        }];
        let err = check_compatible(&d, &st, &info(Some(4096), true, "f.bin")).unwrap_err();
        assert!(err.to_string().contains("range state"), "{err}");
    }

    #[test]
    fn retarget_keeps_progress_and_swaps_identity() {
        let mut st = state();
        let kept = kept_bytes(&st);
        retarget(
            &mut st,
            "https://cdn.example.com/f.bin?token=new",
            Some("\"next\"".into()),
            Some(4096),
        );
        assert_eq!(st.url, "https://cdn.example.com/f.bin?token=new");
        assert_eq!(st.etag.as_deref(), Some("\"next\""));
        assert_eq!(st.state, TaskState::Paused);
        assert_eq!(kept_bytes(&st), kept);
        assert_eq!(kept, 3072);
        assert_eq!(st.segments.len(), 2);
        assert_eq!(st.segments[1].done, 2048);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join("ripdl-refresh-unit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.join("f.bin");
        let mut st = state();
        retarget(
            &mut st,
            "https://cdn.example.com/f.bin?token=new",
            None,
            Some(4096),
        );
        save(&d, &st).unwrap();
        let back = load(&d).unwrap().expect("sidecar saved");
        assert_eq!(back.url, st.url);
        assert_eq!(back.etag, None);
        assert_eq!(kept_bytes(&back), 3072);
        assert!(load(&dir.join("missing.bin")).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
