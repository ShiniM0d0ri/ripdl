use eframe::egui::{self, Stroke};

use ripdl_core::rpc::BundlePart;
use ripdl_core::task::{TaskKind, TaskState};

use crate::model::{
    humans_bin, CachedDetails, DetailsDraft, DetailsTab, RefreshDraft, Snapshot, TaskRow,
};
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{
    paint_progress_bar, paint_status_pill, semibold, state_visuals, Palette,
};
use crate::vectors::{
    paint_file_tile, paint_focus_ring, paint_icon, paint_missing_badge, vector_button,
    vector_icon_button, IconId,
};
use crate::views::{modal_frame, section_card};

pub fn show(
    ctx: &egui::Context,
    draft: &mut DetailsDraft,
    cached: &mut CachedDetails,
    refresh: &mut RefreshDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
    on_notice: &mut dyn FnMut(String, bool),
) {
    let Some(task_id) = draft.task_id.clone() else {
        return;
    };
    let row = snap.tasks.iter().find(|t| t.id == task_id).cloned();
    let Some(task) = row else {
        draft.task_id = None;
        return;
    };

    let mut open_flag = true;
    let mut close_clicked = false;
    let is_torrent = task.kind == "torrent";
    let is_bundle = task.kind == "bundle";

    if is_torrent && matches!(draft.tab, DetailsTab::Segments | DetailsTab::Parts) {
        draft.tab = DetailsTab::Files;
    } else if !is_torrent
        && matches!(draft.tab, DetailsTab::Files | DetailsTab::Peers | DetailsTab::Trackers)
    {
        draft.tab = if is_bundle {
            DetailsTab::Parts
        } else {
            DetailsTab::Overview
        };
    }

    if is_torrent && draft.synced_selection_for != task_id {
        if let Some(td) = cached.torrent.as_ref().filter(|_| cached.task_id == task_id) {
            draft.selected_files = td
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.included)
                .map(|(i, _)| i)
                .collect();
            draft.synced_selection_for = task_id.clone();
        }
    }

    egui::Window::new(format!("Details — {}", task.name))
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(720.0, 440.0))
        .min_width(600.0)
        .show(ctx, |ui| {
            // Custom header with vector file tile, title, status pill, Reload & Close.
            ui.horizontal(|ui| {
                let (tile_rect, _) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
                paint_file_tile(ui.painter(), tile_rect, &task.kind, task.category, pal);
                if task.file_missing {
                    paint_missing_badge(ui.painter(), tile_rect, pal);
                }

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(egui::Label::new(semibold(&task.name, 14.5, pal.ink)).truncate());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let size_str = match task.total {
                            Some(t) => format!("{} / {}", humans_bin(task.downloaded), humans_bin(t)),
                            None => humans_bin(task.downloaded),
                        };
                        ui.label(egui::RichText::new(size_str).small().monospace().color(pal.ink_secondary));
                        if let Some(spd) = task.speed_bps {
                            ui.label(
                                egui::RichText::new(format!("{}/s", humans_bin(spd as u64)))
                                    .small()
                                    .monospace()
                                    .strong()
                                    .color(pal.accent_dark),
                            );
                        }
                    });
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if vector_icon_button(ui, pal, IconId::Close, "Close details", 26.0, false, false).clicked() {
                        close_clicked = true;
                    }
                    if vector_icon_button(ui, pal, IconId::Refresh, "Reload details", 26.0, false, true)
                        .on_hover_text("Reload task details")
                        .clicked()
                    {
                        cached.loading = true;
                        worker.send(UiCommand::FetchDetails { id: task_id.clone() });
                    }
                    let (st_lbl, st_fg, st_bg, st_dot) = state_visuals(&task.state, pal);
                    paint_status_pill(ui, st_lbl, st_fg, st_bg, st_dot);
                });
            });

            ui.add_space(6.0);

            // Underline-style tab bar matching design reference.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let mut tabs: Vec<(DetailsTab, &str)> = vec![(DetailsTab::Overview, "Overview")];
                if is_torrent {
                    tabs.push((DetailsTab::Files, "Files"));
                    tabs.push((DetailsTab::Peers, "Peers"));
                    tabs.push((DetailsTab::Trackers, "Trackers"));
                } else if is_bundle {
                    tabs.push((DetailsTab::Parts, "Parts"));
                } else {
                    tabs.push((DetailsTab::Segments, "Segments"));
                }
                for (tab, label) in tabs {
                    let active = draft.tab == tab;
                    let fg = if active { pal.accent_dark } else { pal.ink_muted };
                    let font = egui::FontId::new(
                        12.5,
                        if active {
                            egui::FontFamily::Name("semibold".into())
                        } else {
                            egui::FontFamily::Proportional
                        },
                    );
                    let galley = ui.painter().layout_no_wrap(label.into(), font, fg);
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(galley.size().x + 8.0, 28.0),
                        egui::Sense::click(),
                    );
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Tab, true, active, label)
                    });
                    if ui.is_rect_visible(rect) {
                        paint_focus_ring(ui.painter(), rect, 4, pal, resp.has_focus());
                        ui.painter().galley(
                            egui::pos2(rect.min.x + 4.0, rect.center().y - galley.size().y * 0.5),
                            galley,
                            fg,
                        );
                        if active {
                            ui.painter().line_segment(
                                [
                                    egui::pos2(rect.min.x, rect.max.y - 1.0),
                                    egui::pos2(rect.max.x, rect.max.y - 1.0),
                                ],
                                Stroke::new(2.0, pal.accent),
                            );
                        }
                    }
                    if resp.clicked() {
                        draft.tab = tab;
                    }
                }
            });
            ui.separator();
            ui.add_space(6.0);

            if cached.loading && cached.task_id != task_id {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading task details…");
                });
            }
            if let Some(err) = &cached.error {
                if cached.task_id == task_id {
                    ui.label(egui::RichText::new(err).color(pal.danger));
                }
            }

            egui::ScrollArea::vertical()
                .max_height(265.0)
                .show(ui, |ui| match draft.tab {
                    DetailsTab::Overview => render_overview(ui, &task, cached, pal),
                    DetailsTab::Segments => render_segments(ui, &task, cached, pal),
                    DetailsTab::Parts => render_bundle_parts(ui, &task, draft, cached, refresh, pal),
                    DetailsTab::Files => render_torrent_files(ui, &task_id, draft, cached, worker, pal),
                    DetailsTab::Peers => render_torrent_peers(ui, &task_id, cached, pal),
                    DetailsTab::Trackers => render_torrent_trackers(ui, &task_id, cached, pal),
                });

            if is_bundle && draft.add_parts_open {
                ui.add_space(6.0);
                section_card(ui, pal, |ui| {
                    ui.label(semibold("Append missing parts (one link per line)", 12.5, pal.ink));
                    ui.add(
                        egui::TextEdit::multiline(&mut draft.add_parts_text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("https://example.com/archive.part04.rar"),
                    );
                    if let Some(err) = &draft.add_parts_error {
                        ui.label(egui::RichText::new(err).small().color(pal.danger));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            draft.add_parts_open = false;
                        }
                        if vector_button(ui, pal, IconId::Add, "Add parts", 26.0, true, true).clicked() {
                            let parts: Vec<BundlePart> = draft
                                .add_parts_text
                                .lines()
                                .map(str::trim)
                                .filter(|l| !l.is_empty())
                                .map(|url| BundlePart {
                                    url: url.to_string(),
                                    filename: None,
                                    headers: Default::default(),
                                    state: TaskState::Queued,
                                    downloaded: 0,
                                    total: None,
                                    seq: None,
                                    error: None,
                                    attempted: false,
                                })
                                .collect();
                            if parts.is_empty() {
                                draft.add_parts_error = Some("Paste at least one link.".into());
                            } else {
                                draft.add_parts_error = None;
                                worker.send(UiCommand::AddBundleParts {
                                    id: task_id.clone(),
                                    parts,
                                });
                            }
                        }
                    });
                });
            }

            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if task.can_open_file()
                    && vector_button(ui, pal, IconId::OpenFile, "Open file", 28.0, false, true).clicked()
                {
                    worker.send(UiCommand::Open {
                        id: task_id.clone(),
                        folder: false,
                    });
                }
                if task.raw_state == TaskState::Completed
                    && task.file_missing
                    && vector_button(ui, pal, IconId::Refresh, "Re-download", 28.0, true, true).clicked()
                {
                    worker.send(UiCommand::Redownload {
                        id: task_id.clone(),
                    });
                }
                if (task.dest_file.is_some() || task.dest_dir.is_some())
                    && vector_button(ui, pal, IconId::FolderOpen, "Open folder", 28.0, false, true).clicked()
                {
                    worker.send(UiCommand::Open {
                        id: task_id.clone(),
                        folder: true,
                    });
                }
                if vector_button(ui, pal, IconId::Copy, "Copy link", 28.0, false, true).clicked() {
                    ctx.copy_text(task.url.clone());
                    on_notice("Download link copied to clipboard.".into(), false);
                }
                if task.can_refresh_url()
                    && vector_button(ui, pal, IconId::Refresh, "Refresh address…", 28.0, false, true).clicked()
                {
                    refresh.open = true;
                    refresh.task_id = task_id.clone();
                    refresh.task_name = task.name.clone();
                    refresh.part_seq = None;
                    refresh.url = task.url.clone();
                    refresh.last_probed_url.clear();
                    refresh.verdict_text.clear();
                    refresh.verdict_ok = false;
                    refresh.error = None;
                    refresh.resume_after = true;
                }
                if is_bundle
                    && vector_button(ui, pal, IconId::Add, "Add missing parts…", 28.0, false, true).clicked()
                {
                    draft.add_parts_open = !draft.add_parts_open;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close_clicked = true;
                    }
                });
            });
        });

    if !open_flag || close_clicked {
        draft.task_id = None;
    }
}

fn kv_row(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::vec2(118.0, 20.0),
            egui::Label::new(egui::RichText::new(label).small().color(pal.ink_muted)),
        );
        ui.add(egui::Label::new(egui::RichText::new(value).color(pal.ink)).wrap());
    });
}

fn render_overview(ui: &mut egui::Ui, task: &TaskRow, cached: &CachedDetails, pal: &Palette) {
    if task.file_missing {
        egui::Frame::new()
            .fill(pal.warn_soft)
            .stroke(Stroke::new(1.0, pal.warn))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::hover());
                    paint_icon(ui.painter(), ir, IconId::Alert, pal.warn);
                    ui.label(
                        egui::RichText::new("File does not exist on disk (it may have been deleted or moved outside ripdl).")
                            .color(pal.warn),
                    );
                });
            });
        ui.add_space(6.0);
    }
    section_card(ui, pal, |ui| {
        kv_row(ui, pal, "File / Title", &task.name);
        kv_row(ui, pal, "Source URL", &task.url);
        if let Some(dest) = &task.dest_file {
            kv_row(ui, pal, "Output path", &dest.display().to_string());
        } else if let Some(dir) = &task.dest_dir {
            kv_row(ui, pal, "Save folder", &dir.display().to_string());
        }
        if let Some(http) = cached.http.as_ref().filter(|h| h.id == task.id) {
            if let Some(etag) = &http.etag {
                kv_row(ui, pal, "Server ETag", etag);
            }
            if let Some(reason) = &http.stale_reason {
                kv_row(ui, pal, "Stale reason", reason);
            }
            if let Some(err) = &http.error {
                kv_row(ui, pal, "Error detail", err);
            }
            if http.kind == TaskKind::Bundle {
                kv_row(
                    ui,
                    pal,
                    "Parts progress",
                    &format!("{} / {}", task.parts_done, task.parts_total),
                );
            }
        }
        if let Some(td) = cached.torrent.as_ref().filter(|_| cached.task_id == task.id) {
            kv_row(ui, pal, "Info hash", &td.info_hash);
            kv_row(
                ui,
                pal,
                "Swarm counters",
                &format!(
                    "Down {} · Up {} · {} connected peers",
                    humans_bin(td.downloaded),
                    humans_bin(td.uploaded),
                    td.peers_connected
                ),
            );
        }
    });
}

fn render_segments(ui: &mut egui::Ui, task: &TaskRow, cached: &CachedDetails, pal: &Palette) {
    let Some(http) = cached.http.as_ref().filter(|h| h.id == task.id) else {
        ui.label(egui::RichText::new("Fetching segment table…").color(pal.ink_muted));
        return;
    };
    if http.segments.is_empty() {
        ui.label(
            egui::RichText::new("No active byte-range segments (completed or single un-probed stream).")
                .color(pal.ink_muted),
        );
        return;
    }
    egui::Grid::new("seg_grid")
        .striped(true)
        .min_col_width(80.0)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("#").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("BYTE RANGE").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("PROGRESS").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("DONE").small().strong().color(pal.ink_muted));
            ui.end_row();
            for s in &http.segments {
                ui.label(egui::RichText::new(format!("{}", s.index)).monospace());
                if s.end == u64::MAX {
                    ui.label(format!("{} -> to EOF", humans_bin(s.start)));
                    paint_progress_bar(ui, 0.0, &task.state, 180.0, pal);
                    ui.label(egui::RichText::new(humans_bin(s.done)).monospace());
                } else {
                    let span = s.end.saturating_sub(s.start).saturating_add(1).max(1);
                    let ratio = (s.done as f64 / span as f64) as f32;
                    ui.label(format!("{} - {}", humans_bin(s.start), humans_bin(s.end)));
                    paint_progress_bar(ui, ratio, &task.state, 180.0, pal);
                    ui.label(
                        egui::RichText::new(format!("{} / {}", humans_bin(s.done), humans_bin(span)))
                            .monospace(),
                    );
                }
                ui.end_row();
            }
        });
}

fn render_bundle_parts(
    ui: &mut egui::Ui,
    task: &TaskRow,
    _draft: &mut DetailsDraft,
    cached: &CachedDetails,
    refresh: &mut RefreshDraft,
    pal: &Palette,
) {
    let Some(http) = cached.http.as_ref().filter(|h| h.id == task.id) else {
        ui.label(egui::RichText::new("Loading bundle parts…").color(pal.ink_muted));
        return;
    };
    if http.parts.is_empty() {
        ui.label("No parts recorded.");
        return;
    }
    egui::Grid::new("parts_grid")
        .striped(true)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("#").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("FILENAME").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("PROGRESS").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("BYTES").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("ACTION").small().strong().color(pal.ink_muted));
            ui.end_row();
            for (idx, p) in http.parts.iter().enumerate() {
                ui.label(
                    egui::RichText::new(
                        p.seq
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| format!("{}", idx + 1)),
                    )
                    .monospace(),
                );
                let fname = p
                    .filename
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&p.url);
                ui.label(fname);
                let ratio = p
                    .total
                    .filter(|&tot| tot > 0)
                    .map(|tot| (p.downloaded as f64 / tot as f64) as f32)
                    .unwrap_or(if p.state == TaskState::Completed { 1.0 } else { 0.0 });
                paint_progress_bar(ui, ratio, &format!("{:?}", p.state).to_lowercase(), 160.0, pal);
                let bytes = match p.total {
                    Some(t) => format!("{} / {}", humans_bin(p.downloaded), humans_bin(t)),
                    None => humans_bin(p.downloaded),
                };
                ui.label(egui::RichText::new(bytes).monospace());
                if p.state != TaskState::Completed && p.seq.is_some() {
                    if vector_button(ui, pal, IconId::Refresh, "Refresh", 24.0, false, true).clicked() {
                        refresh.open = true;
                        refresh.task_id = task.id.clone();
                        refresh.task_name = fname.to_string();
                        refresh.part_seq = p.seq;
                        refresh.url = p.url.clone();
                        refresh.last_probed_url.clear();
                        refresh.verdict_text.clear();
                        refresh.verdict_ok = false;
                        refresh.error = None;
                        refresh.resume_after = true;
                    }
                } else {
                    ui.label(egui::RichText::new("—").color(pal.ink_muted));
                }
                ui.end_row();
            }
        });
}

fn render_torrent_files(
    ui: &mut egui::Ui,
    task_id: &str,
    draft: &mut DetailsDraft,
    cached: &CachedDetails,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    let Some(td) = cached.torrent.as_ref().filter(|_| cached.task_id == task_id) else {
        ui.label(egui::RichText::new("Loading torrent file list…").color(pal.ink_muted));
        return;
    };
    ui.horizontal(|ui| {
        if ui.small_button("Select All").clicked() {
            draft.selected_files = (0..td.files.len()).collect();
        }
        if ui.small_button("Select None").clicked() {
            draft.selected_files.clear();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if vector_button(ui, pal, IconId::Check, "Apply file selection", 26.0, true, true).clicked() {
                let mut only: Vec<usize> = draft.selected_files.iter().copied().collect();
                only.sort_unstable();
                let payload = if only.len() == td.files.len() {
                    None
                } else {
                    Some(only)
                };
                worker.send(UiCommand::UpdateTorrentSelection {
                    id: task_id.to_string(),
                    only_files: payload,
                });
            }
        });
    });
    ui.add_space(6.0);
    egui::Grid::new("tf_grid")
        .striped(true)
        .spacing([22.0, 7.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("WANT").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("FILE").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("PROGRESS").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("SIZE").small().strong().color(pal.ink_muted));
            ui.end_row();
            for (idx, f) in td.files.iter().enumerate() {
                let mut inc = draft.selected_files.contains(&idx);
                if ui.checkbox(&mut inc, "").changed() {
                    if inc {
                        draft.selected_files.insert(idx);
                    } else {
                        draft.selected_files.remove(&idx);
                    }
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (fr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                    paint_icon(ui.painter(), fr, IconId::Document, pal.ink_secondary);
                    ui.label(&f.name);
                });
                let ratio = if f.size > 0 {
                    (f.downloaded as f64 / f.size as f64) as f32
                } else {
                    1.0
                };
                paint_progress_bar(
                    ui,
                    ratio,
                    if ratio >= 1.0 { "completed" } else { "running" },
                    160.0,
                    pal,
                );
                ui.label(egui::RichText::new(humans_bin(f.size)).monospace());
                ui.end_row();
            }
        });
}

fn render_torrent_peers(ui: &mut egui::Ui, task_id: &str, cached: &CachedDetails, pal: &Palette) {
    let Some(td) = cached.torrent.as_ref().filter(|_| cached.task_id == task_id) else {
        ui.label(egui::RichText::new("Loading peer list…").color(pal.ink_muted));
        return;
    };
    if td.peers.is_empty() {
        ui.label(egui::RichText::new("No connected peers yet.").color(pal.ink_muted));
        return;
    }
    egui::Grid::new("tp_grid")
        .striped(true)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("ADDRESS").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("CLIENT").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("STATE").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("TOTAL DOWN").small().strong().color(pal.ink_muted));
            ui.label(egui::RichText::new("TOTAL UP").small().strong().color(pal.ink_muted));
            ui.end_row();
            for p in &td.peers {
                ui.label(egui::RichText::new(&p.addr).monospace());
                ui.label(p.client.as_deref().unwrap_or("—"));
                ui.label(&p.state);
                ui.label(egui::RichText::new(humans_bin(p.downloaded)).monospace());
                ui.label(egui::RichText::new(humans_bin(p.uploaded)).monospace());
                ui.end_row();
            }
        });
}

fn render_torrent_trackers(ui: &mut egui::Ui, task_id: &str, cached: &CachedDetails, pal: &Palette) {
    let Some(td) = cached.torrent.as_ref().filter(|_| cached.task_id == task_id) else {
        ui.label(egui::RichText::new("Loading tracker list…").color(pal.ink_muted));
        return;
    };
    if td.trackers.is_empty() {
        ui.label(egui::RichText::new("DHT / peerless or no trackers configured.").color(pal.ink_muted));
        return;
    }
    section_card(ui, pal, |ui| {
        for trk in &td.trackers {
            ui.label(egui::RichText::new(trk).monospace().small());
        }
    });
}
