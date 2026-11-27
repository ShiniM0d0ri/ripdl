//! Cohesive ripdl visual tokens, typography, Light/Dark palettes, and shared widgets.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, CornerRadius, Stroke, StrokeKind};

use crate::vectors::{paint_icon, IconId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    pub accent_dark: Color32,
    pub accent_soft: Color32,
    pub canvas: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub track: Color32,
    pub ink: Color32,
    pub ink_secondary: Color32,
    pub ink_muted: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub info: Color32,
    pub info_soft: Color32,
    pub violet: Color32,
    pub violet_soft: Color32,
    pub scrim: Color32,
}

impl Palette {
    pub fn light() -> Self {
        Self {
            accent: Color32::from_rgb(15, 118, 110),
            accent_dark: Color32::from_rgb(12, 92, 86),
            accent_soft: Color32::from_rgb(230, 244, 241),
            canvas: Color32::from_rgb(239, 243, 246),
            surface: Color32::WHITE,
            surface_alt: Color32::from_rgb(248, 250, 252),
            surface_hover: Color32::from_rgb(241, 245, 249),
            border: Color32::from_rgb(219, 227, 234),
            border_strong: Color32::from_rgb(198, 210, 220),
            track: Color32::from_rgb(228, 234, 240),
            ink: Color32::from_rgb(15, 23, 42),
            ink_secondary: Color32::from_rgb(51, 65, 85),
            ink_muted: Color32::from_rgb(100, 116, 139),
            success: Color32::from_rgb(21, 128, 61),
            success_soft: Color32::from_rgb(228, 246, 234),
            warn: Color32::from_rgb(168, 82, 12),
            warn_soft: Color32::from_rgb(254, 243, 222),
            danger: Color32::from_rgb(185, 28, 28),
            danger_soft: Color32::from_rgb(254, 232, 232),
            info: Color32::from_rgb(29, 78, 216),
            info_soft: Color32::from_rgb(232, 240, 254),
            violet: Color32::from_rgb(109, 40, 217),
            violet_soft: Color32::from_rgb(243, 236, 254),
            scrim: Color32::from_rgba_unmultiplied(15, 23, 42, 72),
        }
    }

    pub fn dark() -> Self {
        Self {
            accent: Color32::from_rgb(20, 184, 166),
            accent_dark: Color32::from_rgb(13, 148, 136),
            accent_soft: Color32::from_rgb(19, 53, 52),
            canvas: Color32::from_rgb(15, 20, 25),
            surface: Color32::from_rgb(22, 29, 36),
            surface_alt: Color32::from_rgb(28, 37, 46),
            surface_hover: Color32::from_rgb(34, 45, 56),
            border: Color32::from_rgb(44, 57, 71),
            border_strong: Color32::from_rgb(64, 82, 100),
            track: Color32::from_rgb(38, 49, 61),
            ink: Color32::from_rgb(235, 241, 246),
            ink_secondary: Color32::from_rgb(186, 198, 212),
            ink_muted: Color32::from_rgb(148, 163, 184),
            success: Color32::from_rgb(74, 222, 128),
            success_soft: Color32::from_rgb(20, 56, 37),
            warn: Color32::from_rgb(251, 191, 36),
            warn_soft: Color32::from_rgb(64, 45, 18),
            danger: Color32::from_rgb(248, 113, 113),
            danger_soft: Color32::from_rgb(68, 28, 30),
            info: Color32::from_rgb(96, 165, 250),
            info_soft: Color32::from_rgb(24, 44, 72),
            violet: Color32::from_rgb(192, 132, 252),
            violet_soft: Color32::from_rgb(49, 30, 71),
            scrim: Color32::from_rgba_unmultiplied(5, 8, 12, 135),
        }
    }

    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }
}

pub mod metrics {
    pub const TOOLBAR_H: f32 = 30.0;
    pub const ROW_H: f32 = 48.0;
    pub const HEADER_H: f32 = 30.0;
    pub const SIDEBAR_W: f32 = 204.0;
}

pub fn apply_visuals(ctx: &egui::Context, mode: ThemeMode) {
    let pal = Palette::for_mode(mode);
    let mut v = match mode {
        ThemeMode::Light => egui::Visuals::light(),
        ThemeMode::Dark => egui::Visuals::dark(),
    };
    v.panel_fill = pal.canvas;
    v.window_fill = pal.surface;
    v.window_stroke = Stroke::new(1.0, pal.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = pal.surface;
    v.faint_bg_color = pal.surface_alt;
    v.selection.bg_fill = pal.accent_soft;
    v.selection.stroke = Stroke::new(1.0, pal.accent);

    let radius = CornerRadius::same(6);
    v.widgets.noninteractive.bg_fill = pal.surface;
    v.widgets.noninteractive.weak_bg_fill = pal.surface_alt;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.ink);
    v.widgets.noninteractive.corner_radius = radius;

    v.widgets.inactive.bg_fill = pal.surface;
    v.widgets.inactive.weak_bg_fill = pal.surface_alt;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.ink);
    v.widgets.inactive.corner_radius = radius;

    v.widgets.hovered.bg_fill = pal.accent_soft;
    v.widgets.hovered.weak_bg_fill = pal.accent_soft;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, pal.accent_dark);
    v.widgets.hovered.corner_radius = radius;

    v.widgets.active.bg_fill = pal.accent_soft;
    v.widgets.active.weak_bg_fill = pal.accent_soft;
    v.widgets.active.bg_stroke = Stroke::new(1.2, pal.accent_dark);
    v.widgets.active.fg_stroke = Stroke::new(1.0, pal.accent_dark);
    v.widgets.active.corner_radius = radius;
    ctx.set_visuals(v);
}

pub fn configure_style(ctx: &egui::Context, mode: ThemeMode) {
    let mut fonts = egui::FontDefinitions::default();
    let semibold_family = egui::FontFamily::Name("semibold".into());
    let mut proportional_front = Vec::new();
    let mut mono_front = Vec::new();
    let mut semibold_front = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts
                .font_data
                .insert("segoeui".to_owned(), egui::FontData::from_owned(bytes).into());
            proportional_front.push("segoeui".to_owned());
        }
        if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\seguisb.ttf") {
            fonts
                .font_data
                .insert("seguisb".to_owned(), egui::FontData::from_owned(bytes).into());
            semibold_front.push("seguisb".to_owned());
        }
        if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\CascadiaMono.ttf")
            .or_else(|_| std::fs::read("C:\\Windows\\Fonts\\consola.ttf"))
        {
            fonts
                .font_data
                .insert("winmono".to_owned(), egui::FontData::from_owned(bytes).into());
            mono_front.push("winmono".to_owned());
        }
    }

    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        for name in proportional_front.iter().rev() {
            fam.insert(0, name.clone());
        }
    }
    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        for name in mono_front.iter().rev() {
            fam.insert(0, name.clone());
        }
    }
    let mut sb_list = semibold_front;
    if let Some(prop) = fonts.families.get(&egui::FontFamily::Proportional) {
        sb_list.extend(prop.iter().cloned());
    }
    fonts.families.insert(semibold_family, sb_list);

    ctx.set_fonts(fonts);
    apply_visuals(ctx, mode);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(11.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(17.5, egui::FontFamily::Name("semibold".into())),
    );
    ctx.set_global_style(style);
}

pub fn semibold(text: &str, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(text)
        .font(egui::FontId::new(size, egui::FontFamily::Name("semibold".into())))
        .color(color)
}

pub fn kind_visuals(kind: &str, pal: &Palette) -> (IconId, &'static str, Color32, Color32) {
    match kind {
        "torrent" => (IconId::Torrent, "Torrent", pal.accent_dark, pal.accent_soft),
        "video" => (IconId::Video, "Video", pal.violet, pal.violet_soft),
        "bundle" => (IconId::Bundle, "Bundle", pal.warn, pal.warn_soft),
        _ => (IconId::Download, "HTTP", pal.info, pal.info_soft),
    }
}

pub fn state_visuals(state: &str, pal: &Palette) -> (&'static str, Color32, Color32, Color32) {
    match state {
        "running" => ("Downloading", pal.accent_dark, pal.accent_soft, pal.accent),
        "connecting" => ("Connecting", pal.info, pal.info_soft, pal.info),
        "metadata" => ("Metadata", pal.violet, pal.violet_soft, pal.violet),
        "verifying" => ("Verifying", pal.info, pal.info_soft, pal.info),
        "hunt" => ("Capturing", pal.violet, pal.violet_soft, pal.violet),
        "seeding" => ("Seeding", pal.success, pal.success_soft, pal.success),
        "completed" => ("Completed", pal.success, pal.success_soft, pal.success),
        "paused" => ("Paused", pal.warn, pal.warn_soft, pal.warn),
        "expired" => ("Link expired", pal.danger, pal.danger_soft, pal.danger),
        "failed" => ("Failed", pal.danger, pal.danger_soft, pal.danger),
        "timed" => ("Scheduled", pal.info, pal.info_soft, pal.info),
        _ => ("Queued", pal.ink_secondary, pal.track, pal.ink_muted),
    }
}

/// Crisp status pill with a 6px dot indicator and optional vector icon.
pub fn paint_status_pill(ui: &mut egui::Ui, label: &str, fg: Color32, bg: Color32, dot: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (drect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                ui.painter().circle_filled(drect.center(), 3.0, dot);
                ui.label(egui::RichText::new(label).small().color(fg));
            });
        });
}

/// Outlined pill containing a 12px vector glyph + short text (Kind column, footer pills).
pub fn paint_kind_pill(ui: &mut egui::Ui, icon: IconId, label: &str, fg: Color32, pal: &Palette) {
    egui::Frame::new()
        .fill(pal.surface_alt)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (irect, _) = ui.allocate_exact_size(egui::vec2(11.0, 11.0), egui::Sense::hover());
                paint_icon(ui.painter(), irect, icon, fg);
                ui.label(egui::RichText::new(label).small().color(pal.ink_secondary));
            });
        });
}

/// Horizontal progress bar with animated indeterminate pulse for connecting/metadata states.
pub fn paint_progress_bar(ui: &mut egui::Ui, ratio: f32, state: &str, width: f32, pal: &Palette) {
    let pct = (ratio.clamp(0.0, 1.0) * 100.0).round() as u32;
    let bar_w = (width - 42.0).max(32.0);
    let fill = match state {
        "completed" | "seeding" => pal.success,
        "expired" | "failed" => pal.danger,
        "paused" => pal.warn,
        "metadata" | "hunt" => pal.violet,
        "connecting" | "verifying" => pal.info,
        _ => pal.accent,
    };
    let is_indeterminate = matches!(state, "connecting" | "metadata" | "verifying") && ratio <= 0.005;
    let reduce_motion = ui.style().animation_time <= 0.0;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 6.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(3), pal.track);
        if is_indeterminate {
            let t = if reduce_motion { 0.5 } else { (ui.input(|i| i.time) as f32 * 0.9) % 1.0 };
            let seg_w = rect.width() * 0.34;
            let x0 = if reduce_motion {
                rect.center().x - seg_w * 0.5
            } else {
                rect.min.x + (rect.width() - seg_w) * (0.5 - 0.5 * (t * std::f32::consts::TAU).cos())
            };
            let r = egui::Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x0 + seg_w, rect.max.y));
            p.rect_filled(r, CornerRadius::same(3), fill);
        } else {
            let fill_w = rect.width() * ratio.clamp(0.0, 1.0);
            if fill_w > 0.5 {
                let mut r = rect;
                r.max.x = r.min.x + fill_w;
                p.rect_filled(r, CornerRadius::same(3), fill);
            }
        }
        ui.label(
            egui::RichText::new(format!("{pct:>3}%"))
                .small()
                .monospace()
                .color(pal.ink_muted),
        );
    });
}

/// Area-filled 60-second sparkline in the status bar with soft accent wash and end dot.
pub fn paint_sparkline(ui: &mut egui::Ui, size: egui::Vec2, history: &VecDeque<f64>, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), pal.surface_alt);
    p.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, pal.border), StrokeKind::Inside);
    if history.len() < 2 {
        return;
    }
    let max_val = history.iter().copied().fold(1.0_f64, f64::max) * 1.18;
    let n = history.len();
    let pts: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.min.x + 2.0 + (i as f32 / (n - 1) as f32) * (rect.width() - 4.0);
            let frac = (*v / max_val).clamp(0.0, 1.0) as f32;
            let y = rect.max.y - 2.0 - frac * (rect.height() - 4.0);
            egui::pos2(x, y)
        })
        .collect();

    let wash = Color32::from_rgba_unmultiplied(pal.accent.r(), pal.accent.g(), pal.accent.b(), 34);
    for seg in pts.windows(2) {
        let quad = vec![
            seg[0],
            seg[1],
            egui::pos2(seg[1].x, rect.max.y - 1.5),
            egui::pos2(seg[0].x, rect.max.y - 1.5),
        ];
        p.add(egui::epaint::PathShape::convex_polygon(quad, wash, Stroke::NONE));
    }
    p.add(egui::epaint::PathShape::line(pts.clone(), Stroke::new(1.5, pal.accent)));
    if let Some(&last) = pts.last() {
        p.circle_filled(last, 2.2, pal.accent_dark);
    }
}

/// Uniform dialog header strip inside frameless `egui::Window` modals.
pub fn dialog_header(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: IconId,
    title: &str,
    subtitle: Option<&str>,
) -> bool {
    let mut closed = false;
    ui.horizontal(|ui| {
        let (badge, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
        ui.painter().rect_filled(badge, CornerRadius::same(6), pal.accent_soft);
        paint_icon(ui.painter(), badge.shrink(5.5), icon, pal.accent_dark);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(semibold(title, 14.5, pal.ink));
            if let Some(sub) = subtitle {
                ui.label(egui::RichText::new(sub).small().color(pal.ink_muted));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::vectors::vector_icon_button(ui, pal, IconId::Close, "Close dialog", 24.0, false, false).clicked() {
                closed = true;
            }
        });
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
    closed
}

pub fn primary_button<'a>(label: &'a str, pal: &Palette) -> egui::Button<'a> {
    egui::Button::new(egui::RichText::new(label).color(Color32::WHITE).strong())
        .fill(pal.accent)
        .stroke(Stroke::new(1.0, pal.accent_dark))
}

pub fn add_primary_button(ui: &mut egui::Ui, enabled: bool, label: &str, pal: &Palette) -> egui::Response {
    if enabled {
        ui.add(primary_button(label, pal))
    } else {
        ui.add_enabled(
            false,
            egui::Button::new(egui::RichText::new(label).color(pal.ink_muted))
                .fill(pal.track)
                .stroke(Stroke::new(1.0, pal.border)),
        )
    }
}
