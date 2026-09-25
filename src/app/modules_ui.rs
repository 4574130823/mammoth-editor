//! Modules UI: a drawer that floats over the editor (or docks when pinned), plus
//! the status-bar chips for active detectors.

use egui::{
    Align, Color32, FontId, Frame, Id, Layout, Margin, Order, Popup, Rect, RichText, ScrollArea,
    Sense, Shadow, Stroke, TextEdit, Ui, pos2, vec2,
};

use super::commands::Cmd;
use super::{MammothApp, ModuleAction, toggle};
use crate::editor::fmt_int;
use crate::icons::{self, Icon};
use crate::modules;
use crate::theme;

const DRAWER_WIDTH: f32 = 340.0;

impl MammothApp {
    /// Floating drawer over the editor area (`area`), when open and not pinned.
    pub(super) fn modules_drawer(&mut self, ctx: &egui::Context, area: Rect) {
        let show = self.modules_open && !self.settings.modules_pinned;
        let t = ctx.animate_bool_with_time(Id::new("modules-drawer-anim"), show, 0.16);
        if t <= 0.0 {
            return;
        }
        let x = area.right() - DRAWER_WIDTH - 10.0 + (1.0 - t) * (DRAWER_WIDTH + 24.0);
        let rect = Rect::from_min_size(
            pos2(x, area.top() + 10.0),
            vec2(DRAWER_WIDTH, (area.height() - 20.0).max(200.0)),
        );
        let mut cmd = None;
        let resp = egui::Area::new(Id::new("modules-drawer"))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                Frame::new()
                    .fill(theme::PANEL_BG)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(14.0)
                    .shadow(Shadow {
                        offset: [0, 12],
                        blur: 32,
                        spread: 0,
                        color: Color32::from_black_alpha(150),
                    })
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(DRAWER_WIDTH - 32.0);
                        ui.set_height(rect.height() - 32.0);
                        cmd = self.modules_content(ui);
                    });
            });

        // Click on the editor (outside the drawer) or Esc closes it.
        if show {
            let pressed_at = ctx.input(|i| {
                i.pointer
                    .primary_pressed()
                    .then(|| i.pointer.interact_pos())
                    .flatten()
            });
            if let Some(p) = pressed_at {
                let on_background = ctx
                    .layer_id_at(p)
                    .is_none_or(|l| l.order == Order::Background);
                if on_background
                    && !resp.response.rect.contains(p)
                    && !self.modules_button_rect.contains(p)
                {
                    self.modules_open = false;
                }
            }
            if !ctx.any_popup_open()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.modules_open = false;
            }
        }
        if let Some(c) = cmd {
            self.run(c, ctx);
        }
    }

    /// Docked version of the drawer.
    pub(super) fn modules_side_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let cmd = self.modules_content(ui);
        if let Some(c) = cmd {
            self.run(c, ctx);
        }
    }

    fn modules_content(&mut self, ui: &mut Ui) -> Option<Cmd> {
        let mut cmd = None;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Modules, theme::ACCENT);
            ui.label(
                RichText::new("Modules")
                    .size(17.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icons::button(ui, Icon::Close, "Close (Esc)").clicked() {
                    cmd = Some(Cmd::ToggleModules);
                }
                let pinned = self.settings.modules_pinned;
                let pin_tip = if pinned {
                    "Unpin: float over the editor"
                } else {
                    "Pin to the side"
                };
                let pin = icons::button(ui, Icon::Pin, pin_tip);
                if pinned {
                    ui.painter().rect_stroke(
                        pin.rect,
                        5.0,
                        Stroke::new(1.0, theme::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                }
                if pin.clicked() {
                    cmd = Some(Cmd::PinModules);
                }
                if icons::button(ui, Icon::Reload, "Reload modules from disk").clicked() {
                    cmd = Some(Cmd::ReloadModules);
                }
            });
        });
        ui.label(
            RichText::new("Highlight, find, count, extract and mask patterns in any file.")
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
        ui.add_space(8.0);
        ui.add(
            TextEdit::singleline(&mut self.module_filter)
                .hint_text("Filter modules…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(10.0);

        let filter = self.module_filter.to_lowercase();
        let matches = |e: &modules::ModuleEntry| {
            filter.is_empty()
                || e.module.name().to_lowercase().contains(&filter)
                || e.module.description().to_lowercase().contains(&filter)
        };
        let builtin: Vec<usize> = self
            .registry
            .detectors()
            .filter(|(_, e)| e.origin.is_none() && matches(e))
            .map(|(i, _)| i)
            .collect();
        let user: Vec<usize> = self
            .registry
            .detectors()
            .filter(|(_, e)| e.origin.is_some() && matches(e))
            .map(|(i, _)| i)
            .collect();
        let on = self.registry.detectors().filter(|(_, e)| e.enabled).count();

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                section_label(ui, &format!("DETECTORS · {on} ON"));
                if on > 0 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if link(ui, "Turn all off").clicked() {
                            for e in &mut self.registry.entries {
                                if e.is_detector() {
                                    e.enabled = false;
                                }
                            }
                        }
                    });
                }
            });
            for i in builtin {
                if let Some(c) = self.detector_card(ui, i) {
                    cmd = Some(c);
                }
            }

            ui.add_space(8.0);
            section_label(ui, "YOUR MODULES");
            for i in &user {
                if let Some(c) = self.detector_card(ui, *i) {
                    cmd = Some(c);
                }
            }
            if user.is_empty() && filter.is_empty() {
                Frame::new()
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(10.0)
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Make your own detector").strong().color(theme::TEXT));
                        ui.label(
                            RichText::new(
                                "A module is a tiny .toml file: a name, a regex and a colour. It gets \
                                 highlighting, Find, Count, Extract and Mask automatically.",
                            )
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("New module").clicked() {
                                cmd = Some(Cmd::CreateModule);
                            }
                            if ui.button("Open folder").clicked() {
                                cmd = Some(Cmd::OpenModulesFolder);
                            }
                        });
                    });
            } else if !user.is_empty() {
                ui.horizontal(|ui| {
                    if link(ui, "+ New module").clicked() {
                        cmd = Some(Cmd::CreateModule);
                    }
                    if link(ui, "Open folder").clicked() {
                        cmd = Some(Cmd::OpenModulesFolder);
                    }
                });
            }
            for err in &self.registry.load_errors {
                ui.add_space(4.0);
                ui.label(RichText::new(err).size(11.0).color(theme::ERROR));
            }
        });
        cmd
    }

    fn detector_card(&mut self, ui: &mut Ui, i: usize) -> Option<Cmd> {
        let mut cmd = None;
        let has_tab = !self.tabs.is_empty();
        let e = &mut self.registry.entries[i];
        let color = e.module.color();
        let fill = if e.enabled {
            theme::PANEL_BG_2
        } else {
            Color32::TRANSPARENT
        };
        let stroke = if e.enabled {
            Stroke::new(1.0, color.gamma_multiply(0.35))
        } else {
            Stroke::new(1.0, theme::BORDER)
        };
        Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(10.0)
            .inner_margin(Margin {
                left: 12,
                right: 10,
                top: 9,
                bottom: 9,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.5, color);
                    let name_color = if e.enabled {
                        theme::TEXT
                    } else {
                        theme::TEXT_DIM
                    };
                    ui.label(RichText::new(e.module.name()).strong().color(name_color));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if e.error.is_none() {
                            toggle(ui, &mut e.enabled)
                                .on_hover_text("Highlight matches in the editor");
                        }
                    });
                });
                if !e.module.description().is_empty() {
                    ui.label(
                        RichText::new(e.module.description())
                            .size(11.5)
                            .color(theme::TEXT_DIM),
                    );
                }
                if let Some(path) = &e.origin {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(RichText::new(name).size(10.5).color(theme::ACCENT))
                        .on_hover_text(path.display().to_string());
                }
                if let Some(err) = &e.error {
                    ui.label(RichText::new(err).size(11.0).color(theme::ERROR));
                    return;
                }
                ui.add_space(2.0);
                ui.add_enabled_ui(has_tab, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let id = e.module.id().to_string();
                        if let Some(g) = e.grouper() {
                            let by = g.category_label.unwrap_or(g.label).to_lowercase();
                            if link(ui, "Breakdown")
                                .on_hover_text(format!("Count matches by {by}"))
                                .clicked()
                            {
                                cmd = Some(Cmd::Module(ModuleAction::Breakdown(id.clone())));
                            }
                        }
                        for (label, tip, action) in [
                            (
                                "Find",
                                "Jump to the next match",
                                ModuleAction::Find(id.clone()),
                            ),
                            (
                                "Count",
                                "Count matches in the whole file",
                                ModuleAction::Count(id.clone()),
                            ),
                            (
                                "Extract",
                                "Copy every match into a new tab",
                                ModuleAction::Extract(id.clone()),
                            ),
                            (
                                "Mask",
                                "Replace every match, e.g. to redact it",
                                ModuleAction::Mask(id.clone()),
                            ),
                        ] {
                            if link(ui, label).on_hover_text(tip).clicked() {
                                cmd = Some(Cmd::Module(action));
                            }
                        }
                    });
                });
            });
        ui.add_space(6.0);
        cmd
    }

    /// Pills in the status bar for each active detector, with its count.
    /// Click: open its insights (providers, domains, …). Right-click: more actions.
    pub(super) fn detector_chips(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut cmd = None;
        let active: Vec<(String, String, Color32, bool)> = self
            .registry
            .detectors()
            .filter(|(_, e)| e.enabled)
            .map(|(_, e)| {
                (
                    e.module.id().to_string(),
                    e.module.name().to_string(),
                    e.module.color(),
                    e.grouper().is_some(),
                )
            })
            .collect();
        for (id, name, color, has_insights) in active {
            let count = self
                .tabs
                .get(self.active)
                .and_then(|t| t.heat.total_for(&name))
                .or_else(|| self.insights_total(&id));
            let name_g = ui.painter().layout_no_wrap(
                name.clone(),
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
            let count_g = count.map(|n| {
                ui.painter().layout_no_wrap(
                    fmt_int(n as usize),
                    FontId::proportional(12.0),
                    Color32::WHITE,
                )
            });
            let count_w = count_g.as_ref().map_or(0.0, |g| g.size().x + 6.0);
            let (rect, resp) = ui
                .allocate_exact_size(vec2(name_g.size().x + count_w + 30.0, 20.0), Sense::click());
            let open_here = has_insights && self.insights.open && self.insights.module_id == id;
            let fill = if open_here {
                theme::ACCENT_DIM
            } else if resp.hovered() {
                theme::BORDER
            } else {
                theme::PANEL_BG_2
            };
            ui.painter().rect_filled(rect, 10.0, fill);
            ui.painter()
                .circle_filled(pos2(rect.left() + 10.0, rect.center().y), 3.5, color);
            let y = rect.center().y - name_g.size().y / 2.0;
            let name_w = name_g.size().x;
            ui.painter()
                .galley(pos2(rect.left() + 18.0, y), name_g, theme::TEXT_DIM);
            if let Some(g) = count_g {
                ui.painter().galley(
                    pos2(rect.left() + 18.0 + name_w + 6.0, y),
                    g,
                    Color32::WHITE,
                );
            }
            let tip = if has_insights {
                format!(
                    "{name}: click for providers, domains and top values · right-click for more"
                )
            } else {
                format!("{name}: click for actions")
            };
            let resp = resp.on_hover_text(tip);
            let menu = |ui: &mut Ui, cmd: &mut Option<Cmd>| {
                ui.set_min_width(200.0);
                ui.label(RichText::new(&name).strong().color(theme::TEXT));
                ui.separator();
                if has_insights && ui.button("Insights").clicked() {
                    *cmd = Some(Cmd::Module(ModuleAction::Breakdown(id.clone())));
                }
                for (label, action) in [
                    ("Find next", ModuleAction::Find(id.clone())),
                    ("Count in file", ModuleAction::Count(id.clone())),
                    ("Extract to new tab", ModuleAction::Extract(id.clone())),
                    ("Mask all…", ModuleAction::Mask(id.clone())),
                ] {
                    if ui.button(label).clicked() {
                        *cmd = Some(Cmd::Module(action));
                    }
                }
                ui.separator();
                if ui.button("Turn off highlighting").clicked() {
                    *cmd = Some(Cmd::ToggleDetector(id.clone()));
                }
                if ui.button("All modules…").clicked() {
                    *cmd = Some(Cmd::ToggleModules);
                }
            };
            if has_insights {
                if resp.clicked() {
                    cmd = Some(if open_here {
                        Cmd::ToggleInsights
                    } else {
                        Cmd::Module(ModuleAction::Breakdown(id.clone()))
                    });
                }
                Popup::context_menu(&resp).show(|ui| menu(ui, &mut cmd));
            } else {
                Popup::menu(&resp).show(|ui| menu(ui, &mut cmd));
            }
        }
        if let Some(c) = cmd {
            self.run(c, ctx);
        }
    }

    pub(super) fn reload_modules(&mut self) {
        self.registry.reload();
        self.find.cache = None;
        let n = self.registry.entries.len();
        if self.registry.load_errors.is_empty() {
            self.toast_ok(format!("Reloaded {n} modules."));
        } else {
            self.toast_err(format!(
                "Reloaded with {} error(s) — see the Modules panel.",
                self.registry.load_errors.len()
            ));
        }
    }

    pub(super) fn create_module(&mut self, ctx: &egui::Context) {
        match super::create_example_module() {
            Ok(path) => {
                self.registry.reload();
                self.find.cache = None;
                self.open_path(path, ctx);
                self.toast_info(
                    "Edit the pattern, save (Ctrl+S), then press reload in the Modules panel.",
                );
            }
            Err(e) => self.toast_err(format!("Could not create a module: {e}")),
        }
    }
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .strong()
            .color(theme::LINENO_ACTIVE),
    );
}

/// Frameless text button that lights up on hover.
fn link(ui: &mut Ui, text: &str) -> egui::Response {
    let font = FontId::proportional(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, theme::TEXT_DIM);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(0.0, 4.0), Sense::click());
    let color = if !ui.is_enabled() {
        theme::LINENO
    } else if resp.hovered() {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    ui.painter()
        .galley_with_override_text_color(rect.left_top() + vec2(0.0, 2.0), galley, color);
    resp
}
