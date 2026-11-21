//! Native UI spike for ripdl: egui/eframe, no WebView2.
//!
//! It reads the daemon's v1 `List`/`GetConfig` commands and the unified
//! `GetTrafficSnapshot` when the daemon is built with `--features unified-traffic`.
//! No rate, total or ETA is computed here: the daemon is the only source.

use std::io::{BufRead, BufReader, Write};
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
    state: String,
    downloaded: u64,
    total: Option<u64>,
    speed_bps: Option<f64>,
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

    let mut reader = BufReader::new(stream);
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
                state: format!("{:?}", t.state).to_lowercase(),
                downloaded: t.downloaded,
                total: t.total,
                speed_bps: t.speed_bps,
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
        Ok(Reply::Error { .. }) => {
            snap.daemon = "connected (legacy daemon)".into();
        }
        Err(_) => {}
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
    settings_open: bool,
}

impl SpikeApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            state: spawn_poller(),
            category: "all".into(),
            selected: None,
            add_open: false,
            url: String::new(),
            dest: String::new(),
            settings_open: false,
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("ripdl");
        ui.separator();
        for (id, label) in [
            ("all", "All downloads"),
            ("running", "Downloading"),
            ("completed", "Completed"),
            ("categories", "Categories"),
        ] {
            if ui.selectable_label(self.category == id, label).clicked() {
                self.category = id.to_string();
            }
        }
        ui.separator();
        if ui.button("Add URL").clicked() {
            self.add_open = true;
        }
        if ui.button("Settings").clicked() {
            self.settings_open = true;
        }
    }

    fn task_list(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| match self.category.as_str() {
                "running" => t.state == "running" || t.state == "queued",
                "completed" => t.state == "completed" || t.state == "seeding",
                _ => true,
            })
            .collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("tasks")
                .num_columns(5)
                .striped(true)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.strong("Name");
                    ui.strong("State");
                    ui.strong("Size");
                    ui.strong("Speed");
                    ui.strong("Done");
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
                        ui.label(&t.state);
                        ui.label(t.total.map(humans).unwrap_or_else(|| "?".into()));
                        ui.label(
                            t.speed_bps
                                .map(|v| format!("{}/s", humans(v as u64)))
                                .unwrap_or_else(|| "-".into()),
                        );
                        let pct = match t.total {
                            Some(total) if total > 0 => {
                                (t.downloaded as f64 / total as f64) * 100.0
                            }
                            _ => 0.0,
                        };
                        ui.label(format!("{pct:.0}%"));
                        ui.end_row();
                    }
                });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("daemon: {}", snap.daemon));
            ui.label("rate:");
            ui.strong(match snap.total_bps {
                Some(v) => format!("{}/s", humans(v as u64)),
                None => "n/a (legacy daemon)".into(),
            });
            ui.label("limit:");
            ui.strong(
                snap.global_limit
                    .map(|v| format!("{}/s", humans(v)))
                    .unwrap_or_else(|| "unlimited".into()),
            );
            if !snap.links.is_empty() {
                ui.label("links:");
                for l in &snap.links {
                    ui.label(format!(
                        "{} {}/s w{} ({} conn)",
                        l.name,
                        humans(l.rate_bps),
                        l.weight,
                        l.in_use
                    ));
                }
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.add_open {
            let mut open = true;
            let mut cancel = false;
            let url = &mut self.url;
            let dest = &mut self.dest;
            egui::Window::new("Add URL")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("URL");
                    ui.text_edit_singleline(url);
                    ui.label("Save to (optional)");
                    ui.text_edit_singleline(dest);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        ui.add_enabled(false, egui::Button::new("Download"));
                    });
                });
            self.add_open = open && !cancel;
        }
        if self.settings_open {
            let mut open = true;
            let settings = self.state.lock().unwrap().settings.clone();
            egui::Window::new("Settings")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.heading("Network");
                    if let Some(s) = &settings {
                        ui.label(format!(
                            "total speed limit: {}",
                            s.max_speed_bytes
                                .map(humans)
                                .unwrap_or_else(|| "unlimited".into())
                        ));
                        ui.label(format!(
                            "bonding: {}",
                            if s.bond_interfaces.unwrap_or(false) {
                                "on"
                            } else {
                                "off"
                            }
                        ));
                    } else {
                        ui.label("settings unavailable");
                    }
                });
            self.settings_open = open;
        }
    }
}

impl eframe::App for SpikeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snap = { self.state.lock().unwrap().clone() };
        egui::Panel::left("nav").show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Downloads");
            self.task_list(ui, &snap);
            self.status_bar(ui, &snap);
        });
        let ctx = ui.ctx().clone();
        self.dialogs(&ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "ripdl native UI spike",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}
