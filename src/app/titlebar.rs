//! Custom window chrome: title bar with menus, window controls, drag-to-move,
//! double-click to maximise and edge/corner resizing for the frameless window.

use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Id, Layout, Order, PointerButton, Rect,
    ResizeDirection, RichText, Sense, Stroke, Ui, UiBuilder, ViewportCommand, pos2, vec2,
};

use super::MammothApp;
use super::commands::Cmd;
use crate::editor::Command;
use crate::fonts::Role;
use crate::icons::{self, Icon};
use crate::theme;

pub(super) const TITLE_BAR_HEIGHT: f32 = 40.0;

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
    })
}

impl MammothApp {
    pub(super) fn title_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let bar = ui.max_rect();
        let maximized = is_maximized(ctx);

        // The whole bar is a drag handle; widgets added afterwards sit on top of it.
        let drag = ui.interact(bar, Id::new("title-bar-drag"), Sense::click_and_drag());
        if drag.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }

        // Left: logo + menus.
        let mut chosen: Option<Cmd> = None;
        // (MenuBar claims the full width, so measure where the last menu ends instead.)
        let left_end = ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(bar)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.add_space(12.0);
                    let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    self.logo.paint(ui, r);
                    ui.add_space(6.0);
                    ui.spacing_mut().button_padding = vec2(9.0, 5.0);
                    egui::MenuBar::new()
                        .ui(ui, |ui| self.menus(ui, &mut chosen))
                        .inner
                },
            )
            .inner;

        // Right: window controls, then app buttons.
        let right = ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(bar)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if control(ui, Control::Close).clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                    let (kind, tip) = if maximized {
                        (Control::Restore, "Restore")
                    } else {
                        (Control::Maximize, "Maximize")
                    };
                    if control(ui, kind).on_hover_text(tip).clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                    }
                    if control(ui, Control::Minimize)
                        .on_hover_text("Minimize")
                        .clicked()
                    {
                        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                    }
                    ui.add_space(10.0);
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let active = self.registry.detectors().filter(|(_, e)| e.enabled).count();
                    let r = bar_button(
                        ui,
                        Icon::Modules,
                        self.modules_open,
                        "Modules (Ctrl+Shift+M)",
                    );
                    if active > 0 {
                        badge(ui, r.rect, active);
                    }
                    self.modules_button_rect = r.rect;
                    if r.clicked() {
                        chosen = Some(Cmd::ToggleModules);
                    }
                    if bar_button(
                        ui,
                        Icon::Chart,
                        self.insights.open,
                        "Insights: email providers, domains… (Ctrl+Shift+E)",
                    )
                    .clicked()
                    {
                        chosen = Some(Cmd::ToggleInsights);
                    }
                    if bar_button(
                        ui,
                        Icon::Search,
                        self.palette.is_some(),
                        "Command palette (Ctrl+Shift+P)",
                    )
                    .clicked()
                    {
                        chosen = Some(Cmd::Palette);
                    }
                },
            )
            .response
            .rect;

        // Centre: document title, if there's room between menus and buttons.
        let title = match self.tabs.get(self.active) {
            Some(t) => format!(
                "{}{}",
                t.doc.title,
                if t.doc.is_dirty() { "  •" } else { "" }
            ),
            None => "Mammoth".into(),
        };
        let galley =
            ui.painter()
                .layout_no_wrap(title, FontId::proportional(13.0), theme::TEXT_DIM);
        let gap = Rect::from_min_max(
            pos2(left_end + 16.0, bar.top()),
            pos2(right.left() - 16.0, bar.bottom()),
        );
        if gap.width() > galley.size().x {
            let x = bar.center().x.clamp(
                gap.left() + galley.size().x / 2.0,
                gap.right() - galley.size().x / 2.0,
            );
            let pos = Align2::CENTER_CENTER
                .anchor_size(pos2(x, bar.center().y), galley.size())
                .min;
            ui.painter().galley(pos, galley, theme::TEXT_DIM);
        }

        ui.painter().hline(
            bar.x_range(),
            bar.bottom() - 0.5,
            Stroke::new(1.0, theme::BORDER),
        );
        if let Some(c) = chosen {
            self.run(c, ctx);
        }
    }

    /// The menus; returns the x where the last one ends.
    fn menus(&self, ui: &mut Ui, out: &mut Option<Cmd>) -> f32 {
        ui.menu_button(menu_title("File"), |ui| {
            self.menu_item(ui, Cmd::NewFile, out);
            self.menu_item(ui, Cmd::Open, out);
            ui.menu_button("Open recent", |ui| {
                if self.settings.recent.is_empty() {
                    ui.label(RichText::new("Nothing yet").color(theme::TEXT_DIM));
                }
                for p in &self.settings.recent {
                    if ui.button(p.display().to_string()).clicked() {
                        *out = Some(Cmd::OpenRecent(p.clone()));
                        ui.close();
                    }
                }
            });
            ui.separator();
            self.menu_item(ui, Cmd::Save, out);
            self.menu_item(ui, Cmd::SaveAs, out);
            ui.separator();
            self.menu_item(ui, Cmd::LoadEntire, out);
            self.menu_item(ui, Cmd::Convert(None), out);
            ui.separator();
            self.menu_item(ui, Cmd::CloseTab, out);
            self.menu_item(ui, Cmd::Exit, out);
        });
        ui.menu_button(menu_title("Edit"), |ui| {
            for c in [Command::Undo, Command::Redo] {
                self.menu_item(ui, Cmd::Edit(c), out);
            }
            ui.separator();
            for c in [Command::Cut, Command::Copy, Command::SelectAll] {
                self.menu_item(ui, Cmd::Edit(c), out);
            }
            ui.separator();
            for c in [
                Command::DuplicateLine,
                Command::DeleteLine,
                Command::MoveLineUp,
                Command::MoveLineDown,
                Command::Indent,
                Command::Outdent,
            ] {
                self.menu_item(ui, Cmd::Edit(c), out);
            }
        });
        ui.menu_button(menu_title("Search"), |ui| {
            for c in [Cmd::Find, Cmd::Replace, Cmd::FindNext, Cmd::FindPrev] {
                self.menu_item(ui, c, out);
            }
            ui.separator();
            self.menu_item(ui, Cmd::CountMatches, out);
            self.menu_item(ui, Cmd::ExtractMatches, out);
            self.menu_item(ui, Cmd::FilterLines, out);
            ui.separator();
            self.menu_item(ui, Cmd::RunJq, out);
            self.menu_item(ui, Cmd::JsonPretty, out);
            self.menu_item(ui, Cmd::JsonMinify, out);
            ui.separator();
            self.menu_item(ui, Cmd::GoToLine, out);
        });
        ui.menu_button(menu_title("View"), |ui| {
            self.menu_item(ui, Cmd::Palette, out);
            self.menu_item(ui, Cmd::ToggleInsights, out);
            self.menu_item(ui, Cmd::ToggleTable, out);
            self.menu_item(ui, Cmd::ToggleJson, out);
            self.menu_item(ui, Cmd::ToggleHeatmap, out);
            self.menu_item(ui, Cmd::ToggleModules, out);
            self.menu_item(ui, Cmd::PinModules, out);
            ui.separator();
            self.menu_item(ui, Cmd::ChooseFont(Role::Editor), out);
            self.menu_item(ui, Cmd::ChooseFont(Role::Interface), out);
            self.menu_item(ui, Cmd::ToggleLineHighlight, out);
            ui.separator();
            for c in [Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset] {
                self.menu_item(ui, c, out);
            }
            ui.separator();
            self.menu_item(ui, Cmd::Settings, out);
        });
        let help = ui.menu_button(menu_title("Help"), |ui| {
            self.menu_item(ui, Cmd::Shortcuts, out);
            self.menu_item(ui, Cmd::Palette, out);
        });
        help.response.rect.right()
    }
}

fn menu_title(s: &str) -> RichText {
    RichText::new(s).size(13.5).color(theme::TEXT)
}

/// Minimise / maximise / close: full-height, square-ish buttons like Windows 11.
fn control(ui: &mut Ui, kind: Control) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(46.0, TITLE_BAR_HEIGHT), Sense::click());
    let hot = resp.hovered();
    let down = resp.is_pointer_button_down_on();
    let p = ui.painter();
    let bg = match (kind, hot, down) {
        (Control::Close, _, true) => Color32::from_rgb(0xb3, 0x1b, 0x1b),
        (Control::Close, true, _) => Color32::from_rgb(0xe8, 0x11, 0x23),
        (_, _, true) => Color32::from_white_alpha(20),
        (_, true, _) => Color32::from_white_alpha(12),
        _ => Color32::TRANSPARENT,
    };
    p.rect_filled(rect, 0.0, bg);
    let c = rect.center();
    let color = if hot { Color32::WHITE } else { theme::TEXT };
    let st = Stroke::new(1.0, color);
    let s = 5.0;
    match kind {
        Control::Minimize => {
            p.hline(c.x - s..=c.x + s, c.y, st);
        }
        Control::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, vec2(2.0 * s, 2.0 * s)),
                1.0,
                st,
                egui::StrokeKind::Middle,
            );
        }
        Control::Restore => {
            let back =
                Rect::from_center_size(c + vec2(1.5, -1.5), vec2(2.0 * s - 1.0, 2.0 * s - 1.0));
            let front =
                Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(2.0 * s - 1.0, 2.0 * s - 1.0));
            p.rect_stroke(back, 1.0, st, egui::StrokeKind::Middle);
            p.rect_filled(
                front,
                1.0,
                if bg == Color32::TRANSPARENT {
                    theme::APP_BG
                } else {
                    bg
                },
            );
            p.rect_stroke(front, 1.0, st, egui::StrokeKind::Middle);
        }
        Control::Close => {
            p.line_segment([c - vec2(s, s), c + vec2(s, s)], st);
            p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], st);
        }
    }
    resp
}

/// A rounded icon button for the title bar; `on` draws it highlighted.
fn bar_button(ui: &mut Ui, icon: Icon, on: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
    let hot = resp.hovered();
    let fill = if on {
        theme::ACCENT_DIM
    } else if hot {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    let color = if on || hot {
        Color32::WHITE
    } else {
        theme::TEXT_DIM
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(15.0, 15.0)),
        icon,
        color,
    );
    resp.on_hover_text(tip)
}

fn badge(ui: &Ui, button: Rect, n: usize) {
    let c = pos2(button.right() - 5.0, button.top() + 5.0);
    ui.painter().circle_filled(c, 7.0, theme::ACCENT);
    ui.painter().text(
        c,
        Align2::CENTER_CENTER,
        n.min(9).to_string(),
        FontId::proportional(9.5),
        Color32::WHITE,
    );
}

/// Invisible handles along the window edges for resizing the frameless window.
pub(super) fn resize_handles(ctx: &egui::Context) {
    if is_maximized(ctx) {
        return;
    }
    let r = ctx.content_rect();
    let (m, c) = (5.0, 12.0);
    use ResizeDirection::*;
    // Corners last so they win where they overlap the edges.
    let zones = [
        (
            Rect::from_min_max(r.left_top(), pos2(r.right(), r.top() + m)),
            North,
        ),
        (
            Rect::from_min_max(pos2(r.left(), r.bottom() - m), r.right_bottom()),
            South,
        ),
        (
            Rect::from_min_max(r.left_top(), pos2(r.left() + m, r.bottom())),
            West,
        ),
        (
            Rect::from_min_max(pos2(r.right() - m, r.top()), r.right_bottom()),
            East,
        ),
        (Rect::from_min_size(r.left_top(), vec2(c, c)), NorthWest),
        (
            Rect::from_min_max(pos2(r.right() - c, r.top()), pos2(r.right(), r.top() + c)),
            NorthEast,
        ),
        (
            Rect::from_min_max(
                pos2(r.left(), r.bottom() - c),
                pos2(r.left() + c, r.bottom()),
            ),
            SouthWest,
        ),
        (
            Rect::from_min_max(r.right_bottom() - vec2(c, c), r.right_bottom()),
            SouthEast,
        ),
    ];
    egui::Area::new(Id::new("resize-handles"))
        .order(Order::Foreground)
        .fixed_pos(r.min)
        .interactable(true)
        .show(ctx, |ui| {
            for (i, (zone, dir)) in zones.into_iter().enumerate() {
                let resp = ui.interact(zone, Id::new(("resize", i)), Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ctx.set_cursor_icon(match dir {
                        North | South => CursorIcon::ResizeVertical,
                        East | West => CursorIcon::ResizeHorizontal,
                        NorthWest | SouthEast => CursorIcon::ResizeNwSe,
                        NorthEast | SouthWest => CursorIcon::ResizeNeSw,
                    });
                }
                if resp.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_pressed()) {
                    ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            }
        });
}

/// A hairline border so the frameless window has a crisp edge.
pub(super) fn window_border(ctx: &egui::Context) {
    if is_maximized(ctx) {
        return;
    }
    let p = ctx.layer_painter(egui::LayerId::new(
        Order::Foreground,
        Id::new("window-border"),
    ));
    p.rect_stroke(
        ctx.content_rect(),
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );
}
