use eframe::egui;

use ripdl_core::engine::describe_torrent_bond;

use crate::ext_host::{self, BrowserChoice};
use crate::model::{
    default_downloads_fallback, FilePickTarget, SettingsDraft, SettingsTab, Snapshot, SpeedUnit,
};
use crate::rpc_worker::{UiCommand, WorkerHandle};
use crate::theme::{dialog_header, paint_status_pill, semibold, Palette};
use crate::vectors::{paint_focus_ring, paint_icon, vector_button, IconId};
use crate::views::{build_redacted_diagnostics, modal_frame, open_external_url, section_card};

fn tab_icon(tab: SettingsTab) -> IconId {
    match tab {
        SettingsTab::General => IconId::Folder,
        SettingsTab::Connection => IconId::Sliders,
        SettingsTab::Bonding => IconId::Bond,
        SettingsTab::Torrents => IconId::Torrent,
        SettingsTab::Browser => IconId::External,
        SettingsTab::Lan => IconId::Upload,
        SettingsTab::Updates => IconId::Refresh,
    }
}

fn tab_subtitle(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::General => "Storage folders and helper binaries",
        SettingsTab::Connection => "Concurrency, segment sizing, and global speed cap",
        SettingsTab::Bonding => "Combine Wi-Fi, Ethernet, and tethers across uplinks",
        SettingsTab::Torrents => "Seeding duration and share ratio policies",
        SettingsTab::Browser => "Chrome/Edge extension bridge and custom location",
        SettingsTab::Lan => "Local drop box receiver, port, and pairing security",
        SettingsTab::Updates => "Release channel checks, about info, and diagnostics",
    }
}

fn section_heading(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text).small().strong().color(pal.ink_muted));
    ui.add_space(2.0);
}

fn property_row(ui: &mut egui::Ui, pal: &Palette, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::vec2(206.0, 26.0),
            egui::Label::new(egui::RichText::new(label).color(pal.ink_secondary)),
        );
        add(ui);
    });
    ui.add_space(2.0);
}

pub fn show(
    ctx: &egui::Context,
    draft: &mut SettingsDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
    on_notice: &mut dyn FnMut(String, bool),
) {
    if !draft.open {
        return;
    }

    if !draft.loaded {
        if let Some(s) = &snap.settings {
            draft.load_from(s);
        }
    }

    let mut open_flag = true;
    let mut close_clicked = false;
    let mut save_clicked = false;

    egui::Window::new("Options")
        .open(&mut open_flag)
        .title_bar(false)
        .frame(modal_frame(ctx, pal))
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_size(egui::vec2(760.0, 480.0))
        .min_width(660.0)
        .min_height(460.0)
        .show(ctx, |ui| {
            if dialog_header(
                ui,
                pal,
                IconId::Settings,
                "Options",
                Some("Configure daemon engine, bandwidth caps, bonding, and integrations"),
            ) {
                close_clicked = true;
            }

            ui.horizontal(|ui| {
                // Left navigation rail with crisp vector icons.
                ui.vertical(|ui| {
                    ui.set_min_width(166.0);
                    ui.set_max_width(166.0);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for &tab in SettingsTab::all() {
                        let active = draft.tab == tab;
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(162.0, 30.0), egui::Sense::click());
                        resp.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::Role::Tab,
                                true,
                                active,
                                tab.label(),
                            )
                        });
                        if ui.is_rect_visible(rect) {
                            let bg = if active {
                                pal.accent_soft
                            } else if resp.hovered() {
                                pal.surface_hover
                            } else {
                                egui::Color32::TRANSPARENT
                            };
                            let fg = if active { pal.accent_dark } else { pal.ink_secondary };
                            ui.painter().rect_filled(rect, egui::CornerRadius::same(6), bg);
                            paint_focus_ring(ui.painter(), rect, 6, pal, resp.has_focus());
                            if active {
                                let bar = egui::Rect::from_min_max(
                                    rect.min + egui::vec2(0.0, 4.0),
                                    egui::pos2(rect.min.x + 3.0, rect.max.y - 4.0),
                                );
                                ui.painter().rect_filled(bar, egui::CornerRadius::same(2), pal.accent);
                            }
                            let ir = egui::Rect::from_center_size(
                                egui::pos2(rect.min.x + 17.0, rect.center().y),
                                egui::vec2(14.0, 14.0),
                            );
                            paint_icon(ui.painter(), ir, tab_icon(tab), if active { pal.accent } else { pal.ink_muted });
                            ui.painter().text(
                                egui::pos2(rect.min.x + 34.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                tab.label(),
                                egui::FontId::new(
                                    12.5,
                                    if active {
                                        egui::FontFamily::Name("semibold".into())
                                    } else {
                                        egui::FontFamily::Proportional
                                    },
                                ),
                                fg,
                            );
                        }
                        if resp.clicked() {
                            draft.tab = tab;
                        }
                    }
                });

                ui.separator();

                ui.vertical(|ui| {
                    ui.set_min_width(530.0);
                    ui.label(semibold(draft.tab.label(), 15.0, pal.ink));
                    ui.label(egui::RichText::new(tab_subtitle(draft.tab)).small().color(pal.ink_muted));
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .min_scrolled_height(280.0)
                        .max_height(320.0)
                        .show(ui, |ui| match draft.tab {
                            SettingsTab::General => render_general(ui, draft, worker, pal),
                            SettingsTab::Connection => render_connection(ui, draft, pal),
                            SettingsTab::Bonding => render_bonding(ui, draft, snap, worker, pal),
                            SettingsTab::Torrents => render_torrents(ui, draft, pal),
                            SettingsTab::Browser => render_browser(ui, draft, snap, worker, pal),
                            SettingsTab::Lan => render_lan(ui, draft, worker, pal),
                            SettingsTab::Updates => {
                                render_updates(ui, ctx, draft, snap, worker, pal, on_notice)
                            }
                        });
                });
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
                if vector_button(ui, pal, IconId::Refresh, "Reload live", 26.0, false, true).clicked() {
                    if let Some(s) = &snap.settings {
                        draft.load_from(s);
                    }
                }
                ui.add_space(6.0);
                if draft.dirty {
                    paint_status_pill(ui, "Unsaved changes", pal.warn, pal.warn_soft, pal.warn);
                } else {
                    paint_status_pill(ui, "All changes saved", pal.success, pal.success_soft, pal.success);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save_lbl = if draft.dirty { "Save changes" } else { "Save" };
                    if vector_button(ui, pal, IconId::Check, save_lbl, 28.0, true, true).clicked() {
                        save_clicked = true;
                    }
                });
            });
        });

    if !open_flag || close_clicked {
        draft.open = false;
        return;
    }

    if save_clicked {
        match draft.build_payload() {
            Ok(settings) => {
                draft.error = None;
                draft.dirty = false;
                worker.send(UiCommand::SetConfig { settings });
            }
            Err(err) => draft.error = Some(err),
        }
    }
}

fn render_general(ui: &mut egui::Ui, draft: &mut SettingsDraft, worker: &WorkerHandle, pal: &Palette) {
    section_heading(ui, pal, "STORAGE & TOOLS");
    property_row(ui, pal, "Default download folder", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.default_download_dir)
                    .hint_text(default_downloads_fallback().display().to_string())
                    .desired_width(235.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
        if vector_button(ui, pal, IconId::Folder, "Browse…", 26.0, false, true).clicked() {
            worker.pick_path(
                FilePickTarget::SettingsDownloadDir,
                true,
                None,
                None,
                ui.ctx().clone(),
            );
        }
    });

    property_row(ui, pal, "yt-dlp sidecar path", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.ytdlp_path)
                    .hint_text("yt-dlp (resolved from PATH)")
                    .desired_width(235.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
        if vector_button(ui, pal, IconId::Folder, "Browse…", 26.0, false, true).clicked() {
            worker.pick_path(
                FilePickTarget::SettingsYtDlp,
                false,
                None,
                None,
                ui.ctx().clone(),
            );
        }
    });

    section_heading(ui, pal, "WINDOW & TRAY");
    if ui
        .checkbox(
            &mut draft.hide_to_tray,
            "Hide to system tray when closing the window",
        )
        .changed()
    {
        draft.dirty = true;
    }
    ui.label(
        egui::RichText::new(
            "Use Tray -> Quit or File -> Quit UI to exit completely while downloads stay alive in the background daemon.",
        )
        .small()
        .color(pal.ink_muted),
    );
}

fn render_connection(ui: &mut egui::Ui, draft: &mut SettingsDraft, pal: &Palette) {
    section_heading(ui, pal, "ENGINE");
    property_row(ui, pal, "Downloads at once (1–64)", |ui| {
        if ui
            .add(egui::DragValue::new(&mut draft.max_concurrent).range(1..=64))
            .changed()
        {
            draft.dirty = true;
        }
    });
    property_row(ui, pal, "Connections per download (1–64)", |ui| {
        if ui
            .add(egui::DragValue::new(&mut draft.max_segments).range(1..=64))
            .changed()
        {
            draft.dirty = true;
        }
    });
    property_row(ui, pal, "Minimum segment size", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.min_segment_text)
                    .hint_text("1MiB")
                    .desired_width(110.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
        ui.label(egui::RichText::new("e.g. 512KiB, 1MiB, 4MiB").small().color(pal.ink_muted));
    });

    section_heading(ui, pal, "BANDWIDTH");
    property_row(ui, pal, "Global speed cap", |ui| {
        if ui.selectable_label(draft.speed_unit == SpeedUnit::Unlimited, "Unlimited").clicked() {
            draft.speed_unit = SpeedUnit::Unlimited;
            draft.speed_value.clear();
            draft.dirty = true;
        }
        if ui.selectable_label(draft.speed_unit == SpeedUnit::KiB, "KiB/s").clicked() {
            draft.speed_unit = SpeedUnit::KiB;
            if draft.speed_value.is_empty() {
                draft.speed_value = "500".into();
            }
            draft.dirty = true;
        }
        if ui.selectable_label(draft.speed_unit == SpeedUnit::MiB, "MiB/s").clicked() {
            draft.speed_unit = SpeedUnit::MiB;
            if draft.speed_value.is_empty() {
                draft.speed_value = "5".into();
            }
            draft.dirty = true;
        }
        if draft.speed_unit != SpeedUnit::Unlimited
            && ui
                .add(egui::TextEdit::singleline(&mut draft.speed_value).desired_width(78.0))
                .changed()
        {
            draft.dirty = true;
        }
    });
}

fn render_bonding(
    ui: &mut egui::Ui,
    draft: &mut SettingsDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    section_heading(ui, pal, "MULTI-NIC AGGREGATION");
    if ui
        .checkbox(
            &mut draft.bond_interfaces,
            "Combine all active network interfaces (Wi-Fi + Ethernet + USB tether)",
        )
        .changed()
    {
        draft.dirty = true;
    }

    let bind: Vec<std::net::IpAddr> = snap
        .torrent_bind
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let bond_desc = describe_torrent_bond(draft.bond_interfaces, snap.torrent_session, &bind);
    ui.label(egui::RichText::new(format!("Torrent session: {bond_desc}")).small().color(pal.ink_secondary));

    if !snap.torrent_unreachable.is_empty() {
        ui.label(
            egui::RichText::new(format!("Unreachable uplinks: {}", snap.torrent_unreachable.join(", ")))
                .small()
                .color(pal.warn),
        );
    }

    ui.add_space(6.0);
    section_card(ui, pal, |ui| {
        ui.label(egui::RichText::new("DETECTED UPLINKS (UNCHECK TO EXCLUDE)").small().strong().color(pal.ink_muted));
        if snap.detected_nics.is_empty() {
            ui.label(egui::RichText::new("No active non-loopback interfaces detected.").small().color(pal.ink_muted));
        } else {
            for nic in &snap.detected_nics {
                let key = nic.name.trim().to_lowercase();
                let mut included = !draft.bond_excluded.contains(&key);
                let label = format!(
                    "{} ({}{})",
                    nic.name,
                    nic.addr,
                    if nic.is_default_route { ", default route" } else { "" }
                );
                if ui.checkbox(&mut included, label).changed() {
                    if included {
                        draft.bond_excluded.remove(&key);
                    } else {
                        draft.bond_excluded.insert(key);
                    }
                    draft.dirty = true;
                }
            }
        }
    });

    ui.add_space(8.0);
    if vector_button(ui, pal, IconId::Refresh, "Restart daemon cleanly", 28.0, false, true).clicked() {
        worker.send(UiCommand::RestartDaemon);
    }
}

fn render_torrents(ui: &mut egui::Ui, draft: &mut SettingsDraft, pal: &Palette) {
    section_heading(ui, pal, "BITTORRENT SEEDING RULES");
    if ui
        .checkbox(&mut draft.seed_enabled, "Seed completed torrents back to the swarm")
        .changed()
    {
        draft.dirty = true;
    }
    ui.add_space(4.0);
    property_row(ui, pal, "Stop at share ratio (0 = unl.)", |ui| {
        if ui
            .add(egui::TextEdit::singleline(&mut draft.seed_ratio_text).desired_width(80.0))
            .changed()
        {
            draft.dirty = true;
        }
    });
    property_row(ui, pal, "Stop after minutes (0 = unl.)", |ui| {
        if ui
            .add(egui::DragValue::new(&mut draft.seed_time_min).range(0..=525_600))
            .changed()
        {
            draft.dirty = true;
        }
    });
}

fn render_browser(
    ui: &mut egui::Ui,
    draft: &mut SettingsDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
) {
    section_heading(ui, pal, "BROWSER EXTENSION BRIDGE");
    ui.horizontal(|ui| {
        if let Some(ext) = &snap.extension {
            if ext.last_seen > 0 {
                paint_status_pill(
                    ui,
                    &format!("Extension v{} active", ext.version),
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
        if let Some(v) = ext_host::folder_version(&draft.extension_dir) {
            ui.label(
                egui::RichText::new(format!("Folder manifest v{v}"))
                    .small()
                    .color(pal.ink_muted),
            );
        }
    });

    ui.add_space(6.0);
    if let Ok(managed) = ext_host::managed_extension_dir() {
        property_row(ui, pal, "Managed extension folder", |ui| {
            let mut ro = managed.display().to_string();
            ui.add(
                egui::TextEdit::singleline(&mut ro)
                    .interactive(false)
                    .desired_width(240.0),
            );
            if vector_button(ui, pal, IconId::Copy, "Copy path", 24.0, false, true).clicked() {
                ui.ctx().copy_text(managed.display().to_string());
            }
        });
    }

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if vector_button(ui, pal, IconId::Shield, "Register browser host", 26.0, true, true)
            .clicked()
        {
            worker.send(UiCommand::RegisterBrowserHost);
        }
        egui::ComboBox::from_id_salt("settings_browser_pick")
            .selected_text(draft.browser_choice.display_label())
            .show_ui(ui, |ui| {
                for &choice in BrowserChoice::all() {
                    ui.selectable_value(&mut draft.browser_choice, choice, choice.display_label());
                }
            });
        if vector_button(ui, pal, IconId::External, "Open extensions page", 26.0, false, true)
            .clicked()
        {
            worker.send(UiCommand::OpenExtensionsPage {
                override_dir: draft.extension_dir.clone(),
                browser: draft.browser_choice,
            });
        }
        if vector_button(ui, pal, IconId::External, "Welcome tab", 26.0, false, true).clicked() {
            worker.send(UiCommand::OpenExtensionWelcome {
                browser: draft.browser_choice,
            });
        }
        if vector_button(ui, pal, IconId::Add, "Install plugin (.js)…", 26.0, false, true)
            .clicked()
        {
            worker.pick_path(
                FilePickTarget::SettingsPluginScript,
                false,
                None,
                Some(("JavaScript plugin", &["js"])),
                ui.ctx().clone(),
            );
        }
    });

    ui.add_space(6.0);
    property_row(ui, pal, "Custom extension folder", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.extension_dir)
                    .hint_text("Uses app-managed directory")
                    .desired_width(220.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
        if vector_button(ui, pal, IconId::Folder, "Browse…", 26.0, false, true).clicked() {
            worker.pick_path(
                FilePickTarget::SettingsExtensionDir,
                true,
                None,
                None,
                ui.ctx().clone(),
            );
        }
    });
}

fn render_lan(ui: &mut egui::Ui, draft: &mut SettingsDraft, worker: &WorkerHandle, pal: &Palette) {
    section_heading(ui, pal, "DROP BOX");
    if ui
        .checkbox(&mut draft.lan_enabled, "Accept files from devices on this network")
        .changed()
    {
        draft.dirty = true;
    }
    property_row(ui, pal, "TCP port", |ui| {
        if ui
            .add(egui::TextEdit::singleline(&mut draft.lan_port_text).desired_width(80.0))
            .changed()
        {
            draft.dirty = true;
        }
    });
    property_row(ui, pal, "Inbox folder", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.lan_dir)
                    .hint_text("<Downloads>/ripdl-inbox")
                    .desired_width(220.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
        if vector_button(ui, pal, IconId::Folder, "Browse…", 26.0, false, true).clicked() {
            worker.pick_path(
                FilePickTarget::SettingsLanDir,
                true,
                None,
                None,
                ui.ctx().clone(),
            );
        }
    });

    section_heading(ui, pal, "SECURITY");
    if ui
        .checkbox(&mut draft.lan_open, "Trusted home network (no pairing code required)")
        .changed()
    {
        draft.dirty = true;
    }
    property_row(ui, pal, "Pairing code (6+ chars)", |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.lan_code)
                    .hint_text("Auto-generated if blank")
                    .desired_width(130.0),
            )
            .changed()
        {
            draft.dirty = true;
        }
    });
}

fn render_updates(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    draft: &mut SettingsDraft,
    snap: &Snapshot,
    worker: &WorkerHandle,
    pal: &Palette,
    on_notice: &mut dyn FnMut(String, bool),
) {
    section_heading(ui, pal, "UPDATES");
    if ui
        .checkbox(&mut draft.update_check, "Check GitHub Releases at startup")
        .changed()
    {
        draft.dirty = true;
    }
    if ui
        .checkbox(
            &mut draft.update_auto_install,
            "Auto-install verified updates cleanly when available",
        )
        .changed()
    {
        draft.dirty = true;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(!snap.update_checking, |ui| {
                vector_button(ui, pal, IconId::Refresh, "Check for updates now", 26.0, false, true)
            })
            .inner
            .clicked()
        {
            worker.send(UiCommand::CheckUpdate);
        }
        if let Some(upd) = &snap.update {
            if upd.update_available
                && vector_button(
                    ui,
                    pal,
                    IconId::Download,
                    &format!("Download & Install v{}", upd.latest),
                    26.0,
                    true,
                    true,
                )
                .clicked()
            {
                worker.send(UiCommand::UpdateInstall);
            }
        }
    });
    if let Some(status) = &snap.update_install_status {
        ui.label(egui::RichText::new(status).small().color(pal.info));
    }

    section_heading(ui, pal, "SUPPORT & DIAGNOSTICS");
    section_card(ui, pal, |ui| {
        ui.label(semibold(&format!("ripdl v{}", env!("CARGO_PKG_VERSION")), 13.0, pal.ink));
        ui.label(
            egui::RichText::new("Free, open-source (MIT / Apache-2.0), zero telemetry, zero ads.")
                .small()
                .color(pal.ink_secondary),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if vector_button(ui, pal, IconId::External, "Release notes", 26.0, false, true).clicked() {
                open_external_url("https://github.com/ShiniM0d0ri/ripdl/releases");
            }
            if vector_button(ui, pal, IconId::Copy, "Copy redacted diagnostics", 26.0, false, true).clicked() {
                ctx.copy_text(build_redacted_diagnostics(snap));
                on_notice("Redacted diagnostic summary copied (tokens & URLs omitted).".into(), false);
            }
        });
    });
}
