//! DPI-independent Lucide v1.82.0 SVG vector renderer, brand artwork, and UI illustrations.
//!
//! Renders official 24x24 Lucide SVG primitives (`M/L/H/V/C/S/Q/T/A/Z`, `<circle>`, `<rect>`,
//! `<line>`, `<polyline>`, `<polygon>`) directly through `egui::Painter` with W3C elliptical arc
//! and Bezier curve flattening plus WCAG 2.2 visible keyboard focus rings.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Vec2};
use eframe::epaint::PathShape;

use crate::lucide_icons::{shapes_for, SvgShape};
use crate::theme::Palette;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconId {
    Add,
    Download,
    Upload,
    Torrent,
    Bundle,
    Video,
    Audio,
    Image,
    Document,
    Archive,
    Program,
    AllDownloads,
    Active,
    Play,
    Pause,
    Stop,
    Trash,
    ClearDone,
    Refresh,
    Lan,
    Bond,
    Settings,
    Details,
    Search,
    Close,
    Folder,
    FolderOpen,
    OpenFile,
    Copy,
    Shield,
    Check,
    CheckCircle,
    Alert,
    Gauge,
    Clock,
    Sliders,
    Sun,
    Moon,
    ChevronDown,
    ChevronRight,
    External,
}

impl IconId {
    #[cfg(test)]
    pub fn all() -> &'static [IconId] {
        use IconId::*;
        &[
            Add,
            Download,
            Upload,
            Torrent,
            Bundle,
            Video,
            Audio,
            Image,
            Document,
            Archive,
            Program,
            AllDownloads,
            Active,
            Play,
            Pause,
            Stop,
            Trash,
            ClearDone,
            Refresh,
            Lan,
            Bond,
            Settings,
            Details,
            Search,
            Close,
            Folder,
            FolderOpen,
            OpenFile,
            Copy,
            Shield,
            Check,
            CheckCircle,
            Alert,
            Gauge,
            Clock,
            Sliders,
            Sun,
            Moon,
            ChevronDown,
            ChevronRight,
            External,
        ]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Subpath {
    pub(crate) points: Vec<(f32, f32)>,
    pub(crate) closed: bool,
}

struct PathLexer<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> PathLexer<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_sep(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() || b == b',' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_sep();
        self.bytes.get(self.pos).copied()
    }

    fn next_cmd(&mut self) -> Option<u8> {
        let b = self.peek()?;
        if b.is_ascii_alphabetic() {
            self.pos += 1;
            Some(b)
        } else {
            None
        }
    }

    fn next_flag(&mut self) -> Option<bool> {
        let b = self.peek()?;
        match b {
            b'0' => {
                self.pos += 1;
                Some(false)
            }
            b'1' => {
                self.pos += 1;
                Some(true)
            }
            _ => None,
        }
    }

    fn next_f32(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.pos;
        if self.pos >= self.bytes.len() {
            return None;
        }
        if matches!(self.bytes[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        let mut saw_digit = false;
        let mut saw_dot = false;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_digit() {
                saw_digit = true;
                self.pos += 1;
            } else if b == b'.' && !saw_dot {
                saw_dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            self.pos = start;
            return None;
        }
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
            let exp_start = self.pos;
            self.pos += 1;
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'+' | b'-') {
                self.pos += 1;
            }
            let mut exp_digits = false;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                exp_digits = true;
                self.pos += 1;
            }
            if !exp_digits {
                self.pos = exp_start;
            }
        }
        let slice = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        slice.parse::<f32>().ok()
    }
}

fn push_cubic(pts: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    const STEPS: usize = 12;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let mt2 = mt * mt;
        let t2 = t * t;
        let x = mt2 * mt * p0.0 + 3.0 * mt2 * t * p1.0 + 3.0 * mt * t2 * p2.0 + t2 * t * p3.0;
        let y = mt2 * mt * p0.1 + 3.0 * mt2 * t * p1.1 + 3.0 * mt * t2 * p2.1 + t2 * t * p3.1;
        pts.push((x, y));
    }
}

fn push_quad(pts: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    const STEPS: usize = 10;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let x = mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0;
        let y = mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1;
        pts.push((x, y));
    }
}

fn vector_angle(ux: f64, uy: f64, vx: f64, vy: f64) -> f64 {
    let dot = ux * vx + uy * vy;
    let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
    if len <= 1e-12 {
        return 0.0;
    }
    let ratio = (dot / len).clamp(-1.0, 1.0);
    let ang = ratio.acos();
    if ux * vy - uy * vx < 0.0 {
        -ang
    } else {
        ang
    }
}

/// W3C SVG 1.1 F.6.5/F.6.6 endpoint-to-center elliptical arc subdivision.
#[allow(clippy::too_many_arguments)]
fn push_svg_arc(
    pts: &mut Vec<(f32, f32)>,
    p0: (f32, f32),
    rx_in: f32,
    ry_in: f32,
    x_rot_deg: f32,
    large_arc: bool,
    sweep: bool,
    p1: (f32, f32),
) {
    let (x1, y1) = (p0.0 as f64, p0.1 as f64);
    let (x2, y2) = (p1.0 as f64, p1.1 as f64);
    if (x1 - x2).abs() < 1e-5 && (y1 - y2).abs() < 1e-5 {
        return;
    }
    let mut rx = (rx_in as f64).abs();
    let mut ry = (ry_in as f64).abs();
    if rx < 1e-6 || ry < 1e-6 {
        pts.push(p1);
        return;
    }

    let phi = (x_rot_deg as f64).to_radians();
    let cos_phi = phi.cos();
    let sin_phi = phi.sin();

    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;

    let rx_sq = rx * rx;
    let ry_sq = ry * ry;
    let x1p_sq = x1p * x1p;
    let y1p_sq = y1p * y1p;
    let lambda = x1p_sq / rx_sq + y1p_sq / ry_sq;
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let num = (rx2 * ry2 - rx2 * y1p_sq - ry2 * x1p_sq).max(0.0);
    let den = (rx2 * y1p_sq + ry2 * x1p_sq).max(1e-12);
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coef = sign * (num / den).sqrt();
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);

    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) * 0.5;

    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;

    let theta1 = vector_angle(1.0, 0.0, ux, uy);
    let mut dtheta = vector_angle(ux, uy, vx, vy);
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }

    let steps = ((dtheta.abs() / (std::f64::consts::PI / 14.0)).ceil() as usize).clamp(4, 32);
    for i in 1..=steps {
        let ang = theta1 + dtheta * (i as f64 / steps as f64);
        let ca = rx * ang.cos();
        let sa = ry * ang.sin();
        let x = cos_phi * ca - sin_phi * sa + cx;
        let y = sin_phi * ca + cos_phi * sa + cy;
        pts.push((x as f32, y as f32));
    }
}

pub(crate) fn parse_svg_path(d: &str) -> Vec<Subpath> {
    let mut lex = PathLexer::new(d);
    let mut subpaths = Vec::new();
    let mut cur_pts: Vec<(f32, f32)> = Vec::new();
    let mut cur = (0.0_f32, 0.0_f32);
    let mut start = (0.0_f32, 0.0_f32);
    let mut prev_c2: Option<(f32, f32)> = None;
    let mut prev_q1: Option<(f32, f32)> = None;
    let mut cmd = b'M';

    loop {
        if let Some(c) = lex.next_cmd() {
            cmd = c;
        } else if lex.peek().is_none() {
            break;
        } else {
            cmd = match cmd {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            };
        }

        match cmd {
            b'M' | b'm' => {
                if !cur_pts.is_empty() {
                    subpaths.push(Subpath {
                        points: std::mem::take(&mut cur_pts),
                        closed: false,
                    });
                }
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                cur = if cmd == b'm' { (cur.0 + x, cur.1 + y) } else { (x, y) };
                start = cur;
                cur_pts.push(cur);
                prev_c2 = None;
                prev_q1 = None;
            }
            b'L' | b'l' => {
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                cur = if cmd == b'l' { (cur.0 + x, cur.1 + y) } else { (x, y) };
                cur_pts.push(cur);
                prev_c2 = None;
                prev_q1 = None;
            }
            b'H' | b'h' => {
                let Some(x) = lex.next_f32() else { break };
                cur.0 = if cmd == b'h' { cur.0 + x } else { x };
                cur_pts.push(cur);
                prev_c2 = None;
                prev_q1 = None;
            }
            b'V' | b'v' => {
                let Some(y) = lex.next_f32() else { break };
                cur.1 = if cmd == b'v' { cur.1 + y } else { y };
                cur_pts.push(cur);
                prev_c2 = None;
                prev_q1 = None;
            }
            b'C' | b'c' => {
                let Some(x1) = lex.next_f32() else { break };
                let Some(y1) = lex.next_f32() else { break };
                let Some(x2) = lex.next_f32() else { break };
                let Some(y2) = lex.next_f32() else { break };
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                let (c1, c2, end) = if cmd == b'c' {
                    (
                        (cur.0 + x1, cur.1 + y1),
                        (cur.0 + x2, cur.1 + y2),
                        (cur.0 + x, cur.1 + y),
                    )
                } else {
                    ((x1, y1), (x2, y2), (x, y))
                };
                push_cubic(&mut cur_pts, cur, c1, c2, end);
                cur = end;
                prev_c2 = Some(c2);
                prev_q1 = None;
            }
            b'S' | b's' => {
                let Some(x2) = lex.next_f32() else { break };
                let Some(y2) = lex.next_f32() else { break };
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                let c1 = match prev_c2 {
                    Some(pc) => (2.0 * cur.0 - pc.0, 2.0 * cur.1 - pc.1),
                    None => cur,
                };
                let (c2, end) = if cmd == b's' {
                    ((cur.0 + x2, cur.1 + y2), (cur.0 + x, cur.1 + y))
                } else {
                    ((x2, y2), (x, y))
                };
                push_cubic(&mut cur_pts, cur, c1, c2, end);
                cur = end;
                prev_c2 = Some(c2);
                prev_q1 = None;
            }
            b'Q' | b'q' => {
                let Some(x1) = lex.next_f32() else { break };
                let Some(y1) = lex.next_f32() else { break };
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                let (q1, end) = if cmd == b'q' {
                    ((cur.0 + x1, cur.1 + y1), (cur.0 + x, cur.1 + y))
                } else {
                    ((x1, y1), (x, y))
                };
                push_quad(&mut cur_pts, cur, q1, end);
                cur = end;
                prev_q1 = Some(q1);
                prev_c2 = None;
            }
            b'T' | b't' => {
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                let q1 = match prev_q1 {
                    Some(pq) => (2.0 * cur.0 - pq.0, 2.0 * cur.1 - pq.1),
                    None => cur,
                };
                let end = if cmd == b't' { (cur.0 + x, cur.1 + y) } else { (x, y) };
                push_quad(&mut cur_pts, cur, q1, end);
                cur = end;
                prev_q1 = Some(q1);
                prev_c2 = None;
            }
            b'A' | b'a' => {
                let Some(rx) = lex.next_f32() else { break };
                let Some(ry) = lex.next_f32() else { break };
                let Some(rot) = lex.next_f32() else { break };
                let Some(large) = lex.next_flag() else { break };
                let Some(sweep) = lex.next_flag() else { break };
                let Some(x) = lex.next_f32() else { break };
                let Some(y) = lex.next_f32() else { break };
                let end = if cmd == b'a' { (cur.0 + x, cur.1 + y) } else { (x, y) };
                push_svg_arc(&mut cur_pts, cur, rx, ry, rot, large, sweep, end);
                cur = end;
                prev_c2 = None;
                prev_q1 = None;
            }
            b'Z' | b'z' => {
                if !cur_pts.is_empty() {
                    subpaths.push(Subpath {
                        points: std::mem::take(&mut cur_pts),
                        closed: true,
                    });
                }
                cur = start;
                prev_c2 = None;
                prev_q1 = None;
            }
            _ => break,
        }
    }

    if !cur_pts.is_empty() {
        subpaths.push(Subpath {
            points: cur_pts,
            closed: false,
        });
    }
    subpaths
}

#[inline]
fn map_24(rect: Rect, x: f32, y: f32) -> Pos2 {
    egui::pos2(
        rect.min.x + (x / 24.0) * rect.width(),
        rect.min.y + (y / 24.0) * rect.height(),
    )
}

#[inline]
fn scale_24(rect: Rect, v: f32) -> f32 {
    (v / 24.0) * rect.width().min(rect.height())
}

fn draw_poly(p: &egui::Painter, rect: Rect, coords: &[(f32, f32)], closed: bool, st: Stroke) {
    if coords.is_empty() {
        return;
    }
    let pts: Vec<Pos2> = coords.iter().map(|&(x, y)| map_24(rect, x, y)).collect();
    let cap_r = (st.width * 0.48).max(0.5);
    if pts.len() == 1 {
        p.circle_filled(pts[0], cap_r, st.color);
        return;
    }
    if !closed {
        if let Some(&first) = pts.first() {
            p.circle_filled(first, cap_r, st.color);
        }
        if let Some(&last) = pts.last() {
            p.circle_filled(last, cap_r, st.color);
        }
        p.add(PathShape::line(pts, st));
    } else {
        p.add(PathShape::closed_line(pts, st));
    }
}

/// Render an authentic Lucide 24x24 vector icon scaled sharply into `rect`.
pub fn paint_icon(p: &egui::Painter, rect: Rect, icon: IconId, color: Color32) {
    let dim = rect.width().min(rect.height());
    let box_rect = Rect::from_center_size(rect.center(), Vec2::splat(dim));
    let sw = (dim * (2.0 / 24.0)).clamp(1.2, 2.5);
    let st = Stroke::new(sw, color);

    for shape in shapes_for(icon) {
        match *shape {
            SvgShape::Path(d) => {
                for sp in parse_svg_path(d) {
                    draw_poly(p, box_rect, &sp.points, sp.closed, st);
                }
            }
            SvgShape::Circle(cx, cy, r) => {
                p.circle_stroke(map_24(box_rect, cx, cy), scale_24(box_rect, r), st);
            }
            SvgShape::Ellipse(cx, cy, rx, ry) => {
                let pts: Vec<(f32, f32)> = (0..24)
                    .map(|i| {
                        let a = (i as f32 / 24.0) * std::f32::consts::TAU;
                        (cx + rx * a.cos(), cy + ry * a.sin())
                    })
                    .collect();
                draw_poly(p, box_rect, &pts, true, st);
            }
            SvgShape::Rect(x, y, w, h, rx) => {
                let r = Rect::from_min_max(map_24(box_rect, x, y), map_24(box_rect, x + w, y + h));
                let cr = CornerRadius::same(scale_24(box_rect, rx).round().clamp(0.0, 12.0) as u8);
                p.rect_stroke(r, cr, st, StrokeKind::Middle);
            }
            SvgShape::Line(x1, y1, x2, y2) => {
                draw_poly(p, box_rect, &[(x1, y1), (x2, y2)], false, st);
            }
            SvgShape::Polyline(pts) => draw_poly(p, box_rect, pts, false, st),
            SvgShape::Polygon(pts) => draw_poly(p, box_rect, pts, true, st),
        }
    }
}

/// Paint an accessible WCAG 2.2 AA outer focus outline when a custom control holds focus.
pub fn paint_focus_ring(p: &egui::Painter, rect: Rect, radius: u8, pal: &Palette, has_focus: bool) {
    if has_focus {
        p.rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(radius.saturating_add(1)),
            Stroke::new(2.0, pal.accent),
            StrokeKind::Outside,
        );
    }
}

/// Draw the official `docs/logo.svg` vector brand mark directly at any pixel size.
pub fn paint_brand_logo(p: &egui::Painter, rect: Rect) {
    let dim = rect.width().min(rect.height());
    let box_rect = Rect::from_center_size(rect.center(), Vec2::splat(dim));
    let s = |v: f32| (v / 512.0) * dim;
    let pxy = |x: f32, y: f32| egui::pos2(box_rect.min.x + s(x), box_rect.min.y + s(y));

    let tile_bg = Color32::from_rgb(11, 50, 48);
    let corner = CornerRadius::same((s(112.0)).round().clamp(3.0, 28.0) as u8);
    p.rect_filled(box_rect, corner, tile_bg);

    for (y, w, alpha) in [
        (176.0_f32, 86.0_f32, 140_u8),
        (242.0, 64.0, 191),
        (308.0, 42.0, 255),
    ] {
        let dash = Rect::from_min_max(pxy(104.0, y), pxy(104.0 + w, y + 24.0));
        let r = CornerRadius::same((s(12.0)).round().clamp(1.0, 8.0) as u8);
        p.rect_filled(dash, r, Color32::from_rgba_unmultiplied(78, 224, 181, alpha));
    }

    let stem = Rect::from_min_max(pxy(222.0, 132.0), pxy(290.0, 258.0));
    p.rect_filled(stem, CornerRadius::same((s(12.0)).round().clamp(1.0, 6.0) as u8), Color32::WHITE);
    let head = vec![pxy(352.0, 254.0), pxy(256.0, 384.0), pxy(160.0, 254.0)];
    p.add(PathShape::convex_polygon(head, Color32::WHITE, Stroke::NONE));
}

/// Paint a crisp 30x30 soft-tinted file/source tile with a category/kind Lucide vector icon.
pub fn paint_file_tile(p: &egui::Painter, rect: Rect, kind: &str, category: ripdl_core::categories::Category, pal: &Palette) {
    use ripdl_core::categories::Category;
    let (fg, bg, ico) = match kind {
        "torrent" => (pal.accent_dark, pal.accent_soft, IconId::Torrent),
        "video" => (pal.violet, pal.violet_soft, IconId::Video),
        "bundle" => (pal.warn, pal.warn_soft, IconId::Bundle),
        _ => match category {
            Category::Video => (pal.violet, pal.violet_soft, IconId::Video),
            Category::Audio => (pal.info, pal.info_soft, IconId::Audio),
            Category::Image => (pal.success, pal.success_soft, IconId::Image),
            Category::Document => (pal.info, pal.info_soft, IconId::Document),
            Category::Archive => (pal.warn, pal.warn_soft, IconId::Archive),
            Category::Program => (pal.accent_dark, pal.accent_soft, IconId::Program),
            Category::Other => (pal.info, pal.info_soft, IconId::Download),
        },
    };
    p.rect_filled(rect, CornerRadius::same(7), bg);
    p.rect_stroke(rect, CornerRadius::same(7), Stroke::new(1.0, pal.border), StrokeKind::Inside);
    paint_icon(p, rect.shrink(6.0), ico, fg);
}

/// Overlay a crisp amber warning badge when a download's file is missing from disk.
pub fn paint_missing_badge(p: &egui::Painter, tile_rect: Rect, pal: &Palette) {
    let center = tile_rect.max - Vec2::new(3.5, 3.5);
    let r = 6.2_f32;
    p.circle_filled(center, r + 1.5, pal.surface);
    p.circle_filled(center, r, pal.warn);
    p.line_segment(
        [center - Vec2::new(0.0, 3.0), center + Vec2::new(0.0, 0.8)],
        Stroke::new(1.7, Color32::WHITE),
    );
    p.circle_filled(center + Vec2::new(0.0, 3.0), 0.95, Color32::WHITE);
}

/// Crisp multi-element vector illustration for empty queue state.
pub fn paint_empty_illustration(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(124.0, 84.0), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();

    p.circle_filled(c, 38.0, pal.accent_soft);
    p.circle_stroke(c, 38.0, Stroke::new(1.0, pal.border));

    let card_back = Rect::from_center_size(c + Vec2::new(-16.0, -4.0), Vec2::new(34.0, 42.0));
    p.rect_filled(card_back, CornerRadius::same(6), pal.surface_alt);
    p.rect_stroke(card_back, CornerRadius::same(6), Stroke::new(1.0, pal.border), StrokeKind::Middle);

    let card = Rect::from_center_size(c + Vec2::new(4.0, 2.0), Vec2::new(48.0, 46.0));
    p.rect_filled(card, CornerRadius::same(8), pal.surface);
    p.rect_stroke(card, CornerRadius::same(8), Stroke::new(1.2, pal.border_strong), StrokeKind::Middle);

    let mark = Rect::from_center_size(c + Vec2::new(28.0, -20.0), Vec2::splat(24.0));
    paint_brand_logo(p, mark);

    let r1 = Rect::from_min_size(card.min + Vec2::new(8.0, 12.0), Vec2::new(28.0, 4.0));
    let r2 = Rect::from_min_size(card.min + Vec2::new(8.0, 21.0), Vec2::new(32.0, 4.0));
    let r3 = Rect::from_min_size(card.min + Vec2::new(8.0, 30.0), Vec2::new(22.0, 4.0));
    p.rect_filled(r1, CornerRadius::same(2), pal.track);
    p.rect_filled(r2, CornerRadius::same(2), pal.accent_soft);
    p.rect_filled(Rect::from_min_size(r2.min, Vec2::new(20.0, 4.0)), CornerRadius::same(2), pal.accent);
    p.rect_filled(r3, CornerRadius::same(2), pal.track);
}

/// Crisp vector illustration for filtered-empty view.
pub fn paint_filter_empty_illustration(ui: &mut egui::Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), egui::Sense::hover());
    let p = ui.painter();
    p.circle_filled(rect.center(), 28.0, pal.surface_alt);
    p.circle_stroke(rect.center(), 28.0, Stroke::new(1.0, pal.border));
    paint_icon(p, rect.shrink(17.0), IconId::Search, pal.ink_muted);
}

/// Button with crisp centered Lucide vector glyph + text label (zero baseline drift).
pub fn vector_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: IconId,
    label: &str,
    height: f32,
    primary: bool,
    subtle_frame: bool,
) -> egui::Response {
    let icon_sz = 14.0_f32;
    let pad_x = 10.0_f32;
    let gap = 6.0_f32;
    let font_id = egui::FontId::new(12.5, egui::FontFamily::Proportional);
    let fg = if primary { Color32::WHITE } else { pal.ink_secondary };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font_id, fg);
    let desired_w = (pad_x * 2.0 + icon_sz + gap + galley.size().x).ceil();

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(desired_w, height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let enabled = ui.is_enabled();
        let (fill, stroke, text_col, ico_col) = if primary {
            if !enabled {
                (pal.track, Stroke::new(1.0, pal.border), pal.ink_muted, pal.ink_muted)
            } else if resp.is_pointer_button_down_on() || resp.hovered() {
                (pal.accent_dark, Stroke::new(1.0, pal.accent_dark), Color32::WHITE, Color32::WHITE)
            } else {
                (pal.accent, Stroke::new(1.0, pal.accent), Color32::WHITE, Color32::WHITE)
            }
        } else if !enabled {
            (
                Color32::TRANSPARENT,
                if subtle_frame { Stroke::new(1.0, pal.border) } else { Stroke::NONE },
                pal.ink_muted,
                pal.ink_muted,
            )
        } else if resp.is_pointer_button_down_on() {
            (pal.accent_soft, Stroke::new(1.0, pal.accent), pal.accent_dark, pal.accent_dark)
        } else if resp.hovered() {
            (pal.accent_soft, Stroke::new(1.0, pal.accent), pal.accent_dark, pal.accent)
        } else if subtle_frame {
            (pal.surface, Stroke::new(1.0, pal.border), pal.ink, pal.ink_secondary)
        } else {
            (Color32::TRANSPARENT, Stroke::NONE, pal.ink_secondary, pal.ink_secondary)
        };

        p.rect_filled(rect, CornerRadius::same(6), fill);
        if stroke != Stroke::NONE {
            p.rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
        }
        paint_focus_ring(p, rect, 6, pal, resp.has_focus());

        let ico_rect = Rect::from_center_size(
            egui::pos2(rect.min.x + pad_x + icon_sz * 0.5, rect.center().y),
            Vec2::splat(icon_sz),
        );
        paint_icon(p, ico_rect, icon, ico_col);

        let text_galley = p.layout_no_wrap(
            label.to_owned(),
            egui::FontId::new(12.5, egui::FontFamily::Proportional),
            text_col,
        );
        let text_pos = egui::pos2(
            rect.min.x + pad_x + icon_sz + gap,
            rect.center().y - text_galley.size().y * 0.5,
        );
        p.galley(text_pos, text_galley, text_col);
    }

    resp
}

/// Square icon-only Lucide vector button with accessible name and visible keyboard focus ring.
pub fn vector_icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: IconId,
    accessible_name: &str,
    size: f32,
    danger: bool,
    framed: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), accessible_name)
    });

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let enabled = ui.is_enabled();
        let (fill, stroke, ico_col) = if !enabled {
            (Color32::TRANSPARENT, Stroke::NONE, pal.ink_muted)
        } else if resp.hovered() || resp.is_pointer_button_down_on() {
            if danger {
                (pal.danger_soft, Stroke::new(1.0, pal.danger), pal.danger)
            } else {
                (pal.accent_soft, Stroke::new(1.0, pal.accent), pal.accent_dark)
            }
        } else if framed {
            (
                pal.surface,
                Stroke::new(1.0, pal.border),
                if danger { pal.danger } else { pal.ink_secondary },
            )
        } else {
            (Color32::TRANSPARENT, Stroke::NONE, pal.ink_secondary)
        };

        if fill != Color32::TRANSPARENT {
            p.rect_filled(rect, CornerRadius::same(6), fill);
        }
        if stroke != Stroke::NONE {
            p.rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
        }
        paint_focus_ring(p, rect, 6, pal, resp.has_focus());
        paint_icon(p, rect.shrink((size * 0.22).max(4.0)), icon, ico_col);
    }

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lucide_icons_parse_cleanly_within_bounds() {
        let mut unique = std::collections::HashSet::new();
        for &icon in IconId::all() {
            assert!(unique.insert(icon), "duplicate icon in IconId::all: {icon:?}");
            let shapes = shapes_for(icon);
            assert!(!shapes.is_empty(), "{icon:?} has no SVG shapes");
            for &s in shapes {
                if let SvgShape::Path(d) = s {
                    let subpaths = parse_svg_path(d);
                    assert!(!subpaths.is_empty(), "{icon:?} failed to parse path: {d}");
                    for sp in &subpaths {
                        assert!(!sp.points.is_empty());
                        for &(x, y) in &sp.points {
                            assert!(x.is_finite() && y.is_finite());
                            assert!(
                                (-2.0..=26.0).contains(&x) && (-2.0..=26.0).contains(&y),
                                "{icon:?} coordinate out of 24x24 box: ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
        assert!(unique.len() >= 40);
    }

    #[test]
    fn svg_arc_parses_compact_flags_with_no_spaces() {
        // From lucide `monitor-smartphone.svg`: M18 5a2 2 0 00-2-2H4a2 2 0 00-2 2v7a2 2 0 002 2h7
        let subpaths = parse_svg_path("M18 5a2 2 0 00-2-2H4a2 2 0 00-2 2v7a2 2 0 002 2h7");
        assert_eq!(subpaths.len(), 1);
        let pts = &subpaths[0].points;
        assert!(pts.len() >= 10);
        assert_eq!(pts[0], (18.0, 5.0));
        let last = *pts.last().unwrap();
        assert!((last.0 - 11.0).abs() < 0.01 && (last.1 - 14.0).abs() < 0.01, "last={last:?}");
    }
}
