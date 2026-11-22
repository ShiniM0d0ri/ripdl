//! Native UI spike for ripdl: IDM-style shell, adapted to ripdl features.
//! eframe/egui, no WebView2. All real numbers come from the daemon snapshot.

use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use ripdl_core::rpc::{Command, Reply, Request, Response, TaskDetailsSnapshot};
use ripdl_core::torrent::TorrentDetailsSnapshot;

#[derive(Clone, Default)]
struct Snapshot {
    daemon: String,
    tasks: Vec<TaskRow>,
    links: Vec<LinkRow>,
    total_bps: Option<f64>,
    global_limit: Option<u64>,
    settings: Option<ripdl_core::rpc::DaemonSettings>,
}

#[derive(Clone)]
struct TaskRow {
    id: String,
    name: String,
    kind: String,
    source: String,
    state: String,
    downloaded: u64,
    total: Option<u64>,
    speed_bps: Option<f64>,
    last_try: String,
}

#[derive(Clone)]
struct LinkRow {
    name: String,
    rate_bps: u64,
    weight: u32,
    in_use: u32,
}

fn humans(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = bytes as f64;
    let mut i = 0;
    while n >= 1024.0 && i + 1 < UNITS.len() {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n:.0} {}", UNITS[i])
    } else {
        format!("{n:.1} {}", UNITS[i])
    }
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "torrent" => "◈",
        "video" => "▷",
        "bundle" => "▤",
        _ => "↓",
    }
}

fn display_kind(kind: &str) -> &'static str {
    match kind {
        "torrent" => "Torrent",
        "video" => "Video",
        "bundle" => "Bundle",
        "http" => "HTTP",
        _ => "File",
    }
}

fn display_state(state: &str) -> String {
    match state {
        "completed" => "Completed".into(),
        "running" => "Running".into(),
        "queued" => "Queued".into(),
        "connecting" => "Connecting".into(),
        "paused" => "Paused".into(),
        "failed" => "Failed".into(),
        "stale" => "Stale".into(),
        "seeding" => "Seeding".into(),
        "verifying" => "Verifying".into(),
        _ => state.to_string(),
    }
}

fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(0, 128, 116)
}

fn kind_color(kind: &str) -> egui::Color32 {
    match kind {
        "torrent" => egui::Color32::from_rgb(45, 200, 170),
        "video" => egui::Color32::from_rgb(90, 160, 240),
        "bundle" => egui::Color32::from_rgb(180, 180, 180),
        _ => egui::Color32::from_rgb(130, 170, 210),
    }
}

fn status_color(state: &str) -> egui::Color32 {
    match state {
        "completed" => egui::Color32::from_rgb(76, 200, 120),
        "running" => egui::Color32::from_rgb(90, 170, 250),
        "queued" | "connecting" => egui::Color32::from_rgb(165, 175, 180),
        "paused" => egui::Color32::from_rgb(235, 190, 80),
        "failed" | "stale" => egui::Color32::from_rgb(235, 100, 100),
        _ => egui::Color32::from_rgb(190, 190, 190),
    }
}

fn task_ratio(t: &TaskRow) -> f32 {
    if let Some(total) = t.total {
        if total > 0 {
            return (t.downloaded as f32 / total as f32).clamp(0.0, 1.0);
        }
    }
    if t.state == "completed" || t.state == "seeding" {
        1.0
    } else {
        0.0
    }
}

fn host_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        .unwrap_or(url)
        .to_string()
}

static RPC_ID: AtomicU64 = AtomicU64::new(0);

fn rpc_token() -> Result<String, String> {
    ripdl_core::transport::read_rpc_token().map_err(|e| e.to_string())
}

fn rpc(cmd: Command) -> Result<Reply, String> {
    let token = rpc_token()?;
    let stream = TcpStream::connect(("127.0.0.1", ripdl_core::transport::rpc_port()))
        .map_err(|e| format!("daemon unreachable: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(700)))
        .ok();
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let id = RPC_ID.fetch_add(1, Ordering::Relaxed) + 1;
    let request = Request { id, cmd };
    let mut line = ripdl_core::transport::auth_line(&token);
    line.push_str(&serde_json::to_string(&request).map_err(|e| e.to_string())?);
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    writer.flush().ok();

    let mut reader = std::io::BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| format!("daemon reply failed: {e}"))?;
    let parsed: Response = serde_json::from_str(response.trim()).map_err(|e| e.to_string())?;
    if parsed.id != id {
        return Err(format!("reply id mismatch: {} != {id}", parsed.id));
    }
    Ok(parsed.reply)
}

fn poll_once() -> Snapshot {
    let mut snap = Snapshot {
        daemon: "offline".into(),
        ..Default::default()
    };
    if rpc_token().is_err() {
        return snap;
    }
    snap.daemon = "connected".into();

    if let Ok(Reply::Tasks { tasks }) = rpc(Command::List) {
        snap.tasks = tasks
            .into_iter()
            .map(|t| TaskRow {
                id: t.id,
                name: t
                    .dest_file
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| t.url.clone()),
                kind: format!("{:?}", t.kind).to_lowercase(),
                source: host_of(&t.url),
                state: if t.seeding {
                    "seeding".to_string()
                } else {
                    format!("{:?}", t.state).to_lowercase()
                },
                downloaded: t.downloaded,
                total: t.total,
                speed_bps: if matches!(t.state, ripdl_core::TaskState::Running) {
                    t.speed_bps
                } else {
                    None
                },
                last_try: t.stale_reason.or(t.error).unwrap_or_default(),
            })
            .collect();
    }

    match rpc(Command::GetTrafficSnapshot) {
        Ok(Reply::TrafficSnapshot { controller, links }) => {
            snap.total_bps = Some(controller.total_bps);
            snap.global_limit = controller.global_limit_bps;
            snap.links = links
                .into_iter()
                .map(|l| LinkRow {
                    name: l.name,
                    rate_bps: l.rate_bps,
                    weight: l.weight,
                    in_use: l.in_use,
                })
                .collect();
        }
        Ok(Reply::Error { .. }) => snap.daemon = "connected (legacy daemon)".into(),
        _ => {}
    }

    if let Ok(Reply::Config { settings, .. }) = rpc(Command::GetConfig) {
        snap.settings = Some(settings);
    }
    snap
}

fn fixture_snapshot() -> Snapshot {
    Snapshot {
        daemon: "fixture".into(),
        tasks: (0..200)
            .map(|i| TaskRow {
                id: format!("fixture-{i}"),
                name: format!("download-{i:03}.bin"),
                kind: if i % 5 == 0 { "torrent" } else { "http" }.into(),
                source: format!("cdn-{}.example.com", i % 8),
                state: if i % 3 == 0 { "running" } else { "queued" }.into(),
                downloaded: (i as u64) * 1024 * 1024,
                total: Some(100 * 1024 * 1024),
                speed_bps: Some(1_048_576.0),
                last_try: if i % 17 == 0 {
                    "expired link".into()
                } else {
                    String::new()
                },
            })
            .collect(),
        ..Default::default()
    }
}

fn spawn_poller(dense: bool) -> Arc<Mutex<Snapshot>> {
    let state = Arc::new(Mutex::new(if dense {
        fixture_snapshot()
    } else {
        poll_once()
    }));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || loop {
        let next = if dense {
            fixture_snapshot()
        } else {
            poll_once()
        };
        *shared.lock().unwrap() = next;
        std::thread::sleep(Duration::from_secs(1));
    });
    state
}

struct SpikeApp {
    state: Arc<Mutex<Snapshot>>,
    category: String,
    selected: Option<String>,
    details_id: Option<String>,
    details_tab: String,
    add_open: bool,
    advanced_open: bool,
    url: String,
    dest: String,
    category_text: String,
    headers_text: String,
    video: bool,
    torrent: bool,
    sequential: bool,
    notice: String,
    settings_open: bool,
    settings_tab: String,
    settings_edit: Option<ripdl_core::rpc::DaemonSettings>,
    settings_dir_text: String,
    settings_dirty: bool,
    scheduler_open: bool,
    scheduler_tab: String,
    selected_queue: String,
}

impl SpikeApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let screen = std::env::var("RIPDL_QA_SCREEN").unwrap_or_default();
        let dense = screen == "dense" || std::env::var("RIPDL_QA_FIXTURE").as_deref() == Ok("200");
        let state = spawn_poller(dense);
        let first_id = state.lock().unwrap().tasks.first().map(|t| t.id.clone());
        let details_id = if screen == "details" {
            first_id.clone()
        } else {
            None
        };
        let selected = if screen == "selected" || screen == "details" {
            first_id
        } else {
            None
        };
        Self {
            state,
            category: if screen == "empty" {
                "torrent".into()
            } else {
                "all".into()
            },
            selected,
            details_id,
            details_tab: "Files".into(),
            add_open: screen == "add" || screen == "add-advanced",
            advanced_open: screen == "add-advanced",
            url: String::new(),
            dest: String::new(),
            category_text: String::new(),
            headers_text: String::new(),
            video: false,
            torrent: false,
            sequential: false,
            notice: String::new(),
            settings_open: screen.starts_with("settings"),
            settings_tab: match screen.as_str() {
                "settings-connection" => "Connection",
                "settings-torrents" => "Torrents",
                "settings-updates" => "Updates",
                _ => "General",
            }
            .into(),
            settings_edit: None,
            settings_dir_text: String::new(),
            settings_dirty: false,
            scheduler_open: screen == "scheduler",
            scheduler_tab: "Schedule".into(),
            selected_queue: "default".into(),
        }
    }

    fn rpc_action(&mut self, cmd: Command) {
        if let Err(e) = rpc(cmd) {
            self.notice = e;
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            let _ = ui.menu_button("Tasks", |ui| {
                if ui.button("Add URL").clicked() {
                    self.add_open = true;
                }
                if ui.button("Add torrent").clicked() {
                    self.notice = "Add torrent is not wired in the spike yet".into();
                }
                if ui.button("Add bundle").clicked() {
                    self.notice = "Add bundle is not wired in the spike yet".into();
                }
                ui.separator();
                if ui.button("Pause all").clicked() {
                    let ids: Vec<String> = snap
                        .tasks
                        .iter()
                        .filter(|t| matches!(t.state.as_str(), "running" | "queued"))
                        .map(|t| t.id.clone())
                        .collect();
                    for id in ids {
                        self.rpc_action(Command::Pause { id });
                    }
                }
                if ui.button("Resume all").clicked() {
                    let ids: Vec<String> = snap
                        .tasks
                        .iter()
                        .filter(|t| matches!(t.state.as_str(), "running" | "queued"))
                        .map(|t| t.id.clone())
                        .collect();
                    for id in ids {
                        self.rpc_action(Command::Resume { id });
                    }
                }
            });
            let _ = ui.menu_button("File", |ui| {
                if let Some(id) = &self.selected {
                    let id = id.clone();
                    if ui.button("Open file").clicked() {
                        self.rpc_action(Command::Open {
                            id: id.clone(),
                            folder: false,
                        });
                    }
                    if ui.button("Open folder").clicked() {
                        self.rpc_action(Command::Open { id, folder: true });
                    }
                } else {
                    let _ = ui.button("Open file");
                    let _ = ui.button("Open folder");
                }
                let _ = ui.button("Remove");
            });
            let _ = ui.menu_button("Downloads", |ui| {
                if ui.button("Start queue").clicked() {
                    self.notice = "Queue start is not wired in the spike yet".into();
                }
                if ui.button("Stop queue").clicked() {
                    self.notice = "Queue stop is not wired in the spike yet".into();
                }
                if ui.button("Scheduler").clicked() {
                    self.scheduler_open = true;
                }
            });
            let _ = ui.menu_button("View", |ui| {
                let _ = ui.button("Columns");
                let _ = ui.button("Theme");
            });
            let _ = ui.menu_button("Tools", |ui| {
                if ui.button("Options").clicked() {
                    self.settings_open = true;
                }
                if ui.button("Send & Receive").clicked() {
                    self.notice = "LAN drop box is available from the daemon".into();
                }
            });
            let _ = ui.menu_button("Help", |ui| {
                let _ = ui.button("About ripdl");
                let _ = ui.button("Run setup again");
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let actions: [(&str, &str); 9] = [
            ("+", "Add URL"),
            ("▷", "Resume"),
            ("■", "Stop"),
            ("■", "Stop All"),
            ("✕", "Delete"),
            ("✓", "Delete Completed"),
            ("⚙", "Options"),
            ("◷", "Scheduler"),
            ("◗", "Details"),
        ];
        let has_selection = self.selected.is_some();
        let has_active = snap
            .tasks
            .iter()
            .any(|t| matches!(t.state.as_str(), "running" | "queued"));
        let has_completed = snap
            .tasks
            .iter()
            .any(|t| t.state == "completed" || t.state == "seeding");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (icon, label) in actions.iter() {
                let enabled = match *label {
                    "Add URL" | "Options" | "Scheduler" => true,
                    "Resume" | "Stop" | "Delete" | "Details" => has_selection,
                    "Stop All" => has_active,
                    "Delete Completed" => has_completed,
                    _ => true,
                };
                let mut button =
                    egui::Button::new(egui::RichText::new(format!("{icon} {label}")).size(13.0))
                        .min_size(egui::vec2(0.0, 30.0))
                        .corner_radius(6.0);
                if *label == "Add URL" {
                    button = button.fill(accent());
                }
                let btn = ui.add_enabled(enabled, button).on_hover_text(*label);
                if !btn.clicked() {
                    continue;
                }
                match *label {
                    "Add URL" => self.add_open = true,
                    "Resume" => {
                        if let Some(id) = self.selected.clone() {
                            self.rpc_action(Command::Resume { id });
                        }
                    }
                    "Stop" => {
                        if let Some(id) = self.selected.clone() {
                            self.rpc_action(Command::Pause { id });
                        }
                    }
                    "Stop All" => {
                        let ids: Vec<String> = snap
                            .tasks
                            .iter()
                            .filter(|t| matches!(t.state.as_str(), "running" | "queued"))
                            .map(|t| t.id.clone())
                            .collect();
                        for id in ids {
                            self.rpc_action(Command::Pause { id });
                        }
                    }
                    "Delete" => {
                        if let Some(id) = self.selected.clone() {
                            self.rpc_action(Command::Remove {
                                id,
                                delete_files: false,
                            });
                        }
                    }
                    "Delete Completed" => {
                        let ids: Vec<String> = snap
                            .tasks
                            .iter()
                            .filter(|t| t.state == "completed" || t.state == "seeding")
                            .map(|t| t.id.clone())
                            .collect();
                        for id in ids {
                            self.rpc_action(Command::Remove {
                                id,
                                delete_files: false,
                            });
                        }
                    }
                    "Options" => self.settings_open = true,
                    "Scheduler" => self.scheduler_open = true,
                    "Details" => {
                        if let Some(id) = self.selected.clone() {
                            self.details_id = Some(id);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let nav: [(&str, &str, &str); 7] = [
            ("all", "All", "▣"),
            ("downloading", "Downloading", "▼"),
            ("completed", "Completed", "✓"),
            ("torrent", "Torrents", "◈"),
            ("bundle", "Bundles", "▤"),
            ("video", "Video", "▶"),
            ("unfinished", "Unfinished", "○"),
        ];
        ui.add_space(4.0);
        for (id, label, glyph) in nav {
            let count = snap
                .tasks
                .iter()
                .filter(|t| match id {
                    "downloading" => {
                        matches!(t.state.as_str(), "running" | "queued" | "connecting")
                    }
                    "completed" => t.state == "completed" || t.state == "seeding",
                    "torrent" => t.kind == "torrent",
                    "bundle" => t.kind == "bundle",
                    "video" => t.kind == "video",
                    "unfinished" => t.state != "completed" && t.state != "seeding",
                    _ => true,
                })
                .count();
            if ui
                .selectable_label(self.category == id, format!("{glyph}  {label}  ({count})"))
                .clicked()
            {
                self.category = id.to_string();
            }
        }
        ui.add_space(10.0);
        ui.separator();
        if ui.button("Send & Receive").clicked() {
            self.notice = "LAN drop box is available from the daemon".into();
        }
    }

    fn task_list(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| match self.category.as_str() {
                "downloading" => matches!(t.state.as_str(), "running" | "queued" | "connecting"),
                "completed" => t.state == "completed" || t.state == "seeding",
                "torrent" => t.kind == "torrent",
                "bundle" => t.kind == "bundle",
                "video" => t.kind == "video",
                "unfinished" => t.state != "completed" && t.state != "seeding",
                _ => true,
            })
            .collect();
        if rows.is_empty() {
            ui.add_space(16.0);
            ui.weak("No downloads yet.");
            ui.weak("Drop a .torrent file here or press Add URL.");
            ui.add_space(6.0);
            if ui
                .add(egui::Button::new("Add URL").fill(accent()))
                .clicked()
            {
                self.add_open = true;
            }
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("tasks")
                .num_columns(8)
                .striped(true)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for header in [
                        "File Name",
                        "Kind",
                        "Size",
                        "Progress",
                        "Status",
                        "Rate",
                        "Last Try",
                        "Actions",
                    ] {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for t in rows {
                        let id = t.id.clone();
                        let sel = ui.selectable_label(
                            self.selected.as_deref() == Some(id.as_str()),
                            &t.name,
                        );
                        if sel.double_clicked() {
                            self.details_id = Some(id.clone());
                        }
                        if sel.clicked() {
                            self.selected = Some(id.clone());
                        }
                        sel.on_hover_text(format!("source: {}", t.source));
                        ui.colored_label(
                            kind_color(&t.kind),
                            format!("{} {}", kind_icon(&t.kind), display_kind(&t.kind)),
                        );
                        ui.label(t.total.map(humans).unwrap_or_else(|| "?".into()))
                            .on_hover_text(format!("{} done", humans(t.downloaded)));
                        let done = t.state == "completed" || t.state == "seeding";
                        let fill = if done {
                            status_color("completed")
                        } else {
                            status_color("running")
                        };
                        ui.add(
                            egui::ProgressBar::new(task_ratio(t))
                                .desired_width(64.0)
                                .fill(fill)
                                .show_percentage(),
                        );
                        ui.colored_label(status_color(&t.state), display_state(&t.state));
                        ui.label(
                            t.speed_bps
                                .map(|v| format!("{}/s", humans(v as u64)))
                                .unwrap_or_else(|| "—".into()),
                        );
                        ui.label(if t.last_try.is_empty() {
                            "—".to_string()
                        } else {
                            t.last_try.clone()
                        });
                        ui.horizontal(|ui| {
                            let action = match t.state.as_str() {
                                "running" | "queued" | "connecting" => "Pause",
                                "paused" | "failed" | "stale" => "Resume",
                                "completed" | "seeding" => "Open",
                                _ => "Resume",
                            };
                            if ui.small_button(action).clicked() {
                                match action {
                                    "Pause" => {
                                        self.rpc_action(Command::Pause { id: id.clone() });
                                    }
                                    "Resume" => {
                                        self.rpc_action(Command::Resume { id: id.clone() });
                                    }
                                    "Open" => {
                                        self.rpc_action(Command::Open {
                                            id: id.clone(),
                                            folder: t.kind == "bundle",
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            if ui.small_button("Delete").clicked() {
                                self.rpc_action(Command::Remove {
                                    id: id.clone(),
                                    delete_files: false,
                                });
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let running = snap.tasks.iter().filter(|t| t.state == "running").count();
        let total: u64 = snap.tasks.iter().map(|t| t.downloaded).sum();
        ui.horizontal_centered(|ui| {
            ui.label("●");
            ui.strong(format!("{running} running"));
            ui.separator();
            ui.label("rate");
            ui.strong(
                snap.total_bps
                    .map(|v| format!("{}/s", humans(v as u64)))
                    .unwrap_or_else(|| "—".into()),
            );
            ui.separator();
            ui.label("downloaded");
            ui.strong(humans(total));
            ui.separator();
            ui.label("limit");
            ui.strong(
                snap.global_limit
                    .map(|v| format!("{}/s", humans(v)))
                    .unwrap_or_else(|| "unlimited".into()),
            );
            for l in &snap.links {
                ui.separator();
                ui.label(&l.name);
                ui.strong(format!(
                    "{}/s · w{} · {} conn",
                    humans(l.rate_bps),
                    l.weight,
                    l.in_use
                ));
            }
            if snap.daemon.starts_with("offline") {
                ui.separator();
                ui.colored_label(status_color("failed"), "daemon offline");
            }
        });
    }

    fn show_add(&mut self, ctx: &egui::Context) {
        if !self.add_open {
            return;
        }
        let mut open = true;
        let mut cancel = false;
        let url = &mut self.url;
        let dest = &mut self.dest;
        let cat = &mut self.category_text;
        let headers = &mut self.headers_text;
        let video = &mut self.video;
        let torrent = &mut self.torrent;
        let sequential = &mut self.sequential;
        let advanced = &mut self.advanced_open;
        let notice = &mut self.notice;
        egui::Window::new("Add URL")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_size(egui::vec2(700.0, 280.0))
            .show(ctx, |ui| {
                ui.strong("Address");
                ui.add(
                    egui::TextEdit::singleline(url)
                        .hint_text("https://...")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.strong("Save to");
                    let _ = ui.add(
                        egui::TextEdit::singleline(dest)
                            .hint_text("Default download folder")
                            .desired_width(430.0),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Category");
                    let _ = ui.add(
                        egui::TextEdit::singleline(cat)
                            .hint_text("All")
                            .desired_width(180.0),
                    );
                });
                ui.separator();
                if ui
                    .small_button(if *advanced {
                        "Advanced ▾"
                    } else {
                        "Advanced ▸"
                    })
                    .clicked()
                {
                    *advanced = !*advanced;
                }
                if *advanced {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(video, "Video / stream page (yt-dlp)");
                        ui.checkbox(torrent, "Torrent payload");
                    });
                    ui.checkbox(sequential, "Torrent: sequential download");
                    ui.label("Extra headers (Name: value, one per line)");
                    let _ = ui.add(
                        egui::TextEdit::multiline(headers)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                }
                if !notice.is_empty() {
                    ui.add_space(4.0);
                    ui.weak(notice.as_str());
                }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new("Download").fill(accent()))
                        .clicked()
                    {
                        if url.trim().is_empty() {
                            *notice = "Enter an address first.".to_string();
                        } else {
                            let headers_map: std::collections::HashMap<String, String> = headers
                                .lines()
                                .filter_map(|line| {
                                    line.split_once(':')
                                        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                                })
                                .filter(|(k, _)| !k.is_empty())
                                .collect();
                            let kind = if *video {
                                ripdl_core::task::TaskKind::Video
                            } else if *torrent {
                                ripdl_core::task::TaskKind::Torrent
                            } else {
                                ripdl_core::task::TaskKind::Http
                            };
                            let dest_dir = if dest.trim().is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(dest.trim()))
                            };
                            let cmd = Command::Add {
                                url: url.trim().to_string(),
                                dest_dir,
                                headers: headers_map,
                                start_at: None,
                                kind,
                                sequential: *sequential,
                                only_files: None,
                                on_duplicate: ripdl_core::rpc::DuplicatePolicy::Share,
                            };
                            match rpc(cmd) {
                                Ok(Reply::Added { id }) => {
                                    *notice = format!("Queued {id}");
                                    cancel = true;
                                }
                                Ok(other) => {
                                    *notice = format!("Daemon answered: {other:?}");
                                }
                                Err(e) => {
                                    *notice = e;
                                }
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.add_open = open && !cancel;
    }

    fn setting_row(ui: &mut egui::Ui, label: &str, value: String) {
        ui.horizontal(|ui| {
            ui.add_sized(
                egui::vec2(210.0, 18.0),
                egui::Label::new(egui::RichText::new(label).strong()),
            );
            ui.label(value);
        });
    }

    fn show_scheduler(&mut self, ctx: &egui::Context) {
        if !self.scheduler_open {
            return;
        }
        let mut open = true;
        let tab = &mut self.scheduler_tab;
        let queue = &mut self.selected_queue;
        let mut one_time = true;
        egui::Window::new("Scheduler")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_size(egui::vec2(760.0, 460.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.heading("Queues");
                        for q in ["default", "fast", "slow"] {
                            if ui.selectable_label(*queue == q, q).clicked() {
                                *queue = q.to_string();
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(*tab == "Schedule", "Schedule")
                                .clicked()
                            {
                                *tab = "Schedule".into();
                            }
                            if ui
                                .selectable_label(
                                    *tab == "Files in the queue",
                                    "Files in the queue",
                                )
                                .clicked()
                            {
                                *tab = "Files in the queue".into();
                            }
                        });
                        ui.separator();
                        if tab == "Schedule" {
                            ui.horizontal(|ui| {
                                let _ = ui.button("Start Now");
                                let _ = ui.button("Stop");
                            });
                            ui.label(format!("Queue: {queue}"));
                            ui.checkbox(&mut one_time, "One-time downloading");
                            Self::setting_row(ui, "Start download at:", "-".into());
                            Self::setting_row(ui, "Stop download at:", "-".into());
                            Self::setting_row(ui, "Retries per file:", "engine default".into());
                            Self::setting_row(ui, "Files at once:", "daemon setting".into());
                            Self::setting_row(ui, "On finish:", "hooks later".into());
                        } else {
                            ui.label("Files in this queue appear here.");
                        }
                    });
                });
            });
        self.scheduler_open = open;
    }
}

#[derive(Clone)]
enum DetailsData {
    Torrent(TorrentDetailsSnapshot),
    Http(TaskDetailsSnapshot),
}

impl DetailsData {
    fn id(&self) -> &str {
        match self {
            DetailsData::Torrent(d) => &d.info_hash,
            DetailsData::Http(d) => &d.id,
        }
    }
    fn name(&self) -> String {
        match self {
            DetailsData::Torrent(d) => d.name.clone().unwrap_or_else(|| d.info_hash.clone()),
            DetailsData::Http(d) => d
                .dest_file
                .as_ref()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| host_of(&d.url)),
        }
    }
    fn state(&self) -> String {
        match self {
            DetailsData::Torrent(d) => {
                if d.paused {
                    "paused".into()
                } else if d.seeding {
                    "seeding".into()
                } else {
                    "running".into()
                }
            }
            DetailsData::Http(d) => format!("{:?}", d.state).to_lowercase(),
        }
    }
    fn total(&self) -> Option<u64> {
        match self {
            DetailsData::Torrent(d) => Some(d.total),
            DetailsData::Http(d) => d.total,
        }
    }
    fn downloaded(&self) -> u64 {
        match self {
            DetailsData::Torrent(d) => d.downloaded,
            DetailsData::Http(d) => d.downloaded,
        }
    }
    fn segments(&self) -> Vec<(usize, u64, String)> {
        match self {
            DetailsData::Torrent(_) => Vec::new(),
            DetailsData::Http(d) => d
                .segments
                .iter()
                .map(|s| (s.index, s.done, format!("{}..{}", s.start, s.end)))
                .collect(),
        }
    }
    fn files(&self) -> Vec<(String, u64, u64, bool)> {
        match self {
            DetailsData::Torrent(d) => d
                .files
                .iter()
                .map(|f| (f.name.clone(), f.size, f.downloaded, f.included))
                .collect(),
            DetailsData::Http(d) => d
                .parts
                .iter()
                .map(|p| {
                    let name = p.filename.clone().unwrap_or_else(|| {
                        p.url
                            .rsplit('/')
                            .next()
                            .unwrap_or(p.url.as_str())
                            .to_string()
                    });
                    (name, p.total.unwrap_or(0), p.downloaded, false)
                })
                .collect(),
        }
    }
    fn peers(&self) -> Vec<(String, Option<String>, String, u64, u64)> {
        match self {
            DetailsData::Torrent(d) => d
                .peers
                .iter()
                .map(|p| {
                    (
                        p.addr.clone(),
                        p.client.clone(),
                        p.state.clone(),
                        p.downloaded,
                        p.uploaded,
                    )
                })
                .collect(),
            DetailsData::Http(_) => Vec::new(),
        }
    }
    fn trackers(&self) -> Vec<String> {
        match self {
            DetailsData::Torrent(d) => d.trackers.clone(),
            DetailsData::Http(_) => Vec::new(),
        }
    }
    fn address(&self) -> Option<String> {
        match self {
            DetailsData::Torrent(_) => None,
            DetailsData::Http(d) => Some(d.url.clone()),
        }
    }
    fn dest_file(&self) -> Option<String> {
        match self {
            DetailsData::Torrent(d) => d.root.as_ref().map(|p| p.display().to_string()),
            DetailsData::Http(d) => d.dest_file.as_ref().map(|p| p.display().to_string()),
        }
    }
}

impl SpikeApp {
    fn show_details(&mut self, ctx: &egui::Context) {
        let Some(id) = self.details_id.clone() else {
            return;
        };
        let details = rpc(Command::Details { id }).ok();
        let data: Option<DetailsData> = details.and_then(|r| match r {
            Reply::TorrentDetails { details } => Some(DetailsData::Torrent(details)),
            Reply::TaskDetails { details } => Some(DetailsData::Http(details)),
            _ => None,
        });
        let mut open = true;
        egui::Window::new("Details")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(true)
            .default_size(egui::vec2(620.0, 500.0))
            .show(ctx, |ui| {
                let Some(data) = data.as_ref() else {
                    ui.label("Details unavailable. Is the daemon running?");
                    return;
                };
                ui.horizontal(|ui| {
                    ui.strong(&data.name());
                    ui.label(format!("state: {}", display_state(&data.state())));
                    ui.label(format!(
                        "size: {}",
                        data.total().map(humans).unwrap_or_else(|| "-".into())
                    ));
                    ui.label(format!("done: {}", humans(data.downloaded())));
                });
                if let Some(a) = data.address() {
                    ui.horizontal(|ui| {
                        ui.strong("Address");
                        ui.add(egui::Label::new(a.as_str()).truncate())
                            .on_hover_text(a.as_str());
                    });
                }
                if let Some(f) = data.dest_file() {
                    ui.horizontal(|ui| {
                        ui.strong("File");
                        ui.add(egui::Label::new(f.as_str()).truncate())
                            .on_hover_text(f.as_str());
                    });
                }
                ui.separator();
                for t in ["Files", "Peers", "Trackers", "Segments"] {
                    if ui.selectable_label(&self.details_tab == t, t).clicked() {
                        self.details_tab = t.into();
                    }
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| match self.details_tab.as_str() {
                        "Files" => {
                            let files = data.files();
                            if files.is_empty() {
                                ui.label("No file list for this task.");
                            }
                            for (name, size, done, included) in files {
                                let ratio = if size > 0 {
                                    (done as f32 / size as f32).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                };
                                let fill = if ratio >= 1.0 {
                                    status_color("completed")
                                } else {
                                    status_color("running")
                                };
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(name.as_str()).truncate())
                                        .on_hover_text(name.as_str());
                                    ui.add(
                                        egui::ProgressBar::new(ratio)
                                            .desired_width(90.0)
                                            .fill(fill)
                                            .show_percentage(),
                                    );
                                    ui.label(format!("{}/{}", humans(done), humans(size)));
                                    if included {
                                        ui.label("*");
                                    }
                                });
                            }
                        }
                        "Peers" => {
                            let peers = data.peers();
                            if peers.is_empty() {
                                ui.label("No peers.");
                            }
                            for (addr, client, state, dl, ul) in peers {
                                ui.horizontal(|ui| {
                                    ui.label(&addr);
                                    ui.label(client.clone().unwrap_or_else(|| "-".into()));
                                    ui.label(state.clone());
                                    ui.label(format!("dl {}/s", humans(dl)));
                                    ui.label(format!("ul {}/s", humans(ul)));
                                });
                            }
                        }
                        "Trackers" => {
                            let t = data.trackers();
                            if t.is_empty() {
                                ui.label("No trackers.");
                            }
                            for url in t {
                                ui.label(&url);
                            }
                        }
                        "Segments" => {
                            let segs = data.segments();
                            if segs.is_empty() {
                                ui.label("No segments.");
                            }
                            for (i, done, range) in segs {
                                ui.horizontal(|ui| {
                                    ui.label(format!("#{i}"));
                                    ui.label(format!("{}/{}", humans(done), range));
                                });
                            }
                        }
                        _ => {}
                    });
                ui.add_space(8.0);
                let id = data.id().to_string();
                if ui.button("Open file").clicked() {
                    self.rpc_action(Command::Open {
                        id: id.clone(),
                        folder: false,
                    });
                }
                if ui.button("Open folder").clicked() {
                    self.rpc_action(Command::Open { id, folder: true });
                }
            });
        if !open {
            self.details_id = None;
        }
    }
}

impl SpikeApp {
    fn control_label(ui: &mut egui::Ui, label: &str) {
        ui.add_sized(
            egui::vec2(210.0, 18.0),
            egui::Label::new(egui::RichText::new(label).strong()),
        );
    }

    fn dir_row(ui: &mut egui::Ui, label: &str, text: &mut String) -> bool {
        ui.horizontal(|ui| {
            Self::control_label(ui, label);
            ui.add(egui::TextEdit::singleline(text).desired_width(320.0))
                .changed()
        })
        .inner
    }

    fn bool_row(ui: &mut egui::Ui, label: &str, value: &mut Option<bool>, default: bool) -> bool {
        let mut v = value.unwrap_or(default);
        let changed = ui
            .horizontal(|ui| {
                Self::control_label(ui, label);
                ui.checkbox(&mut v, "").changed()
            })
            .inner;
        if changed {
            *value = Some(v);
        }
        changed
    }

    fn usize_row(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut Option<usize>,
        default: usize,
        min: u32,
        max: u32,
    ) -> bool {
        let mut v = value.unwrap_or(default) as u32;
        let changed = ui
            .horizontal(|ui| {
                Self::control_label(ui, label);
                ui.add(egui::DragValue::new(&mut v).range(min..=max))
                    .changed()
            })
            .inner;
        if changed {
            *value = Some(v as usize);
        }
        changed
    }

    fn u32_row(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut Option<u32>,
        default: u32,
        min: u32,
        max: u32,
    ) -> bool {
        let mut v = value.unwrap_or(default);
        let changed = ui
            .horizontal(|ui| {
                Self::control_label(ui, label);
                ui.add(egui::DragValue::new(&mut v).range(min..=max))
                    .changed()
            })
            .inner;
        if changed {
            *value = Some(v);
        }
        changed
    }

    fn f64_row(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut Option<f64>,
        default: f64,
        min: f64,
        max: f64,
        speed: f64,
    ) -> bool {
        let mut v = value.unwrap_or(default);
        let changed = ui
            .horizontal(|ui| {
                Self::control_label(ui, label);
                ui.add(egui::DragValue::new(&mut v).speed(speed).range(min..=max))
                    .changed()
            })
            .inner;
        if changed {
            *value = Some(v);
        }
        changed
    }

    fn mib_row(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut Option<u64>,
        default_mib: u64,
        min: u64,
        max: u64,
    ) -> bool {
        let mut v = value.map(|b| b / (1024 * 1024)).unwrap_or(default_mib);
        let changed = ui
            .horizontal(|ui| {
                Self::control_label(ui, label);
                ui.add(egui::DragValue::new(&mut v).range(min..=max).suffix(" MiB"))
                    .changed()
            })
            .inner;
        if changed {
            *value = Some(v.saturating_mul(1024 * 1024));
        }
        changed
    }

    fn speed_row(ui: &mut egui::Ui, label: &str, value: &mut Option<u64>) -> bool {
        let mut enabled = value.is_some();
        let mut mib = value.map(|b| b as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
        let mut changed = false;
        ui.horizontal(|ui| {
            Self::control_label(ui, label);
            if ui.checkbox(&mut enabled, "Limit").changed() {
                changed = true;
                if enabled {
                    mib = mib.max(1.0);
                    *value = Some((mib * 1024.0 * 1024.0) as u64);
                } else {
                    *value = None;
                }
            }
            if enabled
                && ui
                    .add(
                        egui::DragValue::new(&mut mib)
                            .range(1.0..=100_000.0)
                            .suffix(" MiB/s"),
                    )
                    .changed()
            {
                changed = true;
                *value = Some((mib * 1024.0 * 1024.0) as u64);
            }
        });
        changed
    }

    #[allow(clippy::too_many_lines)]
    fn show_settings_editable(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let snapshot = self.state.lock().unwrap().settings.clone();
        let offline = snapshot.is_none();
        if self.settings_edit.is_none() {
            let init = snapshot.clone().unwrap_or_default();
            self.settings_dir_text = init
                .default_download_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            self.settings_edit = Some(init);
            self.settings_dirty = false;
        }

        let mut edit = self.settings_edit.clone().unwrap();
        let mut dir_text = std::mem::take(&mut self.settings_dir_text);
        let mut dirty = self.settings_dirty;
        let mut save = false;
        let mut revert = false;
        let mut notice = self.notice.clone();
        let tab = &mut self.settings_tab;
        let tabs = [
            "General",
            "File Types",
            "Connection",
            "Save To",
            "Downloads",
            "Torrents",
            "Bundles",
            "Browser",
            "Updates",
            "Diagnostics",
        ];
        let mut open = true;
        egui::Window::new("Options")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_size(egui::vec2(760.0, 380.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.strong("Options");
                        ui.add_space(6.0);
                        for t in tabs {
                            if ui.selectable_label(*tab == t, t).clicked() {
                                *tab = t.to_string();
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.heading(tab.as_str());
                            if dirty {
                                ui.colored_label(status_color("paused"), "unsaved");
                            }
                        });
                        if offline {
                            ui.weak("daemon offline - showing defaults");
                        }
                        egui::ScrollArea::vertical()
                            .max_height(330.0)
                            .show(ui, |ui| match tab.as_str() {
                                "General" => {
                                    if Self::dir_row(ui, "Download folder", &mut dir_text) {
                                        dirty = true;
                                    }
                                    if Self::bool_row(
                                        ui,
                                        "Hide to tray",
                                        &mut edit.hide_to_tray,
                                        true,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::bool_row(
                                        ui,
                                        "Check for updates",
                                        &mut edit.update_check,
                                        true,
                                    ) {
                                        dirty = true;
                                    }
                                }
                                "File Types" => {
                                    Self::setting_row(
                                        ui,
                                        "Capture types",
                                        "documents, archives, media, video".into(),
                                    );
                                    Self::setting_row(ui, "Site exclusions", "none".into());
                                    Self::setting_row(ui, "Address exceptions", "none".into());
                                }
                                "Connection" => {
                                    if Self::usize_row(
                                        ui,
                                        "Downloads at once",
                                        &mut edit.max_concurrent,
                                        3,
                                        1,
                                        64,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::usize_row(
                                        ui,
                                        "Connections/download",
                                        &mut edit.max_segments,
                                        8,
                                        1,
                                        32,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::mib_row(
                                        ui,
                                        "Min segment size",
                                        &mut edit.min_segment_size,
                                        1,
                                        1,
                                        1024,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::speed_row(
                                        ui,
                                        "Total speed limit",
                                        &mut edit.max_speed_bytes,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::bool_row(
                                        ui,
                                        "Bond interfaces",
                                        &mut edit.bond_interfaces,
                                        false,
                                    ) {
                                        dirty = true;
                                    }
                                }
                                "Save To" => {
                                    if Self::dir_row(ui, "Default folder", &mut dir_text) {
                                        dirty = true;
                                    }
                                    Self::setting_row(
                                        ui,
                                        "Temp directory",
                                        "alongside download".into(),
                                    );
                                    Self::setting_row(
                                        ui,
                                        "Remember last path",
                                        "per category".into(),
                                    );
                                }
                                "Downloads" => {
                                    for (k, v) in [
                                        ("Progress dialog", "inline"),
                                        ("Complete dialog", "toast + row"),
                                        ("Silent mode", "no modals"),
                                        ("Start immediately", "explicit Add"),
                                        ("Resume mtime check", "daemon policy"),
                                    ] {
                                        Self::setting_row(ui, k, v.into());
                                    }
                                }
                                "Torrents" => {
                                    if Self::bool_row(
                                        ui,
                                        "Seed finished torrents",
                                        &mut edit.seed_enabled,
                                        true,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::f64_row(
                                        ui,
                                        "Seed ratio",
                                        &mut edit.seed_ratio,
                                        1.0,
                                        0.0,
                                        10.0,
                                        0.1,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::u32_row(
                                        ui,
                                        "Seed time (min)",
                                        &mut edit.seed_time_min,
                                        0,
                                        0,
                                        100_000,
                                    ) {
                                        dirty = true;
                                    }
                                    Self::setting_row(ui, "Per-task cap", "set when adding".into());
                                }
                                "Bundles" => {
                                    Self::setting_row(ui, "Auto-extract", "not implemented".into());
                                    Self::setting_row(
                                        ui,
                                        "Part-set policy",
                                        "wait for complete set".into(),
                                    );
                                }
                                "Browser" => {
                                    Self::setting_row(ui, "Extension", "connected".into());
                                    Self::setting_row(ui, "Capture", "user-confirmable".into());
                                    Self::setting_row(ui, "Referer rules", "offer-first".into());
                                }
                                "Updates" => {
                                    if Self::bool_row(
                                        ui,
                                        "Check at startup",
                                        &mut edit.update_check,
                                        true,
                                    ) {
                                        dirty = true;
                                    }
                                    if Self::bool_row(
                                        ui,
                                        "Install automatically",
                                        &mut edit.update_auto_install,
                                        false,
                                    ) {
                                        dirty = true;
                                    }
                                    Self::setting_row(ui, "Verification", "SHA256SUMS".into());
                                }
                                "Diagnostics" => {
                                    Self::setting_row(ui, "Log level", "info".into());
                                    Self::setting_row(ui, "Telemetry", "off (fixed)".into());
                                    if ui.button("Copy diagnostics").clicked() {
                                        notice = "Diagnostics export is not wired yet".into();
                                    }
                                }
                                _ => {}
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new("Save").fill(accent())).clicked() {
                                save = true;
                            }
                            if ui.button("Revert").clicked() {
                                revert = true;
                            }
                            if !notice.is_empty() {
                                ui.weak(notice.as_str());
                            }
                        });
                    });
                });
            });

        if revert {
            self.settings_edit = None;
            self.settings_dirty = false;
            self.settings_dir_text.clear();
            self.notice = "Settings reverted".into();
            self.settings_open = open;
            return;
        }

        edit.default_download_dir = if dir_text.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(dir_text.trim()))
        };
        self.settings_dir_text = dir_text;
        self.settings_edit = Some(edit.clone());
        self.settings_dirty = dirty;
        if save {
            match rpc(Command::SetConfig { settings: edit }) {
                Ok(_) => {
                    self.notice = "Settings saved".into();
                    self.settings_dirty = false;
                }
                Err(e) => {
                    self.notice = e;
                }
            }
        } else if !notice.is_empty() {
            self.notice = notice;
        }
        if !open {
            self.settings_edit = None;
        }
        self.settings_open = open;
    }
}

impl eframe::App for SpikeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(font) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Body) {
            font.size = 14.0;
        }
        let snap = { self.state.lock().unwrap().clone() };
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &snap));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &snap));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &snap));
        egui::Panel::left("categories")
            .default_size(160.0)
            .show(ui, |ui| self.sidebar(ui, &snap));
        egui::CentralPanel::default().show(ui, |ui| self.task_list(ui, &snap));
        let ctx = ui.ctx().clone();
        self.show_add(&ctx);
        self.show_settings_editable(&ctx);
        self.show_scheduler(&ctx);
        self.show_details(&ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(egui::vec2(1180.0, 720.0)),
        ..Default::default()
    };
    eframe::run_native(
        "ripdl",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}
