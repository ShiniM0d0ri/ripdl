use eframe::egui::{self, CornerRadius, Margin, Stroke};

use crate::model::RefreshDraft;
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{add_primary_button, dialog_header, semibold, Palette};
use crate::vectors::{paint_icon, vector_button, IconId};
use crate::views::{modal_frame, section_card};

pub fn show(
    ctx: &egui::Context,
    draft: &mut RefreshDraft,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    if !draft.open {
        return;
    }

    let mut open_flag = true;
    let mut close_clicked = false;
    let mut commit_clicked = false;

    let title = match draft.part_seq {
        Some(seq) => format!("Refresh Address (Part {seq})"),
        None => "Refresh Download Address".to_string(),
    };

    egui::Window::new(&title)
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(600.0, 290.0))
        .min_width(500.0)
        .show(ctx, |ui| {
            if dialog_header(
                ui,
                pal,
                IconId::Refresh,
                &title,
                Some("Swap a fresh signed URL and keep all verified bytes on disk"),
            ) {
                close_clicked = true;
            }

            section_card(ui, pal, |ui| {
                ui.label(semibold(&draft.task_name, 13.0, pal.ink));
                ui.label(
                    egui::RichText::new(
                        "Paste a link for the exact same file (filename, byte size, and server ETag must match).",
                    )
                    .small()
                    .color(pal.ink_muted),
                );
            });
            ui.add_space(6.0);

            ui.add(
                egui::TextEdit::multiline(&mut draft.url)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("https://..."),
            );
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.resume_after, "Resume download immediately after swap");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let clean_url = draft.url.trim().to_string();
                    if ui
                        .add_enabled_ui(!clean_url.is_empty() && !draft.probing, |ui| {
                            vector_button(ui, pal, IconId::Refresh, "Verify link", 28.0, false, true)
                        })
                        .inner
                        .clicked()
                    {
                        draft.probing = true;
                        draft.verdict_ok = false;
                        draft.last_probed_url = clean_url.clone();
                        draft.error = None;
                        worker.send(UiCommand::RefreshUrl {
                            id: draft.task_id.clone(),
                            url: clean_url,
                            part: draft.part_seq,
                            probe_only: true,
                            resume: false,
                        });
                    }
                });
            });

            if draft.probing {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Checking fresh URL against partial file…").color(pal.ink_secondary));
                });
            } else if !draft.verdict_text.is_empty() {
                ui.add_space(4.0);
                let (fg, bg, ico) = if draft.verdict_ok {
                    (pal.success, pal.success_soft, IconId::CheckCircle)
                } else {
                    (pal.danger, pal.danger_soft, IconId::Alert)
                };
                egui::Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.0, fg))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            paint_icon(ui.painter(), ir, ico, fg);
                            ui.label(egui::RichText::new(&draft.verdict_text).color(fg));
                        });
                    });
            }

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
                    let valid = !draft.url.trim().is_empty()
                        && !draft.submitting
                        && draft.last_probed_url == draft.url.trim()
                        && draft.verdict_ok;
                    if add_primary_button(ui, valid, "Swap Address", pal).clicked() {
                        commit_clicked = true;
                    }
                });
            });
        });

    if !open_flag || close_clicked {
        draft.open = false;
        return;
    }

    if commit_clicked {
        draft.submitting = true;
        draft.error = None;
        worker.send(UiCommand::RefreshUrl {
            id: draft.task_id.clone(),
            url: draft.url.trim().to_string(),
            part: draft.part_seq,
            probe_only: false,
            resume: draft.resume_after,
        });
    }
}
