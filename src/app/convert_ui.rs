//! Convert dialog: CSV / TSV / PSV ↔ JSON Lines / JSON array, streamed to a file
//! (then opened) or into a new tab.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use egui::{Align, Button, Color32, ComboBox, Id, Layout, RichText};

use super::{Job, JobKind, JobOut, MammothApp, Pending};
use crate::convert::{self, Format, Spec, Stats};
use crate::editor::{fmt_bytes, fmt_int};
use crate::search::{self, Snapshot};
use crate::theme;

/// Results bigger than this must go to a file rather than a tab.
const TAB_LIMIT: usize = 256 << 20;

const FORMATS: [Format; 5] = [
    Format::Delimited(b','),
    Format::Delimited(b'\t'),
    Format::Delimited(b'|'),
    Format::JsonLines,
    Format::JsonArray,
];

pub(super) struct ConvertDialog {
    tab: u64,
    title: String,
    size: u64,
    from: Format,
    to: Format,
    header: bool,
    typed: bool,
    flatten: bool,
    to_file: bool,
}

/// A `Vec` writer that refuses to grow past `TAB_LIMIT`.
struct Capped(Vec<u8>);

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + buf.len() > TAB_LIMIT {
            return Err(std::io::Error::other("too big for a tab"));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn syntax_for(f: Format) -> Option<String> {
    Some(
        match f {
            Format::Delimited(b'\t') => "tsv",
            Format::Delimited(b'|') => "psv",
            Format::Delimited(_) => "csv",
            Format::JsonLines | Format::JsonArray => "json",
        }
        .into(),
    )
}

impl MammothApp {
    pub(super) fn open_convert(&mut self, to: Option<Format>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let ext = tab
            .doc
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let from = match (Format::from_syntax(tab.syntax.as_deref()), ext.as_str()) {
            (_, "jsonl" | "ndjson") => Format::JsonLines,
            (Some(Format::JsonLines), "json") => {
                // A .json file starting with '[' is most likely one array.
                let first = tab.doc.line(0).trim_start().chars().next();
                if first == Some('[') {
                    Format::JsonArray
                } else {
                    Format::JsonLines
                }
            }
            (Some(f), _) => f,
            (None, _) => {
                // No hint from the syntax: look at the first line.
                let first = tab.doc.line(0);
                let t = first.trim_start();
                if t.starts_with('{') {
                    Format::JsonLines
                } else if t.trim_end() == "[" || t.starts_with("[{") || t.starts_with("[\"") {
                    Format::JsonArray
                } else if first.contains('\t') {
                    Format::Delimited(b'\t')
                } else if first.contains('|') && !first.contains(',') {
                    Format::Delimited(b'|')
                } else {
                    Format::Delimited(b',')
                }
            }
        };
        let default_to = if matches!(from, Format::Delimited(_)) {
            Format::JsonLines
        } else {
            Format::Delimited(b',')
        };
        let size = tab.doc.source.as_ref().map_or(0, |s| s.len());
        self.convert = Some(ConvertDialog {
            tab: tab.id,
            title: tab.doc.title.clone(),
            size,
            from,
            to: to.filter(|t| *t != from).unwrap_or(default_to),
            header: true,
            typed: true,
            flatten: true,
            to_file: size > 64 << 20,
        });
    }

    pub(super) fn convert_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.convert.take() else {
            return;
        };
        let mut go = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::new("convert-dialog")).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(
                RichText::new(format!("Convert “{}”", d.title))
                    .size(17.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                RichText::new("Streams line by line, so it works on files of any size.")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(10.0);
            egui::Grid::new("convert-grid")
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    ui.label("From");
                    ComboBox::from_id_salt("convert-from")
                        .selected_text(d.from.label())
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for f in FORMATS {
                                ui.selectable_value(&mut d.from, f, f.label());
                            }
                        });
                    ui.end_row();
                    ui.label("To");
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for f in [
                            Format::JsonLines,
                            Format::JsonArray,
                            Format::Delimited(b','),
                            Format::Delimited(b'\t'),
                        ] {
                            let on = d.to == f;
                            let b = Button::new(RichText::new(f.label()).color(if on {
                                Color32::WHITE
                            } else {
                                theme::TEXT_DIM
                            }))
                            .fill(if on {
                                theme::ACCENT_DIM
                            } else {
                                theme::PANEL_BG_2
                            });
                            if ui.add_enabled(f != d.from, b).clicked() {
                                d.to = f;
                            }
                        }
                    });
                    ui.end_row();
                });
            ui.add_space(8.0);
            let delimited_in = matches!(d.from, Format::Delimited(_));
            let json_out = matches!(d.to, Format::JsonLines | Format::JsonArray);
            if delimited_in {
                ui.checkbox(&mut d.header, "First row is the header (column names)");
            }
            if delimited_in && json_out {
                ui.checkbox(&mut d.typed, "Write numbers and true/false as JSON values")
                    .on_hover_text("Values like 007 or 1,5 stay text, so nothing changes meaning.");
            }
            if !delimited_in && !json_out {
                ui.checkbox(
                    &mut d.flatten,
                    "Flatten nested objects into columns (user.email)",
                );
            }
            ui.add_space(8.0);
            let too_big_for_tab = d.size > TAB_LIMIT as u64;
            ui.radio_value(&mut d.to_file, true, "Save to a file, then open it");
            ui.add_enabled_ui(!too_big_for_tab, |ui| {
                ui.radio_value(&mut d.to_file, false, "Open in a new tab")
                    .on_disabled_hover_text(format!(
                        "This file is {}; save to a file instead.",
                        fmt_bytes(d.size)
                    ));
            });
            if too_big_for_tab {
                d.to_file = true;
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = Button::new(RichText::new("Convert").color(Color32::WHITE).strong())
                        .fill(theme::ACCENT);
                    if ui.add_enabled(d.from != d.to, b).clicked() {
                        go = true;
                    }
                });
            });
        });
        if modal.should_close() {
            cancel = true;
        }
        if go {
            let spec = Spec {
                from: d.from,
                to: d.to,
                header: d.header,
                typed: d.typed,
                flatten: d.flatten,
            };
            let path = if d.to_file {
                let stem = std::path::Path::new(&d.title)
                    .file_stem()
                    .map_or("converted".into(), |s| s.to_string_lossy().into_owned());
                let mut dlg = rfd::FileDialog::new()
                    .set_title("Save converted file")
                    .set_file_name(format!("{stem}.{}", d.to.extension()));
                if let Some(dir) = self
                    .tab_index(d.tab)
                    .and_then(|i| self.tabs[i].doc.path.as_ref())
                    .and_then(|p| p.parent())
                {
                    dlg = dlg.set_directory(dir);
                }
                match dlg.save_file() {
                    Some(p)
                        if self
                            .tab_index(d.tab)
                            .and_then(|i| self.tabs[i].doc.path.as_ref())
                            == Some(&p) =>
                    {
                        self.toast_err("Pick a different file than the one you're converting.");
                        self.convert = Some(d);
                        return;
                    }
                    Some(p) => Some(p),
                    None => {
                        self.convert = Some(d);
                        return;
                    }
                }
            } else {
                None
            };
            if let Some(i) = self.tab_index(d.tab) {
                self.start_convert(i, spec, path, ctx);
            }
        } else if !cancel {
            self.convert = Some(d);
        }
    }

    pub(super) fn start_convert(
        &mut self,
        i: usize,
        spec: Spec,
        path: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        let tab = &mut self.tabs[i];
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            tab.pending = Some(Pending::Convert { spec, path });
            self.toast_info(
                "Loading the entire file first — the conversion starts as soon as it's done.",
            );
            return;
        }
        let snap = Snapshot::of(&tab.doc);
        let stem = std::path::Path::new(&tab.doc.title)
            .file_stem()
            .map_or("converted".into(), |s| s.to_string_lossy().into_owned());
        let title = format!("{stem}.{}", spec.to.extension());
        let syntax = syntax_for(spec.to);
        let out_path = path.clone();
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            let res: Result<(Stats, Option<Vec<u8>>), String> = match &out_path {
                Some(p) => (|| {
                    let f = std::fs::File::create(p)
                        .map_err(|e| format!("Couldn't create {}: {e}", p.display()))?;
                    let mut w = std::io::BufWriter::with_capacity(4 << 20, f);
                    let stats = convert::convert(&snap, &spec, &mut w, ctl)?;
                    w.flush().map_err(|e| e.to_string())?;
                    Ok((stats, None))
                })(),
                None => {
                    let mut w = Capped(Vec::new());
                    convert::convert(&snap, &spec, &mut w, ctl)
                        .map(|s| (s, Some(w.0)))
                        .map_err(|e| {
                            if e.contains("too big for a tab") {
                                "The result is over 256 MB — choose “Save to a file” instead."
                                    .into()
                            } else {
                                e
                            }
                        })
                }
            };
            if res.is_err()
                && let Some(p) = &out_path
            {
                let _ = std::fs::remove_file(p);
            }
            JobOut::Converted(res, out_path)
        });
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::Convert { title, syntax },
            ctl,
            rx,
            version,
            label: "Converting",
            started: Instant::now(),
        });
    }

    pub(super) fn finish_convert(
        &mut self,
        title: String,
        syntax: Option<String>,
        res: Result<(Stats, Option<Vec<u8>>), String>,
        path: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        match res {
            Err(e) => self.toast_err(e),
            Ok((stats, bytes)) => {
                let mut msg = format!("Converted {} records", fmt_int(stats.records as usize));
                if stats.columns > 0 {
                    msg.push_str(&format!(" · {} columns", stats.columns));
                }
                if stats.skipped > 0 {
                    msg.push_str(&format!(
                        " · skipped {} lines that weren't JSON objects",
                        fmt_int(stats.skipped as usize)
                    ));
                }
                match (bytes, path) {
                    (Some(bytes), _) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let lines: Vec<String> = text.lines().map(str::to_string).collect();
                        self.push_tab(crate::document::Document::from_lines(title, lines), syntax);
                    }
                    (None, Some(p)) => {
                        msg.push_str(&format!(" → {}", p.display()));
                        self.open_path(p, ctx);
                    }
                    (None, None) => {}
                }
                self.toast_ok(msg);
            }
        }
    }
}
