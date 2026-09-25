//! Colours and egui styling.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(0x4f, 0x8c, 0xff);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x2c, 0x4f, 0x8f);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xb3, 0x47);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const OK: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);

pub const APP_BG: Color32 = Color32::from_rgb(0x11, 0x13, 0x17);
pub const PANEL_BG: Color32 = Color32::from_rgb(0x19, 0x1c, 0x22);
pub const PANEL_BG_2: Color32 = Color32::from_rgb(0x1f, 0x23, 0x2a);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x37);

pub const EDITOR_BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
pub const GUTTER_BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
pub const CURRENT_LINE: Color32 = Color32::from_rgb(0x1d, 0x21, 0x28);
pub const TEXT: Color32 = Color32::from_rgb(0xd6, 0xda, 0xe1);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x92, 0xa0);
pub const LINENO: Color32 = Color32::from_rgb(0x46, 0x4d, 0x5a);
pub const LINENO_ACTIVE: Color32 = Color32::from_rgb(0xa8, 0xb0, 0xbe);
pub const SELECTION: Color32 = Color32::from_rgba_premultiplied(0x1f, 0x38, 0x66, 0x90);
pub const FIND_HIT: Color32 = Color32::from_rgba_premultiplied(0x6b, 0x4a, 0x12, 0x90);
pub const CURSOR: Color32 = Color32::from_rgb(0x8a, 0xb4, 0xff);
pub const BOOKMARK: Color32 = Color32::from_rgb(0xff, 0xd5, 0x4f);
pub const SCROLL_TRACK: Color32 = Color32::from_rgb(0x18, 0x1b, 0x20);
pub const SCROLL_THUMB: Color32 = Color32::from_rgb(0x33, 0x39, 0x44);
pub const SCROLL_THUMB_HOT: Color32 = Color32::from_rgb(0x4a, 0x52, 0x60);

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL_BG;
    v.window_fill = PANEL_BG_2;
    v.extreme_bg_color = Color32::from_rgb(0x12, 0x14, 0x18);
    v.faint_bg_color = PANEL_BG_2;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;

    let r = CornerRadius::same(6);
    v.widgets.noninteractive.bg_fill = PANEL_BG;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    v.widgets.noninteractive.corner_radius = r;

    v.widgets.inactive.bg_fill = Color32::from_rgb(0x26, 0x2a, 0x33);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x23, 0x27, 0x2f);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = r;

    v.widgets.hovered.bg_fill = Color32::from_rgb(0x30, 0x35, 0x40);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2d, 0x32, 0x3c);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x40, 0x4c));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.hovered.corner_radius = r;

    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.active.corner_radius = r;

    v.widgets.open.bg_fill = Color32::from_rgb(0x2a, 0x2f, 0x39);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(0x2a, 0x2f, 0x39);
    v.widgets.open.corner_radius = r;

    // Always dark, whatever the OS theme is.
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(14);
        for font in s.text_styles.values_mut() {
            if font.family == egui::FontFamily::Proportional {
                font.size = (font.size * 1.05).round();
            }
        }
    });
}
