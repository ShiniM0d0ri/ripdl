pub mod add_dialog;
pub mod details_dialog;
pub mod lan_dialog;
pub mod onboarding_dialog;
pub mod refresh_dialog;
pub mod settings_dialog;

use std::path::Path;

use eframe::egui::{self, CornerRadius, Margin, Stroke};

use crate::theme::Palette;

pub fn modal_frame(ctx: &egui::Context, pal: &Palette) -> egui::Frame {
    egui::Frame::window(&ctx.global_style())
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border_strong))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
}

pub fn section_card(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(pal.surface_alt)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, add);
}

pub fn open_folder_native(path: &Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

pub fn open_external_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Produce a redacted support diagnostic summary (strips URL tokens, cookies,
/// headers, and pairing codes).
pub fn build_redacted_diagnostics(snap: &crate::model::Snapshot) -> String {
    let mut lines = Vec::new();
    lines.push(format!("ripdl v{}", env!("CARGO_PKG_VERSION")));
    lines.push(format!("daemon: {}", snap.daemon_label));
    lines.push(format!(
        "traffic: {}",
        if snap.unified_traffic {
            "unified-traffic"
        } else {
            "standard"
        }
    ));
    lines.push(format!("tasks: {}", snap.tasks.len()));
    lines.push(format!(
        "detected NICs: {}",
        snap.detected_nics
            .iter()
            .map(|n| format!("{} ({})", n.name, n.addr))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if let Some(s) = &snap.settings {
        lines.push(format!(
            "config: concurrent={}, segments={}, min_seg={}, speed={:?}, bond={}",
            s.max_concurrent(),
            s.max_segments(),
            s.min_segment_size(),
            s.max_speed(),
            s.bond_interfaces(),
        ));
    }
    lines.join("\n")
}
