//! Native UI spike for ripdl: IDM-style shell, adapted to ripdl features.
//! eframe/egui, no WebView2. All numbers come from the daemon snapshot.

use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use ripdl_core::rpc::{Command, Reply, Request, Response};

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
        "torrent" => "[T]",
        "video" => "[V]",
        "bundle" => "[B]",
        _ => "[D]",
    }
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

fn spawn_poller() -> Arc<Mutex<Snapshot>> {
    let state = Arc::new(Mutex::new(poll_once()));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || loop {
        let next = poll_once();
        *shared.lock().unwrap() = next;
        std::thread::sleep(Duration::from_secs(1));
    });
    state
}

struct SpikeApp {
    state: Arc<Mutex<Snapshot>>,
    category: String,
    selected: Option<String>,
    add_open: bool,
    url: String,
    dest: String,
    notice: String,
    settings_open: bool,
    settings_tab: String,
    scheduler_open: bool,
    scheduler_tab: String,
}

impl SpikeApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let screen = std::env::var("RIPDL_QA_SCREEN").unwrap_or_default();
        Self {
            state: spawn_poller(),
            category: "all".into(),
            selected: None,
            add_open: screen == "add",
            url: String::new(),
            dest: String::new(),
            notice: String::new(),
            settings_open: screen == "settings",
            settings_tab: "General".into(),
            scheduler_open: screen == "scheduler",
            scheduler_tab: "Schedule".into(),
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for label in ["Tasks", "File", "Downloads", "View", "Tools", "Help"] {
                let _ = ui.button(label);
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let actions: [(&str, &str); 11] = [
            ("+", "Add URL"),
            ("R", "Resume"),
            ("S", "Stop"),
            ("SA", "Stop All"),
            ("X", "Delete"),
            ("XC", "Delete C..."),
            ("G", "Options"),
            ("C", "Scheduler"),
            ("PQ", "Start Qu..."),
            ("SQ", "Stop Qu..."),
            ("F", "Refresh"),
        ];
        ui.horizontal(|ui| {
            for (icon, label) in actions {
                let text = format!("{icon}\n{label}");
                ui.add(egui::Button::new(text).min_size(egui::vec2(66.0, 48.0)));
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Categories");
        ui.separator();
        let categories = [
            ("all", "All Downloads"),
            ("downloading", "Downloading"),
            ("completed", "Completed"),
            ("torrent", "Torrents"),
            ("bundle", "Bundles"),
            ("video", "Video"),
            ("unfinished", "Unfinished"),
            ("grabber", "Grabber projects"),
            ("queues", "Queues"),
        ];
        for (id, label) in categories {
            if ui.selectable_label(self.category == id, label).clicked() {
                self.category = id.to_string();
            }
        }
        ui.separator();
        ui.collapsing("All Downloads", |ui| {
            for sub in ["Compressed", "Documents", "Music", "Programs"] {
                ui.label(sub);
            }
        });
        if ui.button("Send & Receive").clicked() {
            self.notice = "LAN drop box is available from the daemon".into();
        }
    }

    fn task_list(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| match self.category.as_str() {
                "downloading" => {
                    t.state == "running" || t.state == "queued" || t.state == "connecting"
                }
                "completed" => t.state == "completed" || t.state == "seeding",
                "torrent" => t.kind == "torrent",
                "bundle" => t.kind == "bundle",
                "video" => t.kind == "video",
                "unfinished" => t.state != "completed" && t.state != "seeding",
                _ => true,
            })
            .collect();
        if rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(70.0);
                ui.heading("Nothing here yet");
                ui.label("Add a URL or drop a .torrent file to start a download.");
            });
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
                        "Status",
                        "Time left",
                        "Transfer rate",
                        "Last Try",
                        "Description",
                    ] {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for t in rows {
                        let id = t.id.clone();
                        if ui
                            .selectable_label(
                                self.selected.as_deref() == Some(id.as_str()),
                                &t.name,
                            )
                            .clicked()
                        {
                            self.selected = Some(id);
                        }
                        ui.label(format!("{} {}", kind_icon(&t.kind), t.kind));
                        ui.label(t.total.map(humans).unwrap_or_else(|| "?".into()));
                        ui.label(&t.state);
                        ui.label("-");
                        ui.label(
                            t.speed_bps
                                .map(|v| format!("{}/s", humans(v as u64)))
                                .unwrap_or_else(|| "-".into()),
                        );
                        ui.label(if t.last_try.is_empty() {
                            "-"
                        } else {
                            &t.last_try
                        });
                        ui.label(format!(
                            "{}/{}",
                            humans(t.downloaded),
                            t.total.map(humans).unwrap_or_else(|| "?".into())
                        ));
                        ui.end_row();
                    }
                });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label(format!("daemon: {}", snap.daemon));
            ui.label("rate:");
            ui.strong(match snap.total_bps {
                Some(v) => format!("{}/s", humans(v as u64)),
                None => "n/a".into(),
            });
            ui.label("limit:");
            ui.strong(
                snap.global_limit
                    .map(|v| format!("{}/s", humans(v)))
                    .unwrap_or_else(|| "unlimited".into()),
            );
            for l in &snap.links {
                ui.label(format!(
                    "{} {}/s w{} ({} conn)",
                    l.name,
                    humans(l.rate_bps),
                    l.weight,
                    l.in_use
                ));
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
        let notice = &mut self.notice;
        egui::Window::new("Add URL")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("URL");
                ui.text_edit_singleline(url);
                ui.label("Save to (optional)");
                ui.text_edit_singleline(dest);
                if !notice.is_empty() {
                    ui.label(notice.as_str());
                }
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Download").clicked() {
                        *notice = "UI spike: add is not wired yet".to_string();
                    }
                });
            });
        self.add_open = open && !cancel;
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let settings = self.state.lock().unwrap().settings.clone();
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
        egui::Window::new("Options")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_size(egui::vec2(620.0, 420.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for t in tabs {
                        if ui.selectable_label(*tab == t, t).clicked() {
                            *tab = t.to_string();
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(tab.as_str());
                    if let Some(s) = &settings {
                        match tab.as_str() {
                            "General" => {
                                ui.label(format!("download folder: {}", s.default_download_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(user Downloads)".into())));
                                ui.label(format!("hide to tray: {}", s.hide_to_tray.unwrap_or(true)));
                            }
                            "Connection" => {
                                ui.label(format!("total speed limit: {}", s.max_speed_bytes.map(humans).unwrap_or_else(|| "unlimited".into())));
                                ui.label(format!("connections per download: {}", s.max_segments.unwrap_or(8)));
                                ui.label(format!("min segment: {}", humans(s.min_segment_size.unwrap_or(1024 * 1024))));
                                ui.label(format!("bonding: {}", if s.bond_interfaces.unwrap_or(false) { "on" } else { "off" }));
                            }
                            "Torrents" => {
                                ui.label(format!("seeding: {}", if s.seed_enabled.unwrap_or(true) { "on" } else { "off" }));
                                ui.label(format!("seed ratio: {}", s.seed_ratio.unwrap_or(1.0)));
                                ui.label(format!("seed time (min): {}", s.seed_time_min.unwrap_or(0)));
                            }
                            "Browser" => {
                                ui.label("extension status: available");
                                ui.label("capture policy: user-confirmable");
                            }
                            "Updates" => {
                                ui.label(format!("update check: {}", if s.update_check.unwrap_or(true) { "on" } else { "off" }));
                                ui.label(format!("auto-install: {}", if s.update_auto_install.unwrap_or(false) { "on" } else { "off" }));
                            }
                            _ => {
                                ui.label("Settings are generated from the daemon schema in the final UI.");
                            }
                        }
                    } else {
                        ui.label("settings unavailable");
                    }
                });
            });
        self.settings_open = open;
    }

    fn show_scheduler(&mut self, ctx: &egui::Context) {
        if !self.scheduler_open {
            return;
        }
        let mut open = true;
        let tab = &mut self.scheduler_tab;
        let mut one_time = true;
        egui::Window::new("Scheduler")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .default_size(egui::vec2(620.0, 420.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(*tab == "Schedule", "Schedule")
                        .clicked()
                    {
                        *tab = "Schedule".into();
                    }
                    if ui
                        .selectable_label(*tab == "Files in the queue", "Files in the queue")
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
                    ui.label("Queue: default");
                    ui.checkbox(&mut one_time, "One-time downloading");
                    ui.label("Start download at: -");
                    ui.label("Stop download at: -");
                    ui.label("Number of retries for each file: engine default");
                    ui.label("Files at the same time: daemon setting");
                    ui.label("Turn off computer when done: excluded in ripdl");
                } else {
                    ui.label("No files in queue (spike view).");
                }
            });
        self.scheduler_open = open;
    }
}

impl eframe::App for SpikeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(font) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Body) {
            font.size = 14.0;
        }
        let snap = { self.state.lock().unwrap().clone() };
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &snap));
        egui::Panel::left("categories")
            .default_size(175.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.task_list(ui, &snap));
        let ctx = ui.ctx().clone();
        self.show_add(&ctx);
        self.show_settings(&ctx);
        self.show_scheduler(&ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(egui::vec2(1180.0, 720.0)),
        ..Default::default()
    };
    eframe::run_native(
        "ripdl native UI spike",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}
