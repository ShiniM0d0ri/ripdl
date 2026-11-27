#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Ripdl Desktop GUI (`eframe`/`egui`) — fast virtualized desktop shell over daemon RPC.

mod ext_host;
mod lucide_icons;
mod model;
mod rpc_worker;
mod single_instance;
mod theme;
mod tray;
mod vectors;
mod views;

use std::collections::HashSet;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, Stroke, StrokeKind};
use ripdl_core::categories::Category;
use ripdl_core::task::TaskState;
use ripdl_core::torrent::looks_like_torrent_payload;

use crate::model::{
    count_filter, eta_text, humans_bin, matches_filter, sort_tasks, should_auto_open_onboarding,
    AddDraft, AddMode, CachedDetails, DeleteConfirm, DetailsDraft, DuplicatePrompt,
    FilePickTarget, LanDraft, OnboardingDraft, RefreshDraft, SettingsDraft, SettingsTab,
    SidebarFilter, Snapshot, SortDir, SortKey, TaskRow, UiNotice,
};
use crate::rpc_worker::{is_installing, spawn_worker, UiCommand, UiEvent, WorkerHandle};
use crate::theme::{
    apply_visuals, configure_style, dialog_header, kind_visuals, metrics, paint_kind_pill,
    paint_progress_bar, paint_sparkline, paint_status_pill, semibold, state_visuals, Palette,
    ThemeMode,
};
use crate::tray::{TrayAction, TrayHandle};
use crate::vectors::{
    paint_brand_logo, paint_empty_illustration, paint_file_tile, paint_filter_empty_illustration,
    paint_focus_ring, paint_icon, vector_button, vector_icon_button, IconId,
};

struct RipdlNativeApp {
    worker: WorkerHandle,
    tray_rx: mpsc::Receiver<TrayAction>,
    _tray: Option<TrayHandle>,
    theme_mode: ThemeMode,
    filter: SidebarFilter,
    sort_key: SortKey,
    sort_dir: SortDir,
    search: String,
    focus_search: bool,
    show_eta: bool,
    show_peers: bool,
    selected_ids: HashSet<String>,
    anchor_id: Option<String>,
    details: DetailsDraft,
    cached_details: CachedDetails,
    add: AddDraft,
    refresh: RefreshDraft,
    lan: LanDraft,
    settings: SettingsDraft,
    onboarding: OnboardingDraft,
    onboarding_checked: bool,
    delete_confirm: Option<DeleteConfirm>,
    duplicate_prompt: Option<DuplicatePrompt>,
    about_open: bool,
    update_banner_dismissed: bool,
    hide_to_tray: bool,
    force_quit: bool,
    notice: Option<UiNotice>,
    last_details_poll: Instant,
    last_lan_poll: Instant,
    saved_window_size: Option<egui::Vec2>,
}

impl RipdlNativeApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        tray_rx: mpsc::Receiver<TrayAction>,
        tray: Option<TrayHandle>,
    ) -> Self {
        let theme_mode = load_theme_mode();
        configure_style(&cc.egui_ctx, theme_mode);
        let worker = spawn_worker(cc.egui_ctx.clone());

        Self {
            worker,
            tray_rx,
            _tray: tray,
            theme_mode,
            filter: SidebarFilter::All,
            sort_key: SortKey::Name,
            sort_dir: SortDir::Asc,
            search: String::new(),
            focus_search: false,
            show_eta: true,
            show_peers: false,
            selected_ids: HashSet::new(),
            anchor_id: None,
            details: DetailsDraft::default(),
            cached_details: CachedDetails::default(),
            add: AddDraft::default(),
            refresh: RefreshDraft::default(),
            lan: LanDraft::default(),
            settings: SettingsDraft::default(),
            onboarding: OnboardingDraft::default(),
            onboarding_checked: false,
            delete_confirm: None,
            duplicate_prompt: None,
            about_open: false,
            update_banner_dismissed: false,
            hide_to_tray: true,
            force_quit: false,
            notice: None,
            last_details_poll: Instant::now(),
            last_lan_poll: Instant::now(),
            saved_window_size: None,
        }
    }

    fn set_notice(&mut self, text: String, is_error: bool) {
        self.notice = Some(UiNotice {
            text,
            is_error,
            until: Instant::now() + Duration::from_secs(5),
        });
    }

    fn any_modal_open(&self) -> bool {
        self.duplicate_prompt.is_some()
            || self.onboarding.open
            || self.add.open
            || self.settings.open
            || self.lan.open
            || self.refresh.open
            || self.details.task_id.is_some()
            || self.delete_confirm.is_some()
            || self.about_open
    }

    fn drain_worker_events(&mut self) {
        for ev in self.worker.drain_events() {
            match ev {
                UiEvent::Notice { text, is_error } => self.set_notice(text, is_error),
                UiEvent::Details {
                    task_id,
                    http,
                    torrent,
                    error,
                } => {
                    self.cached_details = CachedDetails {
                        task_id,
                        loading: false,
                        http,
                        torrent,
                        error,
                    };
                }
                UiEvent::TorrentPreview { payload, result } => {
                    if self.add.torrent_preview_for == payload {
                        self.add.torrent_preview_busy = false;
                        match result {
                            Ok(prev) => {
                                self.add.torrent_selected = (0..prev.files.len()).collect();
                                self.add.torrent_preview = Some(prev);
                                self.add.torrent_preview_error = None;
                            }
                            Err(e) => self.add.torrent_preview_error = Some(e),
                        }
                    }
                }
                UiEvent::BundlePreview(res) => {
                    self.add.bundle_preview_busy = false;
                    match res {
                        Ok(groups) => {
                            if let Some(first) = groups.first() {
                                if self.add.bundle_name.trim().is_empty() {
                                    self.add.bundle_name = first.name.clone();
                                }
                            }
                            self.add.bundle_preview = Some(groups);
                            self.add.bundle_preview_error = None;
                        }
                        Err(e) => self.add.bundle_preview_error = Some(e),
                    }
                }
                UiEvent::RefreshProbe { url, result } => {
                    if self.refresh.url.trim() == url {
                        self.refresh.probing = false;
                        match result {
                            Ok((kept, total, warning)) => {
                                self.refresh.verdict_ok = true;
                                let tot = total.map(humans_bin).unwrap_or_else(|| "?".into());
                                self.refresh.verdict_text = match warning {
                                    Some(w) => format!("Keeps {} of {tot} — {w}", humans_bin(kept)),
                                    None => format!("Verified — keeps {} of {tot} already on disk", humans_bin(kept)),
                                };
                            }
                            Err(e) => {
                                self.refresh.verdict_ok = false;
                                self.refresh.verdict_text = e;
                            }
                        }
                    }
                }
                UiEvent::RefreshCommitted { id, result } => {
                    self.refresh.submitting = false;
                    match result {
                        Ok((kept, total)) => {
                            self.refresh.open = false;
                            let tot = total.map(humans_bin).unwrap_or_else(|| "?".into());
                            self.set_notice(
                                format!("Address refreshed (kept {} of {tot}).", humans_bin(kept)),
                                false,
                            );
                            if self.details.task_id.as_deref() == Some(id.as_str()) {
                                self.worker.send(UiCommand::FetchDetails { id });
                            }
                        }
                        Err(e) => self.refresh.error = Some(e),
                    }
                }
                UiEvent::AddPartsResult { id, result } => match result {
                    Ok(added) => {
                        self.details.add_parts_open = false;
                        self.details.add_parts_text.clear();
                        let msg = if added == 0 {
                            "No new parts added (already present).".to_string()
                        } else {
                            format!("Added {added} missing part(s) — bundle restarting.")
                        };
                        self.set_notice(msg, false);
                        self.worker.send(UiCommand::FetchDetails { id });
                    }
                    Err(e) => self.details.add_parts_error = Some(e),
                },
                UiEvent::AddDone(res) => {
                    self.add.submitting = false;
                    match res {
                        Ok(new_id) => {
                            self.add.open = false;
                            self.duplicate_prompt = None;
                            self.selected_ids.clear();
                            self.selected_ids.insert(new_id.clone());
                            self.anchor_id = Some(new_id);
                            self.set_notice("Download added to queue.".into(), false);
                        }
                        Err(e) => self.add.error = Some(e),
                    }
                }
                UiEvent::DuplicatePrompt { request, existing } => {
                    self.add.submitting = false;
                    self.add.open = false;
                    self.duplicate_prompt = Some(DuplicatePrompt { request, existing });
                }
                UiEvent::FilePicked { target, path } => {
                    let s = path.display().to_string();
                    match target {
                        FilePickTarget::AddTorrentOpen => {
                            self.add.reset_for_open();
                            self.add.mode = AddMode::Torrent;
                            self.add.url_text = s.clone();
                            self.add.torrent_preview_for = s.clone();
                            self.add.torrent_preview_busy = true;
                            self.worker.send(UiCommand::TorrentPreview { payload: s });
                        }
                        FilePickTarget::AddTorrentPayload => {
                            self.add.mode = AddMode::Torrent;
                            self.add.url_text = s.clone();
                            self.add.torrent_preview_for = s.clone();
                            self.add.torrent_preview_busy = true;
                            self.worker.send(UiCommand::TorrentPreview { payload: s });
                        }
                        FilePickTarget::AddDestDir => self.add.dest_dir = s,
                        FilePickTarget::SettingsDownloadDir => {
                            self.settings.default_download_dir = s;
                            self.settings.dirty = true;
                        }
                        FilePickTarget::SettingsYtDlp => {
                            self.settings.ytdlp_path = s;
                            self.settings.dirty = true;
                        }
                        FilePickTarget::SettingsExtensionDir => {
                            self.settings.extension_dir = s;
                            self.settings.dirty = true;
                        }
                        FilePickTarget::SettingsLanDir => {
                            self.settings.lan_dir = s;
                            self.settings.dirty = true;
                        }
                        FilePickTarget::OnboardingDownloadDir => {
                            self.onboarding.download_dir = s;
                        }
                        FilePickTarget::SettingsPluginScript => {
                            self.worker.send(UiCommand::InstallPlugin {
                                path,
                                override_dir: self.settings.extension_dir.clone(),
                            });
                        }
                        FilePickTarget::LanSendFile => {
                            let url = self.lan.send_to.trim().to_string();
                            let code = self.lan.send_code.trim().to_string();
                            if !url.is_empty() {
                                self.worker.send(UiCommand::LanSend { url, code, path });
                            }
                        }
                    }
                }
                UiEvent::ConfigSaved(res) => match res {
                    Ok(()) => {
                        self.settings.open = false;
                        self.settings.loaded = false;
                        self.set_notice("Options saved.".into(), false);
                    }
                    Err(e) => self.settings.error = Some(e),
                },
                UiEvent::HostRegisterResult(res) => match res {
                    Ok(msg) => self.onboarding.host_status = Some((true, msg)),
                    Err(msg) => self.onboarding.host_status = Some((false, msg)),
                },
            }
        }
    }

    fn filtered_rows<'a>(&self, snap: &'a Snapshot) -> Vec<&'a TaskRow> {
        let mut rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| matches_filter(t, self.filter, &self.search))
            .collect();
        sort_tasks(&mut rows, self.sort_key, self.sort_dir);
        rows
    }

    fn open_details_for(&mut self, id: String) {
        self.details.task_id = Some(id.clone());
        self.details.synced_selection_for.clear();
        self.details.add_parts_open = false;
        self.cached_details.loading = true;
        self.last_details_poll = Instant::now();
        self.worker.send(UiCommand::FetchDetails { id });
    }

    fn open_refresh_for(&mut self, row: &TaskRow) {
        self.refresh = RefreshDraft {
            open: true,
            task_id: row.id.clone(),
            task_name: row.name.clone(),
            part_seq: None,
            url: row.url.clone(),
            last_probed_url: String::new(),
            probing: false,
            verdict_ok: false,
            verdict_text: String::new(),
            submitting: false,
            error: None,
            resume_after: true,
        };
    }

    fn request_delete(&mut self, snap: &Snapshot, ids: Vec<String>, delete_files: bool) {
        if ids.is_empty() {
            return;
        }
        let label = if ids.len() == 1 {
            snap.tasks
                .iter()
                .find(|t| t.id == ids[0])
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "selected item".into())
        } else {
            format!("{} selected items", ids.len())
        };
        self.delete_confirm = Some(DeleteConfirm {
            ids,
            label,
            delete_files,
        });
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.theme_mode = match self.theme_mode {
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
        };
        apply_visuals(ctx, self.theme_mode);
        save_theme_mode(self.theme_mode);
    }

    fn handle_shortcuts_and_drop(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(first_file) = dropped.first() {
            let path = first_file.path();
            let s = path.display().to_string();
            self.add.reset_for_open();
            if looks_like_torrent_payload(&s)
                || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
            {
                self.add.mode = AddMode::Torrent;
                self.add.url_text = s.clone();
                self.add.torrent_preview_for = s.clone();
                self.add.torrent_preview_busy = true;
                self.worker.send(UiCommand::TorrentPreview { payload: s });
            } else {
                self.add.url_text = s;
            }
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.duplicate_prompt.is_some() {
                self.duplicate_prompt = None;
                return;
            }
            if self.delete_confirm.is_some() {
                self.delete_confirm = None;
                return;
            }
            if self.details.add_parts_open {
                self.details.add_parts_open = false;
                return;
            }
            if self.refresh.open {
                self.refresh.open = false;
                return;
            }
            if self.lan.open {
                self.lan.open = false;
                return;
            }
            if self.details.task_id.is_some() {
                self.details.task_id = None;
                return;
            }
            if self.settings.open {
                self.settings.open = false;
                return;
            }
            if self.onboarding.open {
                self.onboarding.open = false;
                return;
            }
            if self.add.open {
                self.add.open = false;
                return;
            }
            if self.about_open {
                self.about_open = false;
                return;
            }
            if !self.search.is_empty() {
                self.search.clear();
                return;
            }
        }

        let cmd = ctx.input(|i| i.modifiers.command);
        if cmd && ctx.input(|i| i.key_pressed(egui::Key::N)) {
            self.add.reset_for_open();
            return;
        }
        if cmd && ctx.input(|i| i.key_pressed(egui::Key::T)) {
            self.worker.pick_path(
                FilePickTarget::AddTorrentOpen,
                false,
                None,
                Some(("Torrent", &["torrent"])),
                ctx.clone(),
            );
            return;
        }
        if cmd && ctx.input(|i| i.key_pressed(egui::Key::B)) {
            self.add.reset_for_open();
            self.add.mode = AddMode::Bundle;
            return;
        }
        if cmd && ctx.input(|i| i.key_pressed(egui::Key::F)) {
            self.focus_search = true;
            return;
        }

        if self.any_modal_open() || ctx.egui_wants_keyboard_input() {
            return;
        }

        let visible_ids: Vec<String> = self
            .filtered_rows(snap)
            .into_iter()
            .map(|r| r.id.clone())
            .collect();
        if visible_ids.is_empty() {
            return;
        }

        if cmd && ctx.input(|i| i.key_pressed(egui::Key::A)) {
            self.selected_ids = visible_ids.iter().cloned().collect();
            self.anchor_id = visible_ids.first().cloned();
            return;
        }

        let down = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
        let up = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
        if down || up {
            let cur = self
                .anchor_id
                .as_ref()
                .and_then(|id| visible_ids.iter().position(|x| x == id));
            let next_idx = match (cur, down) {
                (None, true) => 0,
                (None, false) => visible_ids.len() - 1,
                (Some(i), true) => (i + 1).min(visible_ids.len() - 1),
                (Some(i), false) => i.saturating_sub(1),
            };
            let id = visible_ids[next_idx].clone();
            self.selected_ids.clear();
            self.selected_ids.insert(id.clone());
            self.anchor_id = Some(id);
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(id) = self.primary_selected_id(snap) {
                self.open_details_for(id);
            }
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
            let ids: Vec<String> = self.selected_ids.iter().cloned().collect();
            self.request_delete(snap, ids, false);
        }
    }

    fn primary_selected_id(&self, snap: &Snapshot) -> Option<String> {
        if let Some(a) = &self.anchor_id {
            if self.selected_ids.contains(a) && snap.tasks.iter().any(|t| t.id == *a) {
                return Some(a.clone());
            }
        }
        snap.tasks
            .iter()
            .find(|t| self.selected_ids.contains(&t.id))
            .map(|t| t.id.clone())
    }

    fn render_menu_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: &Palette) {
        let sel_rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| self.selected_ids.contains(&t.id))
            .collect();
        let single = sel_rows.first().copied();

        ui.horizontal(|ui| {
            // Crisp SVG-identical brand logo mark (`docs/logo.svg`).
            let (mark, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
            paint_brand_logo(ui.painter(), mark);
            ui.label(semibold("ripdl", 14.0, pal.ink));
            ui.add_space(8.0);

            // Flat frameless top-level menus matching clean desktop chrome.
            ui.scope(|ui| {
                let w = &mut ui.visuals_mut().widgets;
                w.inactive.bg_fill = Color32::TRANSPARENT;
                w.inactive.weak_bg_fill = Color32::TRANSPARENT;
                w.inactive.bg_stroke = Stroke::NONE;

                ui.menu_button("File", |ui| {
                    if ui.button("New download… (Ctrl+N)").clicked() {
                        self.add.reset_for_open();
                        ui.close();
                    }
                    if ui.button("Open .torrent… (Ctrl+T)").clicked() {
                        ui.close();
                        self.worker.pick_path(
                            FilePickTarget::AddTorrentOpen,
                            false,
                            None,
                            Some(("Torrent", &["torrent"])),
                            ui.ctx().clone(),
                        );
                    }
                    if ui.button("Add multi-part bundle… (Ctrl+B)").clicked() {
                        self.add.reset_for_open();
                        self.add.mode = AddMode::Bundle;
                        ui.close();
                    }
                    ui.separator();
                    let has_file = single.is_some_and(|t| t.dest_file.is_some());
                    if ui.add_enabled(has_file, egui::Button::new("Open file")).clicked() {
                        if let Some(t) = single {
                            self.worker.send(UiCommand::Open {
                                id: t.id.clone(),
                                folder: false,
                            });
                        }
                        ui.close();
                    }
                    let has_dir = single.is_some_and(|t| t.dest_file.is_some() || t.dest_dir.is_some());
                    if ui
                        .add_enabled(has_dir, egui::Button::new("Open containing folder"))
                        .clicked()
                    {
                        if let Some(t) = single {
                            self.worker.send(UiCommand::Open {
                                id: t.id.clone(),
                                folder: true,
                            });
                        }
                        ui.close();
                    }
                    let can_refresh = single.is_some_and(|t| t.can_refresh_url());
                    if ui
                        .add_enabled(can_refresh, egui::Button::new("Refresh address…"))
                        .clicked()
                    {
                        if let Some(t) = single {
                            self.open_refresh_for(t);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit UI").clicked() {
                        self.force_quit = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });

                ui.menu_button("Tasks", |ui| {
                    let any_resumable = sel_rows.iter().any(|t| t.can_resume());
                    let any_pausable = sel_rows.iter().any(|t| t.can_pause_or_stop());
                    if ui
                        .add_enabled(any_resumable, egui::Button::new("Resume selected"))
                        .clicked()
                    {
                        let ids: Vec<String> = sel_rows
                            .iter()
                            .filter(|t| t.can_resume())
                            .map(|t| t.id.clone())
                            .collect();
                        self.worker.send(UiCommand::Resume { ids });
                        ui.close();
                    }
                    if ui
                        .add_enabled(any_pausable, egui::Button::new("Pause / Stop selected"))
                        .clicked()
                    {
                        let ids: Vec<String> = sel_rows
                            .iter()
                            .filter(|t| t.can_pause_or_stop())
                            .map(|t| t.id.clone())
                            .collect();
                        self.worker.send(UiCommand::Pause { ids });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Resume all paused").clicked() {
                        self.worker.send(UiCommand::ResumeAll);
                        ui.close();
                    }
                    if ui.button("Stop all active").clicked() {
                        self.worker.send(UiCommand::StopAll);
                        ui.close();
                    }
                    if ui.button("Clear completed (keeps seeding torrents)").clicked() {
                        self.worker.send(UiCommand::ClearCompleted);
                        ui.close();
                    }
                });

                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_eta, "Show ETA column");
                    ui.checkbox(&mut self.show_peers, "Show Peers column");
                    ui.separator();
                    let is_dark = self.theme_mode == ThemeMode::Dark;
                    if ui.selectable_label(!is_dark, "Light theme (Daylight)").clicked() {
                        self.theme_mode = ThemeMode::Light;
                        apply_visuals(ui.ctx(), self.theme_mode);
                        save_theme_mode(self.theme_mode);
                        ui.close();
                    }
                    if ui.selectable_label(is_dark, "Dark theme (Slate)").clicked() {
                        self.theme_mode = ThemeMode::Dark;
                        apply_visuals(ui.ctx(), self.theme_mode);
                        save_theme_mode(self.theme_mode);
                        ui.close();
                    }
                });

                ui.menu_button("Tools", |ui| {
                    if ui.button("Options…").clicked() {
                        self.settings.open = true;
                        self.settings.loaded = false;
                        ui.close();
                    }
                    if ui.button("Send & Receive (LAN)…").clicked() {
                        self.lan.open = true;
                        self.worker.send(UiCommand::FetchLan);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Restart background daemon").clicked() {
                        self.worker.send(UiCommand::RestartDaemon);
                        ui.close();
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("Setup Wizard…").clicked() {
                        self.onboarding.reset_from(snap.settings.as_ref());
                        ui.close();
                    }
                    if ui.button("Extension Welcome Page").clicked() {
                        self.worker.send(UiCommand::OpenExtensionWelcome {
                            browser: self.settings.browser_choice,
                        });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Check for updates").clicked() {
                        self.worker.send(UiCommand::CheckUpdate);
                        ui.close();
                    }
                    if ui.button("About ripdl").clicked() {
                        self.about_open = true;
                        ui.close();
                    }
                });
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (theme_ico, theme_tip) = match self.theme_mode {
                    ThemeMode::Light => (IconId::Moon, "Switch to dark theme"),
                    ThemeMode::Dark => (IconId::Sun, "Switch to light theme"),
                };
                if vector_icon_button(ui, pal, theme_ico, theme_tip, 24.0, false, false)
                    .on_hover_text(theme_tip)
                    .clicked()
                {
                    self.toggle_theme(ui.ctx());
                }

                let ver = if snap.daemon_version.is_empty() {
                    format!("v{}", env!("CARGO_PKG_VERSION"))
                } else {
                    format!("v{}", snap.daemon_version)
                };
                ui.label(egui::RichText::new(ver).small().monospace().color(pal.ink_muted));

                let (conn_fg, conn_bg, conn_dot) = if snap.connected {
                    (pal.success, pal.success_soft, pal.success)
                } else {
                    (pal.warn, pal.warn_soft, pal.warn)
                };
                paint_status_pill(ui, &snap.daemon_label, conn_fg, conn_bg, conn_dot);

                if let Some(upd) = &snap.update {
                    if upd.update_available
                        && vector_button(
                            ui,
                            pal,
                            IconId::Download,
                            &format!("Update v{}", upd.latest),
                            22.0,
                            false,
                            true,
                        )
                        .clicked()
                    {
                        self.settings.open = true;
                        self.settings.tab = SettingsTab::Updates;
                    }
                }
            });
        });
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: &Palette) {
        let sel_rows: Vec<&TaskRow> = snap
            .tasks
            .iter()
            .filter(|t| self.selected_ids.contains(&t.id))
            .collect();
        let any_resumable = sel_rows.iter().any(|t| t.can_resume());
        let any_pausable = sel_rows.iter().any(|t| t.can_pause_or_stop());
        let any_active = snap.tasks.iter().any(|t| t.can_pause_or_stop());
        let any_clearable = snap
            .tasks
            .iter()
            .any(|t| t.raw_state == TaskState::Completed && !t.seeding);
        let single_stale = sel_rows.len() == 1
            && sel_rows[0].can_refresh_url()
            && sel_rows[0].raw_state == TaskState::Stale;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            if vector_button(ui, pal, IconId::Add, "Add", metrics::TOOLBAR_H, true, true)
                .on_hover_text("Add URL, magnet, or media link (Ctrl+N)")
                .clicked()
            {
                self.add.reset_for_open();
            }
            if vector_button(ui, pal, IconId::Torrent, "Torrent", metrics::TOOLBAR_H, false, false)
                .on_hover_text("Add .torrent or magnet URI (Ctrl+T)")
                .clicked()
            {
                self.add.reset_for_open();
                self.add.mode = AddMode::Torrent;
            }
            if vector_button(ui, pal, IconId::Bundle, "Bundle", metrics::TOOLBAR_H, false, false)
                .on_hover_text("Group multi-part links into one folder task (Ctrl+B)")
                .clicked()
            {
                self.add.reset_for_open();
                self.add.mode = AddMode::Bundle;
            }

            toolbar_sep(ui, pal);

            if ui
                .add_enabled_ui(any_resumable, |ui| {
                    vector_button(ui, pal, IconId::Play, "Resume", metrics::TOOLBAR_H, false, false)
                })
                .inner
                .clicked()
            {
                let ids: Vec<String> = sel_rows
                    .iter()
                    .filter(|t| t.can_resume())
                    .map(|t| t.id.clone())
                    .collect();
                self.worker.send(UiCommand::Resume { ids });
            }

            if ui
                .add_enabled_ui(any_pausable, |ui| {
                    vector_button(ui, pal, IconId::Pause, "Pause", metrics::TOOLBAR_H, false, false)
                })
                .inner
                .clicked()
            {
                let ids: Vec<String> = sel_rows
                    .iter()
                    .filter(|t| t.can_pause_or_stop())
                    .map(|t| t.id.clone())
                    .collect();
                self.worker.send(UiCommand::Pause { ids });
            }

            if ui
                .add_enabled_ui(any_active, |ui| {
                    vector_button(ui, pal, IconId::Stop, "Stop All", metrics::TOOLBAR_H, false, false)
                })
                .inner
                .on_hover_text("Pause all running downloads and stop seeding")
                .clicked()
            {
                self.worker.send(UiCommand::StopAll);
            }

            if ui
                .add_enabled_ui(!sel_rows.is_empty(), |ui| {
                    vector_button(ui, pal, IconId::Trash, "Remove", metrics::TOOLBAR_H, false, false)
                })
                .inner
                .clicked()
            {
                let ids: Vec<String> = sel_rows.iter().map(|t| t.id.clone()).collect();
                self.request_delete(snap, ids, false);
            }

            if single_stale
                && vector_button(ui, pal, IconId::Refresh, "Fix link", metrics::TOOLBAR_H, false, true).clicked()
            {
                self.open_refresh_for(sel_rows[0]);
            }

            if ui
                .add_enabled_ui(any_clearable, |ui| {
                    vector_button(ui, pal, IconId::ClearDone, "Clear", metrics::TOOLBAR_H, false, false)
                })
                .inner
                .on_hover_text("Clear finished tasks from list (keeps active seeders)")
                .clicked()
            {
                self.worker.send(UiCommand::ClearCompleted);
            }

            toolbar_sep(ui, pal);

            if vector_button(ui, pal, IconId::Lan, "LAN", metrics::TOOLBAR_H, false, false)
                .on_hover_text("Send & Receive files on local network")
                .clicked()
            {
                self.lan.open = true;
                self.worker.send(UiCommand::FetchLan);
            }

            if vector_button(ui, pal, IconId::Settings, "Options", metrics::TOOLBAR_H, false, false).clicked() {
                self.settings.open = true;
                self.settings.loaded = false;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::new()
                    .fill(pal.surface_alt)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (sr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                            paint_icon(ui.painter(), sr, IconId::Search, pal.ink_muted);

                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .frame(egui::Frame::NONE)
                                    .hint_text("Search downloads… (Ctrl+F)")
                                    .desired_width(164.0),
                            );
                            if self.focus_search {
                                edit.request_focus();
                                self.focus_search = false;
                            }
                            if !self.search.is_empty()
                                && vector_icon_button(ui, pal, IconId::Close, "Clear search", 18.0, false, false)
                                    .clicked()
                            {
                                self.search.clear();
                            }
                        });
                    });
            });
        });
    }

    fn render_sidebar(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: &Palette) {
        ui.spacing_mut().item_spacing.y = 2.0;

        let section_head = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(7.0);
            ui.label(
                egui::RichText::new(text)
                    .size(10.0)
                    .strong()
                    .color(pal.ink_muted),
            );
            ui.add_space(2.0);
        };

        let mut nav_item = |ui: &mut egui::Ui, icon: IconId, f: SidebarFilter| {
            let count = count_filter(&snap.tasks, f);
            let active = self.filter == f;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::Button,
                    true,
                    active,
                    f.label(),
                )
            });

            if ui.is_rect_visible(rect) {
                let bg = if active {
                    pal.accent_soft
                } else if resp.hovered() {
                    pal.surface_hover
                } else {
                    Color32::TRANSPARENT
                };
                let fg = if active { pal.accent_dark } else { pal.ink_secondary };
                let ico_fg = if active { pal.accent } else { pal.ink_muted };

                ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
                paint_focus_ring(ui.painter(), rect, 6, pal, resp.has_focus());
                if active {
                    let bar = Rect::from_min_max(
                        rect.min + egui::vec2(0.0, 4.0),
                        egui::pos2(rect.min.x + 3.0, rect.max.y - 4.0),
                    );
                    ui.painter().rect_filled(bar, CornerRadius::same(2), pal.accent);
                }

                let ico_rect = Rect::from_center_size(
                    egui::pos2(rect.min.x + 17.0, rect.center().y),
                    egui::vec2(14.0, 14.0),
                );
                paint_icon(ui.painter(), ico_rect, icon, ico_fg);

                let font = egui::FontId::new(
                    12.5,
                    if active {
                        egui::FontFamily::Name("semibold".into())
                    } else {
                        egui::FontFamily::Proportional
                    },
                );
                ui.painter().text(
                    egui::pos2(rect.min.x + 33.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    f.label(),
                    font,
                    fg,
                );

                // Crisp pill counter on right edge.
                let count_str = count.to_string();
                let count_galley = ui.painter().layout_no_wrap(
                    count_str,
                    egui::FontId::new(10.5, egui::FontFamily::Proportional),
                    if active { pal.accent_dark } else { pal.ink_muted },
                );
                let pill_w = (count_galley.size().x + 11.0).max(22.0);
                let pill_rect = Rect::from_center_size(
                    egui::pos2(rect.max.x - 8.0 - pill_w * 0.5, rect.center().y),
                    egui::vec2(pill_w, 17.0),
                );
                let pill_bg = if active {
                    Color32::from_rgba_unmultiplied(pal.accent.r(), pal.accent.g(), pal.accent.b(), 36)
                } else {
                    pal.surface_hover
                };
                ui.painter().rect_filled(pill_rect, CornerRadius::same(9), pill_bg);
                ui.painter().galley(
                    pill_rect.center() - count_galley.size() * 0.5,
                    count_galley,
                    if active { pal.accent_dark } else { pal.ink_muted },
                );
            }
            if resp.clicked() {
                self.filter = f;
            }
        };

        section_head(ui, "LIBRARY");
        nav_item(ui, IconId::AllDownloads, SidebarFilter::All);
        nav_item(ui, IconId::Active, SidebarFilter::Active);
        nav_item(ui, IconId::Pause, SidebarFilter::PausedOrExpired);
        nav_item(ui, IconId::Check, SidebarFilter::Completed);

        section_head(ui, "SOURCES");
        nav_item(ui, IconId::Torrent, SidebarFilter::Torrents);
        nav_item(ui, IconId::Bundle, SidebarFilter::Bundles);
        nav_item(ui, IconId::Video, SidebarFilter::Streams);

        section_head(ui, "FILE TYPES");
        nav_item(ui, IconId::Video, SidebarFilter::Category(Category::Video));
        nav_item(ui, IconId::Audio, SidebarFilter::Category(Category::Audio));
        nav_item(ui, IconId::Image, SidebarFilter::Category(Category::Image));
        nav_item(ui, IconId::Document, SidebarFilter::Category(Category::Document));
        nav_item(ui, IconId::Archive, SidebarFilter::Category(Category::Archive));
        nav_item(ui, IconId::Program, SidebarFilter::Category(Category::Program));

        // Push LAN drop box card cleanly to bottom of sidebar.
        let rem = (ui.available_height() - 62.0).max(8.0);
        ui.add_space(rem);
        let listening = snap.lan.as_ref().is_some_and(|l| l.running);
        let lan_sub = snap
            .lan
            .as_ref()
            .filter(|l| l.running)
            .map(|l| format!("Listening :{}", l.port))
            .unwrap_or_else(|| "Drop box off".into());
        let (card_rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Send & Receive on Local Network"));
        if ui.is_rect_visible(card_rect) {
            let bg = if self.lan.open || resp.hovered() {
                pal.accent_soft
            } else {
                pal.surface_alt
            };
            ui.painter().rect_filled(card_rect, CornerRadius::same(8), bg);
            ui.painter().rect_stroke(card_rect, CornerRadius::same(8), Stroke::new(1.0, pal.border), StrokeKind::Inside);
            paint_focus_ring(ui.painter(), card_rect, 8, pal, resp.has_focus());
            let ir = Rect::from_center_size(egui::pos2(card_rect.min.x + 17.0, card_rect.center().y), egui::vec2(15.0, 15.0));
            paint_icon(ui.painter(), ir, IconId::Lan, if listening { pal.accent } else { pal.ink_muted });
            ui.painter().text(
                egui::pos2(card_rect.min.x + 32.0, card_rect.min.y + 14.0),
                egui::Align2::LEFT_CENTER,
                "Send & Receive",
                egui::FontId::new(12.0, egui::FontFamily::Name("semibold".into())),
                pal.ink,
            );
            let dot_c = if listening { pal.success } else { pal.ink_muted };
            ui.painter().circle_filled(egui::pos2(card_rect.min.x + 35.0, card_rect.max.y - 12.0), 3.0, dot_c);
            ui.painter().text(
                egui::pos2(card_rect.min.x + 42.0, card_rect.max.y - 12.0),
                egui::Align2::LEFT_CENTER,
                lan_sub,
                egui::FontId::new(11.0, egui::FontFamily::Proportional),
                pal.ink_muted,
            );
        }
        if resp.clicked() {
            self.lan.open = true;
            self.worker.send(UiCommand::FetchLan);
        }
    }

    fn render_table_area(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: &Palette) {
        if let Some(upd) = &snap.update {
            if upd.update_available && !self.update_banner_dismissed {
                let busy = snap
                    .update_install_status
                    .as_deref()
                    .is_some_and(|s| s.starts_with("Downloading"));
                egui::Frame::new()
                    .fill(pal.info_soft)
                    .stroke(Stroke::new(1.0, pal.info))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            paint_icon(ui.painter(), ir, IconId::Download, pal.info);
                            let status_suffix = snap
                                .update_install_status
                                .as_deref()
                                .map(|s| format!(" · {s}"))
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!(
                                    "ripdl v{} is available (running v{}){}",
                                    upd.latest, upd.current, status_suffix
                                ))
                                .color(pal.info),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if vector_icon_button(ui, pal, IconId::Close, "Dismiss update banner", 20.0, false, false).clicked() {
                                    self.update_banner_dismissed = true;
                                }
                                if vector_button(ui, pal, IconId::External, "Release notes", 22.0, false, false).clicked() {
                                    views::open_external_url("https://github.com/ShiniM0d0ri/ripdl/releases");
                                }
                                if ui
                                    .add_enabled_ui(!busy, |ui| {
                                        vector_button(
                                            ui,
                                            pal,
                                            IconId::Download,
                                            &format!("Download & Install v{}", upd.latest),
                                            22.0,
                                            true,
                                            true,
                                        )
                                    })
                                    .inner
                                    .clicked()
                                {
                                    self.worker.send(UiCommand::UpdateInstall);
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            }
        }

        let mut dismiss_notice = false;
        if let Some(notice) = self.notice.clone() {
            if Instant::now() < notice.until {
                let (fg, bg, ico) = if notice.is_error {
                    (pal.danger, pal.danger_soft, IconId::Alert)
                } else {
                    (pal.accent_dark, pal.accent_soft, IconId::CheckCircle)
                };
                egui::Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.0, fg))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            paint_icon(ui.painter(), ir, ico, fg);
                            ui.label(egui::RichText::new(&notice.text).color(fg));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if vector_icon_button(ui, pal, IconId::Close, "Dismiss notice", 20.0, false, false).clicked() {
                                    dismiss_notice = true;
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            } else {
                dismiss_notice = true;
            }
        }
        if dismiss_notice {
            self.notice = None;
        }

        let rows = self.filtered_rows(snap);
        let owned_rows: Vec<TaskRow> = rows.into_iter().cloned().collect();

        // Content header bar matching UI design reference.
        let downloaded_total: u64 = owned_rows.iter().map(|r| r.downloaded).sum();
        let active_cnt = owned_rows
            .iter()
            .filter(|r| r.raw_state == TaskState::Running)
            .count();
        let has_active_filter = self.filter != SidebarFilter::All || !self.search.trim().is_empty();
        let eyebrow = match self.filter {
            SidebarFilter::All
            | SidebarFilter::Active
            | SidebarFilter::PausedOrExpired
            | SidebarFilter::Completed => "LIBRARY",
            SidebarFilter::Torrents | SidebarFilter::Bundles | SidebarFilter::Streams => "SOURCES",
            SidebarFilter::Category(_) => "FILE TYPES",
        };

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(egui::RichText::new(eyebrow).size(10.0).strong().color(pal.ink_muted));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(semibold(self.filter.label(), 18.0, pal.ink));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {} · {} downloaded · {} active",
                            owned_rows.len(),
                            if owned_rows.len() == 1 { "task" } else { "tasks" },
                            humans_bin(downloaded_total),
                            active_cnt,
                        ))
                        .small()
                        .color(pal.ink_muted),
                    );
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("Columns", |ui| {
                    ui.checkbox(&mut self.show_eta, "Show ETA column");
                    ui.checkbox(&mut self.show_peers, "Show Peers column");
                });
                if has_active_filter
                    && vector_button(ui, pal, IconId::Close, "Reset view", 24.0, false, true).clicked()
                {
                    self.filter = SidebarFilter::All;
                    self.search.clear();
                }
            });
        });
        ui.add_space(6.0);

        if owned_rows.is_empty() {
            egui::Frame::new()
                .fill(pal.surface)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(CornerRadius::same(10))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.vertical_centered(|ui| {
                        let top_pad = ((ui.available_height() - 220.0) * 0.42).max(28.0);
                        ui.add_space(top_pad);
                        if snap.tasks.is_empty() {
                            paint_empty_illustration(ui, pal);
                            ui.add_space(10.0);
                            ui.label(semibold("No downloads queued", 16.5, pal.ink));
                            ui.label(
                                egui::RichText::new("Paste a URL or drop a .torrent file anywhere in this window.")
                                    .color(pal.ink_muted),
                            );
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                let w_group = 250.0_f32;
                                ui.add_space(((ui.available_width() - w_group) * 0.5).max(0.0));
                                if vector_button(ui, pal, IconId::Add, "Add Download", 32.0, true, true).clicked() {
                                    self.add.reset_for_open();
                                }
                                if vector_button(ui, pal, IconId::Torrent, "Open .torrent…", 32.0, false, true).clicked() {
                                    self.worker.pick_path(
                                        FilePickTarget::AddTorrentOpen,
                                        false,
                                        None,
                                        Some(("Torrent", &["torrent"])),
                                        ui.ctx().clone(),
                                    );
                                }
                            });
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("Shortcuts: Ctrl+N Add URL  ·  Ctrl+T Torrent  ·  Ctrl+B Bundle")
                                    .small()
                                    .color(pal.ink_muted),
                            );
                        } else {
                            paint_filter_empty_illustration(ui, pal);
                            ui.add_space(8.0);
                            ui.label(semibold(&format!("No downloads match \"{}\"", self.filter.label()), 15.0, pal.ink));
                            ui.label(
                                egui::RichText::new("Try clearing your search filter or switching category.")
                                    .color(pal.ink_muted),
                            );
                            ui.add_space(10.0);
                            if vector_button(ui, pal, IconId::Close, "Clear active filter", 28.0, false, true).clicked() {
                                self.filter = SidebarFilter::All;
                                self.search.clear();
                            }
                        }
                        ui.add_space(40.0);
                    });
                });
            return;
        }

        let card_avail_w = ui.available_width().max(720.0);
        // Reserve inner margins + ScrollArea scrollbar so Actions column never clips.
        let header_inner_w = (card_avail_w - 28.0).max(690.0);
        let cols = table_columns(header_inner_w, self.show_eta, self.show_peers);

        egui::Frame::new()
            .fill(pal.surface)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(CornerRadius::same(10))
            .show(ui, |ui| {
                // Header strip pinned via exact computed X ranges (cannot squish or clip).
                let (hdr_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), metrics::HEADER_H),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(
                    hdr_rect,
                    CornerRadius {
                        nw: 10,
                        ne: 10,
                        sw: 0,
                        se: 0,
                    },
                    pal.surface_alt,
                );
                ui.painter().line_segment(
                    [egui::pos2(hdr_rect.min.x, hdr_rect.max.y), hdr_rect.max],
                    Stroke::new(1.0, pal.border),
                );
                let hdr_origin_x = hdr_rect.min.x + 8.0;
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.name, "FILE NAME", SortKey::Name, pal);
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.kind, "KIND", SortKey::Kind, pal);
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.size, "SIZE", SortKey::Size, pal);
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.progress, "PROGRESS", SortKey::Progress, pal);
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.status, "STATUS", SortKey::Status, pal);
                self.paint_sort_header(ui, hdr_origin_x, hdr_rect, cols.rate, "RATE", SortKey::Rate, pal);
                if let Some(peers_span) = cols.peers {
                    paint_static_header(ui, hdr_origin_x, hdr_rect, peers_span, "PEERS", pal);
                }
                if let Some(eta_span) = cols.eta {
                    paint_static_header(ui, hdr_origin_x, hdr_rect, eta_span, "ETA", pal);
                }
                paint_static_header(ui, hdr_origin_x, hdr_rect, cols.actions, "ACTIONS", pal);

                let mut clicked_row: Option<(usize, String)> = None;
                let mut action_cmd: Option<UiCommand> = None;
                let mut open_details_id: Option<String> = None;
                let mut open_refresh_row: Option<TaskRow> = None;
                let mut req_delete: Option<(Vec<String>, bool)> = None;
                let mut copy_url_txt: Option<String> = None;

                let total_rows = owned_rows.len();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, metrics::ROW_H, total_rows, |ui, range| {
                        for idx in range {
                            let row = &owned_rows[idx];
                            let selected = self.selected_ids.contains(&row.id);

                            let (row_rect, row_resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), metrics::ROW_H),
                                egui::Sense::click(),
                            );
                            let bg = if selected {
                                pal.accent_soft
                            } else if row_resp.hovered() {
                                pal.surface_hover
                            } else if idx % 2 == 1 {
                                pal.surface_alt
                            } else {
                                pal.surface
                            };
                            ui.painter().rect_filled(row_rect, CornerRadius::ZERO, bg);
                            ui.painter().line_segment(
                                [egui::pos2(row_rect.min.x, row_rect.max.y), row_rect.max],
                                Stroke::new(1.0, pal.border),
                            );
                            if selected {
                                let bar = Rect::from_min_max(
                                    row_rect.min,
                                    egui::pos2(row_rect.min.x + 3.0, row_rect.max.y),
                                );
                                ui.painter().rect_filled(bar, CornerRadius::ZERO, pal.accent);
                            }

                            let ox = row_rect.min.x + 8.0;
                            let col_rect = |span: (f32, f32)| -> Rect {
                                Rect::from_min_max(
                                    egui::pos2(ox + span.0, row_rect.min.y + 3.0),
                                    egui::pos2(ox + span.0 + span.1, row_rect.max.y - 3.0),
                                )
                            };

                            // 1. File tile + Name + concise source/error subline.
                            let (k_ico, k_lbl, k_fg, _) = kind_visuals(&row.kind, pal);
                            in_rect_left(ui, col_rect(cols.name), |ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let (tile_rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                                paint_file_tile(ui.painter(), tile_rect, &row.kind, row.category, pal);

                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(
                                        egui::Label::new(semibold(&row.name, 13.0, pal.ink)).truncate(),
                                    )
                                    .on_hover_text(&row.name);
                                    let sub = if let Some(err) = &row.error {
                                        err.to_string()
                                    } else if let Some(reason) = row
                                        .stale_reason
                                        .as_deref()
                                        .filter(|_| row.raw_state == TaskState::Stale)
                                    {
                                        format!("{} · {reason}", row.source)
                                    } else if !row.parts_missing.is_empty() {
                                        format!("{} · missing parts: {:?}", row.source, row.parts_missing)
                                    } else if let Some(start) = row.start_at.filter(|&s| s > crate::model::now_unix()) {
                                        let mins = ((start - crate::model::now_unix()) + 59) / 60;
                                        format!("{} · starts in ~{}m", row.source, mins)
                                    } else if row.seeding && row.uploaded > 0 {
                                        format!("{} · \u{2191} {} uploaded", row.source, humans_bin(row.uploaded))
                                    } else if row.kind == "torrent" && row.peers > 0 {
                                        format!("{} · {} peers", row.source, row.peers)
                                    } else {
                                        row.source.clone()
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(sub)
                                                .small()
                                                .color(if row.error.is_some() { pal.danger } else { pal.ink_muted }),
                                        )
                                        .truncate(),
                                    );
                                });
                            });

                            // 2. Kind outline pill with vector icon.
                            in_rect_left(ui, col_rect(cols.kind), |ui| {
                                paint_kind_pill(ui, k_ico, k_lbl, k_fg, pal);
                            });

                            // 3. Size.
                            let size_str = match row.total {
                                Some(t) => format!("{} / {}", humans_bin(row.downloaded), humans_bin(t)),
                                None if row.downloaded > 0 => humans_bin(row.downloaded),
                                None => "—".into(),
                            };
                            in_rect_left(ui, col_rect(cols.size), |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(size_str)
                                            .small()
                                            .color(pal.ink_secondary),
                                    )
                                    .truncate(),
                                );
                            });

                            // 4. Progress bar + percentage.
                            in_rect_left(ui, col_rect(cols.progress), |ui| {
                                paint_progress_bar(ui, row.ratio(), &row.state, cols.progress.1, pal);
                            });

                            // 5. Status pill with colored dot.
                            let (st_lbl, st_fg, st_bg, st_dot) = state_visuals(&row.state, pal);
                            in_rect_left(ui, col_rect(cols.status), |ui| {
                                paint_status_pill(ui, st_lbl, st_fg, st_bg, st_dot);
                            });

                            // 6. Rate.
                            let rate_str = if row.seeding {
                                row.upload_bps
                                    .map(|r| format!("\u{2191} {}/s", humans_bin(r as u64)))
                                    .unwrap_or_else(|| "\u{2191} —".into())
                            } else if let Some(spd) = row.speed_bps {
                                format!("{}/s", humans_bin(spd as u64))
                            } else {
                                "—".into()
                            };
                            in_rect_left(ui, col_rect(cols.rate), |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(rate_str)
                                            .small()
                                            .monospace()
                                            .color(if row.speed_bps.is_some() || row.seeding {
                                                pal.ink_secondary
                                            } else {
                                                pal.ink_muted
                                            }),
                                    )
                                    .truncate(),
                                );
                            });

                            // Optional Peers.
                            if let Some(peers_span) = cols.peers {
                                in_rect_left(ui, col_rect(peers_span), |ui| {
                                    let txt = if row.kind == "torrent" {
                                        row.peers.to_string()
                                    } else {
                                        "—".into()
                                    };
                                    ui.label(egui::RichText::new(txt).small().monospace().color(pal.ink_muted));
                                });
                            }

                            // Optional ETA.
                            if let Some(eta_span) = cols.eta {
                                in_rect_left(ui, col_rect(eta_span), |ui| {
                                    let raw = eta_text(row);
                                    let disp = if raw == "-" { "—" } else { raw.as_str() };
                                    ui.label(egui::RichText::new(disp).small().monospace().color(pal.ink_muted));
                                });
                            }

                            // Last column: Unclipped crisp square vector action buttons.
                            in_rect_left(ui, col_rect(cols.actions), |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if row.raw_state == TaskState::Stale && row.can_refresh_url() {
                                    if vector_button(ui, pal, IconId::Refresh, "Fix", 26.0, false, true)
                                        .on_hover_text("Refresh expired link")
                                        .clicked()
                                    {
                                        open_refresh_row = Some(row.clone());
                                    }
                                } else if row.can_pause_or_stop() {
                                    let (ico, tip) = if row.seeding {
                                        (IconId::Stop, "Stop seeding")
                                    } else {
                                        (IconId::Pause, "Pause download")
                                    };
                                    if vector_icon_button(ui, pal, ico, tip, 26.0, false, true)
                                        .on_hover_text(tip)
                                        .clicked()
                                    {
                                        action_cmd = Some(UiCommand::Pause { ids: vec![row.id.clone()] });
                                    }
                                } else if row.can_resume() {
                                    if vector_icon_button(ui, pal, IconId::Play, "Resume download", 26.0, false, true)
                                        .on_hover_text("Resume")
                                        .clicked()
                                    {
                                        action_cmd = Some(UiCommand::Resume { ids: vec![row.id.clone()] });
                                    }
                                } else if row.dest_file.is_some() {
                                    if vector_icon_button(ui, pal, IconId::OpenFile, "Open file", 26.0, false, true)
                                        .on_hover_text("Open file")
                                        .clicked()
                                    {
                                        action_cmd = Some(UiCommand::Open {
                                            id: row.id.clone(),
                                            folder: false,
                                        });
                                    }
                                }

                                if vector_icon_button(ui, pal, IconId::Details, "Inspect details", 26.0, false, true)
                                    .on_hover_text("Inspect details")
                                    .clicked()
                                {
                                    open_details_id = Some(row.id.clone());
                                }
                                if vector_icon_button(ui, pal, IconId::Trash, "Remove task", 26.0, true, true)
                                    .on_hover_text("Remove")
                                    .clicked()
                                {
                                    req_delete = Some((vec![row.id.clone()], false));
                                }
                            });

                            if row_resp.double_clicked() {
                                open_details_id = Some(row.id.clone());
                            } else if row_resp.clicked() {
                                clicked_row = Some((idx, row.id.clone()));
                            }

                            row_resp.context_menu(|ui| {
                                if row.can_resume() && ui.button("Resume / Start").clicked() {
                                    action_cmd = Some(UiCommand::Resume { ids: vec![row.id.clone()] });
                                    ui.close();
                                }
                                if row.can_pause_or_stop() && ui.button("Pause / Stop").clicked() {
                                    action_cmd = Some(UiCommand::Pause { ids: vec![row.id.clone()] });
                                    ui.close();
                                }
                                if row.can_refresh_url() && ui.button("Refresh download address…").clicked() {
                                    open_refresh_row = Some(row.clone());
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Details…").clicked() {
                                    open_details_id = Some(row.id.clone());
                                    ui.close();
                                }
                                if row.dest_file.is_some() && ui.button("Open file").clicked() {
                                    action_cmd = Some(UiCommand::Open {
                                        id: row.id.clone(),
                                        folder: false,
                                    });
                                    ui.close();
                                }
                                if (row.dest_file.is_some() || row.dest_dir.is_some())
                                    && ui.button("Show in folder").clicked()
                                {
                                    action_cmd = Some(UiCommand::Open {
                                        id: row.id.clone(),
                                        folder: true,
                                    });
                                    ui.close();
                                }
                                if ui.button("Copy address").clicked() {
                                    copy_url_txt = Some(row.url.clone());
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Remove from list").clicked() {
                                    req_delete = Some((vec![row.id.clone()], false));
                                    ui.close();
                                }
                                if ui.button("Delete task & files from disk…").clicked() {
                                    req_delete = Some((vec![row.id.clone()], true));
                                    ui.close();
                                }
                            });
                        }
                    });

                if let Some((idx, id)) = clicked_row {
                    let ctrl = ui.input(|i| i.modifiers.command);
                    let shift = ui.input(|i| i.modifiers.shift);
                    if shift {
                        if let Some(anchor) = self.anchor_id.as_ref().and_then(|a| owned_rows.iter().position(|r| &r.id == a)) {
                            let (lo, hi) = (anchor.min(idx), anchor.max(idx));
                            self.selected_ids.clear();
                            for r in &owned_rows[lo..=hi] {
                                self.selected_ids.insert(r.id.clone());
                            }
                        } else {
                            self.selected_ids.insert(id.clone());
                            self.anchor_id = Some(id);
                        }
                    } else if ctrl {
                        if !self.selected_ids.remove(&id) {
                            self.selected_ids.insert(id.clone());
                        }
                        self.anchor_id = Some(id);
                    } else {
                        self.selected_ids.clear();
                        self.selected_ids.insert(id.clone());
                        self.anchor_id = Some(id);
                    }
                }
                if let Some(cmd) = action_cmd {
                    self.worker.send(cmd);
                }
                if let Some(id) = open_details_id {
                    self.open_details_for(id);
                }
                if let Some(r) = open_refresh_row {
                    self.open_refresh_for(&r);
                }
                if let Some((ids, del_files)) = req_delete {
                    self.request_delete(snap, ids, del_files);
                }
                if let Some(u) = copy_url_txt {
                    ui.ctx().copy_text(u);
                    self.set_notice("Address copied to clipboard.".into(), false);
                }
            });
    }

    fn paint_sort_header(
        &mut self,
        ui: &mut egui::Ui,
        origin_x: f32,
        hdr_rect: Rect,
        span: (f32, f32),
        label: &str,
        key: SortKey,
        pal: &Palette,
    ) {
        let arrow = if self.sort_key == key {
            match self.sort_dir {
                SortDir::Asc => " \u{25b4}",
                SortDir::Desc => " \u{25be}",
            }
        } else {
            ""
        };
        let rect = Rect::from_min_max(
            egui::pos2(origin_x + span.0, hdr_rect.min.y + 2.0),
            egui::pos2(origin_x + span.0 + span.1, hdr_rect.max.y - 2.0),
        );
        let mut clicked = false;
        in_rect_left(ui, rect, |ui| {
            let color = if self.sort_key == key {
                pal.accent_dark
            } else {
                pal.ink_muted
            };
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("{label}{arrow}"))
                            .size(10.5)
                            .strong()
                            .color(color),
                    )
                    .frame(false),
                )
                .clicked()
            {
                clicked = true;
            }
        });
        if clicked {
            if self.sort_key == key {
                self.sort_dir = match self.sort_dir {
                    SortDir::Asc => SortDir::Desc,
                    SortDir::Desc => SortDir::Asc,
                };
            } else {
                self.sort_key = key;
                self.sort_dir = SortDir::Asc;
            }
        }
    }

    fn render_footer(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: &Palette) {
        let active_cnt = snap
            .tasks
            .iter()
            .filter(|t| t.raw_state == TaskState::Running)
            .count();
        let seeding_cnt = snap.tasks.iter().filter(|t| t.seeding).count();

        ui.horizontal(|ui| {
            let sel_txt = if self.selected_ids.len() > 1 {
                format!(" · {} selected", self.selected_ids.len())
            } else {
                String::new()
            };
            ui.label(
                egui::RichText::new(format!(
                    "{} total · {active_cnt} running · {seeding_cnt} seeding{sel_txt}",
                    snap.tasks.len()
                ))
                .small()
                .color(pal.ink_secondary),
            );

            for link in &snap.links {
                let chip_fg = if link.weight == 0 {
                    pal.danger
                } else if link.fails > 0 {
                    pal.warn
                } else {
                    pal.accent_dark
                };
                ui.scope(|ui| {
                    paint_status_pill(
                        ui,
                        &format!("{} {}/s", link.name, humans_bin(link.rate_bps)),
                        chip_fg,
                        pal.surface_hover,
                        chip_fg,
                    );
                })
                .response
                .on_hover_text(format!(
                    "{} ({}{}) · cap {}/s · {} conn · weight {}",
                    link.name,
                    link.ip,
                    if link.default_route { ", default" } else { "" },
                    humans_bin(link.capacity_bps),
                    link.in_use,
                    link.weight,
                ));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let cap_label = snap
                    .global_limit
                    .map(|bps| format!("Cap: {}/s", humans_bin(bps)))
                    .unwrap_or_else(|| "Cap: Unlimited".into());
                ui.menu_button(cap_label, |ui| {
                    let presets: &[(&str, u64)] = &[
                        ("Unlimited", 0),
                        ("500 KiB/s", 500 * 1024),
                        ("1 MiB/s", 1024 * 1024),
                        ("2 MiB/s", 2 * 1024 * 1024),
                        ("5 MiB/s", 5 * 1024 * 1024),
                        ("10 MiB/s", 10 * 1024 * 1024),
                    ];
                    for &(lbl, val) in presets {
                        if ui.button(lbl).clicked() {
                            if let Some(mut s) = snap.settings.clone() {
                                s.max_speed_bytes = Some(val);
                                self.worker.send(UiCommand::SetConfig { settings: s });
                            }
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Custom limit…").clicked() {
                        self.settings.open = true;
                        self.settings.loaded = false;
                        self.settings.tab = SettingsTab::Connection;
                        ui.close();
                    }
                });

                paint_sparkline(ui, egui::vec2(124.0, 22.0), &snap.rate_history, pal);

                let (gr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                paint_icon(ui.painter(), gr, IconId::Gauge, pal.accent);

                let avg_bps = if snap.rate_history.is_empty() {
                    0.0
                } else {
                    snap.rate_history.iter().sum::<f64>() / snap.rate_history.len() as f64
                };
                ui.label(
                    egui::RichText::new(format!(
                        "Now {}/s · 60s avg {}/s",
                        humans_bin(snap.total_bps as u64),
                        humans_bin(avg_bps as u64)
                    ))
                    .small()
                    .monospace()
                    .color(pal.ink),
                );
            });
        });
    }

    fn render_delete_confirm(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(mut confirm) = self.delete_confirm.clone() else {
            return;
        };
        let mut open = true;
        let mut cancel = false;
        let mut do_delete = false;
        egui::Window::new("Remove Download")
            .open(&mut open)
            .title_bar(false)
            .frame(views::modal_frame(ctx, pal))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(egui::vec2(430.0, 190.0))
            .show(ctx, |ui| {
                if dialog_header(ui, pal, IconId::Trash, "Remove Download", None) {
                    cancel = true;
                }
                ui.label(format!("Remove {} from the download queue?", confirm.label));
                ui.add_space(6.0);
                ui.checkbox(
                    &mut confirm.delete_files,
                    "Also permanently delete downloaded file(s) from disk",
                );
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new(if confirm.delete_files {
                                "Delete Files & Remove"
                            } else {
                                "Remove Task"
                            })
                            .color(Color32::WHITE)
                            .strong(),
                        )
                        .fill(if confirm.delete_files { pal.danger } else { pal.accent });
                        if ui.add(btn).clicked() {
                            do_delete = true;
                        }
                    });
                });
            });
        if !open || cancel {
            self.delete_confirm = None;
        } else if do_delete {
            self.worker.send(UiCommand::Remove {
                ids: confirm.ids,
                delete_files: confirm.delete_files,
            });
            self.delete_confirm = None;
        } else {
            self.delete_confirm = Some(confirm);
        }
    }

    fn render_about_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("About ripdl")
            .open(&mut open)
            .title_bar(false)
            .frame(views::modal_frame(ctx, pal))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(egui::vec2(430.0, 220.0))
            .show(ctx, |ui| {
                if dialog_header(ui, pal, IconId::Details, "About ripdl", None) {
                    close = true;
                }
                ui.horizontal(|ui| {
                    let (logo_rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
                    paint_brand_logo(ui.painter(), logo_rect);
                    ui.vertical(|ui| {
                        ui.label(semibold("ripdl", 16.0, pal.ink));
                        ui.label(
                            egui::RichText::new(format!("Version v{}", env!("CARGO_PKG_VERSION")))
                                .small()
                                .monospace()
                                .color(pal.ink_secondary),
                        );
                    });
                });
                ui.add_space(8.0);
                ui.label("Free, open-source multi-segment download manager in Rust.");
                ui.label(
                    egui::RichText::new("Zero telemetry, zero ads, MIT / Apache-2.0 licensed.")
                        .small()
                        .color(pal.ink_muted),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if vector_button(ui, pal, IconId::External, "GitHub Repo", 26.0, false, true).clicked() {
                        views::open_external_url("https://github.com/ShiniM0d0ri/ripdl");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
            });
        if !open || close {
            self.about_open = false;
        }
    }

    fn render_duplicate_prompt(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(prompt) = self.duplicate_prompt.clone() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let existing = &prompt.existing;
        let req = &prompt.request;

        egui::Window::new("Already in Queue")
            .open(&mut open)
            .title_bar(false)
            .frame(views::modal_frame(ctx, pal))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_size(egui::vec2(500.0, 260.0))
            .show(ctx, |ui| {
                if dialog_header(
                    ui,
                    pal,
                    IconId::Alert,
                    "Already in Queue",
                    Some("This URL matches an existing task in your download list"),
                ) {
                    close = true;
                }

                views::section_card(ui, pal, |ui| {
                    ui.horizontal(|ui| {
                        let (tr, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                        paint_file_tile(ui.painter(), tr, &existing.kind, existing.category, pal);
                        ui.vertical(|ui| {
                            ui.label(semibold(&existing.name, 13.5, pal.ink));
                            let progress_txt = match existing.total {
                                Some(tot) => format!("{} / {}", humans_bin(existing.downloaded), humans_bin(tot)),
                                None if existing.downloaded > 0 => humans_bin(existing.downloaded),
                                None => "size unknown".into(),
                            };
                            let folder = existing
                                .dest_file
                                .as_ref()
                                .and_then(|p| p.parent())
                                .or(existing.dest_dir.as_deref())
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "—".into());
                            ui.label(
                                egui::RichText::new(format!("Progress: {progress_txt}  ·  {folder}"))
                                    .small()
                                    .color(pal.ink_muted),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (st_lbl, st_fg, st_bg, st_dot) = state_visuals(&existing.state, pal);
                            paint_status_pill(ui, st_lbl, st_fg, st_bg, st_dot);
                        });
                    });
                });

                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if existing.raw_state == TaskState::Stale && existing.can_refresh_url() {
                        if vector_button(ui, pal, IconId::Refresh, "Refresh Address…", 28.0, true, true).clicked() {
                            self.open_refresh_for(existing);
                            close = true;
                        }
                    } else if existing.can_resume() {
                        if vector_button(ui, pal, IconId::Play, "Resume Existing", 28.0, true, true).clicked() {
                            self.worker.send(UiCommand::Resume {
                                ids: vec![existing.id.clone()],
                            });
                            self.selected_ids.clear();
                            self.selected_ids.insert(existing.id.clone());
                            self.anchor_id = Some(existing.id.clone());
                            close = true;
                        }
                    } else if existing.raw_state == TaskState::Completed && existing.dest_file.is_some() {
                        if vector_button(ui, pal, IconId::OpenFile, "Open File", 28.0, true, true).clicked() {
                            self.worker.send(UiCommand::Open {
                                id: existing.id.clone(),
                                folder: false,
                            });
                            close = true;
                        }
                    }

                    if req.kind != ripdl_core::task::TaskKind::Torrent
                        && vector_button(ui, pal, IconId::Add, "Download Numbered Copy", 28.0, false, true).clicked()
                    {
                        self.worker.send(UiCommand::Add {
                            url: req.url.clone(),
                            dest_dir: req.dest_dir.clone(),
                            headers: req.headers.clone(),
                            start_at: req.start_at,
                            kind: req.kind,
                            sequential: req.sequential,
                            only_files: req.only_files.clone(),
                            duplicate: ripdl_core::rpc::DuplicatePolicy::Copy,
                        });
                        close = true;
                    }

                    if ui.button("Select in Queue").clicked() {
                        self.selected_ids.clear();
                        self.selected_ids.insert(existing.id.clone());
                        self.anchor_id = Some(existing.id.clone());
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if !open || close {
            self.duplicate_prompt = None;
        }
    }
}

impl eframe::App for RipdlNativeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_worker_events();
        let snap = self.worker.state.lock().unwrap().clone();
        let ctx = ui.ctx().clone();
        let pal = Palette::for_mode(self.theme_mode);

        if let Some(s) = snap.settings.as_ref() {
            self.hide_to_tray = s.hide_to_tray.unwrap_or(true);
            if !self.onboarding_checked && snap.connected {
                self.onboarding_checked = true;
                let has_profile_files = ripdl_core::transport::app_data_dir()
                    .is_ok_and(|d| d.join("config.json").is_file() || d.join("queue.json").is_file());
                if should_auto_open_onboarding(
                    s,
                    &snap.tasks,
                    snap.extension.as_ref(),
                    has_profile_files,
                ) {
                    self.onboarding.reset_from(Some(s));
                }
            }
        }

        while let Ok(action) = self.tray_rx.try_recv() {
            match action {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Quit => {
                    self.force_quit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        if ctx.input(|i| i.viewport().close_requested())
            && !self.force_quit
            && !is_installing()
            && self.hide_to_tray
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        if self.details.task_id.is_some()
            && self.last_details_poll.elapsed() >= Duration::from_secs(2)
        {
            if let Some(id) = self.details.task_id.clone() {
                self.last_details_poll = Instant::now();
                self.worker.send(UiCommand::FetchDetails { id });
            }
        }
        if self.lan.open && self.last_lan_poll.elapsed() >= Duration::from_secs(2) {
            self.last_lan_poll = Instant::now();
            self.worker.send(UiCommand::FetchLan);
        }

        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let sz = rect.size();
            if sz.x >= 600.0 && sz.y >= 400.0 && self.saved_window_size != Some(sz) {
                self.saved_window_size = Some(sz);
                save_window_size(sz.x, sz.y);
            }
        }

        self.handle_shortcuts_and_drop(&ctx, &snap);

        egui::Panel::top("menu")
            .frame(egui::Frame::default().fill(pal.surface).inner_margin(egui::Margin::symmetric(12, 5)))
            .show(ui, |ui| self.render_menu_bar(ui, &snap, &pal));

        egui::Panel::top("toolbar")
            .frame(egui::Frame::default().fill(pal.surface).inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ui, |ui| self.render_toolbar(ui, &snap, &pal));

        egui::Panel::bottom("status")
            .frame(egui::Frame::default().fill(pal.surface).inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ui, |ui| self.render_footer(ui, &snap, &pal));

        egui::Panel::left("sidebar")
            .default_size(metrics::SIDEBAR_W)
            .resizable(false)
            .frame(egui::Frame::default().fill(pal.surface).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| self.render_sidebar(ui, &snap, &pal));

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(pal.canvas).inner_margin(egui::Margin::symmetric(14, 10)))
            .show(ui, |ui| self.render_table_area(ui, &snap, &pal));

        // Drag-and-drop overlay indicator.
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let layer = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("drop_hint")));
            layer.rect_filled(screen, 0.0, pal.scrim);
            let box_rect = Rect::from_center_size(screen.center(), egui::vec2(340.0, 140.0));
            layer.rect_filled(box_rect, CornerRadius::same(12), pal.surface);
            layer.rect_stroke(box_rect, CornerRadius::same(12), Stroke::new(2.0, pal.accent), StrokeKind::Inside);
            let ir = Rect::from_center_size(box_rect.center() - egui::vec2(0.0, 22.0), egui::vec2(32.0, 32.0));
            paint_icon(&layer, ir, IconId::Download, pal.accent);
            layer.text(
                box_rect.center() + egui::vec2(0.0, 20.0),
                egui::Align2::CENTER_CENTER,
                "Drop .torrent file or URL to queue",
                egui::FontId::new(14.5, egui::FontFamily::Name("semibold".into())),
                pal.ink,
            );
        }

        // Dimmed backdrop scrim underneath modals without polluting AccessKit with unnamed nodes.
        // Painted on Order::Background (non-area layers drain after all panels in Order::Background
        // and strictly before modal egui::Windows in Order::Middle).
        if self.any_modal_open() {
            let screen = ctx.content_rect();
            let layer = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, ui.scope_id().with("modal_scrim")));
            layer.rect_filled(screen, 0.0, pal.scrim);
        }

        views::onboarding_dialog::show(&ctx, &mut self.onboarding, &snap, &self.worker, &pal);
        views::add_dialog::show(&ctx, &mut self.add, &snap, &self.worker, &pal);
        let mut queued_notice: Option<(String, bool)> = None;
        {
            let mut emit = |msg: String, is_err: bool| {
                queued_notice = Some((msg, is_err));
            };
            views::details_dialog::show(
                &ctx,
                &mut self.details,
                &mut self.cached_details,
                &mut self.refresh,
                &snap,
                &self.worker,
                &pal,
                &mut emit,
            );
            views::refresh_dialog::show(&ctx, &mut self.refresh, &self.worker, &pal);
            views::lan_dialog::show(&ctx, &mut self.lan, &snap, &self.worker, &pal, &mut emit);
            views::settings_dialog::show(&ctx, &mut self.settings, &snap, &self.worker, &pal, &mut emit);
        }
        if let Some((msg, is_err)) = queued_notice {
            self.set_notice(msg, is_err);
        }
        self.render_delete_confirm(&ctx, &pal);
        self.render_duplicate_prompt(&ctx, &pal);
        self.render_about_dialog(&ctx, &pal);

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

#[derive(Debug, Clone, Copy)]
struct TableColumns {
    name: (f32, f32),
    kind: (f32, f32),
    size: (f32, f32),
    progress: (f32, f32),
    status: (f32, f32),
    rate: (f32, f32),
    peers: Option<(f32, f32)>,
    eta: Option<(f32, f32)>,
    actions: (f32, f32),
}

fn table_columns(total_w: f32, show_eta: bool, show_peers: bool) -> TableColumns {
    let gap = 6.0_f32;
    let kind_w = 74.0_f32;
    let size_w = 102.0_f32;
    let prog_w = 128.0_f32;
    let stat_w = 102.0_f32;
    let rate_w = 84.0_f32;
    let peers_w = if show_peers { 46.0 } else { 0.0 };
    let eta_w = if show_eta { 48.0 } else { 0.0 };
    // Fits `[Refresh Fix] [Details] [Trash]` cleanly (`58 + 4 + 26 + 4 + 26 = 118`) without clipping.
    let act_w = 120.0_f32;

    let gaps = 6 + usize::from(show_peers) + usize::from(show_eta);
    let mut fixed = kind_w + size_w + prog_w + stat_w + rate_w + act_w + gap * (gaps as f32);
    if show_peers {
        fixed += peers_w;
    }
    if show_eta {
        fixed += eta_w;
    }
    let name_w = (total_w - fixed).max(140.0);

    let mut x = 0.0_f32;
    let mut take = |w: f32| -> (f32, f32) {
        let span = (x, w);
        x += w + gap;
        span
    };

    let name = take(name_w);
    let kind = take(kind_w);
    let size = take(size_w);
    let progress = take(prog_w);
    let status = take(stat_w);
    let rate = take(rate_w);
    let peers = if show_peers { Some(take(peers_w)) } else { None };
    let eta = if show_eta { Some(take(eta_w)) } else { None };
    let actions = take(act_w);

    TableColumns {
        name,
        kind,
        size,
        progress,
        status,
        rate,
        peers,
        eta,
        actions,
    }
}

fn paint_static_header(
    ui: &mut egui::Ui,
    origin_x: f32,
    hdr_rect: Rect,
    span: (f32, f32),
    label: &str,
    pal: &Palette,
) {
    let rect = Rect::from_min_max(
        egui::pos2(origin_x + span.0, hdr_rect.min.y + 2.0),
        egui::pos2(origin_x + span.0 + span.1, hdr_rect.max.y - 2.0),
    );
    in_rect_left(ui, rect, |ui| {
        ui.label(egui::RichText::new(label).size(10.5).strong().color(pal.ink_muted));
    });
}

fn in_rect_left(ui: &mut egui::Ui, rect: Rect, add: impl FnOnce(&mut egui::Ui)) {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    add(&mut child);
}

fn toolbar_sep(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 22.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.center().x, rect.min.y + 2.0),
            egui::pos2(rect.center().x, rect.max.y - 2.0),
        ],
        Stroke::new(1.0, pal.border),
    );
}

fn geometry_path() -> Option<std::path::PathBuf> {
    ripdl_core::transport::app_data_dir()
        .ok()
        .map(|d| d.join("gui-geometry.json"))
}

fn load_window_size() -> egui::Vec2 {
    if let Some(p) = geometry_path() {
        if let Ok(bytes) = std::fs::read(p) {
            if let Ok(v) = serde_json::from_slice::<[f32; 2]>(&bytes) {
                if v[0] >= 720.0 && v[1] >= 480.0 && v[0] <= 4096.0 && v[1] <= 2160.0 {
                    return egui::vec2(v[0], v[1]);
                }
            }
        }
    }
    egui::vec2(1180.0, 720.0)
}

fn save_window_size(w: f32, h: f32) {
    if let Some(p) = geometry_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, format!("[{:.0},{:.0}]", w, h));
    }
}

fn theme_path() -> Option<std::path::PathBuf> {
    ripdl_core::transport::app_data_dir()
        .ok()
        .map(|d| d.join("gui-theme.txt"))
}

fn load_theme_mode() -> ThemeMode {
    if let Some(p) = theme_path() {
        if let Ok(s) = std::fs::read_to_string(p) {
            if s.trim().eq_ignore_ascii_case("dark") {
                return ThemeMode::Dark;
            }
        }
    }
    ThemeMode::Light
}

fn save_theme_mode(mode: ThemeMode) {
    if let Some(p) = theme_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let val = match mode {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        let _ = std::fs::write(p, val);
    }
}

fn main() -> eframe::Result<()> {
    let instance_guard = match single_instance::claim() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => return Ok(()),
        Err(_) => None,
    };

    std::thread::spawn(|| {
        let _ = ext_host::sync_managed_extension();
    });

    let size = load_window_size();
    let mut viewport = egui::ViewportBuilder::default()
        .with_visible(true)
        .with_inner_size(size)
        .with_min_inner_size(egui::vec2(860.0, 520.0));
    if let Some((rgba, width, height)) = tray::load_icon_rgba() {
        viewport = viewport.with_icon(Arc::new(egui::IconData {
            rgba,
            width,
            height,
        }));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "ripdl",
        options,
        Box::new(move |cc| {
            let (tray_tx, tray_rx) = mpsc::channel();
            let tray = tray::init(tray_tx.clone(), cc.egui_ctx.clone());
            if let Some(guard) = instance_guard {
                let wake_ctx = cc.egui_ctx.clone();
                let show_tx = tray_tx;
                guard.spawn_listener(move || {
                    let _ = show_tx.send(TrayAction::Show);
                    wake_ctx.request_repaint();
                });
            }
            Ok(Box::new(RipdlNativeApp::new(cc, tray_rx, tray)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_columns_never_overflow_and_stay_non_overlapping() {
        for total_w in [900.0, 960.0, 1280.0] {
            for (show_eta, show_peers) in [(true, false), (true, true), (false, false)] {
                let c = table_columns(total_w, show_eta, show_peers);
                let mut spans = vec![c.name, c.kind, c.size, c.progress, c.status, c.rate];
                if let Some(p) = c.peers {
                    spans.push(p);
                }
                if let Some(e) = c.eta {
                    spans.push(e);
                }
                spans.push(c.actions);
                for win in spans.windows(2) {
                    assert!(
                        win[0].0 + win[0].1 <= win[1].0,
                        "columns overlapped at width {total_w}: {win:?}"
                    );
                }
                let last = spans.last().unwrap();
                assert!(
                    last.0 + last.1 <= total_w + 0.01,
                    "actions column clipped ({} > {total_w}) at eta={show_eta} peers={show_peers}",
                    last.0 + last.1
                );
            }
        }
    }
}
