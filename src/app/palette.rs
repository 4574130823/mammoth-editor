//! Command palette (Ctrl+Shift+P): fuzzy-search every action, module and setting.

use egui::{
    Align2, Color32, EventFilter, FontId, Frame, Id, Key, Margin, Modifiers, Order, Rect, RichText,
    ScrollArea, Sense, Shadow, Stroke, TextEdit, pos2, vec2,
};

use super::MammothApp;
use super::commands::{Cmd, fuzzy_score};
use super::titlebar::TITLE_BAR_HEIGHT;
use crate::icons::{self, Icon};
use crate::theme;

pub(super) struct PaletteState {
    pub query: String,
    pub selected: usize,
    focus: bool,
    scroll_to_selected: bool,
}

impl MammothApp {
    pub(super) fn open_palette(&mut self) {
        self.palette = match self.palette {
            Some(_) => None,
            None => Some(PaletteState {
                query: String::new(),
                selected: 0,
                focus: true,
                scroll_to_selected: false,
            }),
        };
    }

    /// Commands matching the current query, best first.
    pub(super) fn palette_items(&self, query: &str) -> Vec<(Cmd, String)> {
        let mut scored: Vec<(i32, usize, Cmd, String)> = self
            .all_commands()
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let label = self.cmd_label(&c);
                let score = fuzzy_score(query, &label).or_else(|| {
                    fuzzy_score(query, &format!("{} {label}", c.category())).map(|s| s - 20)
                })?;
                Some((score, i, c, label))
            })
            .collect();
        if !query.trim().is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        scored
            .into_iter()
            .take(200)
            .map(|(_, _, c, l)| (c, l))
            .collect()
    }

    pub(super) fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.palette.take() else {
            return;
        };
        let items = self.palette_items(&st.query);

        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if !items.is_empty() {
            if up {
                st.selected = (st.selected + items.len() - 1) % items.len();
                st.scroll_to_selected = true;
            }
            if down {
                st.selected = (st.selected + 1) % items.len();
                st.scroll_to_selected = true;
            }
        }
        st.selected = st.selected.min(items.len().saturating_sub(1));

        let screen = ctx.content_rect();
        let w = 600.0_f32.min(screen.width() - 40.0);
        let pos = pos2(
            screen.center().x - w / 2.0,
            screen.top() + TITLE_BAR_HEIGHT + 14.0,
        );
        let mut run: Option<Cmd> = if enter {
            items.get(st.selected).map(|(c, _)| c.clone())
        } else {
            None
        };
        let mut close = esc || enter;

        let resp = egui::Area::new(Id::new("command-palette"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(theme::PANEL_BG)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(14.0)
                    .shadow(Shadow {
                        offset: [0, 16],
                        blur: 40,
                        spread: 0,
                        color: Color32::from_black_alpha(160),
                    })
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(18.0, 30.0), Sense::hover());
                            icons::paint(
                                ui.painter(),
                                Rect::from_center_size(r.center(), vec2(15.0, 15.0)),
                                Icon::Search,
                                theme::TEXT_DIM,
                            );
                            let te = TextEdit::singleline(&mut st.query)
                                .hint_text("Type a command — try “email”, “font”, “csv”, “load”…")
                                .font(FontId::proportional(16.0))
                                .frame(Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .event_filter(EventFilter {
                                    vertical_arrows: true,
                                    escape: true,
                                    ..Default::default()
                                });
                            let r = ui.add(te);
                            if st.focus {
                                r.request_focus();
                                st.focus = false;
                            }
                            if r.changed() {
                                st.selected = 0;
                            }
                        });
                        ui.add_space(4.0);
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            ui.cursor().top(),
                            Stroke::new(1.0, theme::BORDER),
                        );
                        ui.add_space(6.0);
                        if items.is_empty() {
                            ui.add_space(8.0);
                            ui.label(RichText::new("No matching commands").color(theme::TEXT_DIM));
                            ui.add_space(8.0);
                            return;
                        }
                        ScrollArea::vertical()
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (i, (cmd, label)) in items.iter().enumerate() {
                                    let (rect, row) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 34.0),
                                        Sense::click(),
                                    );
                                    let selected = i == st.selected;
                                    if row.hovered()
                                        && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                                    {
                                        st.selected = i;
                                    }
                                    let fill = if selected {
                                        theme::ACCENT_DIM
                                    } else if row.hovered() {
                                        theme::PANEL_BG_2
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    ui.painter().rect_filled(rect, 8.0, fill);
                                    let text_color = if selected {
                                        Color32::WHITE
                                    } else {
                                        theme::TEXT
                                    };
                                    let cat = cmd.category();
                                    let cat_g = ui.painter().layout_no_wrap(
                                        cat.to_string(),
                                        FontId::proportional(11.0),
                                        theme::TEXT_DIM,
                                    );
                                    let cat_rect = Rect::from_min_size(
                                        pos2(rect.left() + 10.0, rect.center().y - 9.0),
                                        vec2(62.0, 18.0),
                                    );
                                    ui.painter().galley(
                                        pos2(
                                            cat_rect.left(),
                                            rect.center().y - cat_g.size().y / 2.0,
                                        ),
                                        cat_g,
                                        theme::TEXT_DIM,
                                    );
                                    ui.painter().text(
                                        pos2(rect.left() + 80.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        label,
                                        FontId::proportional(14.0),
                                        text_color,
                                    );
                                    let sc = cmd.shortcut();
                                    if !sc.is_empty() {
                                        let g = ui.painter().layout_no_wrap(
                                            sc.to_string(),
                                            FontId::monospace(11.5),
                                            theme::TEXT_DIM,
                                        );
                                        let r = Align2::RIGHT_CENTER.anchor_size(
                                            pos2(rect.right() - 10.0, rect.center().y),
                                            g.size() + vec2(10.0, 4.0),
                                        );
                                        ui.painter().rect_filled(r, 5.0, theme::BORDER);
                                        ui.painter().galley(
                                            r.min + vec2(5.0, 2.0),
                                            g,
                                            theme::TEXT_DIM,
                                        );
                                    }
                                    if selected && st.scroll_to_selected {
                                        row.scroll_to_me(None);
                                    }
                                    if row.clicked() {
                                        run = Some(cmd.clone());
                                        close = true;
                                    }
                                }
                            });
                        st.scroll_to_selected = false;
                    });
            });

        // Clicking anywhere else closes the palette.
        let pressed_at = ctx.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if pressed_at.is_some_and(|p| !resp.response.rect.contains(p)) {
            close = true;
        }
        if !close {
            self.palette = Some(st);
        }
        if let Some(c) = run {
            self.run(c, ctx);
        }
    }
}
