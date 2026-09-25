//! Tiny vector icons (the bundled fonts lack most symbol glyphs).

use egui::{
    Color32, ColorImage, Painter, Pos2, Rect, Response, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, pos2, vec2,
};

use crate::theme;

#[derive(Clone, Copy)]
pub enum Icon {
    Up,
    Down,
    ChevronRight,
    ChevronDown,
    Close,
    Reload,
    Modules,
    Search,
    Chart,
    Pin,
}

pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) / 2.0;
    let st = Stroke::new(1.6, color);
    let at = |x: f32, y: f32| pos2(c.x + x * s, c.y + y * s);
    match icon {
        Icon::Up => {
            p.line(vec![at(-0.7, 0.35), at(0.0, -0.4), at(0.7, 0.35)], st);
        }
        Icon::Down => {
            p.line(vec![at(-0.7, -0.35), at(0.0, 0.4), at(0.7, -0.35)], st);
        }
        Icon::ChevronRight => {
            p.line(vec![at(-0.3, -0.65), at(0.35, 0.0), at(-0.3, 0.65)], st);
        }
        Icon::ChevronDown => {
            p.line(vec![at(-0.65, -0.3), at(0.0, 0.35), at(0.65, -0.3)], st);
        }
        Icon::Close => {
            p.line_segment([at(-0.6, -0.6), at(0.6, 0.6)], st);
            p.line_segment([at(-0.6, 0.6), at(0.6, -0.6)], st);
        }
        Icon::Reload => {
            let r = 0.72;
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -0.35 + i as f32 / 20.0 * 5.0;
                    at(r * a.cos(), r * a.sin())
                })
                .collect();
            let end = *pts.last().unwrap();
            p.line(pts, st);
            // Arrow head at the end of the arc.
            let a = -0.35 + 5.0_f32;
            let tangent = vec2(-a.sin(), a.cos());
            let normal = vec2(a.cos(), a.sin());
            let h = s * 0.45;
            p.line(
                vec![
                    end - tangent * h + normal * h * 0.6,
                    end,
                    end - tangent * h - normal * h * 0.6,
                ],
                st,
            );
        }
        Icon::Modules => {
            // Four rounded tiles, the top-right one "plugged in" (accent).
            let t = s * 0.78;
            let g = s * 0.22;
            for (i, (dx, dy)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
                .into_iter()
                .enumerate()
            {
                let r = Rect::from_center_size(c + vec2(dx, dy) * (t / 2.0 + g / 2.0), vec2(t, t));
                if i == 1 {
                    p.rect_filled(r, 2.0, color);
                } else {
                    p.rect_stroke(r, 2.0, st, egui::StrokeKind::Inside);
                }
            }
        }
        Icon::Chart => {
            // Three bars of different heights on a baseline.
            let base = c.y + s * 0.8;
            for (dx, h) in [(-0.6, 0.7), (0.0, 1.5), (0.6, 1.05)] {
                let x = c.x + dx * s;
                p.rect_filled(
                    Rect::from_min_max(pos2(x - s * 0.2, base - h * s), pos2(x + s * 0.2, base)),
                    1.0,
                    color,
                );
            }
        }
        Icon::Search => {
            p.circle_stroke(at(-0.18, -0.18), s * 0.58, st);
            p.line_segment([at(0.25, 0.25), at(0.8, 0.8)], Stroke::new(2.0, color));
        }
        Icon::Pin => {
            p.line(vec![at(-0.5, -0.8), at(0.5, -0.8)], st);
            p.line(
                vec![
                    at(-0.3, -0.8),
                    at(-0.3, -0.1),
                    at(-0.6, 0.25),
                    at(0.6, 0.25),
                    at(0.3, -0.1),
                    at(0.3, -0.8),
                ],
                st,
            );
            p.line_segment([at(0.0, 0.25), at(0.0, 0.95)], st);
        }
    }
}

pub fn button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(rect, 5.0, theme::BORDER);
    }
    paint(
        ui.painter(),
        rect.shrink(6.0),
        icon,
        if hot { Color32::WHITE } else { theme::TEXT_DIM },
    );
    resp.on_hover_text(tip)
}

/// The app logo as crisp textures, rendered on demand for each on-screen pixel size.
#[derive(Default)]
pub struct Logo {
    textures: Vec<(usize, TextureHandle)>,
}

impl Logo {
    pub fn paint(&mut self, ui: &Ui, rect: Rect) {
        let px = (rect.width() * ui.ctx().pixels_per_point())
            .round()
            .clamp(8.0, 512.0) as usize;
        let tex = match self.textures.iter().find(|(s, _)| *s == px) {
            Some((_, t)) => t.clone(),
            None => {
                let img = ColorImage::from_rgba_unmultiplied([px, px], &crate::logo::render(px));
                let t = ui
                    .ctx()
                    .load_texture(format!("logo-{px}"), img, TextureOptions::LINEAR);
                self.textures.push((px, t.clone()));
                t
            }
        };
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
    }
}
