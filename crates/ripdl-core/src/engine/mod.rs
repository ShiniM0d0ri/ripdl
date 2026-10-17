mod planner;

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

pub use planner::PlanInfo;

use crate::error::{Error, Result};
use crate::task::{Progress, SegmentState, TaskState, TaskStateFile};

/// Sentinel end offset for "unknown total size, stream to the end".
const UNBOUNDED: u64 = u64::MAX;

/// Tunables for the download engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Max parallel range segments per download.
    pub max_segments: usize,
    /// Below this size, do not bother splitting into ranges.
    pub min_segment_size: u64,
    pub user_agent: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_segments: 8,
            min_segment_size: 1024 * 1024,
            user_agent: concat!("ripdl/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

pub struct Engine {
    client: reqwest::Client,
    cfg: EngineConfig,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .build()?;
        Ok(Self { client, cfg })
    }

    /// Probe a URL and return the plan without downloading.
    pub async fn plan(&self, url: &str) -> Result<PlanInfo> {
        planner::probe(&self.client, url).await
    }

    /// Download `url` into `dest_dir` (default: current directory).
    ///
    /// `token` cancels the download. Progress is persisted to a
    /// `.ripdl.json` sidecar so an interrupted task can resume.
    pub async fn download(
        &self,
        url: &str,
        dest_dir: Option<PathBuf>,
        token: CancellationToken,
        on_progress: impl FnMut(Progress) + Send + 'static,
    ) -> Result<PathBuf> {
        let info = self.plan(url).await?;

        let dir = match dest_dir {
            Some(d) => d,
            None => std::env::current_dir()?,
        };
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(&info.filename);
        let state_path = sidecar_path(&dest);

        let id = uuid::Uuid::new_v4().to_string();

        // Resume when a compatible sidecar exists (same url + etag, not done).
        let (segments, resumed) = match read_sidecar(&state_path)? {
            Some(prev)
                if prev.url == url
                    && prev.etag == info.etag
                    && prev.state != TaskState::Completed =>
            {
                (prev.segments, true)
            }
            _ => (plan_fresh(&info, &self.cfg), false),
        };
        tracing::info!(id = %id, resumed, segments = segments.len(), "planned download");

        // Prepare the destination file (truncate only when starting over).
        {
            let mut f = if resumed {
                std::fs::OpenOptions::new().create(true).write(true).open(&dest)?
            } else {
                std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&dest)?
            };
            if let Some(total) = info.total {
                f.set_len(total)?;
            }
        }

        let cells: Arc<Mutex<Vec<SegmentState>>> = Arc::new(Mutex::new(segments.clone()));
        let downloaded = Arc::new(AtomicU64::new(segments.iter().map(|s| s.done).sum()));
        let active = Arc::new(AtomicU64::new(segments.len() as u64));

        // Spawn one task per segment; each writes at its own file offset.
        let mut handles = Vec::with_capacity(segments.len());
        for seg in segments {
            let client = self.client.clone();
            let url = url.to_string();
            let etag = info.etag.clone();
            let dest = dest.clone();
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let token = token.clone();
            let active = active.clone();

            handles.push(tokio::spawn(async move {
                let r = run_segment(client, url, etag, seg, cells, dest, downloaded, token).await;
                active.fetch_sub(1, Ordering::SeqCst);
                r
            }));
        }

        // Reporter: emits progress and flushes the sidecar while running.
        let reporter = {
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let active = active.clone();
            let state_path = state_path.clone();
            let meta = TaskStateFile {
                id,
                url: url.to_string(),
                etag: info.etag.clone(),
                total: info.total,
                state: TaskState::Running,
                segments: Vec::new(),
            };
            let mut on_progress = on_progress;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tick.tick().await;
                    if active.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    let segments = cells.lock().expect("state cells").clone();
                    let downloaded_now = downloaded.load(Ordering::Relaxed);
                    let file = TaskStateFile {
                        segments,
                        ..meta.clone()
                    };
                    let _ = write_sidecar(&state_path, &file);
                    on_progress(Progress {
                        id: file.id.clone(),
                        downloaded: downloaded_now,
                        total: file.total,
                    });
                }
            })
        };

        let results = futures::future::join_all(handles).await;
        reporter.abort();

        // Pick the most meaningful error (real failures over Cancelled).
        let mut first_err: Option<Error> = None;
        for r in results {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let priority = !matches!(e, Error::Cancelled);
                    if first_err.is_none() || (priority && matches!(first_err, Some(Error::Cancelled)))
                    {
                        first_err = Some(e);
                    }
                }
                Err(join_err) => {
                    first_err = Some(Error::Io(std::io::Error::other(join_err.to_string())));
                }
            }
        }

        if let Some(e) = first_err {
            let segments = cells.lock().expect("state cells").clone();
            let _ = write_sidecar(
                &state_path,
                &TaskStateFile {
                    segments,
                    state: TaskState::Paused,
                    ..meta
                },
            );
            return Err(e);
        }

        if let Some(total) = info.total {
            let got = downloaded.load(Ordering::Relaxed);
            if got != total {
                let segments = cells.lock().expect("state cells").clone();
                let _ = write_sidecar(
                    &state_path,
                    &TaskStateFile {
                        segments,
                        state: TaskState::Paused,
                        ..meta
                    },
                );
                return Err(Error::SizeMismatch {
                    expected: total,
                    actual: got,
                });
            }
        }

        let _ = std::fs::remove_file(&state_path);
        Ok(dest)
    }
}

/// Fetch one byte range (or the whole body when `end == UNBOUNDED`).
///
/// TODO(perf): move sync writes into `spawn_blocking` once we profile.
async fn run_segment(
    client: reqwest::Client,
    url: String,
    etag: Option<String>,
    seg: SegmentState,
    cells: Arc<Mutex<Vec<SegmentState>>>,
    dest: PathBuf,
    downloaded: Arc<AtomicU64>,
    token: CancellationToken,
) -> Result<()> {
    let mut file = std::fs::OpenOptions::new().create(true).write(true).open(&dest)?;
    file.seek(SeekFrom::Start(seg.start + seg.done))?;

    let mut req = client.get(&url);
    if seg.end != UNBOUNDED {
        req = req.header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", seg.start + seg.done, seg.end),
        );
    }
    // Guard against the file changing underneath a resume.
    if let Some(etag) = &etag {
        req = req.header(reqwest::header::IF_MATCH, etag);
    }

    let mut stream = req.send().await?.error_for_status()?.bytes_stream();
    let mut seg_done = seg.done;

    while let Some(chunk) = tokio::select! {
        _ = token.cancelled() => return Err(Error::Cancelled),
        chunk = stream.next() => match chunk {
            Some(c) => c?,
            None => break,
        },
    } {
        file.write_all(&chunk)?;
        seg_done += chunk.len() as u64;
        downloaded.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        cells.lock().expect("state cells")[seg.index].done = seg_done;
    }
    file.flush()?;

    cells.lock().expect("state cells")[seg.index].done = seg_done;
    Ok(())
}

/// Split the file into at most `max_segments` ranges, each at least
/// `min_segment_size`, unless the server cannot serve ranges.
fn plan_fresh(info: &PlanInfo, cfg: &EngineConfig) -> Vec<SegmentState> {
    match (info.accept_ranges, info.total) {
        (true, Some(total)) if total >= cfg.min_segment_size => {
            let n = ((total / cfg.min_segment_size) as usize).min(cfg.max_segments).max(2);
            let chunk = total / n as u64;
            (0..n)
                .map(|i| {
                    let start = i as u64 * chunk;
                    let end = if i == n - 1 { total - 1 } else { start + chunk - 1 };
                    SegmentState { index: i, start, end, done: 0 }
                })
                .collect()
        }
        (true, Some(total)) => vec![SegmentState {
            index: 0,
            start: 0,
            end: total - 1,
            done: 0,
        }],
        _ => vec![SegmentState {
            index: 0,
            start: 0,
            end: UNBOUNDED,
            done: 0,
        }],
    }
}

fn sidecar_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".ripdl.json");
    PathBuf::from(s)
}

fn write_sidecar(path: &Path, file: &TaskStateFile) -> Result<()> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| Error::State(e.to_string()))?;
    // TODO: atomic write (temp + rename) before first release.
    std::fs::write(path, json)?;
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<Option<TaskStateFile>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(total: Option<u64>, ranges: bool) -> PlanInfo {
        PlanInfo {
            url: "https://example.com/file.zip".parse().unwrap(),
            total,
            accept_ranges: ranges,
            filename: "file.zip".into(),
            etag: None,
        }
    }

    #[test]
    fn small_file_stays_single_segment() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(1024), true), &cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].end, 1023);
    }

    #[test]
    fn large_file_splits_with_contiguous_ranges() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(10 * 1024 * 1024), true), &cfg);
        assert_eq!(plan.len(), 8);
        assert_eq!(plan[0].start, 0);
        assert_eq!(plan[7].end, 10 * 1024 * 1024 - 1);
        for pair in plan.windows(2) {
            assert_eq!(pair[1].start, pair[0].end + 1);
        }
    }

    #[test]
    fn no_ranges_means_single_stream() {
        let cfg = EngineConfig::default();
        let plan = plan_fresh(&info(Some(1024 * 1024 * 500), false), &cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].end, UNBOUNDED);
    }
}
