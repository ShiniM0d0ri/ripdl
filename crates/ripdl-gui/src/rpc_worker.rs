//! Non-blocking background daemon worker over `ripdl_core::transport` (named pipe /
//! unix socket primary, TCP fallback) with fixed-cadence rate history.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc as tokio_mpsc;

use ripdl_core::bundle;
use ripdl_core::engine::bonded_nics;
use ripdl_core::rpc::{BundlePart, Command, DuplicatePolicy, Reply, Request, Response, DaemonSettings};
use ripdl_core::task::{TaskKind, TaskState};
use ripdl_core::torrent::TorrentPreviewSnapshot;
use ripdl_core::transport::connect_daemon;

use crate::ext_host::{self, BrowserChoice};
use crate::model::{
    FilePickTarget, LinkRow, PendingAddRequest, Snapshot, TaskRow, RATE_HISTORY_CAP,
};

/// Latched once an in-app update installer launches so the GUI stops respawning
/// the background daemon and closes cleanly when setup shuts processes down.
static INSTALLING: AtomicBool = AtomicBool::new(false);

pub fn is_installing() -> bool {
    INSTALLING.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    Add {
        url: String,
        dest_dir: Option<PathBuf>,
        headers: std::collections::HashMap<String, String>,
        start_at: Option<i64>,
        kind: TaskKind,
        sequential: bool,
        only_files: Option<Vec<usize>>,
        duplicate: DuplicatePolicy,
    },
    AddBundle {
        name: Option<String>,
        dest_dir: Option<PathBuf>,
        parts: Vec<BundlePart>,
        start_at: Option<i64>,
    },
    PreviewBundle {
        links: Vec<BundlePart>,
    },
    TorrentPreview {
        payload: String,
    },
    Pause {
        ids: Vec<String>,
    },
    Resume {
        ids: Vec<String>,
    },
    Remove {
        ids: Vec<String>,
        delete_files: bool,
    },
    StopAll,
    ResumeAll,
    ClearCompleted,
    Open {
        id: String,
        folder: bool,
    },
    FetchDetails {
        id: String,
    },
    RefreshUrl {
        id: String,
        url: String,
        part: Option<u32>,
        probe_only: bool,
        resume: bool,
    },
    AddBundleParts {
        id: String,
        parts: Vec<BundlePart>,
    },
    UpdateTorrentSelection {
        id: String,
        only_files: Option<Vec<usize>>,
    },
    SetConfig {
        settings: DaemonSettings,
    },
    CheckUpdate,
    UpdateInstall,
    RestartDaemon,
    FetchLan,
    ScanLanPeers,
    LanNewCode,
    LanSend {
        url: String,
        code: String,
        path: PathBuf,
    },
    RegisterBrowserHost,
    OpenExtensionsPage {
        override_dir: String,
        browser: BrowserChoice,
    },
    OpenExtensionWelcome {
        browser: BrowserChoice,
    },
    InstallPlugin {
        path: PathBuf,
        override_dir: String,
    },
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Notice {
        text: String,
        is_error: bool,
    },
    Details {
        task_id: String,
        http: Option<ripdl_core::rpc::TaskDetailsSnapshot>,
        torrent: Option<ripdl_core::torrent::TorrentDetailsSnapshot>,
        error: Option<String>,
    },
    TorrentPreview {
        payload: String,
        result: Result<TorrentPreviewSnapshot, String>,
    },
    BundlePreview(Result<Vec<bundle::Group>, String>),
    RefreshProbe {
        url: String,
        result: Result<(u64, Option<u64>, Option<String>), String>,
    },
    RefreshCommitted {
        id: String,
        result: Result<(u64, Option<u64>), String>,
    },
    AddPartsResult {
        id: String,
        result: Result<usize, String>,
    },
    AddDone(Result<String, String>),
    DuplicatePrompt {
        request: PendingAddRequest,
        existing: TaskRow,
    },
    FilePicked {
        target: FilePickTarget,
        path: PathBuf,
    },
    ConfigSaved(Result<(), String>),
    HostRegisterResult(Result<String, String>),
}

#[derive(Clone)]
pub struct WorkerHandle {
    pub state: Arc<Mutex<Snapshot>>,
    cmd_tx: tokio_mpsc::UnboundedSender<UiCommand>,
    event_tx: mpsc::Sender<UiEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<UiEvent>>>,
}

impl WorkerHandle {
    pub fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Open a native file/folder picker on a helper thread so COM/Explorer shell
    /// initialization never blocks the UI frame loop.
    pub fn pick_path(
        &self,
        target: FilePickTarget,
        folder: bool,
        initial_dir: Option<PathBuf>,
        filter: Option<(&'static str, &'static [&'static str])>,
        ctx: egui::Context,
    ) {
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new();
            if let Some(dir) = initial_dir.filter(|p| p.is_dir()) {
                dlg = dlg.set_directory(dir);
            }
            if let Some((name, exts)) = filter {
                dlg = dlg.add_filter(name, exts);
            }
            let picked = if folder { dlg.pick_folder() } else { dlg.pick_file() };
            if let Some(path) = picked {
                let _ = tx.send(UiEvent::FilePicked { target, path });
                ctx.request_repaint();
            }
        });
    }

    pub fn drain_events(&self) -> Vec<UiEvent> {
        let Ok(rx) = self.event_rx.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

pub fn spawn_worker(ctx: egui::Context) -> WorkerHandle {
    let initial = Snapshot {
        daemon_label: "connecting...".into(),
        ..Default::default()
    };
    let state = Arc::new(Mutex::new(initial));
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    let event_tx_bg = event_tx.clone();

    let state_bg = Arc::clone(&state);
    std::thread::Builder::new()
        .name("ripdl-ui-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("worker tokio runtime");
            rt.block_on(async move {
                run_live_loop(state_bg, cmd_rx, event_tx_bg, ctx).await;
            });
        })
        .expect("spawn worker thread");

    WorkerHandle {
        state,
        cmd_tx,
        event_tx,
        event_rx: Arc::new(Mutex::new(event_rx)),
    }
}

struct LiveEnv {
    seq: u64,
    rate_history: VecDeque<f64>,
    supports_traffic_snapshot: Option<bool>,
    spawned_once: bool,
    boot_update_checked: bool,
}

impl LiveEnv {
    fn new() -> Self {
        Self {
            seq: 0,
            rate_history: VecDeque::with_capacity(RATE_HISTORY_CAP),
            supports_traffic_snapshot: None,
            spawned_once: false,
            boot_update_checked: false,
        }
    }

    async fn rpc(&mut self, cmd: Command, timeout: Duration) -> Result<Reply, String> {
        self.seq = self.seq.wrapping_add(1);
        let id = self.seq;
        let req = Request { id, cmd };
        let payload = serde_json::to_string(&req).map_err(|e| e.to_string())? + "\n";
        let fut = async move {
            let mut stream = connect_daemon().await.map_err(|e| e.to_string())?;
            stream
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            stream.flush().await.ok();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("daemon closed the connection".to_string());
            }
            let resp: Response =
                serde_json::from_str(line.trim()).map_err(|e| format!("bad daemon reply: {e}"))?;
            if resp.id != id && resp.id != 0 {
                return Err(format!("reply id mismatch: {} != {id}", resp.id));
            }
            Ok::<Reply, String>(resp.reply)
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| "daemon request timed out".to_string())?
    }

    async fn poll(&mut self, state: &Arc<Mutex<Snapshot>>) {
        let list_reply = match self.rpc(Command::List, Duration::from_secs(3)).await {
            Ok(r) => {
                self.spawned_once = false;
                r
            }
            Err(err) => {
                if !self.spawned_once && !is_installing() {
                    self.spawned_once = true;
                    let _ = ripdl_core::bridge::spawn_daemon_bg();
                }
                let mut g = state.lock().unwrap();
                g.connected = false;
                g.daemon_label = if is_installing() {
                    "updating...".into()
                } else {
                    "offline (auto-starting...)".into()
                };
                g.last_error = Some(err);
                return;
            }
        };

        let mut tasks = match list_reply {
            Reply::Tasks { tasks } => tasks.into_iter().map(TaskRow::from_rpc).collect(),
            _ => Vec::new(),
        };
        // Enforce invariant UI-side too: only actively running rows report download rate.
        for t in &mut tasks {
            if t.raw_state != TaskState::Running || t.verifying {
                t.speed_bps = None;
            }
        }
        let derived_bps: f64 = tasks.iter().filter_map(|t| t.speed_bps).sum();
        let mut total_bps = derived_bps;
        let mut unified_traffic = false;
        let mut global_limit = None;
        let mut links: Vec<LinkRow> = Vec::new();

        if self.supports_traffic_snapshot != Some(false) {
            match self
                .rpc(Command::GetTrafficSnapshot, Duration::from_secs(2))
                .await
            {
                Ok(Reply::TrafficSnapshot {
                    controller,
                    links: ls,
                }) => {
                    self.supports_traffic_snapshot = Some(true);
                    unified_traffic = true;
                    total_bps = controller.total_bps;
                    global_limit = controller.global_limit_bps;
                    links = ls
                        .into_iter()
                        .map(|l| LinkRow {
                            name: l.name,
                            ip: l.ip.to_string(),
                            default_route: l.default_route,
                            rate_bps: l.rate_bps,
                            capacity_bps: l.capacity_bps,
                            weight: l.weight,
                            in_use: l.in_use,
                            fails: l.fails,
                        })
                        .collect();
                }
                Ok(Reply::Error { .. }) => {
                    self.supports_traffic_snapshot = Some(false);
                }
                _ => {}
            }
        }

        let cfg = self.rpc(Command::GetConfig, Duration::from_secs(2)).await.ok();
        let (
            settings,
            daemon_version,
            bundles_supported,
            torrent_session,
            torrent_bind,
            torrent_rates,
            torrent_unreachable,
            extension,
            fallback_links,
        ) = match cfg {
            Some(Reply::Config {
                settings,
                torrent_session,
                torrent_bind,
                torrent_rates,
                torrent_unreachable,
                bond_links,
                traffic_links,
                bundles,
                version,
                extension,
            }) => {
                global_limit = global_limit.or_else(|| settings.max_speed());
                let mapped_links = if let Some(tl) = traffic_links {
                    tl.into_iter()
                        .map(|l| LinkRow {
                            name: l.name,
                            ip: l.ip.to_string(),
                            default_route: l.default_route,
                            rate_bps: l.rate_bps,
                            capacity_bps: l.capacity_bps,
                            weight: l.weight,
                            in_use: l.in_use,
                            fails: l.fails,
                        })
                        .collect()
                } else {
                    bond_links
                        .into_iter()
                        .map(|b| LinkRow {
                            name: b.name,
                            ip: b.ip,
                            default_route: b.default_route,
                            rate_bps: b.rate_bps,
                            capacity_bps: b.capacity_bps,
                            weight: b.weight,
                            in_use: b.in_use,
                            fails: b.fails,
                        })
                        .collect()
                };
                (
                    Some(settings),
                    version,
                    bundles,
                    torrent_session,
                    torrent_bind,
                    torrent_rates,
                    torrent_unreachable,
                    Some(extension),
                    mapped_links,
                )
            }
            _ => (None, String::new(), true, false, Vec::new(), Vec::new(), Vec::new(), None, Vec::new()),
        };

        if links.is_empty() {
            links = fallback_links;
        }
        let detected_nics = bonded_nics();

        if !self.boot_update_checked {
            self.boot_update_checked = true;
            if settings.as_ref().map(|s| s.update_check()).unwrap_or(true) {
                if let Ok(Reply::Update { info }) = self
                    .rpc(Command::CheckUpdate, Duration::from_secs(10))
                    .await
                {
                    state.lock().unwrap().update = Some(info);
                }
            }
        }

        if self.rate_history.len() >= RATE_HISTORY_CAP {
            self.rate_history.pop_front();
        }
        self.rate_history.push_back(total_bps);

        let mut g = state.lock().unwrap();
        g.connected = true;
        g.daemon_label = if daemon_version.is_empty() {
            "connected".into()
        } else {
            format!("daemon v{daemon_version}")
        };
        g.daemon_version = daemon_version;
        g.bundles_supported = bundles_supported;
        g.unified_traffic = unified_traffic;
        g.tasks = tasks;
        g.links = links;
        g.detected_nics = detected_nics;
        g.total_bps = total_bps;
        g.rate_history = self.rate_history.clone();
        g.global_limit = global_limit;
        g.settings = settings;
        g.torrent_session = torrent_session;
        g.torrent_bind = torrent_bind;
        g.torrent_rates = torrent_rates;
        g.torrent_unreachable = torrent_unreachable;
        g.extension = extension;
        g.last_error = None;
    }
}

async fn run_live_loop(
    state: Arc<Mutex<Snapshot>>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<UiCommand>,
    event_tx: mpsc::Sender<UiEvent>,
    ctx: egui::Context,
) {
    let mut env = LiveEnv::new();
    env.poll(&state).await;
    ctx.request_repaint();

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;

    loop {
        tokio::select! {
            _ = tick.tick() => {
                env.poll(&state).await;
                ctx.request_repaint();
            }
            Some(cmd) = cmd_rx.recv() => {
                handle_live_cmd(&mut env, &state, &event_tx, cmd).await;
                ctx.request_repaint();
            }
            else => break,
        }
    }
}

fn reply_err(r: Reply) -> Result<(), String> {
    match r {
        Reply::Error { message } => Err(message),
        _ => Ok(()),
    }
}

async fn handle_live_cmd(
    env: &mut LiveEnv,
    state: &Arc<Mutex<Snapshot>>,
    tx: &mpsc::Sender<UiEvent>,
    cmd: UiCommand,
) {
    match cmd {
        UiCommand::Add {
            url,
            dest_dir,
            headers,
            start_at,
            kind,
            sequential,
            only_files,
            duplicate,
        } => {
            if let Some(ref d) = dest_dir {
                let _ = tokio::fs::create_dir_all(d).await;
            }
            let pending = PendingAddRequest {
                url: url.clone(),
                dest_dir: dest_dir.clone(),
                headers: headers.clone(),
                start_at,
                kind,
                sequential,
                only_files: only_files.clone(),
            };
            match env
                .rpc(
                    Command::Add {
                        url,
                        dest_dir,
                        headers,
                        start_at,
                        kind,
                        sequential,
                        only_files,
                        on_duplicate: duplicate,
                    },
                    Duration::from_secs(15),
                )
                .await
            {
                Ok(Reply::Added { id }) => {
                    let _ = tx.send(UiEvent::AddDone(Ok(id)));
                }
                Ok(Reply::Duplicate { task }) => {
                    let _ = tx.send(UiEvent::DuplicatePrompt {
                        request: pending,
                        existing: TaskRow::from_rpc(task),
                    });
                }
                Ok(Reply::Error { message }) => {
                    let _ = tx.send(UiEvent::AddDone(Err(message)));
                }
                Ok(other) => {
                    let _ = tx.send(UiEvent::AddDone(Err(format!(
                        "unexpected daemon reply: {other:?}"
                    ))));
                }
                Err(err) => {
                    let _ = tx.send(UiEvent::AddDone(Err(err)));
                }
            }
            env.poll(state).await;
        }
        UiCommand::AddBundle {
            name,
            dest_dir,
            parts,
            start_at,
        } => {
            if let Some(ref d) = dest_dir {
                let _ = tokio::fs::create_dir_all(d).await;
            }
            let res = env
                .rpc(
                    Command::AddBundle {
                        name,
                        dest_dir,
                        parts,
                        start_at,
                    },
                    Duration::from_secs(15),
                )
                .await
                .and_then(|r| match r {
                    Reply::Added { id } => Ok(id),
                    Reply::Error { message } => Err(message),
                    other => Err(format!("unexpected daemon reply: {other:?}")),
                });
            let _ = tx.send(UiEvent::AddDone(res));
            env.poll(state).await;
        }
        UiCommand::PreviewBundle { links } => {
            let res = env
                .rpc(Command::PreviewBundle { links }, Duration::from_secs(15))
                .await
                .and_then(|r| match r {
                    Reply::BundlePreview { groups } => Ok(groups),
                    Reply::Error { message } => Err(message),
                    other => Err(format!("unexpected reply: {other:?}")),
                });
            let _ = tx.send(UiEvent::BundlePreview(res));
        }
        UiCommand::TorrentPreview { payload } => {
            let res = env
                .rpc(
                    Command::TorrentPreview {
                        payload: payload.clone(),
                    },
                    Duration::from_secs(45),
                )
                .await
                .and_then(|r| match r {
                    Reply::TorrentPreview { preview } => Ok(preview),
                    Reply::Error { message } => Err(message),
                    other => Err(format!("unexpected reply: {other:?}")),
                });
            let _ = tx.send(UiEvent::TorrentPreview {
                payload,
                result: res,
            });
        }
        UiCommand::Pause { ids } => {
            for id in ids {
                if let Err(e) = env
                    .rpc(Command::Pause { id }, Duration::from_secs(5))
                    .await
                    .and_then(reply_err)
                {
                    let _ = tx.send(UiEvent::Notice {
                        text: e,
                        is_error: true,
                    });
                }
            }
            env.poll(state).await;
        }
        UiCommand::Resume { ids } => {
            for id in ids {
                if let Err(e) = env
                    .rpc(Command::Resume { id }, Duration::from_secs(5))
                    .await
                    .and_then(reply_err)
                {
                    let _ = tx.send(UiEvent::Notice {
                        text: e,
                        is_error: true,
                    });
                }
            }
            env.poll(state).await;
        }
        UiCommand::Remove { ids, delete_files } => {
            for id in ids {
                if let Err(e) = env
                    .rpc(Command::Remove { id, delete_files }, Duration::from_secs(10))
                    .await
                    .and_then(reply_err)
                {
                    let _ = tx.send(UiEvent::Notice {
                        text: e,
                        is_error: true,
                    });
                }
            }
            env.poll(state).await;
        }
        UiCommand::StopAll => {
            let ids: Vec<String> = state
                .lock()
                .unwrap()
                .tasks
                .iter()
                .filter(|t| t.can_pause_or_stop())
                .map(|t| t.id.clone())
                .collect();
            for id in ids {
                let _ = env
                    .rpc(Command::Pause { id }, Duration::from_secs(5))
                    .await;
            }
            env.poll(state).await;
        }
        UiCommand::ResumeAll => {
            let ids: Vec<String> = state
                .lock()
                .unwrap()
                .tasks
                .iter()
                .filter(|t| t.can_resume())
                .map(|t| t.id.clone())
                .collect();
            for id in ids {
                let _ = env
                    .rpc(Command::Resume { id }, Duration::from_secs(5))
                    .await;
            }
            env.poll(state).await;
        }
        UiCommand::ClearCompleted => {
            // Never remove actively seeding torrents when clearing completed downloads.
            let ids: Vec<String> = state
                .lock()
                .unwrap()
                .tasks
                .iter()
                .filter(|t| t.raw_state == TaskState::Completed && !t.seeding)
                .map(|t| t.id.clone())
                .collect();
            let n = ids.len();
            for id in ids {
                let _ = env
                    .rpc(
                        Command::Remove {
                            id,
                            delete_files: false,
                        },
                        Duration::from_secs(5),
                    )
                    .await;
            }
            if n > 0 {
                let _ = tx.send(UiEvent::Notice {
                    text: format!("Cleared {n} finished task(s) from the list."),
                    is_error: false,
                });
            }
            env.poll(state).await;
        }
        UiCommand::Open { id, folder } => {
            if let Err(e) = env
                .rpc(Command::Open { id, folder }, Duration::from_secs(5))
                .await
                .and_then(reply_err)
            {
                let _ = tx.send(UiEvent::Notice {
                    text: e,
                    is_error: true,
                });
            }
        }
        UiCommand::FetchDetails { id } => {
            match env
                .rpc(Command::Details { id: id.clone() }, Duration::from_secs(5))
                .await
            {
                Ok(Reply::TaskDetails { details }) => {
                    let _ = tx.send(UiEvent::Details {
                        task_id: id,
                        http: Some(details),
                        torrent: None,
                        error: None,
                    });
                }
                Ok(Reply::TorrentDetails { details }) => {
                    let _ = tx.send(UiEvent::Details {
                        task_id: id,
                        http: None,
                        torrent: Some(details),
                        error: None,
                    });
                }
                Ok(Reply::Error { message }) => {
                    let _ = tx.send(UiEvent::Details {
                        task_id: id,
                        http: None,
                        torrent: None,
                        error: Some(message),
                    });
                }
                Ok(other) => {
                    let _ = tx.send(UiEvent::Details {
                        task_id: id,
                        http: None,
                        torrent: None,
                        error: Some(format!("unexpected details reply: {other:?}")),
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiEvent::Details {
                        task_id: id,
                        http: None,
                        torrent: None,
                        error: Some(e),
                    });
                }
            }
        }
        UiCommand::RefreshUrl {
            id,
            url,
            part,
            probe_only,
            resume,
        } => {
            let res = env
                .rpc(
                    Command::RefreshUrl {
                        id: id.clone(),
                        url: url.clone(),
                        resume: resume && !probe_only,
                        part,
                        probe_only,
                    },
                    Duration::from_secs(20),
                )
                .await;
            if probe_only {
                let out = match res {
                    Ok(Reply::Refreshed {
                        kept,
                        total,
                        warning,
                        ..
                    }) => Ok((kept, total, warning)),
                    Ok(Reply::Error { message }) => Err(message),
                    Ok(other) => Err(format!("unexpected reply: {other:?}")),
                    Err(e) => Err(e),
                };
                let _ = tx.send(UiEvent::RefreshProbe { url, result: out });
            } else {
                let out = match res {
                    Ok(Reply::Refreshed { kept, total, .. }) => Ok((kept, total)),
                    Ok(Reply::Error { message }) => Err(message),
                    Ok(other) => Err(format!("unexpected reply: {other:?}")),
                    Err(e) => Err(e),
                };
                let _ = tx.send(UiEvent::RefreshCommitted { id, result: out });
                env.poll(state).await;
            }
        }
        UiCommand::AddBundleParts { id, parts } => {
            let res = env
                .rpc(
                    Command::AddBundleParts {
                        id: id.clone(),
                        parts,
                    },
                    Duration::from_secs(10),
                )
                .await
                .and_then(|r| match r {
                    Reply::BundlePartsAdded { added, .. } => Ok(added),
                    Reply::Error { message } => Err(message),
                    other => Err(format!("unexpected reply: {other:?}")),
                });
            let _ = tx.send(UiEvent::AddPartsResult { id, result: res });
            env.poll(state).await;
        }
        UiCommand::UpdateTorrentSelection { id, only_files } => {
            match env
                .rpc(
                    Command::UpdateTorrentSelection {
                        id: id.clone(),
                        only_files,
                    },
                    Duration::from_secs(8),
                )
                .await
                .and_then(reply_err)
            {
                Ok(()) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: "Torrent file selection updated.".into(),
                        is_error: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: e,
                        is_error: true,
                    });
                }
            }
        }
        UiCommand::SetConfig { settings } => {
            let res = env
                .rpc(Command::SetConfig { settings }, Duration::from_secs(8))
                .await
                .and_then(|r| match r {
                    Reply::Config { .. } => Ok(()),
                    Reply::Error { message } => Err(message),
                    other => Err(format!("unexpected reply: {other:?}")),
                });
            let _ = tx.send(UiEvent::ConfigSaved(res));
            env.poll(state).await;
        }
        UiCommand::CheckUpdate => {
            state.lock().unwrap().update_checking = true;
            let res = env
                .rpc(Command::CheckUpdate, Duration::from_secs(15))
                .await;
            let mut g = state.lock().unwrap();
            g.update_checking = false;
            match res {
                Ok(Reply::Update { info }) => {
                    let msg = if info.update_available {
                        format!("Update v{} is available (current v{}).", info.latest, info.current)
                    } else {
                        format!("Up to date (v{}).", info.current)
                    };
                    g.update = Some(info);
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: false,
                    });
                }
                Ok(Reply::Error { message }) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: message,
                        is_error: true,
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: e,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        UiCommand::UpdateInstall => {
            INSTALLING.store(true, Ordering::Relaxed);
            state.lock().unwrap().update_install_status = Some("Downloading & verifying installer...".into());
            let res = env
                .rpc(Command::UpdateInstall, Duration::from_secs(180))
                .await;
            let mut g = state.lock().unwrap();
            match res {
                Ok(Reply::Ok) => {
                    g.update_install_status = Some("Installer launched cleanly.".into());
                    let _ = tx.send(UiEvent::Notice {
                        text: "Update installer launched.".into(),
                        is_error: false,
                    });
                }
                Ok(Reply::Error { message }) | Err(message) => {
                    INSTALLING.store(false, Ordering::Relaxed);
                    g.update_install_status = Some(format!("Update failed: {message}"));
                    let _ = tx.send(UiEvent::Notice {
                        text: message,
                        is_error: true,
                    });
                }
                _ => {
                    INSTALLING.store(false, Ordering::Relaxed);
                    g.update_install_status = None;
                }
            }
        }
        UiCommand::RestartDaemon => {
            let running_ids: Vec<String> = state
                .lock()
                .unwrap()
                .tasks
                .iter()
                .filter(|t| t.raw_state == TaskState::Running)
                .map(|t| t.id.clone())
                .collect();
            let _ = env.rpc(Command::Shutdown, Duration::from_secs(3)).await;
            tokio::time::sleep(Duration::from_millis(450)).await;
            let _ = ripdl_core::bridge::spawn_daemon_bg();
            let mut back = false;
            for _ in 0..30 {
                if env.rpc(Command::List, Duration::from_secs(1)).await.is_ok() {
                    back = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if back {
                for id in running_ids {
                    let _ = env.rpc(Command::Resume { id }, Duration::from_secs(3)).await;
                }
                env.poll(state).await;
                let _ = tx.send(UiEvent::Notice {
                    text: "Background daemon restarted; active transfers resumed.".into(),
                    is_error: false,
                });
            } else {
                let _ = tx.send(UiEvent::Notice {
                    text: "Daemon did not restart within timeout.".into(),
                    is_error: true,
                });
            }
        }
        UiCommand::FetchLan => {
            if let Ok(Reply::Lan { info }) = env.rpc(Command::LanInfo, Duration::from_secs(3)).await {
                state.lock().unwrap().lan = Some(info);
            }
            if let Ok(Reply::LanSends { sends }) = env.rpc(Command::LanSends, Duration::from_secs(3)).await {
                state.lock().unwrap().lan_sends = sends;
            }
        }
        UiCommand::ScanLanPeers => {
            state.lock().unwrap().lan_scanning = true;
            let res = env.rpc(Command::LanPeers, Duration::from_secs(6)).await;
            let mut g = state.lock().unwrap();
            g.lan_scanning = false;
            match res {
                Ok(Reply::LanPeers { peers }) => {
                    g.lan_peers = peers;
                }
                Ok(Reply::Error { message }) | Err(message) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: message,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        UiCommand::LanNewCode => {
            match env.rpc(Command::LanNewCode, Duration::from_secs(3)).await {
                Ok(Reply::Lan { info }) => {
                    state.lock().unwrap().lan = Some(info);
                    let _ = tx.send(UiEvent::Notice {
                        text: "Rotated LAN pairing code.".into(),
                        is_error: false,
                    });
                }
                Ok(Reply::Error { message }) | Err(message) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: message,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        UiCommand::LanSend { url, code, path } => {
            match env
                .rpc(Command::LanSend { url, code, path }, Duration::from_secs(10))
                .await
            {
                Ok(Reply::LanSend { .. }) => {
                    if let Ok(Reply::LanSends { sends }) =
                        env.rpc(Command::LanSends, Duration::from_secs(3)).await
                    {
                        state.lock().unwrap().lan_sends = sends;
                    }
                    let _ = tx.send(UiEvent::Notice {
                        text: "LAN transfer started.".into(),
                        is_error: false,
                    });
                }
                Ok(Reply::Error { message }) | Err(message) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: message,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        UiCommand::RegisterBrowserHost => {
            let res = tokio::task::spawn_blocking(ext_host::register_host_cli)
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            match &res {
                Ok(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg.clone(),
                        is_error: false,
                    });
                }
                Err(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg.clone(),
                        is_error: true,
                    });
                }
            }
            let _ = tx.send(UiEvent::HostRegisterResult(res));
            env.poll(state).await;
        }
        UiCommand::OpenExtensionsPage { override_dir, browser } => {
            let res = tokio::task::spawn_blocking(move || {
                ext_host::open_extensions_page(&override_dir, browser)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match res {
                Ok(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: false,
                    });
                }
                Err(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: true,
                    });
                }
            }
        }
        UiCommand::OpenExtensionWelcome { browser } => {
            let res = tokio::task::spawn_blocking(move || ext_host::open_welcome_page(browser))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            match res {
                Ok(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: false,
                    });
                }
                Err(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: true,
                    });
                }
            }
        }
        UiCommand::InstallPlugin { path, override_dir } => {
            let res = tokio::task::spawn_blocking(move || {
                ext_host::install_plugin_file(&path, &override_dir)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match res {
                Ok(id) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: format!("Installed browser plugin: {id}"),
                        is_error: false,
                    });
                }
                Err(msg) => {
                    let _ = tx.send(UiEvent::Notice {
                        text: msg,
                        is_error: true,
                    });
                }
            }
        }
    }
}
