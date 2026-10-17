mod limiter;
mod planner;
pub mod retry;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

pub use planner::PlanInfo;

use crate::error::{Error, Result};
use crate::task::{Progress, SegmentState, TaskState, TaskStateFile};

/// Sentinel end offset for "unknown total size, stream to the end".
const UNBOUNDED: u64 = u64::MAX;

/// Write buffer per segment: syscalls drop ~64x and disks can't stall the
/// network pipeline.
const SEGMENT_BUF: usize = 256 * 1024;

/// Tunables for the download engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Max parallel range segments per download.
    pub max_segments: usize,
    /// Below this size, do not bother splitting into ranges.
    pub min_segment_size: u64,
    pub retry: retry::RetryConfig,
    pub user_agent: String,
    /// Global bytes-per-second cap shared by all segments (None = unlimited).
    pub max_speed: Option<u64>,
    /// Extra request headers sent by probe + every segment (cookies,
    /// referer, auth...). Later entries may replace same-named ones.
    pub headers: std::collections::HashMap<String, String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_segments: 8,
            min_segment_size: 1024 * 1024,
            retry: retry::RetryConfig::default(),
            user_agent: concat!("ripdl/", env!("CARGO_PKG_VERSION")).to_string(),
            max_speed: None,
            headers: std::collections::HashMap::new(),
        }
    }
}

pub struct Engine {
    client: reqwest::Client,
    cfg: EngineConfig,
    limiter: std::sync::Arc<limiter::Limiter>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        // If the user supplies their own User-Agent header, it must win over
        // the built-in one, so only set the default when they don't.
        let has_user_ua = cfg.headers.keys().any(|k| k.to_ascii_lowercase() == "user-agent");
        let mut builder = reqwest::Client::builder();
        if !has_user_ua {
            builder = builder.user_agent(cfg.user_agent.clone());
        }
        let mut default_headers = reqwest::header::HeaderMap::new();
        for (k, v) in &cfg.headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                Error::Io(std::io::Error::other(format!("bad header name '{k}': {e}")))
            })?;
            let value = reqwest::header::HeaderValue::from_str(v.as_str()).map_err(|e| {
                Error::Io(std::io::Error::other(format!("bad header value for '{k}': {e}")))
            })?;
            default_headers.insert(name, value);
        }
        let client = builder.default_headers(default_headers).build()?;
        let limiter = std::sync::Arc::new(limiter::Limiter::new(cfg.max_speed));
        Ok(Self { client, cfg, limiter })
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
            let f = if resumed {
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
        let finished = Arc::new(AtomicBool::new(false));

        // Spawn one task per segment; each writes at its own file offset.
        let mut handles = Vec::with_capacity(segments.len());
        for seg in segments {
            let client = self.client.clone();
            let url = url.to_string();
            let etag = info.etag.clone();
            let retry_cfg = self.cfg.retry.clone();
            let dest = dest.clone();
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let token = token.clone();
            let active = active.clone();

            handles.push(tokio::spawn({
                let limiter = self.limiter.clone();
                async move {
                    let r = run_segment(
                        client, url, etag, seg, retry_cfg, cells, dest, downloaded, limiter, token,
                    )
                    .await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    r
                }
            }));
        }

        // Task metadata for the sidecar file (kept for the final writes below).
        let meta = TaskStateFile {
            id,
            url: url.to_string(),
            etag: info.etag.clone(),
            total: info.total,
            state: TaskState::Running,
            segments: Vec::new(),
        };

        // Fire-and-forget: this loop exits itself after the final snapshot.
        let _reporter = {
            let cells = cells.clone();
            let downloaded = downloaded.clone();
            let active = active.clone();
            let finished = finished.clone();
            let state_path = state_path.clone();
            let meta_task = meta.clone();
            let mut on_progress = on_progress;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tick.tick().await;
                    let drained = active.load(Ordering::SeqCst) == 0;
                    if drained || finished.load(Ordering::SeqCst) {
                        // Final snapshot with the true final byte count (no
                        // sidecar write here: the caller owns the last state).
                        on_progress(Progress {
                            id: meta_task.id.clone(),
                            downloaded: downloaded.load(Ordering::Relaxed),
                            total: meta_task.total,
                            percent: None,
                        });
                        break;
                    }
                    let segments = cells.lock().expect("state cells").clone();
                    let downloaded_now = downloaded.load(Ordering::Relaxed);
                    let file = TaskStateFile {
                        segments,
                        ..meta_task.clone()
                    };
                    let _ = write_sidecar(&state_path, &file);
                    on_progress(Progress {
                        id: file.id.clone(),
                        downloaded: downloaded_now,
                        total: file.total,
                        percent: None,
                    });
                }
            })
        };

        let results = futures::future::join_all(handles).await;
        // Un-park the reporter so it emits the final snapshot and exits.
        finished.store(true, Ordering::SeqCst);

        // Pick the most meaningful error (real failures over Cancelled).
        let mut first_err: Option<Error> = None;
        for r in results {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let priority = !matches!(e, Error::Cancelled);
                    if first_err.is_none()
                        || (priority && matches!(first_err, Some(Error::Cancelled)))
                    {
                        first_err = Some(e);
                    }
                }
                Err(join_err) => {
                    first_err = Some(Error::Io(std::io::Error::other(join_err.to_string())));
                }
            }
        }

        let persist_paused = |cells: &Arc<Mutex<Vec<SegmentState>>>, meta: &TaskStateFile| {
            let segments = cells.lock().expect("state cells").clone();
            let _ = write_sidecar(
                &state_path,
                &TaskStateFile {
                    segments,
                    state: TaskState::Paused,
                    ..meta.clone()
                },
            );
        };

        if let Some(e) = first_err {
            persist_paused(&cells, &meta);
            return Err(e);
        }

        if let Some(total) = info.total {
            let got = downloaded.load(Ordering::Relaxed);
            if got != total {
                persist_paused(&cells, &meta);
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

/// Fetch one byte range (or the whole body when `end == UNBOUNDED`) with
/// bounded retries and backoff. A retried segment continues from the byte
/// offset already written; if the server cannot serve ranges, the stream
/// restarts from zero.
///
/// TODO(perf): move sync writes into `spawn_blocking` once we profile.
async fn run_segment(
    client: reqwest::Client,
    url: String,
    etag: Option<String>,
    initial: SegmentState,
    retry_cfg: retry::RetryConfig,
    cells: Arc<Mutex<Vec<SegmentState>>>,
    dest: PathBuf,
    downloaded: Arc<AtomicU64>,
    limiter: std::sync::Arc<limiter::Limiter>,
    token: CancellationToken,
) -> Result<()> {
    let mut attempt = 0u32;
    loop {
        // Re-read state: a previous attempt may have advanced it.
        let mut seg = cells.lock().expect("state cells")[initial.index].clone();

        // Server without range support: partial progress is unusable, roll
        // back the counter and restart the stream from zero.
        if seg.end == UNBOUNDED && seg.done > 0 {
            let undone = seg.done;
            downloaded.fetch_sub(undone, Ordering::Relaxed);
            seg.done = 0;
            cells.lock().expect("state cells")[initial.index].done = 0;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&dest)?
                .set_len(0)?;
            tracing::warn!(index = seg.index, "no range support; restarting stream");
        }

        match segment_attempt(
            &client, &url, etag.as_deref(), &seg, &cells, &dest, &downloaded, &limiter, &token,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) if retry::is_retryable(&e) && attempt < retry_cfg.max_retries => {
                attempt += 1;
                let delay = retry_cfg.backoff(attempt);
                tracing::warn!(
                    index = seg.index,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "segment retry"
                );
                tokio::select! {
                    _ = token.cancelled() => return Err(Error::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// One attempt at fetching a segment. Bytes are buffered and written at their
/// absolute file positions via the blocking pool, so slow disks cannot stall
/// the network pipeline; `cells[index].done` advances only for flushed bytes,
/// so a retry (or a crash) resumes from the last durable offset.
#[allow(unused_assignments)] // final-flush stores are intentionally dead
async fn segment_attempt(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    seg: &SegmentState,
    cells: &Arc<Mutex<Vec<SegmentState>>>,
    dest: &Path,
    downloaded: &Arc<AtomicU64>,
    limiter: &limiter::Limiter,
    token: &CancellationToken,
) -> Result<()> {
    let mut file: Option<std::fs::File> = Some(
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(dest)?,
    );

    let mut req = client.get(url);
    if seg.end != UNBOUNDED {
        req = req.header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", seg.start + seg.done, seg.end),
        );
    }
    // Guard against the file changing underneath a resume.
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_MATCH, etag);
    }

    let mut stream = req.send().await?.error_for_status()?.bytes_stream();
    let mut pos = seg.start + seg.done;
    let mut seg_done = seg.done;
    let mut buf: Vec<u8> = Vec::with_capacity(SEGMENT_BUF + 1024);

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                let mut handle = file.take().expect("segment file handle");
                let block = std::mem::take(&mut buf);
                let pos_abs = pos;
                let (handle, block, flushed) = tokio::task::spawn_blocking(move || {
                    use std::io::{Seek, SeekFrom, Write};
                    let mut flushed = 0usize;
                    if handle.seek(SeekFrom::Start(pos_abs)).is_ok()
                        && handle.write_all(&block).is_ok()
                    {
                        flushed = block.len();
                    }
                    (handle, block, flushed)
                })
                .await
                .expect("blocking pool join");

                if flushed != block.len() {
                    return Err(std::io::Error::other("segment file write failed").into());
                }
                pos += flushed as u64;
                seg_done += flushed as u64;
                downloaded.fetch_add(flushed as u64, Ordering::Relaxed);
                cells.lock().expect("state cells")[seg.index].done = seg_done;

                file = Some(handle);
                buf = block;
                buf.clear();
            }
        };
    }

    loop {
        let chunk = tokio::select! {
            _ = token.cancelled() => return Err(Error::Cancelled),
            chunk = stream.next() => match chunk {
                Some(c) => c?,
                None => break,
            },
        };

        // Pace consumption: all segments draw from one shared budget.
        limiter.acquire(chunk.len() as u64).await;

        buf.extend_from_slice(&chunk);
        if buf.len() >= SEGMENT_BUF {
            flush!();
        }
    }
    flush!();

    // Server ended the stream early: count as a retryable transport fault.
    if seg.end != UNBOUNDED && seg_done < seg.end + 1 - seg.start {
        return Err(Error::ShortSegment {
            index: seg.index,
            expected: seg.end + 1 - seg.start,
            got: seg_done,
        });
    }
    Ok(())
}

/// Split the file into at most `max_segments` ranges, each at least
/// `min_segment_size`, unless the server cannot serve ranges.
fn plan_fresh(info: &PlanInfo, cfg: &EngineConfig) -> Vec<SegmentState> {
    match (info.accept_ranges, info.total) {
        (true, Some(total)) if total >= cfg.min_segment_size => {
            let n = ((total / cfg.min_segment_size) as usize)
                .min(cfg.max_segments)
                .max(2);
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

/// Atomic sidecar write: temp file + rename, so a crash mid-write never
/// leaves a corrupt state file behind.
fn write_sidecar(path: &Path, file: &TaskStateFile) -> Result<()> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| Error::State(e.to_string()))?;
    let tmp = path.with_extension("ripdl.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<Option<TaskStateFile>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).map_err(|e| Error::State(e.to_string()))?,
        )),
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
