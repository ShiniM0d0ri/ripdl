use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{Command, Reply, Request, Response, TaskInfo, DEFAULT_PORT};
use ripdl_core::task::{Progress, TaskKind, TaskState};
use ripdl_core::{atomic_write_json, Engine, EngineConfig};

/// Persisted queue record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskRecord {
    id: String,
    url: String,
    dest_dir: Option<PathBuf>,
    state: TaskState,
    #[serde(default)]
    error: Option<String>,
    /// Extra engine headers (persisted so resume replays them).
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    /// Unix seconds when the task may start (None = immediately).
    #[serde(default)]
    start_at: Option<i64>,
    /// Tool routing (`http` default; `video` = yt-dlp sidecar).
    #[serde(default)]
    kind: TaskKind,
    /// Resolved output file once a tool produced one (completion-time).
    #[serde(default)]
    dest_file: Option<PathBuf>,
    /// Final byte counters, kept so boot-restored records still show a
    /// size/percent in the UI after a daemon restart.
    #[serde(default)]
    downloaded: u64,
    #[serde(default)]
    total: Option<u64>,
    /// Torrent identity + display name, recorded as soon as the engine
    /// manages the torrent (details queries, friendly row titles).
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    torrent_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct QueueFile {
    tasks: Vec<TaskRecord>,
}

struct TaskRuntime {
    token: CancellationToken,
}

#[derive(Debug)]
struct ProgressTrack {
    last: Progress,
    speed: Option<f64>,
    at: Instant,
}

struct Daemon {
    port: u16,
    queue_path: PathBuf,
    config: DaemonConfig,
    records: Mutex<Vec<TaskRecord>>,
    progress: Mutex<HashMap<String, ProgressTrack>>,
    runtime: Mutex<HashMap<String, TaskRuntime>>,
    /// Single torrent session across all torrent tasks; created on first
    /// torrent use (binds DHT/listener only when actually needed). Async
    /// mutex: creation awaits inside the locked section.
    torrents: tokio::sync::Mutex<Option<Arc<ripdl_core::torrent::TorrentSession>>>,
    shutdown: tokio::sync::Notify,
    sem: Arc<tokio::sync::Semaphore>,
}

/// Daemon policy from `<data_dir>/config.json`. Every field is optional so
/// any partial file works: absent fields fall back to built-in defaults.
#[derive(Debug, Default, serde::Deserialize)]
struct DaemonConfig {
    max_concurrent: Option<usize>,
    max_segments: Option<usize>,
    min_segment_size: Option<u64>,
    max_speed_bytes: Option<u64>,
    default_download_dir: Option<PathBuf>,
    /// Where the yt-dlp sidecar lives for video tasks. Bare names resolve
    /// through `PATH`; unset means the literal name `yt-dlp`.
    ytdlp_path: Option<String>,
}

impl DaemonConfig {
    /// Missing file = defaults; unreadable file = warn + defaults (never
    /// rewritten, so a typo doesn't clobber the user's edits).
    fn load(path: &std::path::Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "config file unreadable; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn max_concurrent(&self) -> usize {
        self.max_concurrent.unwrap_or(3).max(1)
    }

    fn max_segments(&self) -> usize {
        self.max_segments.unwrap_or(EngineConfig::default().max_segments)
    }

    fn min_segment_size(&self) -> u64 {
        self.min_segment_size.unwrap_or(EngineConfig::default().min_segment_size)
    }

    /// The sidecar entry point for video tasks: explicit config path or
    /// PATH fallback.
    fn ytdlp_path(&self) -> String {
        self.ytdlp_path
            .clone()
            .unwrap_or_else(|| "yt-dlp".to_string())
    }
}

impl Daemon {
    fn new(queue_path: PathBuf, config: DaemonConfig) -> Result<Self> {
        let max_concurrent = config.max_concurrent();
        let records = match std::fs::read(&queue_path) {
            Ok(bytes) => serde_json::from_slice::<QueueFile>(&bytes)
                .map(|q| q.tasks)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "queue file unreadable; backing it up, starting clean");
                    let _ = std::fs::rename(&queue_path, queue_path.with_extension("bak"));
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };

        Ok(Self {
            port: DEFAULT_PORT,
            queue_path,
            config,
            records: Mutex::new(records),
            progress: Mutex::new(HashMap::new()),
            runtime: Mutex::new(HashMap::new()),
            torrents: tokio::sync::Mutex::new(None),
            shutdown: tokio::sync::Notify::new(),
            sem: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
        })
    }

    /// On startup: anything previously Running/Queued is re-queued. The
    /// engine's sidecar resume makes the restart transparent.
    fn boot_resume(self: &Arc<Self>) -> Result<()> {
        let resume_list = {
            let mut records = self.records.lock().unwrap();
            for r in records.iter_mut() {
                if r.state == TaskState::Running {
                    r.state = TaskState::Queued;
                }
            }
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
            snapshot
                .into_iter()
                .filter(|r| r.state == TaskState::Queued)
                .collect::<Vec<_>>()
        };
        for r in resume_list {
            self.spawn_runner(r);
        }
        Ok(())
    }

    fn persist(&self, records: &[TaskRecord]) -> Result<()> {
        atomic_write_json(&self.queue_path, &QueueFile { tasks: records.to_vec() })?;
        Ok(())
    }

    /// Accept loops: newline-delimited JSON request/response. On Windows the
    /// daemon also serves a named pipe (locally-isolated, port-free); clients
    /// try the pipe first and fall back to this TCP socket.
    async fn run(self: Arc<Self>) -> Result<()> {
        {
            let daemon = Arc::clone(&self);
            // Windows: named pipe. Unix: domain socket. Both feed the same
            // generic connection handler; TCP below is the universal
            // fallback.
            #[cfg(windows)]
            tokio::spawn(daemon.pipe_accept_loop());
            #[cfg(not(windows))]
            tokio::spawn(daemon.unix_accept_loop());
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))
            .await
            .with_context(|| format!("cannot bind 127.0.0.1:{} (is the daemon already running?)", self.port))?;
        tracing::info!(port = self.port, "ripdl daemon listening");

        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _addr)) => {
                        let daemon = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(e) = daemon.handle_conn(stream).await {
                                tracing::debug!(error = %e, "connection ended");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                },
            }
        }

        // Grace period so in-flight replies can flush, then drain.
        tokio::time::sleep(Duration::from_millis(200)).await;
        for (_, t) in self.runtime.lock().unwrap().drain() {
            t.token.cancel();
        }
        let snapshot = self.records.lock().unwrap().clone();
        let _ = self.persist(&snapshot);
        tracing::info!("ripdl daemon stopped");
        Ok(())
    }

    #[cfg(not(windows))]
    /// Unix-socket accept loop: the kernel handles connection establishment.
    /// A stale socket file from a crashed daemon is removed before bind.
    /// Failure is non-fatal: TCP keeps serving.
    async fn unix_accept_loop(self: Arc<Self>) {
        use tokio::net::UnixListener;

        let path = match ripdl_core::transport::unix_socket_path() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "unix socket path unavailable; serving TCP only");
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "unix socket unavailable; serving TCP only");
                return;
            }
        };
        tracing::info!(path = %path.display(), "ripdl daemon listening");
        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _addr)) => {
                        let daemon = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(e) = daemon.handle_conn(stream).await {
                                tracing::debug!(error = %e, "socket connection ended");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                },
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    /// Named-pipe accept loop: one live instance serving, one reserved for
    /// the next client (the tokio server-handle pattern). Failure here is
    /// non-fatal: the TCP listener keeps serving.
    async fn pipe_accept_loop(self: Arc<Self>) {
        use tokio::net::windows::named_pipe::ServerOptions;

        let first = match ServerOptions::new()
            .first_pipe_instance(true)
            .create(ripdl_core::transport::PIPE_NAME)
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "named pipe unavailable; serving TCP only");
                return;
            }
        };
        let mut next = Some(first);
        loop {
            let Some(cur) = next.take() else { return };
            if let Err(e) = cur.connect().await {
                tracing::warn!(error = %e, "pipe accept failed");
                // Recycle an instance so clients don't see a hole.
                next = ServerOptions::new().create(ripdl_core::transport::PIPE_NAME).ok();
                continue;
            }
            // Reserve the next instance BEFORE serving this connection.
            next = ServerOptions::new()
                .create(ripdl_core::transport::PIPE_NAME)
                .ok();
            let daemon = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = daemon.handle_conn(cur).await {
                    tracing::debug!(error = %e, "pipe connection ended");
                }
            });
        }
    }

    async fn handle_conn(self: Arc<Self>, stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin) -> Result<()> {
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(req) => self.dispatch(req).await,
                Err(e) => Response {
                    id: 0,
                    reply: Reply::Error { message: format!("bad request: {e}") },
                },
            };
            let mut out = serde_json::to_string(&resp).context("serialize response")?;
            out.push('\n');
            write.write_all(out.as_bytes()).await?;
        }
        Ok(())
    }

    async fn dispatch(self: &Arc<Self>, req: Request) -> Response {
        match req.cmd {
            Command::Add { url, dest_dir, headers, start_at, kind } => {
                self.cmd_add(req.id, url, dest_dir, headers, start_at, kind)
            }
            Command::List => self.cmd_list(req.id),
            Command::Details { id } => self.cmd_torrent_details(req.id, &id).await,
            Command::Pause { id } => self.cmd_pause(req.id, &id),
            Command::Resume { id } => self.cmd_resume(req.id, &id),
            Command::Remove { id, delete_files } => {
                self.cmd_remove(req.id, &id, delete_files)
            }
            Command::Shutdown => {
                self.shutdown.notify_waiters();
                Response { id: req.id, reply: Reply::Ok }
            }
        }
    }

    fn cmd_add(
        self: &Arc<Self>,
        req_id: u64,
        url: String,
        dest_dir: Option<PathBuf>,
        headers: std::collections::HashMap<String, String>,
        start_at: Option<i64>,
        kind: TaskKind,
    ) -> Response {
        // Tasks without an explicit destination land in the user's Downloads
        // folder - never in the daemon's working directory.
        let dest_dir = dest_dir
            .or_else(|| self.config.default_download_dir.clone())
            .or_else(default_download_dir);
        // Idempotency: the same URL, tool and destination must not
        // double-download into one file (sidecar trample = corrupt output).
        // Re-adding shares the existing task instead.
        {
            let records = self.records.lock().unwrap();
            if let Some(id) = duplicate_task(&records, &url, &dest_dir, kind) {
                return Response {
                    id: req_id,
                    reply: Reply::Added { id },
                };
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let rec = TaskRecord {
            id: id.clone(),
            url: url.clone(),
            dest_dir,
            state: TaskState::Queued,
            error: None,
            headers,
            start_at,
            kind,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
        };
        {
            let mut records = self.records.lock().unwrap();
            records.push(rec.clone());
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        self.spawn_runner(rec);
        Response { id: req_id, reply: Reply::Added { id } }
    }

    /// Torrent details: match the task's recorded info-hash against the
    /// session's managed torrents and snapshot files/peers/trackers.
    async fn cmd_torrent_details(&self, req_id: u64, id: &str) -> Response {
        let reply: Result<Reply, String> = async {
            let record = {
                let records = self.records.lock().unwrap();
                records
                    .iter()
                    .find(|r| r.id == id)
                    .cloned()
                    .ok_or_else(|| "task not found".to_string())?
            };
            if record.kind != TaskKind::Torrent {
                return Err("task is not a torrent".into());
            }
            let session = {
                let guard = self.torrents.lock().await;
                match guard.as_ref() {
                    Some(s) => Arc::clone(s),
                    None => return Err("torrent session not running".into()),
                }
            };
            let hash = record
                .info_hash
                .clone()
                .ok_or_else(|| "torrent not yet resolved (no metadata yet?)".to_string())?;
            session
                .details(&hash)
                .map_err(|e| e.to_string())
                .map(|d| Reply::TorrentDetails { details: d })
        }
        .await;
        let reply = match reply {
            Ok(r) => r,
            Err(message) => Reply::Error { message },
        };
        Response { id: req_id, reply }
    }

    fn cmd_list(&self, req_id: u64) -> Response {
        let records = self.records.lock().unwrap().clone();
        let progress = self.progress.lock().unwrap();

        let tasks = records
            .iter()
            .map(|r| {
                 let (downloaded, total, speed_bps, percent) = match progress.get(&r.id) {
                    Some(t) => (t.last.downloaded, t.last.total, t.speed, t.last.percent),
                    None => (r.downloaded, r.total, None, r.total.map(|_| 100.0)),
                };
                TaskInfo {
                    id: r.id.clone(),
                    url: r.url.clone(),
                    state: r.state,
                    dest_dir: r.dest_dir.clone(),
                    downloaded,
                    total,
                    speed_bps,
                    error: r.error.clone(),
                    start_at: r.start_at,
                    kind: r.kind,
                    percent,
                    dest_file: r.dest_file.clone(),
                    name: r.torrent_name.clone(),
                    info_hash: r.info_hash.clone(),
                }
            })
            .collect();

        Response { id: req_id, reply: Reply::Tasks { tasks } }
    }

    fn cmd_pause(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
        match token {
            Some(t) => {
                t.cancel();
                Response { id: req_id, reply: Reply::Ok }
            }
            None => {
                // Not running: pausing is a no-op success only if already paused.
                let paused = self
                    .records
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.id == id && r.state == TaskState::Paused);
                if paused {
                    Response { id: req_id, reply: Reply::Ok }
                } else {
                    Response {
                        id: req_id,
                        reply: Reply::Error { message: format!("task {id} not found or not running") },
                    }
                }
            }
        }
    }

    fn cmd_resume(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        let resumable = {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) if matches!(r.state, TaskState::Paused | TaskState::Failed) => {
                    r.state = TaskState::Queued;
                    r.error = None;
                    Some(r.clone())
                }
                _ => None,
            }
        };
        match resumable {
            Some(rec) => {
                let snapshot = self.records.lock().unwrap().clone();
                let _ = self.persist(&snapshot);
                self.spawn_runner(rec);
                Response { id: req_id, reply: Reply::Ok }
            }
            None => Response {
                id: req_id,
                reply: Reply::Error {
                    message: format!("task {id} not resumable (missing, running, or completed)"),
                },
            },
        }
    }

    fn cmd_remove(self: &Arc<Self>, req_id: u64, id: &str, delete_files: bool) -> Response {
        let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
        if let Some(t) = token {
            t.cancel();
        }
        let evicted = {
            let mut records = self.records.lock().unwrap();
            let Some(pos) = records.iter().position(|r| r.id == id) else {
                return Response { id: req_id, reply: Reply::Ok };
            };
            let rec = records.remove(pos);
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
            rec
        };
        if delete_files {
            let removed = delete_artifacts(&evicted);
            tracing::info!(id = id, files = removed.len(), "deleted task files");
        }
        self.progress.lock().unwrap().remove(id);
        Response { id: req_id, reply: Reply::Ok }
    }

    /// Spawn the async worker that drives one task from queued to a terminal
    /// state. The engine's cancellation token doubles as the pause switch:
    /// canceling it persists a Paused sidecar, and resume replays it.
    fn spawn_runner(self: &Arc<Self>, rec: TaskRecord) {
        let token = CancellationToken::new();
        self.runtime
            .lock()
            .unwrap()
            .insert(rec.id.clone(), TaskRuntime { token: token.clone() });

        let this = Arc::clone(self);
        tokio::spawn(async move {
            // Scheduled start: hold Queued until the wall clock arrives.
            // Cancel while waiting -> paused, like any queued task.
            match start_delay(&rec.start_at) {
                Some(delay) => {
                    tokio::select! {
                        _ = token.cancelled() => {
                            this.set_state(&rec.id, TaskState::Paused, None);
                            return;
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                None => {}
            }

            // Wait for a concurrency slot; pause/remove while queued bails here.
            let _permit = tokio::select! {
                _ = token.cancelled() => {
                    this.set_state(&rec.id, TaskState::Paused, None);
                    return;
                }
                p = this.sem.clone().acquire_owned() => match p {
                    Ok(p) => p,
                    Err(_) => return,
                },
            };

            // Re-check after acquiring: paused or removed while queued?
            {
                let records = this.records.lock().unwrap();
                match records.iter().find(|r| r.id == rec.id) {
                    Some(r) if r.state == TaskState::Queued => {}
                    _ => return,
                }
            }

            this.set_state(&rec.id, TaskState::Running, None);

            let on_progress_this = Arc::clone(&this);
            let on_progress_id = rec.id.clone();
            let on_progress = move |p| on_progress_this.on_progress(&on_progress_id, p);

            // Route by tool: HTTP gets the multi-segment engine, video gets
            // the yt-dlp sidecar, torrents share one session. All feed the
            // same progress stream and the same pause/cancel semantics; all
            // hand back the produced file/folder (for "open"/"delete").
            let result = match rec.kind {
                TaskKind::Video => {
                    let cfg = ripdl_core::hunt::HuntConfig {
                        id: rec.id.clone(),
                        program: this.config.ytdlp_path(),
                        url: rec.url.clone(),
                        dest_dir: rec.dest_dir.clone().unwrap_or_default(),
                        headers: rec.headers.clone(),
                    };
                    ripdl_core::hunt::download(cfg, &token, on_progress)
                        .await
                        .map(|out| out.file)
                }
                TaskKind::Torrent => {
                    // One session for the whole daemon, created on demand
                    // rooted at the download default (DHT/listener bind once).
                    let session = {
                        let mut guard = this.torrents.lock().await;
                        if guard.is_none() {
                            let root = this
                                .config
                                .default_download_dir
                                .clone()
                                .or_else(default_download_dir)
                                .unwrap_or_else(|| PathBuf::from("."));
                            match ripdl_core::torrent::TorrentSession::new(root).await {
                                Ok(s) => *guard = Some(Arc::new(s)),
                                Err(e) => {
                                    drop(guard);
                                    this.set_state(&rec.id, TaskState::Failed, Some(e.to_string()));
                                    return;
                                }
                            }
                        }
                        Arc::clone(guard.as_ref().unwrap())
                    };
                    let cfg = ripdl_core::torrent::TorrentConfig {
                        id: rec.id.clone(),
                        payload: rec.url.clone(),
                        dest_dir: rec.dest_dir.clone().unwrap_or_else(|| {
                            this.config
                                .default_download_dir
                                .clone()
                                .or_else(default_download_dir)
                                .unwrap_or_else(|| PathBuf::from("."))
                        }),
                        only_files: None,
                    };
                    ripdl_core::torrent::download(&session, &cfg, &token, on_progress, {
                        // Identity lands here the moment the engine manages
                        // the torrent; persist it for details queries and
                        // friendly row titles.
                        let this = Arc::clone(&this);
                        let task_id = rec.id.clone();
                        move |meta| {
                            let snapshot = {
                                let mut records = this.records.lock().unwrap();
                                if let Some(r) = records.iter_mut().find(|r| r.id == task_id) {
                                    r.info_hash = Some(meta.info_hash.clone());
                                    r.torrent_name = meta.name.clone();
                                    // Adopt the root early so delete_files
                                    // works on partial torrents too.
                                    if r.dest_file.is_none() {
                                        r.dest_file = Some(meta.root.clone());
                                    }
                                }
                                records.clone()
                            };
                            let _ = this.persist(&snapshot);
                        }
                    })
                    .await
                    .map(|out| Some(out.root))
                }
                TaskKind::Http => {
                    // Per-task engine: daemon policy (config.json) sets the
                    // defaults; task headers live on the client and always
                    // override. Both the probe and every segment carry them.
                    let engine = {
                        let cfg = EngineConfig {
                            max_segments: this.config.max_segments(),
                            min_segment_size: this.config.min_segment_size(),
                            max_speed: this.config.max_speed_bytes,
                            headers: rec.headers.clone(),
                            ..EngineConfig::default()
                        };
                        match Engine::new(cfg) {
                            Ok(engine) => Arc::new(engine),
                            Err(e) => {
                                this.set_state(&rec.id, TaskState::Failed, Some(e.to_string()));
                                return;
                            }
                        }
                    };
                    engine
                        .download(&rec.url, rec.dest_dir.clone(), token, on_progress)
                        .await
                        .map(Some)
                }
            };

            this.runtime.lock().unwrap().remove(&rec.id);
            match result {
                Ok(file) => this.set_completed(&rec.id, file),
                Err(Error::Cancelled) => this.set_state(&rec.id, TaskState::Paused, None),
                Err(e) => this.set_state(&rec.id, TaskState::Failed, Some(e.to_string())),
            }
        });
    }

    fn set_state(&self, id: &str, state: TaskState, error: Option<String>) {
        {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) => {
                    r.state = state;
                    r.error = error;
                }
                None => return, // removed meanwhile: never resurrect it
            }
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        if state == TaskState::Running {
            // Fresh run: drop stale speed/progress from any previous attempt.
            self.progress.lock().unwrap().remove(id);
        }
    }

    /// Terminal success with the produced file, remembered for the
    /// "open file" / "delete files" flows (and byte counters for the UI).
    /// Size of a path on disk: files measure directly, directories sum
    /// their contents recursively. None for absent paths (torrent roots are
    /// folders, so plain `metadata.len()` would report 0).
    fn disk_size(p: &std::path::Path) -> Option<u64> {
        let meta = std::fs::metadata(p).ok()?;
        if !meta.is_dir() {
            return Some(meta.len());
        }
        let mut total = 0u64;
        let mut stack = vec![p.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                match std::fs::metadata(entry.path()) {
                    Ok(m) if m.is_dir() => stack.push(entry.path()),
                    Ok(m) => total += m.len(),
                    Err(_) => {}
                }
            }
        }
        Some(total)
    }

    fn set_completed(&self, id: &str, file: Option<PathBuf>) {
        {
            let mut records = self.records.lock().unwrap();
            match records.iter_mut().find(|r| r.id == id) {
                Some(r) => {
                    r.state = TaskState::Completed;
                    r.error = None;
                    r.dest_file = file.clone();
                    if let Some(f) = &r.dest_file {
                        // Torrent roots are directories: metadata.len() is 0
                        // there, so sum the tree. Files measure as before.
                        if let Some(n) = Self::disk_size(f) {
                            r.downloaded = n;
                            r.total = Some(n);
                        } else if let Some(t) = self.progress.lock().unwrap().get(id) {
                            r.downloaded = t.last.downloaded;
                            r.total = t.last.total;
                        }
                    }
                }
                None => return, // removed meanwhile: never resurrect it
            }
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
        }
        self.progress.lock().unwrap().remove(id);
    }

    /// Track per-task progress and derive an EMA-smoothed speed.
    fn on_progress(&self, id: &str, p: Progress) {
        let mut map = self.progress.lock().unwrap();
        let entry = map.entry(id.to_string()).or_insert(ProgressTrack {
            last: p.clone(),
            speed: None,
            at: Instant::now(),
        });
        let now = Instant::now();
        match p.percent {
            // Byte-free progress (video sidecar): percent only, no speed.
            Some(_) => entry.speed = None,
            None => {
                let dt = now.duration_since(entry.at).as_secs_f64().max(0.05);
                let inst =
                    ((p.downloaded as f64 - entry.last.downloaded as f64) / dt)
                        .clamp(0.0, 1.0e12);
                entry.speed = Some(match entry.speed {
                    Some(s) => 0.7 * inst + 0.3 * s,
                    None => inst,
                });
            }
        }
        entry.last = p;
        entry.at = now;
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Daemon-added tasks without an explicit `-d` land in the user's Downloads
/// folder, falling back to the home directory - never the daemon's cwd.
fn default_download_dir() -> Option<PathBuf> {
    dirs::download_dir().or_else(dirs::home_dir)
}

/// Disk artifacts owned by a removed task: the known output file and its
/// engine sidecar, plus any sidecar in the destination whose task URL
/// matches (partial/failed runs never resolved a `dest_file`). Returns the
/// paths it deleted; best effort, never fails the removal.
fn delete_artifacts(rec: &TaskRecord) -> Vec<PathBuf> {
    let mut deleted = Vec::new();
    let remove_file = |p: &std::path::Path, out: &mut Vec<PathBuf>| {
        if std::fs::remove_file(p).is_ok() {
            out.push(p.to_path_buf());
        }
    };
    if let Some(f) = &rec.dest_file {
        // Torrent roots are directories (multi-file layout); HTTP/video are
        // plain files. Sidecar logic only applies to files.
        if f.is_dir() {
            if std::fs::remove_dir_all(f).is_ok() {
                deleted.push(f.to_path_buf());
            }
        } else {
            remove_file(f, &mut deleted);
            let mut sidecar = f.as_os_str().to_os_string();
            sidecar.push(".ripdl.json");
            remove_file(std::path::Path::new(&sidecar), &mut deleted);
        }
    }
    if let Some(dir) = &rec.dest_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".ripdl.json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                let parsed = serde_json::from_slice::<ripdl_core::task::TaskStateFile>(&bytes);
                match parsed {
                    Ok(state) if state.url == rec.url => {
                        let stem_len = name.len() - ".ripdl.json".len();
                        let target = dir.join(&name[..stem_len]);
                        remove_file(&target, &mut deleted);
                        remove_file(&entry.path(), &mut deleted);
                    }
                    _ => {}
                }
            }
        }
    }
    deleted
}

/// Idempotency guard: the same URL, tool and destination must not
/// double-download into one file (sidecar trample = corrupt output).
fn duplicate_task(
    records: &[TaskRecord],
    url: &str,
    dest_dir: &Option<PathBuf>,
    kind: TaskKind,
) -> Option<String> {
    records
        .iter()
        .find(|r| r.kind == kind && &r.url == url && &r.dest_dir == dest_dir)
        .map(|r| r.id.clone())
}

/// How long the runner must hold a task Queued before it may start.
fn start_delay(start_at: &Option<i64>) -> Option<std::time::Duration> {
    let start_at = (*start_at)?;
    let now = now_unix();
    if start_at <= now {
        return None; // due now (or scheduled in the past): start immediately
    }
    Some(std::time::Duration::from_secs((start_at - now) as u64))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, url: &str, dest: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            url: url.to_string(),
            dest_dir: dest.map(PathBuf::from),
            state: TaskState::Completed,
            error: None,
            headers: std::collections::HashMap::new(),
            start_at: None,
            kind: TaskKind::Http,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
        }
    }

    #[test]
    fn duplicate_found_for_same_url_dest_and_kind() {
        let records = vec![rec("a", "https://x/f.zip", Some("C:\\dl"))];
        assert_eq!(
            duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Http),
            Some("a".into())
        );
        // Different tool does not collide (a video hunt of the same URL is a
        // separate artifact).
        assert!(duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Video).is_none());
    }

    #[test]
    fn no_duplicate_for_different_dest() {
        let records = vec![rec("a", "https://x/f.zip", Some("C:\\dl"))];
        assert!(duplicate_task(&records, "https://x/f.zip", &Some(PathBuf::from("C:\\other")), TaskKind::Http).is_none());
        assert!(duplicate_task(&records, "https://x/g.zip", &Some(PathBuf::from("C:\\dl")), TaskKind::Http).is_none());
    }

    #[test]
    fn start_delay_windows() {
        let now = now_unix();
        // No schedule -> start now.
        assert_eq!(start_delay(&None), None);
        // Past-due schedule -> start now.
        assert_eq!(start_delay(&Some(now - 100)), None);
        // Future schedule -> wait the remaining gap.
        assert_eq!(start_delay(&Some(now + 90)), Some(std::time::Duration::from_secs(90)));
    }

    #[test]
    fn config_missing_fields_take_defaults() {
        let cfg: DaemonConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.max_concurrent(), 3);
        assert_eq!(cfg.max_segments(), EngineConfig::default().max_segments);
        assert_eq!(cfg.min_segment_size(), EngineConfig::default().min_segment_size);
        assert_eq!(cfg.max_speed_bytes, None);
        assert_eq!(cfg.default_download_dir, None);
    }

    #[test]
    fn config_partial_file_applies_only_present_fields() {
        let cfg: DaemonConfig = serde_json::from_str(r#"{"max_concurrent": 1}"#).unwrap();
        assert_eq!(cfg.max_concurrent(), 1);
        assert_eq!(cfg.max_segments(), EngineConfig::default().max_segments);

        let cfg: DaemonConfig = serde_json::from_str(
            r#"{"max_segments": 16, "max_speed_bytes": 250000,
                "default_download_dir": "D:/dl"}"#,
        )
        .unwrap();
        assert_eq!(cfg.max_concurrent(), 3);
        assert_eq!(cfg.max_segments(), 16);
        assert_eq!(cfg.max_speed_bytes, Some(250_000));
        assert_eq!(cfg.default_download_dir, Some(PathBuf::from("D:/dl")));
    }

    #[test]
    fn config_garbage_is_rejected_not_panicking() {
        assert!(serde_json::from_str::<DaemonConfig>("nope").is_err());
    }

    #[test]
    fn delete_artifacts_removes_output_sidecar_and_partials() {
        let tmp = std::env::temp_dir().join(format!("ripdl-deltest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let out = tmp.join("file.zip");
        let partial = tmp.join("other.zip");
        std::fs::write(&out, b"done").unwrap();
        std::fs::write(&partial, b"half").unwrap();
        std::fs::write(
            tmp.join("file.zip.ripdl.json"),
            r#"{"id":"x","url":"https://x/file.zip","etag":null,"total":null,"state":"paused","segments":[]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("other.zip.ripdl.json"),
            r#"{"id":"y","url":"https://x/other.zip","etag":null,"total":null,"state":"paused","segments":[]}"#,
        )
        .unwrap();
        std::fs::write(tmp.join("unrelated.txt"), b"keep").unwrap();

        let rec = TaskRecord {
            id: "t".into(),
            url: "https://x/file.zip".into(),
            dest_dir: Some(tmp.clone()),
            state: TaskState::Paused,
            error: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Http,
            dest_file: Some(out.clone()),
            downloaded: 4,
            total: Some(4),
            info_hash: None,
            torrent_name: None,
        };
        let deleted = delete_artifacts(&rec);

        assert!(!out.exists(), "output file deleted");
        assert!(!tmp.join("file.zip.ripdl.json").exists(), "its sidecar deleted");
        assert!(partial.exists(), "different-url task files kept");
        assert!(tmp.join("other.zip.ripdl.json").exists(), "different-url sidecar kept");
        assert!(tmp.join("unrelated.txt").exists(), "unrelated files kept");
        assert_eq!(deleted.len(), 2, "out + sidecar only");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_artifacts_is_safe_when_nothing_matches() {
        let tmp = std::env::temp_dir().join(format!("ripdl-delsafe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let rec = TaskRecord {
            id: "t".into(),
            url: "https://x/never.seen".into(),
            dest_dir: Some(tmp.clone()),
            state: TaskState::Failed,
            error: None,
            headers: Default::default(),
            start_at: None,
            kind: TaskKind::Http,
            dest_file: None,
            downloaded: 0,
            total: None,
            info_hash: None,
            torrent_name: None,
        };
        assert!(delete_artifacts(&rec).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

async fn real_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = dirs::data_local_dir()
        .context("cannot resolve local data directory")?
        .join("ripdl");
    std::fs::create_dir_all(&data_dir).context("create data directory")?;
    let queue_path = data_dir.join("queue.json");

    let config = DaemonConfig::load(&data_dir.join("config.json"));
    let daemon = Arc::new(Daemon::new(queue_path, config).context("init daemon")?);
    daemon.boot_resume().context("resume queue")?;
    daemon.run().await
}

use ripdl_core::Error;
