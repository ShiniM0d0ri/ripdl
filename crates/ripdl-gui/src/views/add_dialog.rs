use eframe::egui;

use ripdl_core::rpc::{BundlePart, DuplicatePolicy};
use ripdl_core::task::{TaskKind, TaskState};
use ripdl_core::torrent::looks_like_torrent_payload;

use crate::model::{
    humans_bin, now_unix, parse_after_delay, parse_at_hhmm, parse_headers_block, resolved_add_dir,
    AddDraft, AddMode, FilePickTarget, ScheduleKind, Snapshot,
};
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{dialog_header, Palette};
use crate::vectors::{paint_focus_ring, paint_icon, vector_button, IconId};
use crate::views::{modal_frame, section_card};

pub fn show(
    ctx: &egui::Context,
    draft: &mut AddDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    if !draft.open {
        return;
    }

    let mut close_clicked = false;
    let mut submit_clicked = false;
    let mut open_flag = true;

    let (title, hdr_icon, sub) = match draft.mode {
        AddMode::Bundle => (
            "Add Multi-part Bundle",
            IconId::Bundle,
            "Group split archive links into one organized task folder",
        ),
        AddMode::Torrent => (
            "Add Torrent / Magnet",
            IconId::Torrent,
            "Open a .torrent file or paste a magnet URI",
        ),
        AddMode::Video => (
            "Add Video / Stream URL",
            IconId::Video,
            "Capture media streams through yt-dlp sidecar",
        ),
        AddMode::AutoOrHttp => (
            "New Download",
            IconId::Download,
            "Direct HTTPS download, magnet URI, .torrent file, or batch links",
        ),
    };

    egui::Window::new(title)
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(610.0, 460.0))
        .min_width(530.0)
        .max_width(700.0)
        .show(ctx, |ui| {
            if dialog_header(ui, pal, hdr_icon, title, Some(sub)) {
                close_clicked = true;
            }

            // Segmented vector mode bar.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (m, ico, label) in [
                    (AddMode::AutoOrHttp, IconId::Download, "Auto / Direct"),
                    (AddMode::Video, IconId::Video, "Video"),
                    (AddMode::Torrent, IconId::Torrent, "Torrent / Magnet"),
                    (AddMode::Bundle, IconId::Bundle, "Bundle"),
                ] {
                    let active = draft.mode == m;
                    let (fill, stroke, fg) = if active {
                        (pal.accent_soft, egui::Stroke::new(1.0, pal.accent), pal.accent_dark)
                    } else {
                        (pal.surface_alt, egui::Stroke::new(1.0, pal.border), pal.ink_secondary)
                    };
                    let font = egui::FontId::new(12.0, egui::FontFamily::Proportional);
                    let galley = ui.painter().layout_no_wrap(label.into(), font, fg);
                    let w = 10.0 + 13.0 + 6.0 + galley.size().x + 10.0;
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 28.0), egui::Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, label));
                    if ui.is_rect_visible(rect) {
                        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), fill);
                        ui.painter().rect_stroke(rect, egui::CornerRadius::same(6), stroke, egui::StrokeKind::Inside);
                        paint_focus_ring(ui.painter(), rect, 6, pal, resp.has_focus());
                        let ir = egui::Rect::from_center_size(
                            egui::pos2(rect.min.x + 16.5, rect.center().y),
                            egui::vec2(13.0, 13.0),
                        );
                        paint_icon(ui.painter(), ir, ico, fg);
                        ui.painter().galley(
                            egui::pos2(rect.min.x + 28.0, rect.center().y - galley.size().y * 0.5),
                            galley,
                            fg,
                        );
                    }
                    if resp.clicked() {
                        draft.mode = m;
                        draft.error = None;
                    }
                }
            });
            ui.add_space(8.0);

            // URL / Payload input.
            let hint = match draft.mode {
                AddMode::Bundle => "Paste one URL per line (e.g. part01.rar, part02.rar)…",
                AddMode::Torrent => "magnet:?xt=... or path to local .torrent file",
                AddMode::Video => "Video page URL for yt-dlp",
                AddMode::AutoOrHttp => "https://..., magnet:?..., path/to/file.torrent, or paste multiple URLs",
            };
            let prev_url = draft.url_text.clone();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if draft.mode == AddMode::Bundle {
                        "Links (one per line)"
                    } else {
                        "Address or .torrent path"
                    })
                    .small()
                    .strong()
                    .color(pal.ink_secondary),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if vector_button(ui, pal, IconId::Folder, "Pick .torrent…", 26.0, false, true).clicked() {
                        worker.pick_path(
                            FilePickTarget::AddTorrentPayload,
                            false,
                            None,
                            Some(("Torrent", &["torrent"])),
                            ctx.clone(),
                        );
                    }
                });
            });

            let rows = if draft.mode == AddMode::Bundle { 4 } else { 2 };
            ui.add(
                egui::TextEdit::multiline(&mut draft.url_text)
                    .hint_text(hint)
                    .desired_rows(rows)
                    .desired_width(f32::INFINITY),
            );

            if draft.url_text != prev_url {
                draft.error = None;
                let non_empty: Vec<&str> = draft
                    .url_text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                if draft.mode == AddMode::AutoOrHttp {
                    if non_empty.len() > 1 {
                        draft.mode = AddMode::Bundle;
                    } else if non_empty.len() == 1 && looks_like_torrent_payload(non_empty[0]) {
                        draft.mode = AddMode::Torrent;
                    }
                }
            }

            ui.add_space(8.0);

            let first_url = draft
                .url_text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let preview_dest = resolved_add_dir(
                &draft.dest_dir,
                snap.settings.as_ref(),
                &draft.category,
                first_url,
            );

            section_card(ui, pal, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        egui::vec2(72.0, 22.0),
                        egui::Label::new(egui::RichText::new("Save to").small().strong().color(pal.ink_secondary)),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.dest_dir)
                            .hint_text(preview_dest.display().to_string())
                            .desired_width(350.0),
                    );
                    if vector_button(ui, pal, IconId::Folder, "Browse…", 26.0, false, true).clicked() {
                        worker.pick_path(
                            FilePickTarget::AddDestDir,
                            true,
                            Some(preview_dest.clone()),
                            None,
                            ctx.clone(),
                        );
                    }
                    if !draft.dest_dir.trim().is_empty() && ui.button("Reset").clicked() {
                        draft.dest_dir.clear();
                    }
                });

                if draft.dest_dir.trim().is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            egui::vec2(72.0, 22.0),
                            egui::Label::new(egui::RichText::new("Category").small().strong().color(pal.ink_secondary)),
                        );
                        egui::ComboBox::from_id_salt("add_cat_combo")
                            .selected_text(category_label(&draft.category))
                            .show_ui(ui, |ui| {
                                for (val, lbl) in [
                                    ("auto", "Auto-detect"),
                                    ("video", "Video"),
                                    ("music", "Music"),
                                    ("images", "Images"),
                                    ("documents", "Documents"),
                                    ("archives", "Archives"),
                                    ("programs", "Programs"),
                                    ("other", "Other (root folder)"),
                                ] {
                                    ui.selectable_value(&mut draft.category, val.to_string(), lbl);
                                }
                            });
                        ui.label(
                            egui::RichText::new(format!("-> {}", preview_dest.display()))
                                .small()
                                .color(pal.ink_muted),
                        );
                    });
                }
            });

            if draft.mode == AddMode::Torrent {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut draft.sequential, "Download sequentially (streamable)");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_fetch = !first_url.is_empty() && !draft.torrent_preview_busy;
                        if ui
                            .add_enabled_ui(can_fetch, |ui| {
                                vector_button(ui, pal, IconId::Details, "Inspect files…", 26.0, false, true)
                            })
                            .inner
                            .clicked()
                        {
                            draft.torrent_preview_busy = true;
                            draft.torrent_preview_error = None;
                            draft.torrent_preview_for = first_url.to_string();
                            worker.send(UiCommand::TorrentPreview {
                                payload: first_url.to_string(),
                            });
                        }
                    });
                });
                if draft.torrent_preview_busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Fetching torrent metadata…").small().color(pal.ink_secondary));
                    });
                }
                if let Some(err) = &draft.torrent_preview_error {
                    ui.label(egui::RichText::new(err).small().color(pal.danger));
                }
                if let Some(prev) = &draft.torrent_preview {
                    section_card(ui, pal, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(prev.name.as_deref().unwrap_or("Torrent files")).strong());
                            if ui.small_button("All").clicked() {
                                draft.torrent_selected = (0..prev.files.len()).collect();
                            }
                            if ui.small_button("None").clicked() {
                                draft.torrent_selected.clear();
                            }
                            let sel_bytes: u64 = prev
                                .files
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| draft.torrent_selected.contains(i))
                                .map(|(_, f)| f.size)
                                .sum();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}/{} files · {}",
                                    draft.torrent_selected.len(),
                                    prev.files.len(),
                                    humans_bin(sel_bytes)
                                ))
                                .small()
                                .color(pal.ink_muted),
                            );
                        });
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for (idx, f) in prev.files.iter().enumerate() {
                                let mut checked = draft.torrent_selected.contains(&idx);
                                if ui.checkbox(&mut checked, format!("{}  ({})", f.name, humans_bin(f.size))).changed() {
                                    if checked {
                                        draft.torrent_selected.insert(idx);
                                    } else {
                                        draft.torrent_selected.remove(&idx);
                                    }
                                }
                            }
                        });
                    });
                }
            }

            if draft.mode == AddMode::Bundle {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Bundle folder:");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.bundle_name)
                            .hint_text("Auto from shared filename stem")
                            .desired_width(240.0),
                    );
                    let links: Vec<BundlePart> = draft
                        .url_text
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
                    if ui
                        .add_enabled_ui(!links.is_empty() && !draft.bundle_preview_busy, |ui| {
                            vector_button(ui, pal, IconId::Details, "Group preview", 26.0, false, true)
                        })
                        .inner
                        .clicked()
                    {
                        draft.bundle_preview_busy = true;
                        draft.bundle_preview_error = None;
                        worker.send(UiCommand::PreviewBundle { links });
                    }
                });
                if let Some(err) = &draft.bundle_preview_error {
                    ui.label(egui::RichText::new(err).small().color(pal.danger));
                }
                if let Some(groups) = &draft.bundle_preview {
                    section_card(ui, pal, |ui| {
                        for g in groups {
                            ui.label(egui::RichText::new(format!("{} — {}", g.name, g.parts_summary())).small().strong());
                            if !g.missing.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("Missing part numbers: {:?}", g.missing))
                                        .small()
                                        .color(pal.warn),
                                );
                            }
                        }
                    });
                }
            }

            ui.add_space(6.0);
            let adv_col = pal.ink_muted;
            egui::CollapsingHeader::new("Schedule, duplicate rule & headers")
                .default_open(draft.advanced_open)
                .icon(move |ui, openness, resp| {
                    let ico = if openness > 0.5 {
                        IconId::ChevronDown
                    } else {
                        IconId::ChevronRight
                    };
                    paint_icon(ui.painter(), resp.rect.shrink(1.0), ico, adv_col);
                })
                .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (cr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                    paint_icon(ui.painter(), cr, IconId::Clock, pal.ink_muted);
                    ui.label("Start:");
                    ui.radio_value(&mut draft.schedule_kind, ScheduleKind::Immediate, "Now");
                    ui.radio_value(&mut draft.schedule_kind, ScheduleKind::AfterDelay, "In delay");
                    if draft.schedule_kind == ScheduleKind::AfterDelay {
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.after_text)
                                .hint_text("30m, 1h30m")
                                .desired_width(80.0),
                        );
                    }
                    ui.radio_value(&mut draft.schedule_kind, ScheduleKind::AtTime, "At HH:MM");
                    if draft.schedule_kind == ScheduleKind::AtTime {
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.at_text)
                                .hint_text("23:00")
                                .desired_width(64.0),
                        );
                    }
                });
                if draft.mode != AddMode::Bundle {
                    ui.horizontal(|ui| {
                        ui.label("If URL exists:");
                        ui.radio_value(&mut draft.duplicate, DuplicatePolicy::Share, "Reuse existing");
                        ui.radio_value(&mut draft.duplicate, DuplicatePolicy::Copy, "Download new copy");
                        ui.radio_value(&mut draft.duplicate, DuplicatePolicy::Report, "Warn me");
                    });
                }
                ui.label(egui::RichText::new("Custom HTTP headers (Name: value per line)").small().color(pal.ink_muted));
                ui.add(
                    egui::TextEdit::multiline(&mut draft.headers_text)
                        .hint_text("Referer: https://example.com/\nCookie: session=...")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
            });

            if let Some(err) = &draft.error {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(err).color(pal.danger));
            }

            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_clicked = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ok_lbl = if draft.submitting {
                        "Queueing…"
                    } else if draft.mode == AddMode::Bundle {
                        "Add Bundle"
                    } else {
                        "Start Download"
                    };
                    if ui
                        .add_enabled_ui(!draft.submitting, |ui| {
                            vector_button(ui, pal, IconId::Download, ok_lbl, 30.0, true, true)
                        })
                        .inner
                        .clicked()
                    {
                        submit_clicked = true;
                    }
                });
            });
        });

    if !open_flag || close_clicked {
        draft.open = false;
        return;
    }

    if submit_clicked {
        match submit_add(draft, snap) {
            Ok(cmd) => {
                draft.submitting = true;
                draft.error = None;
                worker.send(cmd);
            }
            Err(err) => draft.error = Some(err),
        }
    }
}

fn category_label(val: &str) -> &'static str {
    match val {
        "video" => "Video",
        "music" => "Music",
        "images" => "Images",
        "documents" => "Documents",
        "archives" => "Archives",
        "programs" => "Programs",
        "other" => "Other (root folder)",
        _ => "Auto-detect",
    }
}

fn submit_add(draft: &AddDraft, snap: &Snapshot) -> Result<UiCommand, String> {
    let lines: Vec<&str> = draft
        .url_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err("Enter at least one download address or pick a .torrent file.".into());
    }
    let headers = parse_headers_block(&draft.headers_text)?;
    let start_at = match draft.schedule_kind {
        ScheduleKind::Immediate => None,
        ScheduleKind::AfterDelay => {
            let secs = parse_after_delay(&draft.after_text)?;
            Some(now_unix() + secs as i64)
        }
        ScheduleKind::AtTime => Some(parse_at_hhmm(&draft.at_text)?),
    };
    let dest = resolved_add_dir(&draft.dest_dir, snap.settings.as_ref(), &draft.category, lines[0]);

    if draft.mode == AddMode::Bundle || lines.len() > 1 {
        let parts: Vec<BundlePart> = lines
            .into_iter()
            .take(100)
            .map(|u| BundlePart {
                url: u.to_string(),
                filename: None,
                headers: headers.clone(),
                state: TaskState::Queued,
                downloaded: 0,
                total: None,
                seq: None,
                error: None,
                attempted: false,
            })
            .collect();
        let name = if draft.bundle_name.trim().is_empty() {
            None
        } else {
            Some(draft.bundle_name.trim().to_string())
        };
        return Ok(UiCommand::AddBundle {
            name,
            parts,
            dest_dir: Some(dest),
            start_at,
        });
    }

    let url = lines[0].to_string();
    let kind = match draft.mode {
        AddMode::Video => TaskKind::Video,
        AddMode::Torrent => TaskKind::Torrent,
        AddMode::Bundle => TaskKind::Bundle,
        AddMode::AutoOrHttp => {
            if looks_like_torrent_payload(&url) {
                TaskKind::Torrent
            } else {
                TaskKind::Http
            }
        }
    };
    let only_files = if kind == TaskKind::Torrent {
        draft.torrent_preview.as_ref().and_then(|p| {
            if draft.torrent_selected.len() == p.files.len() {
                None
            } else {
                let mut v: Vec<usize> = draft.torrent_selected.iter().copied().collect();
                v.sort_unstable();
                Some(v)
            }
        })
    } else {
        None
    };

    Ok(UiCommand::Add {
        url,
        dest_dir: Some(dest),
        headers,
        start_at,
        kind,
        sequential: draft.sequential,
        only_files,
        duplicate: draft.duplicate,
    })
}
