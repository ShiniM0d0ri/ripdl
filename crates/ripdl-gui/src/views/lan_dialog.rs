use eframe::egui;

use crate::model::{humans_bin, FilePickTarget, LanDraft, Snapshot};
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{
    dialog_header, paint_progress_bar, paint_status_pill, semibold, Palette,
};
use crate::vectors::{paint_icon, vector_button, vector_icon_button, IconId};
use crate::views::{modal_frame, open_folder_native, section_card};

pub fn show(
    ctx: &egui::Context,
    draft: &mut LanDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
    on_notice: &mut dyn FnMut(String, bool),
) {
    if !draft.open {
        return;
    }

    let mut open_flag = true;
    let mut close_clicked = false;

    egui::Window::new("Send & Receive on Local Network")
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(700.0, 460.0))
        .min_width(600.0)
        .show(ctx, |ui| {
            if dialog_header(
                ui,
                pal,
                IconId::Lan,
                "Send & Receive on Local Network",
                Some("Direct encrypted-code device drops over your Wi-Fi or Ethernet"),
            ) {
                close_clicked = true;
            }

            // Receive card.
            section_card(ui, pal, |ui| {
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    paint_icon(ui.painter(), ir, IconId::Download, pal.accent_dark);
                    ui.label(semibold("Receive files", 13.5, pal.ink));
                    if let Some(lan) = &snap.lan {
                        if lan.running {
                            paint_status_pill(ui, "Listening", pal.success, pal.success_soft, pal.success);
                        } else if lan.enabled {
                            paint_status_pill(ui, "Port blocked", pal.warn, pal.warn_soft, pal.warn);
                        } else {
                            paint_status_pill(ui, "Off", pal.ink_secondary, pal.track, pal.ink_muted);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut enabled = snap
                            .lan
                            .as_ref()
                            .map(|l| l.enabled)
                            .or_else(|| snap.settings.as_ref().map(|s| s.lan_enabled()))
                            .unwrap_or(false);
                        if ui.checkbox(&mut enabled, "Enable LAN receiver").changed() {
                            if let Some(mut s) = snap.settings.clone() {
                                s.lan_enabled = Some(enabled);
                                worker.send(UiCommand::SetConfig { settings: s });
                                worker.send(UiCommand::FetchLan);
                            }
                        }
                    });
                });

                if let Some(lan) = &snap.lan {
                    ui.add_space(6.0);
                    egui::Grid::new("lan_recv_kv")
                        .num_columns(2)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Drop-box URL").small().color(pal.ink_muted));
                            ui.horizontal(|ui| {
                                let url_txt = if lan.running { lan.url.as_str() } else { "(not listening)" };
                                ui.label(egui::RichText::new(url_txt).monospace().color(pal.ink));
                                if lan.running
                                    && vector_button(ui, pal, IconId::Copy, "Copy URL", 24.0, false, true).clicked()
                                {
                                    ctx.copy_text(lan.url.clone());
                                    on_notice("Drop-box URL copied.".into(), false);
                                }
                            });
                            ui.end_row();

                            ui.label(egui::RichText::new("Pairing code").small().color(pal.ink_muted));
                            ui.horizontal(|ui| {
                                let (sr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                                paint_icon(ui.painter(), sr, IconId::Shield, pal.accent_dark);
                                egui::Frame::new()
                                    .fill(pal.surface)
                                    .stroke(egui::Stroke::new(1.0, pal.border_strong))
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .inner_margin(egui::Margin::symmetric(10, 3))
                                    .show(ui, |ui| {
                                        let code_lbl = if lan.open {
                                            "(trusted open network)"
                                        } else if lan.code.is_empty() {
                                            "------"
                                        } else {
                                            &lan.code
                                        };
                                        ui.label(
                                            egui::RichText::new(code_lbl)
                                                .monospace()
                                                .strong()
                                                .size(15.0)
                                                .color(pal.accent_dark),
                                        );
                                    });
                                if !lan.open
                                    && !lan.code.is_empty()
                                    && vector_button(ui, pal, IconId::Copy, "Copy", 24.0, false, true).clicked()
                                {
                                    ctx.copy_text(lan.code.clone());
                                    on_notice("Pairing code copied.".into(), false);
                                }
                                if vector_button(ui, pal, IconId::Refresh, "New code", 24.0, false, true).clicked() {
                                    worker.send(UiCommand::LanNewCode);
                                }
                                let mut open_net = lan.open;
                                if ui.checkbox(&mut open_net, "Trusted home network").changed() {
                                    if let Some(mut s) = snap.settings.clone() {
                                        s.lan_open = Some(open_net);
                                        worker.send(UiCommand::SetConfig { settings: s });
                                        worker.send(UiCommand::FetchLan);
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(egui::RichText::new("Inbox folder").small().color(pal.ink_muted));
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(lan.dir.display().to_string())
                                        .small()
                                        .color(pal.ink_secondary),
                                );
                                if vector_button(ui, pal, IconId::FolderOpen, "Open inbox", 24.0, false, true).clicked() {
                                    open_folder_native(&lan.dir);
                                }
                                if vector_icon_button(ui, pal, IconId::Refresh, "Refresh inbox", 24.0, false, true)
                                    .on_hover_text("Refresh inbox contents")
                                    .clicked()
                                {
                                    worker.send(UiCommand::FetchLan);
                                }
                            });
                            ui.end_row();
                        });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("RECEIVED FILES").small().strong().color(pal.ink_muted));
                    if lan.files.is_empty() {
                        ui.label(egui::RichText::new("Inbox is empty.").small().color(pal.ink_muted));
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("lan_inbox_files")
                            .max_height(100.0)
                            .show(ui, |ui| {
                                for f in &lan.files {
                                    ui.horizontal(|ui| {
                                        let (fr, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                                        paint_icon(ui.painter(), fr, IconId::Document, pal.ink_secondary);
                                        ui.label(egui::RichText::new(&f.name).color(pal.ink));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if f.partial {
                                                paint_status_pill(ui, "Arriving", pal.info, pal.info_soft, pal.info);
                                            }
                                            ui.label(
                                                egui::RichText::new(humans_bin(f.size))
                                                    .small()
                                                    .monospace()
                                                    .color(pal.ink_muted),
                                            );
                                        });
                                    });
                                }
                            });
                    }
                }
            });

            ui.add_space(8.0);

            // Send card.
            section_card(ui, pal, |ui| {
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    paint_icon(ui.painter(), ir, IconId::Upload, pal.accent_dark);
                    ui.label(semibold("Send a file", 13.5, pal.ink));
                    if ui
                        .add_enabled_ui(!snap.lan_scanning, |ui| {
                            vector_button(ui, pal, IconId::Lan, "Find devices", 26.0, false, true)
                        })
                        .inner
                        .clicked()
                    {
                        worker.send(UiCommand::ScanLanPeers);
                    }
                    if snap.lan_scanning {
                        ui.spinner();
                    }
                });

                if !snap.lan_peers.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("Discovered:").small().color(pal.ink_muted));
                        for p in &snap.lan_peers {
                            let lbl = if p.open {
                                format!("{} (open)", p.name)
                            } else {
                                p.name.clone()
                            };
                            if ui.selectable_label(draft.send_to == p.url, lbl).clicked() {
                                draft.send_to = p.url.clone();
                                if p.open {
                                    draft.send_code.clear();
                                }
                            }
                        }
                    });
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Target URL:").small().color(pal.ink_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.send_to)
                            .hint_text("http://192.168.1.50:8463")
                            .desired_width(240.0),
                    );
                    ui.label(egui::RichText::new("Code:").small().color(pal.ink_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.send_code)
                            .hint_text("RPDL42")
                            .desired_width(90.0),
                    );
                    if ui
                        .add_enabled_ui(!draft.send_to.trim().is_empty(), |ui| {
                            vector_button(ui, pal, IconId::Upload, "Pick & Send…", 28.0, true, true)
                        })
                        .inner
                        .clicked()
                    {
                        worker.pick_path(
                            FilePickTarget::LanSendFile,
                            false,
                            None,
                            None,
                            ctx.clone(),
                        );
                    }
                });

                if !snap.lan_sends.is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("lan_sends")
                        .max_height(86.0)
                        .show(ui, |ui| {
                            for s in &snap.lan_sends {
                                ui.horizontal(|ui| {
                                    ui.label(&s.name);
                                    let ratio = if s.total > 0 {
                                        (s.sent as f64 / s.total as f64) as f32
                                    } else if s.done {
                                        1.0
                                    } else {
                                        0.0
                                    };
                                    paint_progress_bar(
                                        ui,
                                        ratio,
                                        if s.error.is_some() {
                                            "failed"
                                        } else if s.done {
                                            "completed"
                                        } else {
                                            "running"
                                        },
                                        160.0,
                                        pal,
                                    );
                                    if let Some(err) = &s.error {
                                        ui.label(egui::RichText::new(err).small().color(pal.danger));
                                    }
                                });
                            }
                        });
                }
            });

            if let Some(err) = &draft.error {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(err).color(pal.danger));
            }

            ui.add_space(6.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close_clicked = true;
                }
            });
        });

    if !open_flag || close_clicked {
        draft.open = false;
    }
}
