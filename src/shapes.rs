//! Flowchart shapes: the kinds offered in the toolbar's "Shapes" menu, how each one is painted,
//! where its label goes, and where an arrow touches its outline. Everything here works in
//! screen space; callers pass the item's on-screen rect and the current zoom.

use std::sync::Arc;

use eframe::egui::{self, Align, Color32, FontId, Galley, Mesh, Painter, Pos2, Rect, Stroke, Vec2};
use serde::{Deserialize, Serialize};

use crate::theme;

/// Label font size at 100% zoom (scaled with the zoom).
pub const FONT_SIZE: f32 = 15.0;
/// Labels smaller than this on screen aren't drawn (they'd just be noise when zoomed far out).
const MIN_FONT_SIZE: f32 = 4.0;
/// Arrow label font size at 100% zoom (scaled with the zoom).
pub const LINK_FONT_SIZE: f32 = 13.0;
/// Width arrow labels wrap at, at 100% zoom.
pub const LINK_LABEL_WIDTH: f32 = 160.0;
/// Space between a shape's outline and its label, at 100% zoom.
const TEXT_PADDING: f32 = 8.0;

/// Shape colors on offer (the outline color; the fill is a dark tint of it).
pub const PALETTE: [(&str, Color32); 8] = [
    ("Gray", Color32::from_rgb(160, 160, 174)),
    ("Coral", theme::ACCENT),
    ("Amber", Color32::from_rgb(240, 180, 80)),
    ("Green", Color32::from_rgb(110, 200, 130)),
    ("Teal", Color32::from_rgb(80, 200, 200)),
    ("Blue", Color32::from_rgb(105, 150, 255)),
    ("Violet", Color32::from_rgb(170, 130, 250)),
    ("Pink", Color32::from_rgb(240, 120, 180)),
];

/// The standard flowchart symbols, plus a frameless text label.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Process,
    Terminal,
    Decision,
    Data,
    Document,
    Database,
    Predefined,
    Preparation,
    Manual,
    Ellipse,
    Text,
}

impl ShapeKind {
    /// Menu order.
    pub const ALL: [Self; 11] = [
        Self::Process,
        Self::Terminal,
        Self::Decision,
        Self::Data,
        Self::Document,
        Self::Database,
        Self::Predefined,
        Self::Preparation,
        Self::Manual,
        Self::Ellipse,
        Self::Text,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Process => "Process",
            Self::Terminal => "Start / End",
            Self::Decision => "Decision",
            Self::Data => "Input / Output",
            Self::Document => "Document",
            Self::Database => "Database",
            Self::Predefined => "Subprocess",
            Self::Preparation => "Preparation",
            Self::Manual => "Manual step",
            Self::Ellipse => "Ellipse",
            Self::Text => "Text",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Process => "A step or action",
            Self::Terminal => "Where the flow starts or ends",
            Self::Decision => "A question that branches (yes / no)",
            Self::Data => "Data going in or out",
            Self::Document => "A document or report",
            Self::Database => "Stored data",
            Self::Predefined => "A process defined elsewhere",
            Self::Preparation => "Setup before a step",
            Self::Manual => "A step done by hand",
            Self::Ellipse => "A connector, or a free-form circle",
            Self::Text => "A free text label, no outline",
        }
    }

    /// Size (board units) a new shape of this kind gets.
    pub fn default_size(self) -> Vec2 {
        match self {
            Self::Decision => Vec2::new(170.0, 110.0),
            Self::Ellipse => Vec2::new(110.0, 110.0),
            Self::Database => Vec2::new(130.0, 110.0),
            Self::Terminal => Vec2::new(160.0, 64.0),
            Self::Text => Vec2::new(160.0, 44.0),
            _ => Vec2::new(170.0, 86.0),
        }
    }

    /// Starting color, so a fresh diagram already reads by kind.
    pub fn default_color(self) -> Color32 {
        let name = match self {
            Self::Terminal => "Green",
            Self::Decision => "Amber",
            Self::Data | Self::Document => "Blue",
            Self::Database => "Violet",
            Self::Predefined | Self::Preparation | Self::Manual => "Teal",
            _ => "Gray",
        };
        PALETTE.iter().find(|(n, _)| *n == name).map_or(PALETTE[0].1, |(_, c)| *c)
    }
}

/// A shape item on the board.
pub struct Shape {
    pub kind: ShapeKind,
    pub text: String,
    /// Outline color; the fill is derived from it.
    pub color: Color32,
}

impl Shape {
    pub fn new(kind: ShapeKind) -> Self {
        Self { kind, text: String::new(), color: kind.default_color() }
    }
}

/// Opaque dark fill for a shape outlined in `color` (opaque so overlapping shapes don't mix).
fn fill_for(color: Color32) -> Color32 {
    theme::CANVAS.lerp_to_gamma(color, 0.2)
}

/// Inset of the slanted/cut sides of the data, preparation and manual shapes.
fn slant(kind: ShapeKind, r: Rect) -> f32 {
    match kind {
        ShapeKind::Data => (r.height() * 0.35).min(r.width() * 0.25),
        ShapeKind::Preparation => (r.height() * 0.35).min(r.width() * 0.2),
        ShapeKind::Manual => (r.height() * 0.4).min(r.width() * 0.15),
        _ => 0.0,
    }
}

/// Vertical radius of the database cylinder's ellipses.
fn cylinder_ry(r: Rect) -> f32 {
    (r.height() * 0.13).min(r.width() * 0.25)
}

/// Height of the document's wavy bottom edge.
fn wave(r: Rect) -> f32 {
    r.height() * 0.08
}

/// Width of the double side bars of the subprocess shape.
fn bar_inset(r: Rect, zoom: f32) -> f32 {
    (r.width() * 0.08).min(12.0 * zoom)
}

/// Paint the shape's body (fill + outline) in `rect`. Labels are separate (`paint_text`).
pub fn paint(painter: &Painter, kind: ShapeKind, rect: Rect, color: Color32, zoom: f32) {
    let r = rect;
    let fill = fill_for(color);
    let stroke = Stroke::new((1.5 * zoom).clamp(1.0, 3.0), color);
    let polygon = |points: Vec<Pos2>| {
        painter.add(egui::Shape::convex_polygon(points, fill, stroke));
    };
    match kind {
        ShapeKind::Process => {
            painter.rect(r, 3.0 * zoom, fill, stroke, egui::StrokeKind::Middle);
        }
        ShapeKind::Terminal => {
            painter.rect(r, r.height().min(r.width()) / 2.0, fill, stroke, egui::StrokeKind::Middle);
        }
        ShapeKind::Predefined => {
            painter.rect(r, 2.0 * zoom, fill, stroke, egui::StrokeKind::Middle);
            let inset = bar_inset(r, zoom);
            for x in [r.left() + inset, r.right() - inset] {
                painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], stroke);
            }
        }
        ShapeKind::Decision => polygon(vec![r.center_top(), r.right_center(), r.center_bottom(), r.left_center()]),
        ShapeKind::Data => {
            let s = slant(kind, r);
            polygon(vec![Pos2::new(r.left() + s, r.top()), r.right_top(), Pos2::new(r.right() - s, r.bottom()), r.left_bottom()]);
        }
        ShapeKind::Preparation => {
            let s = slant(kind, r);
            polygon(vec![
                Pos2::new(r.left() + s, r.top()),
                Pos2::new(r.right() - s, r.top()),
                r.right_center(),
                Pos2::new(r.right() - s, r.bottom()),
                Pos2::new(r.left() + s, r.bottom()),
                r.left_center(),
            ]);
        }
        ShapeKind::Manual => {
            let s = slant(kind, r);
            polygon(vec![r.left_top(), r.right_top(), Pos2::new(r.right() - s, r.bottom()), Pos2::new(r.left() + s, r.bottom())]);
        }
        ShapeKind::Ellipse => {
            painter.add(egui::Shape::Ellipse(egui::epaint::EllipseShape { center: r.center(), radius: r.size() / 2.0, angle: 0.0, fill, stroke }));
        }
        ShapeKind::Database => {
            let ry = cylinder_ry(r);
            let rx = r.width() / 2.0;
            let (top_c, bottom_c) = (Pos2::new(r.center().x, r.top() + ry), Pos2::new(r.center().x, r.bottom() - ry));
            const N: usize = 24;
            let arc = |c: Pos2, from: f32, to: f32| {
                (0..=N).map(move |i| {
                    let a = from + (to - from) * i as f32 / N as f32;
                    c + Vec2::new(rx * a.cos(), ry * a.sin())
                })
            };
            use std::f32::consts::PI;
            // Silhouette (convex): back half of the top ellipse, then the front half of the bottom one.
            let outline: Vec<Pos2> = arc(top_c, PI, 2.0 * PI).chain(arc(bottom_c, 0.0, PI)).collect();
            polygon(outline);
            // Front rim of the top ellipse.
            painter.add(egui::Shape::line(arc(top_c, 0.0, PI).collect(), stroke));
        }
        ShapeKind::Document => {
            // Not convex (wavy bottom), so fill with a fan from the center, which works for any
            // outline every point of which is visible from the center.
            let amp = wave(r);
            let mut points = vec![r.left_top(), r.right_top()];
            const N: usize = 32;
            for i in 0..=N {
                let u = 1.0 - i as f32 / N as f32; // right to left
                let y = r.bottom() - amp + amp * (u * std::f32::consts::TAU).sin();
                points.push(Pos2::new(r.left() + u * r.width(), y));
            }
            let mut mesh = Mesh::default();
            mesh.colored_vertex(r.center(), fill);
            for &p in &points {
                mesh.colored_vertex(p, fill);
            }
            let n = points.len() as u32;
            for i in 0..n {
                mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
            }
            painter.add(mesh);
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        ShapeKind::Text => {}
    }
}

/// The area inside the shape where its label wraps.
pub fn text_rect(kind: ShapeKind, rect: Rect, zoom: f32) -> Rect {
    let r = rect;
    let inner = match kind {
        ShapeKind::Decision => r.shrink2(r.size() / 4.0),
        ShapeKind::Ellipse => r.shrink2(r.size() * 0.15),
        ShapeKind::Terminal => r.shrink2(Vec2::new(r.height().min(r.width()) * 0.3, 0.0)),
        ShapeKind::Data | ShapeKind::Preparation | ShapeKind::Manual => r.shrink2(Vec2::new(slant(kind, r), 0.0)),
        ShapeKind::Predefined => r.shrink2(Vec2::new(bar_inset(r, zoom), 0.0)),
        ShapeKind::Document => Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - 2.0 * wave(r))),
        ShapeKind::Database => Rect::from_min_max(Pos2::new(r.min.x, r.min.y + 2.0 * cylinder_ry(r)), Pos2::new(r.max.x, r.max.y - cylinder_ry(r))),
        ShapeKind::Process | ShapeKind::Text => r,
    };
    let inner = inner.shrink(TEXT_PADDING * zoom);
    // Never let the padding turn the rect inside out on tiny shapes.
    if inner.width() > 1.0 && inner.height() > 1.0 { inner } else { Rect::from_center_size(inner.center(), Vec2::splat(1.0)) }
}

/// Paint `text` centered in the shape. An empty frameless text shows a dim placeholder so it
/// can still be found on the board.
pub fn paint_text(painter: &Painter, kind: ShapeKind, rect: Rect, text: &str, zoom: f32) {
    let size = FONT_SIZE * zoom;
    if size < MIN_FONT_SIZE {
        return;
    }
    let (text, color) = match (text.is_empty(), kind) {
        (true, ShapeKind::Text) => ("Text", theme::TEXT_DIM),
        (true, _) => return,
        _ => (text, theme::TEXT),
    };
    let inner = text_rect(kind, rect, zoom);
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), FontId::proportional(size), color, inner.width());
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    // `galley.rect` is in the galley's own coordinates (centered rows can start at negative x).
    let at = inner.center() - galley.rect.center().to_vec2();
    painter.with_clip_rect(rect.intersect(painter.clip_rect())).galley(at, galley, color);
}

/// Where the line from the center of `rect` toward `toward` leaves the item's outline. `kind`
/// is `None` for media items (plain rectangles).
pub fn boundary_point(kind: Option<ShapeKind>, rect: Rect, toward: Pos2) -> Pos2 {
    let c = rect.center();
    let d = toward - c;
    if d.length_sq() < 1e-6 {
        return c;
    }
    let (a, b) = (rect.width() / 2.0, rect.height() / 2.0);
    // Scale `t` such that `c + d * t` is on the outline.
    let t = match kind {
        Some(ShapeKind::Ellipse) => 1.0 / ((d.x / a).powi(2) + (d.y / b).powi(2)).sqrt(),
        Some(ShapeKind::Decision) => 1.0 / (d.x.abs() / a + d.y.abs() / b),
        _ => {
            let tx = if d.x != 0.0 { a / d.x.abs() } else { f32::INFINITY };
            let ty = if d.y != 0.0 { b / d.y.abs() } else { f32::INFINITY };
            tx.min(ty)
        }
    };
    if t.is_finite() { c + d * t } else { c }
}

/// An arrow from `from` to `to`, with the head at `to`.
pub fn paint_arrow(shapes: &mut Vec<egui::Shape>, from: Pos2, to: Pos2, width: f32, color: Color32, zoom: f32) {
    let d = to - from;
    let len = d.length();
    if len < 1.0 {
        return;
    }
    let dir = d / len;
    let head_len = (11.0 * zoom).clamp(6.0, 24.0).min(len * 0.6);
    let base = to - dir * head_len;
    let side = dir.rot90() * head_len * 0.5;
    shapes.push(egui::Shape::line_segment([from, base + dir * 0.5], Stroke::new(width, color)));
    shapes.push(egui::Shape::convex_polygon(vec![to, base + side, base - side], color, Stroke::NONE));
}

/// A laid-out arrow label: where to paint the galley, the galley, and its padded on-screen rect.
pub type LinkLabel = (Pos2, Arc<Galley>, Rect);

/// An arrow's label laid out centered on `mid`: where to paint the galley, the galley, and its
/// padded on-screen rect (the plate behind it and its hit area). `None` when there's no text or
/// it would be too small to read.
pub fn layout_link_label(painter: &Painter, text: &str, mid: Pos2, zoom: f32) -> Option<LinkLabel> {
    let size = LINK_FONT_SIZE * zoom;
    if text.is_empty() || size < MIN_FONT_SIZE {
        return None;
    }
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), FontId::proportional(size), theme::TEXT, LINK_LABEL_WIDTH * zoom);
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    // `galley.rect` is in the galley's own coordinates (centered rows can start at negative x).
    let at = mid - galley.rect.center().to_vec2();
    let rect = galley.rect.translate(at.to_vec2()).expand2(Vec2::new(5.0, 2.0) * zoom);
    Some((at, galley, rect))
}

/// Paint a label from `layout_link_label` on a canvas-colored plate, so the line looks broken
/// around the text instead of crossing it.
pub fn paint_link_label(shapes: &mut Vec<egui::Shape>, at: Pos2, galley: Arc<Galley>, rect: Rect, zoom: f32) {
    shapes.push(egui::Shape::rect_filled(rect, 4.0 * zoom, theme::CANVAS));
    shapes.push(egui::Shape::galley(at, galley, theme::TEXT));
}

/// Distance from `p` to the segment `a`–`b`.
pub fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}
