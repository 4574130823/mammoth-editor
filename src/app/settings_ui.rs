//! Settings window and the font picker.

use std::sync::mpsc;

use egui::{
    Align, Color32, FontId, Frame, Id, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, vec2,
};

use super::MammothApp;
use crate::fonts::{self, FontChoice, FontInfo, Role};
use crate::theme;

pub(super) struct FontPicker {
    role: Role,
    filter: String,
    mono_only: bool,
    focus: bool,
}

impl MammothApp {
    /// Rebuild egui's fonts from the current settings.
    pub(super) fn apply_fonts(&mut self, ctx: &egui::Context) {
        let (defs, errors) = fonts::definitions(&self.settings.ui_font, &self.settings.editor_font);
        ctx.set_fonts(defs);
        for e in errors {
            self.toast_err(e);
        }
    }

    pub(super) fn open_font_picker(&mut self, role: Role) {
        self.font_picker = Some(FontPicker {
            role,
            filter: String::new(),
            mono_only: role == Role::Editor,
            focus: true,
        });
        if self.installed_fonts.is_none() && self.font_scan.is_none() {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(fonts::scan());
            });
            self.font_scan = Some(rx);
        }
    }

    fn set_font(&mut self, role: Role, choice: FontChoice, ctx: &egui::Context) {
        match role {
            Role::Editor => self.settings.editor_font = choice,
            Role::Interface => self.settings.ui_font = choice,
        }
        self.apply_fonts(ctx);
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.font_scan {
            if let Ok(found) = rx.try_recv() {
                self.installed_fonts = Some(found);
                self.font_scan = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        if self.show_settings {
            let mut open = true;
            egui::Window::new("Settings")
                .id(Id::new("settings-window"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| self.settings_contents(ui));
            self.show_settings = open;
        }
        self.font_picker_ui(ctx);
    }

    fn settings_contents(&mut self, ui: &mut Ui) {
        ui.set_width(460.0);
        let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui)| {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(170.0, 24.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(170.0);
                        ui.label(RichText::new(label).color(theme::TEXT));
                    },
                );
                add(ui);
            });
        };

        heading(ui, "APPEARANCE");
        let mut pick: Option<Role> = None;
        for (label, role) in [
            ("Interface font", Role::Interface),
            ("Editor font", Role::Editor),
        ] {
            let current = match role {
                Role::Interface => self.settings.ui_font.label(role),
                Role::Editor => self.settings.editor_font.label(role),
            };
            row(ui, label, &mut |ui| {
                let b = egui::Button::new(RichText::new(&current).color(theme::TEXT))
                    .min_size(vec2(250.0, 26.0));
                if ui.add(b).on_hover_text("Choose a font").clicked() {
                    pick = Some(role);
                }
            });
        }
        row(ui, "Editor font size", &mut |ui| {
            ui.add(egui::Slider::new(&mut self.settings.font_size, 8.0..=32.0).step_by(0.5));
        });
        row(ui, "Line spacing", &mut |ui| {
            ui.add(egui::Slider::new(&mut self.settings.line_spacing, 1.0..=2.2).step_by(0.05));
        });
        ui.add_space(4.0);
        let sample_font = FontId::monospace(self.settings.font_size);
        Frame::new()
            .fill(theme::EDITOR_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(8.0)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (text, color) in [
                    (
                        "2026-09-25 ERROR login failed for jane@example.com",
                        theme::TEXT,
                    ),
                    (
                        "0O 1lI {}[]() => != <= #fn main() { println!(\"hi\") }",
                        theme::TEXT_DIM,
                    ),
                ] {
                    ui.label(RichText::new(text).font(sample_font.clone()).color(color));
                }
            });

        ui.add_space(12.0);
        heading(ui, "EDITOR");
        row(ui, "Tab width", &mut |ui| {
            ui.add(egui::Slider::new(&mut self.settings.tab_width, 1..=8));
        });
        row(ui, "Highlight current line", &mut |ui| {
            ui.checkbox(&mut self.settings.highlight_line, "");
        });

        ui.add_space(12.0);
        heading(ui, "LARGE FILES");
        row(ui, "Preview files larger than", &mut |ui| {
            ui.add(
                egui::DragValue::new(&mut self.settings.preview_threshold_mb)
                    .range(1..=1_000_000)
                    .suffix(" MB"),
            );
        });
        row(ui, "Lines shown in preview", &mut |ui| {
            ui.add(
                egui::DragValue::new(&mut self.settings.preview_lines)
                    .range(1..=100_000_000)
                    .speed(100.0),
            );
        });
        ui.label(
            RichText::new("Preview settings apply to files opened afterwards.")
                .size(12.0)
                .color(theme::TEXT_DIM),
        );

        if let Some(role) = pick {
            self.open_font_picker(role);
        }
    }

    fn font_picker_ui(&mut self, ctx: &egui::Context) {
        let Some(mut fp) = self.font_picker.take() else {
            return;
        };
        let current = match fp.role {
            Role::Interface => self.settings.ui_font.clone(),
            Role::Editor => self.settings.editor_font.clone(),
        };
        let mut chosen: Option<FontChoice> = None;
        let mut browse = false;
        let mut done = false;
        let title = match fp.role {
            Role::Interface => "Interface font",
            Role::Editor => "Editor font",
        };

        let modal = egui::Modal::new(Id::new("font-picker")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new(title).size(17.0).strong().color(theme::TEXT));
            ui.label(
                RichText::new("Changes apply immediately. Your pick is remembered.").size(12.0).color(theme::TEXT_DIM),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = ui.add(TextEdit::singleline(&mut fp.filter).hint_text("Search fonts…").desired_width(290.0));
                if fp.focus {
                    r.request_focus();
                    fp.focus = false;
                }
                ui.checkbox(&mut fp.mono_only, "Monospace only");
            });
            if fp.role == Role::Editor && !fp.mono_only {
                ui.label(
                    RichText::new("Proportional fonts will misalign columns in the editor.")
                        .size(11.5)
                        .color(theme::WARN),
                );
            }
            ui.add_space(6.0);
            let filter = fp.filter.to_lowercase();
            ScrollArea::vertical().max_height(330.0).auto_shrink([false, false]).show(ui, |ui| {
                for choice in [FontChoice::System, FontChoice::BuiltIn] {
                    let label = choice.label(fp.role);
                    if (filter.is_empty() || label.to_lowercase().contains(&filter))
                        && font_row(ui, &label, current == choice, None).clicked()
                    {
                        chosen = Some(choice);
                    }
                }
                ui.add_space(4.0);
                match &self.installed_fonts {
                    None => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(RichText::new("Looking for installed fonts…").color(theme::TEXT_DIM));
                        });
                    }
                    Some(list) => {
                        let shown: Vec<&FontInfo> = list
                            .iter()
                            .filter(|f| !fp.mono_only || f.mono)
                            .filter(|f| filter.is_empty() || f.family.to_lowercase().contains(&filter))
                            .collect();
                        if shown.is_empty() {
                            ui.label(RichText::new("No fonts match.").color(theme::TEXT_DIM));
                        }
                        for f in shown {
                            let is_current = matches!(&current, FontChoice::File { path, index, .. } if *path == f.path && *index == f.index);
                            let tag = f.mono.then_some("mono");
                            if font_row(ui, &f.family, is_current, tag).clicked() {
                                chosen = Some(f.choice());
                            }
                        }
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Browse for a font file…").clicked() {
                    browse = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = egui::Button::new(RichText::new("Done").color(Color32::WHITE)).fill(theme::ACCENT);
                    if ui.add(b).clicked() {
                        done = true;
                    }
                });
            });
        });
        if modal.should_close() {
            done = true;
        }

        if browse
            && let Some(path) = rfd::FileDialog::new()
                .set_title("Choose a font")
                .add_filter("Fonts", &["ttf", "otf", "ttc", "otc"])
                .pick_file()
        {
            match fonts::describe_file(&path) {
                Ok(info) => chosen = Some(info.choice()),
                Err(e) => self.toast_err(format!("{}: {e}", path.display())),
            }
        }
        if let Some(c) = chosen {
            self.set_font(fp.role, c, ctx);
        }
        if !done {
            self.font_picker = Some(fp);
        }
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .strong()
            .color(theme::LINENO_ACTIVE),
    );
    ui.add_space(2.0);
}

fn font_row(ui: &mut Ui, label: &str, selected: bool, tag: Option<&str>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let fill = if selected {
        theme::ACCENT_DIM
    } else if resp.hovered() {
        theme::PANEL_BG_2
    } else {
        Color32::TRANSPARENT
    };
    let p = ui.painter();
    p.rect_filled(rect, 6.0, fill);
    let color = if selected {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    p.text(
        rect.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        color,
    );
    if let Some(tag) = tag {
        p.text(
            rect.right_center() - vec2(10.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            tag,
            FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }
    resp
}
