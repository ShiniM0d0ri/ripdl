use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use ripdl_core::rpc::{Command, Reply, Request, Response, TaskInfo, DEFAULT_PORT};
use ripdl_core::task::{Progress, TaskState};
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
    engine: Arc<Engine>,
    port: u16,
    queue_path: PathBuf,
    records: Mutex<Vec<TaskRecord>>,
    progress: Mutex<HashMap<String, ProgressTrack>>,
    runtime: Mutex<HashMap<String, TaskRuntime>>,
    shutdown: tokio::sync::Notify,
    sem: Arc<tokio::sync::Semaphore>,
}

impl Daemon {
    fn new(queue_path: PathBuf) -> Result<Self> {
        let max_concurrent = 3;
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
            engine: Arc::new(Engine::new(EngineConfig::default())?),
            port: DEFAULT_PORT,
            queue_path,
            records: Mutex::new(records),
            progress: Mutex::new(HashMap::new()),
            runtime: Mutex::new(HashMap::new()),
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

    /// Accept loop: newline-delimited JSON request/response over TCP localhost.
    async fn run(self: Arc<Self>) -> Result<()> {
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

    async fn handle_conn(self: Arc<Self>, stream: TcpStream) -> Result<()> {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Request>(&line) {
                Ok(req) => self.dispatch(req),
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

    fn dispatch(self: &Arc<Self>, req: Request) -> Response {
        match req.cmd {
            Command::Add { url, dest_dir } => self.cmd_add(req.id, url, dest_dir),
            Command::List => self.cmd_list(req.id),
            Command::Pause { id } => self.cmd_pause(req.id, &id),
            Command::Resume { id } => self.cmd_resume(req.id, &id),
            Command::Remove { id } => self.cmd_remove(req.id, &id),
            Command::Shutdown => {
                self.shutdown.notify_waiters();
                Response { id: req.id, reply: Reply::Ok }
            }
        }
    }

    fn cmd_add(self: &Arc<Self>, req_id: u64, url: String, dest_dir: Option<PathBuf>) -> Response {
        // Tasks without an explicit destination land in the user's Downloads
        // folder - never in the daemon's working directory.
        let dest_dir = dest_dir.or_else(default_download_dir);
        let id = uuid::Uuid::new_v4().to_string();
        let rec = TaskRecord {
            id: id.clone(),
            url: url.clone(),
            dest_dir,
            state: TaskState::Queued,
            error: None,
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

    fn cmd_list(&self, req_id: u64) -> Response {
        let records = self.records.lock().unwrap().clone();
        let progress = self.progress.lock().unwrap();

        let tasks = records
            .iter()
            .map(|r| {
                let (downloaded, total, speed_bps) = match progress.get(&r.id) {
                    Some(t) => (t.last.downloaded, t.last.total, t.speed),
                    None => (0, None, None),
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

    fn cmd_remove(self: &Arc<Self>, req_id: u64, id: &str) -> Response {
        let token = self.runtime.lock().unwrap().get(id).map(|t| t.token.clone());
        if let Some(t) = token {
            t.cancel();
        }
        {
            let mut records = self.records.lock().unwrap();
            records.retain(|r| r.id != id);
            let snapshot = records.clone();
            drop(records);
            let _ = self.persist(&snapshot);
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
            let result = this
                .engine
                .download(&rec.url, rec.dest_dir.clone(), token, move |p| {
                    on_progress_this.on_progress(&on_progress_id, p)
                })
                .await;

            this.runtime.lock().unwrap().remove(&rec.id);
            match result {
                Ok(_dest) => this.set_state(&rec.id, TaskState::Completed, None),
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

    /// Track per-task progress and derive an EMA-smoothed speed.
    fn on_progress(&self, id: &str, p: Progress) {
        let mut map = self.progress.lock().unwrap();
        let entry = map.entry(id.to_string()).or_insert(ProgressTrack {
            last: p.clone(),
            speed: None,
            at: Instant::now(),
        });
        let now = Instant::now();
        let dt = now.duration_since(entry.at).as_secs_f64().max(0.05);
        let inst = ((p.downloaded as f64 - entry.last.downloaded as f64) / dt).clamp(0.0, 1.0e12);
        entry.speed = Some(match entry.speed {
            Some(s) => 0.7 * inst + 0.3 * s,
            None => inst,
        });
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

async fn real_main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let data_dir = dirs::data_local_dir()
        .context("cannot resolve local data directory")?
        .join("ripdl");
    std::fs::create_dir_all(&data_dir).context("create data directory")?;
    let queue_path = data_dir.join("queue.json");

    let daemon = Arc::new(Daemon::new(queue_path).context("init daemon")?);
    daemon.boot_resume().context("resume queue")?;
    daemon.run().await
}

use ripdl_core::Error;
