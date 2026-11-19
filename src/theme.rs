//! Visual theme: one dark "studio" palette with a warm coral accent, applied to egui's style at
//! startup and reused by the board's custom painting so everything reads as one system.

use eframe::egui::{self, Color32, CornerRadius, Margin, Shadow, Stroke, Vec2};

/// Canvas behind the board items — the darkest surface.
pub const CANVAS: Color32 = Color32::from_rgb(17, 17, 20);
/// Toolbar / panel background.
pub const PANEL: Color32 = Color32::from_rgb(24, 24, 28);
/// Raised surfaces: menus, pills, buttons at rest.
pub const SURFACE: Color32 = Color32::from_rgb(34, 34, 40);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(44, 44, 52);
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(54, 54, 64);
/// Hairline borders between surfaces.
pub const BORDER: Color32 = Color32::from_rgb(46, 46, 54);
pub const TEXT: Color32 = Color32::from_rgb(232, 230, 227);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 139, 148);
/// Brand accent: selection, primary button, drop highlight.
pub const ACCENT: Color32 = Color32::from_rgb(255, 122, 92);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(255, 146, 120);
/// Text drawn on top of an `ACCENT` fill.
pub const ON_ACCENT: Color32 = Color32::from_rgb(28, 16, 12);
pub const DANGER: Color32 = Color32::from_rgb(255, 99, 99);

/// Dot grid colors: minor dots, and every `MAJOR_EVERY`th dot slightly brighter.
pub const DOT: Color32 = Color32::from_rgb(40, 40, 46);
pub const DOT_MAJOR: Color32 = Color32::from_rgb(62, 62, 72);

/// Corner radius of board items at 100% zoom (scaled with the zoom).
pub const ITEM_RADIUS: f32 = 6.0;
/// Soft drop shadow under every board item.
pub const ITEM_SHADOW: Shadow = Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_black_alpha(120) };

/// Install the palette into egui's dark style and force dark mode (the board is designed for it).
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = PANEL;
        v.window_fill = SURFACE;
        v.extreme_bg_color = CANVAS;
        v.faint_bg_color = SURFACE;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(10);
        v.window_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(140) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(130) };
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.5, ACCENT);
        v.hyperlink_color = ACCENT;
        v.error_fg_color = DANGER;

        let radius = CornerRadius::same(7);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
        w.noninteractive.corner_radius = radius;
        for (wv, fill) in [(&mut w.inactive, SURFACE), (&mut w.hovered, SURFACE_HOVER), (&mut w.active, SURFACE_ACTIVE), (&mut w.open, SURFACE_HOVER)] {
            wv.bg_fill = fill;
            wv.weak_bg_fill = fill;
            wv.corner_radius = radius;
            wv.fg_stroke = Stroke::new(1.0, TEXT);
            wv.expansion = 0.0;
        }
        w.inactive.bg_stroke = Stroke::NONE;
        w.hovered.bg_stroke = Stroke::new(1.0, BORDER);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        w.open.bg_stroke = Stroke::new(1.0, BORDER);

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 6.0);
        s.button_padding = Vec2::new(10.0, 5.0);
        s.interact_size.y = 26.0;
        s.menu_margin = Margin::same(6);
    });
}
