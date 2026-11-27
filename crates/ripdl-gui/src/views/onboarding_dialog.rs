use eframe::egui;

use crate::ext_host::{self, BrowserChoice};
use crate::model::{OnboardingDraft, OnboardingStep, Snapshot, SpeedUnit};
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{dialog_header, paint_status_pill, semibold, Palette};
use crate::vectors::{paint_brand_logo, paint_icon, vector_button, IconId};
use crate::views::{modal_frame, section_card};

pub fn show(
    ctx: &egui::Context,
    draft: &mut OnboardingDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    if !draft.open {
        return;
    }

    let mut open_flag = true;
    let mut skip_clicked = false;
    let mut finish_clicked = false;

    egui::Window::new("Welcome to ripdl")
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(560.0, 400.0))
        .show(ctx, |ui| {
            ui.set_min_width(520.0);
            if dialog_header(
                ui,
                pal,
                IconId::Download,
                "Welcome to ripdl",
                Some("Quick setup (3 optional steps) — zero accounts, zero telemetry"),
            ) {
                skip_clicked = true;
            }

            // Step indicator pills.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for (idx, step, title) in [
                    (1, OnboardingStep::Folder, "1. Folder"),
                    (2, OnboardingStep::Browser, "2. Browser"),
                    (3, OnboardingStep::Network, "3. Network"),
                ] {
                    let _ = idx;
                    let active = draft.step == step;
                    let fg = if active { pal.accent_dark } else { pal.ink_muted };
                    let bg = if active { pal.accent_soft } else { pal.surface_alt };
                    egui::Frame::new()
                        .fill(bg)
                        .stroke(egui::Stroke::new(1.0, if active { pal.accent } else { pal.border }))
                        .corner_radius(egui::CornerRadius::same(99))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(title).small().strong().color(fg));
                        });
                }
            });

            ui.add_space(10.0);

            match draft.step {
                OnboardingStep::Folder => render_folder(ui, draft, worker, pal),
                OnboardingStep::Browser => render_browser(ui, draft, snap, worker, pal),
                OnboardingStep::Network => render_network(ui, draft, snap, pal),
            }

            if let Some(err) = &draft.error {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(err).color(pal.danger));
            }

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Skip setup").clicked() {
                    skip_clicked = true;
                }
                if draft.step != OnboardingStep::Folder && ui.button("Back").clicked() {
                    draft.error = None;
                    draft.step = match draft.step {
                        OnboardingStep::Browser => OnboardingStep::Folder,
                        OnboardingStep::Network => OnboardingStep::Browser,
                        OnboardingStep::Folder => OnboardingStep::Folder,
                    };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match draft.step {
                        OnboardingStep::Folder => {
                            if vector_button(ui, pal, IconId::Check, "Continue", 28.0, true, true).clicked() {
                                if draft.download_dir.trim().is_empty() {
                                    draft.error = Some("Choose a download folder or click Skip setup.".into());
                                } else {
                                    draft.error = None;
                                    draft.step = OnboardingStep::Browser;
                                }
                            }
                        }
                        OnboardingStep::Browser => {
                            if vector_button(ui, pal, IconId::Check, "Continue", 28.0, true, true).clicked() {
                                draft.error = None;
                                draft.step = OnboardingStep::Network;
                            }
                            if ui.button("Skip step").clicked() {
                                draft.error = None;
                                draft.step = OnboardingStep::Network;
                            }
                        }
                        OnboardingStep::Network => {
                            if vector_button(ui, pal, IconId::Check, "Finish & Start", 28.0, true, true)
                                .clicked()
                            {
                                finish_clicked = true;
                            }
                        }
                    }
                });
            });
        });

    if !open_flag || skip_clicked {
        let mut s = snap.settings.clone().unwrap_or_default();
        s.onboarding_done = Some(true);
        worker.send(UiCommand::SetConfig { settings: s });
        draft.open = false;
        return;
    }

    if finish_clicked {
        match draft.build_commit_settings(snap.settings.as_ref()) {
            Ok(settings) => {
                draft.error = None;
                draft.open = false;
                worker.send(UiCommand::SetConfig { settings });
            }
            Err(err) => draft.error = Some(err),
        }
    }
}

fn render_folder(
    ui: &mut egui::Ui,
    draft: &mut OnboardingDraft,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    ui.horizontal(|ui| {
        let (logo_rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
        paint_brand_logo(ui.painter(), logo_rect);
        ui.vertical(|ui| {
            ui.label(semibold("Where should downloads go?", 15.0, pal.ink));
            ui.label(
                egui::RichText::new("Files sort automatically by type (Archives, Video, Documents) unless overridden per download.")
                    .small()
                    .color(pal.ink_muted),
            );
        });
    });
    ui.add_space(10.0);

    section_card(ui, pal, |ui| {
        ui.label(egui::RichText::new("DEFAULT DOWNLOAD FOLDER").small().strong().color(pal.ink_muted));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.download_dir)
                    .desired_width(370.0),
            );
            if vector_button(ui, pal, IconId::Folder, "Browse…", 28.0, false, true).clicked() {
                worker.pick_path(
                    crate::model::FilePickTarget::OnboardingDownloadDir,
                    true,
                    None,
                    None,
                    ui.ctx().clone(),
                );
            }
        });
    });
}

fn render_browser(
    ui: &mut egui::Ui,
    draft: &mut OnboardingDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    ui.label(semibold("Connect your browser (optional)", 15.0, pal.ink));
    ui.label(
        egui::RichText::new("Catch downloads automatically and turn copied multi-part links into single folder bundles.")
            .small()
            .color(pal.ink_muted),
    );
    ui.add_space(8.0);

    section_card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            if let Some(ext) = &snap.extension {
                if ext.last_seen > 0 {
                    paint_status_pill(
                        ui,
                        &format!("Extension v{} connected", ext.version),
                        pal.success,
                        pal.success_soft,
                        pal.success,
                    );
                } else {
                    paint_status_pill(
                        ui,
                        "Extension not paired yet",
                        pal.warn,
                        pal.warn_soft,
                        pal.warn,
                    );
                }
            } else {
                paint_status_pill(ui, "Extension bundled & ready", pal.info, pal.info_soft, pal.info);
            }
            if let Some((ok, msg)) = &draft.host_status {
                ui.label(
                    egui::RichText::new(msg)
                        .small()
                        .color(if *ok { pal.success } else { pal.danger }),
                );
            }
        });
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if vector_button(ui, pal, IconId::Shield, "Register browser host", 28.0, true, true).clicked() {
                worker.send(UiCommand::RegisterBrowserHost);
            }
            egui::ComboBox::from_id_salt("ob_browser_pick")
                .selected_text(draft.browser_choice.display_label())
                .show_ui(ui, |ui| {
                    for &choice in BrowserChoice::all() {
                        ui.selectable_value(&mut draft.browser_choice, choice, choice.display_label());
                    }
                });
            if vector_button(ui, pal, IconId::External, "Open extensions", 28.0, false, true).clicked() {
                if let Ok(managed) = ext_host::managed_extension_dir() {
                    ui.ctx().copy_text(managed.display().to_string());
                }
                worker.send(UiCommand::OpenExtensionsPage {
                    override_dir: String::new(),
                    browser: draft.browser_choice,
                });
            }
        });

        ui.add_space(6.0);
        if let Ok(managed) = ext_host::managed_extension_dir() {
            ui.label(
                egui::RichText::new(format!(
                    "Enable Developer mode -> Load unpacked -> select: {}",
                    managed.display()
                ))
                .small()
                .color(pal.ink_secondary),
            );
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("CLI alternative:")
                    .small()
                    .color(pal.ink_muted),
            );
            ui.label(
                egui::RichText::new("ripdl register-host")
                    .small()
                    .monospace()
                    .color(pal.ink),
            );
            if vector_button(ui, pal, IconId::Copy, "Copy", 22.0, false, false).clicked() {
                ui.ctx().copy_text("ripdl register-host".to_string());
            }
            if vector_button(ui, pal, IconId::External, "Welcome tab", 22.0, false, false).clicked() {
                worker.send(UiCommand::OpenExtensionWelcome {
                    browser: draft.browser_choice,
                });
            }
        });
    });
}

fn render_network(
    ui: &mut egui::Ui,
    draft: &mut OnboardingDraft,
    snap: &Snapshot,
    pal: &Palette,
) {
    ui.label(semibold("Speed cap & multi-NIC bonding (optional)", 15.0, pal.ink));
    ui.label(
        egui::RichText::new("Set a shared bandwidth ceiling or combine Ethernet, Wi-Fi, and USB tethers automatically.")
            .small()
            .color(pal.ink_muted),
    );
    ui.add_space(8.0);

    section_card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            let (ir, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            paint_icon(ui.painter(), ir, IconId::Sliders, pal.accent);
            ui.label(semibold("Global speed limit", 13.0, pal.ink));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.selectable_label(draft.speed_unit == SpeedUnit::Unlimited, "Unlimited").clicked() {
                draft.speed_unit = SpeedUnit::Unlimited;
                draft.speed_value.clear();
            }
            if ui.selectable_label(draft.speed_unit == SpeedUnit::MiB, "MiB/s").clicked() {
                draft.speed_unit = SpeedUnit::MiB;
                if draft.speed_value.is_empty() {
                    draft.speed_value = "10".into();
                }
            }
            if ui.selectable_label(draft.speed_unit == SpeedUnit::KiB, "KiB/s").clicked() {
                draft.speed_unit = SpeedUnit::KiB;
                if draft.speed_value.is_empty() {
                    draft.speed_value = "500".into();
                }
            }
            if draft.speed_unit != SpeedUnit::Unlimited {
                ui.add(egui::TextEdit::singleline(&mut draft.speed_value).desired_width(80.0));
            }
        });
    });

    ui.add_space(8.0);

    section_card(ui, pal, |ui| {
        let nic_count = snap.detected_nics.len();
        ui.checkbox(
            &mut draft.bond_interfaces,
            "Combine all active network interfaces (Wi-Fi + Ethernet + USB tether)",
        );
        if nic_count >= 2 {
            ui.label(
                egui::RichText::new(format!("{nic_count} active uplinks detected — bonding can distribute segments across them."))
                    .small()
                    .color(pal.success),
            );
        } else {
            ui.label(
                egui::RichText::new("Only 1 active uplink detected right now — bonding engages automatically whenever another connection joins.")
                    .small()
                    .color(pal.ink_muted),
            );
        }
    });
}
