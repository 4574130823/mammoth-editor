//! JSON inspector: the current line's JSON (even inside a log line) or the whole
//! document as a collapsible tree or pretty text, filtered live with jq, plus
//! "run jq over the whole file" and pretty-print / minify.

use std::collections::HashMap;
use std::time::Instant;

use egui::{
    Align, Button, Color32, FontId, Frame, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, vec2,
};
use serde_json::Value;

use super::{Job, JobKind, JobOut, MammothApp, Pending};
use crate::document::Pos;
use crate::editor::fmt_int;
use crate::json_tools::{self, Jq};
use crate::search::{self, Snapshot};
use crate::theme;

const J_KEY: Color32 = Color32::from_rgb(0x7a, 0xb8, 0xff);
const J_STR: Color32 = Color32::from_rgb(0x98, 0xd4, 0x8a);
const J_NUM: Color32 = Color32::from_rgb(0xf0, 0xa4, 0x6c);
const J_LIT: Color32 = Color32::from_rgb(0xd0, 0x8b, 0xf5);
const MAX_DOC_BYTES: usize = 64 << 20;
const MAX_CHILDREN: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum JsonSource {
    Line,
    Document,
}

pub(super) struct JsonInspector {
    pub open: bool,
    source: JsonSource,
    pub jq: String,
    jq_changed: f64,
    compiled: Option<(String, Result<Jq, String>)>,
    cache: Option<(CacheKey, Inspected)>,
    as_text: bool,
    toggles: HashMap<String, bool>,
    run_per_line: bool,
    run_keep_lines: bool,
    focus_jq: bool,
}

#[derive(Clone, PartialEq)]
struct CacheKey {
    tab: u64,
    version: u64,
    line: usize,
    source: JsonSource,
    jq: String,
}

enum Inspected {
    Nothing(String),
    Values {
        prefix: Option<String>,
        values: Vec<Value>,
        error: Option<String>,
        line: Option<usize>,
    },
}

impl Default for JsonInspector {
    fn default() -> Self {
        Self {
            open: false,
            source: JsonSource::Line,
            jq: String::new(),
            jq_changed: f64::NEG_INFINITY,
            compiled: None,
            cache: None,
            as_text: false,
            toggles: HashMap::new(),
            run_per_line: true,
            run_keep_lines: false,
            focus_jq: false,
        }
    }
}

impl JsonInspector {
    fn jq(&mut self) -> Option<Result<&Jq, &String>> {
        let code = self.jq.trim().to_string();
        if code.is_empty() {
            return None;
        }
        if self.compiled.as_ref().is_none_or(|(c, _)| *c != code) {
            self.compiled = Some((code.clone(), Jq::compile(&code)));
        }
        self.compiled.as_ref().map(|(_, r)| r.as_ref())
    }
}

/// The text of the physical line at `line`, joining the read-only segments of
/// very long lines (capped).
fn physical_line(doc: &crate::document::Document, line: usize) -> String {
    let mut first = line;
    while first > 0 && doc.is_soft(first - 1) {
        first -= 1;
    }
    let mut out = String::new();
    let mut i = first;
    loop {
        out.push_str(&doc.line(i));
        if !doc.is_soft(i) || i + 1 >= doc.line_count() || out.len() > MAX_DOC_BYTES {
            break;
        }
        i += 1;
    }
    out
}

impl MammothApp {
    /// Open the inspector with the cursor in its jq box.
    pub(super) fn focus_jq(&mut self) {
        self.json.open = true;
        self.json.focus_jq = true;
    }

    pub(super) fn toggle_json(&mut self) {
        self.json.open = !self.json.open;
        if self.json.open {
            let json_syntax = self
                .tabs
                .get(self.active)
                .is_some_and(|t| t.syntax.as_deref() == Some("json"));
            let small = self.tabs.get(self.active).is_some_and(|t| {
                t.doc
                    .source
                    .as_ref()
                    .is_none_or(|s| s.len() < MAX_DOC_BYTES as u64)
            });
            // A single .json document: show the whole thing; logs / JSON Lines: the current line.
            if json_syntax && small && self.tabs[self.active].doc.line_count() > 1 {
                self.json.source = JsonSource::Document;
            }
        }
    }

    fn inspect(&mut self) -> Option<&Inspected> {
        let tab = self.tabs.get(self.active)?;
        let line = match (&tab.table, tab.table_mode) {
            (Some(t), true) if t.row_count(&tab.doc) > 0 => t.row_line(t.sel.0),
            _ => tab.view.cursor.line,
        };
        let key = CacheKey {
            tab: tab.id,
            version: tab.doc.version,
            line,
            source: self.json.source,
            jq: self.json.jq.trim().to_string(),
        };
        let fresh = self.json.cache.as_ref().is_some_and(|(k, _)| *k == key);
        // While the jq program is being typed, keep the old result for a moment.
        let typing = self.now - self.json.jq_changed < 0.25 && self.json.cache.is_some();
        if !fresh && !typing {
            let result = self.compute_inspection(line);
            self.json.cache = Some((key, result));
        }
        self.json.cache.as_ref().map(|(_, r)| r)
    }

    fn compute_inspection(&mut self, line: usize) -> Inspected {
        let tab = &self.tabs[self.active];
        let (prefix, json_text, value, at_line) = match self.json.source {
            JsonSource::Line => {
                if line >= tab.doc.line_count() {
                    return Inspected::Nothing("No line selected.".into());
                }
                let text = physical_line(&tab.doc, line);
                let Some((s, e, v)) = json_tools::find_json(&text) else {
                    return Inspected::Nothing("No JSON on this line. Move the cursor to a line with a { … } or [ … ] value.".into());
                };
                let prefix = text[..s].trim();
                let prefix = (!prefix.is_empty()).then(|| prefix.to_string());
                (prefix, text[s..e].to_string(), v, Some(line))
            }
            JsonSource::Document => {
                if !tab.doc.is_fully_loaded() {
                    return Inspected::Nothing(
                        "Load the entire file first (Ctrl+L) to inspect it as one document.".into(),
                    );
                }
                let Some(text) =
                    tab.doc
                        .text_range(Pos::default(), tab.doc.end_pos(), MAX_DOC_BYTES)
                else {
                    return Inspected::Nothing("This file is too big to inspect as one document (limit 64 MB). Use “Current line” for JSON Lines, or run jq on the file below.".into());
                };
                match json_tools::parse_document(&text) {
                    Ok(v) => (None, text, v, None),
                    Err(e) => {
                        return Inspected::Nothing(format!(
                            "This file isn't one JSON document ({e}). For JSON Lines or logs, use “Current line”."
                        ));
                    }
                }
            }
        };
        match self.json.jq() {
            None => Inspected::Values {
                prefix,
                values: vec![value],
                error: None,
                line: at_line,
            },
            Some(Err(e)) => Inspected::Values {
                prefix,
                values: vec![value],
                error: Some(e.clone()),
                line: at_line,
            },
            Some(Ok(jq)) => match jq.run(&json_text, 1000) {
                Ok(outs) => Inspected::Values {
                    prefix,
                    values: outs
                        .iter()
                        .filter_map(|o| serde_json::from_str(o).ok())
                        .collect(),
                    error: None,
                    line: at_line,
                },
                Err(e) => Inspected::Values {
                    prefix,
                    values: vec![value],
                    error: Some(e),
                    line: at_line,
                },
            },
        }
    }

    pub(super) fn json_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut run = false;
        let mut open_pretty = false;
        let mut set_jq: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("{ }")
                    .monospace()
                    .strong()
                    .color(theme::ACCENT),
            );
            ui.label(
                RichText::new("JSON inspector")
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::icons::button(ui, crate::icons::Icon::Close, "Close (Ctrl+Shift+J)")
                    .clicked()
                {
                    self.json.open = false;
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (label, src) in [
                ("Current line", JsonSource::Line),
                ("Whole document", JsonSource::Document),
            ] {
                if segment(ui, label, self.json.source == src, src == JsonSource::Line).clicked() {
                    self.json.source = src;
                }
            }
            ui.add_space(10.0);
            for (label, text) in [("Tree", false), ("Text", true)] {
                if segment(ui, label, self.json.as_text == text, !text).clicked() {
                    self.json.as_text = text;
                }
            }
        });
        ui.add_space(6.0);
        let jq_resp = ui.add(
            TextEdit::singleline(&mut self.json.jq)
                .hint_text("jq filter, e.g. .user.email   or   .items[] | select(.ok)")
                .font(FontId::monospace(13.0))
                .desired_width(f32::INFINITY),
        );
        if self.json.focus_jq {
            jq_resp.request_focus();
            self.json.focus_jq = false;
        }
        if jq_resp.changed() {
            self.json.jq_changed = self.now;
            ctx.request_repaint_after(std::time::Duration::from_millis(260));
        }
        ui.label(
            RichText::new("Tip: click a key in the tree to filter by its path.")
                .size(11.5)
                .color(theme::TEXT_DIM),
        );
        ui.add_space(4.0);

        let as_text = self.json.as_text;
        let mut toggles = std::mem::take(&mut self.json.toggles);
        let inspected = self.inspect();
        Frame::new()
            .fill(theme::EDITOR_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(8.0)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let h = (ui.available_height() - 150.0).max(120.0);
                ScrollArea::both()
                    .max_height(h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.spacing_mut().interact_size.y = 18.0;
                        match inspected {
                            None => {}
                            Some(Inspected::Nothing(msg)) => {
                                ui.label(RichText::new(msg).color(theme::TEXT_DIM));
                            }
                            Some(Inspected::Values {
                                prefix,
                                values,
                                error,
                                line,
                            }) => {
                                if let Some(l) = line {
                                    ui.label(
                                        RichText::new(format!("Line {}", fmt_int(l + 1)))
                                            .size(11.5)
                                            .color(theme::LINENO_ACTIVE),
                                    );
                                }
                                if let Some(p) = prefix {
                                    ui.label(
                                        RichText::new(p)
                                            .monospace()
                                            .size(12.0)
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                if let Some(e) = error {
                                    ui.label(RichText::new(e).size(12.0).color(theme::ERROR));
                                }
                                if values.is_empty() {
                                    ui.label(
                                        RichText::new("jq produced no output.")
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                if values.len() > 1 {
                                    ui.label(
                                        RichText::new(format!("{} results", values.len()))
                                            .size(11.5)
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                for v in values.iter().take(200) {
                                    if as_text {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(json_tools::pretty(v))
                                                    .monospace()
                                                    .size(12.5)
                                                    .color(theme::TEXT),
                                            )
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                        );
                                    } else {
                                        tree(ui, v, None, ".", 0, &mut toggles, &mut set_jq);
                                    }
                                    ui.add_space(4.0);
                                }
                            }
                        }
                    });
            });
        self.json.toggles = toggles;
        if let Some(p) = set_jq {
            self.json.jq = p;
            self.json.jq_changed = f64::NEG_INFINITY;
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Open pretty in new tab").clicked() {
                open_pretty = true;
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new("RUN ON THE WHOLE FILE")
                .size(11.0)
                .strong()
                .color(theme::LINENO_ACTIVE),
        );
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.json.run_per_line,
                true,
                "Each line (JSON Lines / logs)",
            );
            ui.radio_value(&mut self.json.run_per_line, false, "One document");
        });
        ui.add_enabled_ui(self.json.run_per_line, |ui| {
            ui.checkbox(
                &mut self.json.run_keep_lines,
                "Keep the original lines that match (for select(…))",
            );
        });
        let has_program = !self.json.jq.trim().is_empty();
        let b =
            Button::new(RichText::new("Run jq on file").color(Color32::WHITE)).fill(theme::ACCENT);
        if ui
            .add_enabled(has_program, b)
            .on_disabled_hover_text("Type a jq filter first")
            .clicked()
        {
            run = true;
        }

        if open_pretty {
            self.open_pretty_json();
        }
        if run {
            self.start_jq_run(self.active, ctx);
        }
    }

    fn open_pretty_json(&mut self) {
        let Some(Inspected::Values { values, .. }) = self.json.cache.as_ref().map(|(_, r)| r)
        else {
            self.toast_info("Nothing to show — move to a line with JSON first.");
            return;
        };
        let text: Vec<String> = values
            .iter()
            .flat_map(|v| {
                json_tools::pretty(v)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        let title = format!(
            "pretty — {}",
            self.tabs
                .get(self.active)
                .map_or("", |t| t.doc.title.as_str())
        );
        self.push_tab(
            crate::document::Document::from_lines(title, text),
            Some("json".into()),
        );
    }

    /// Pretty-print (or minify) the whole document in place (undoable).
    pub(super) fn reformat_json(&mut self, pretty: bool) {
        let now = self.now;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.doc.is_fully_loaded() {
            self.toast_info("Load the entire file first (Ctrl+L).");
            return;
        }
        let Some(text) = tab
            .doc
            .text_range(Pos::default(), tab.doc.end_pos(), MAX_DOC_BYTES)
        else {
            self.toast_err(
                "That's too big to reformat in place (limit 64 MB). Use jq on the file instead.",
            );
            return;
        };
        let v = match json_tools::parse_document(&text) {
            Ok(v) => v,
            Err(e) => {
                self.toast_err(format!("Not a single JSON document: {e}"));
                return;
            }
        };
        let out = if pretty {
            json_tools::pretty(&v)
        } else {
            json_tools::compact(&v)
        };
        let lines: Vec<String> = out.lines().map(str::to_string).collect();
        let n = tab.doc.line_count();
        let sel = (tab.view.anchor, tab.view.cursor);
        let p = (Pos::default(), Pos::default());
        match tab.doc.replace_lines(0, n, lines, sel, p, now) {
            Ok(()) => {
                tab.view.select(
                    Pos::default(),
                    Pos::default(),
                    crate::editor::Reveal::Nearest,
                );
                self.toast_ok(if pretty {
                    "Pretty-printed (Ctrl+Z to undo)."
                } else {
                    "Minified (Ctrl+Z to undo)."
                });
            }
            Err(e) => self.toast_err(e),
        }
    }

    pub(super) fn start_jq_run(&mut self, i: usize, ctx: &egui::Context) {
        let program = self.json.jq.trim().to_string();
        if let Err(e) = Jq::compile(&program) {
            self.toast_err(e);
            return;
        }
        let (per_line, keep) = (self.json.run_per_line, self.json.run_keep_lines);
        let Some(tab) = self.tabs.get_mut(i) else {
            return;
        };
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            tab.pending = Some(Pending::Jq);
            self.toast_info("Loading the entire file first — jq will run as soon as it's done.");
            return;
        }
        let snap = Snapshot::of(&tab.doc);
        let title = format!("jq {} — {}", program, tab.doc.title);
        let prog = program.clone();
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Jq(run_jq_job(&snap, &prog, per_line, keep, ctl))
        });
        let kind = JobKind::Jq { title, keep };
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind,
            ctl,
            rx,
            version,
            label: "Running jq",
            started: Instant::now(),
        });
    }
}

pub(super) struct JqResult {
    pub lines: Vec<String>,
    pub inputs: u64,
    pub skipped: u64,
    pub error: Option<String>,
    pub truncated: bool,
}

fn run_jq_job(
    snap: &Snapshot,
    program: &str,
    per_line: bool,
    keep: bool,
    ctl: &search::JobCtl,
) -> JqResult {
    let mut r = JqResult {
        lines: Vec::new(),
        inputs: 0,
        skipped: 0,
        error: None,
        truncated: false,
    };
    // jq values aren't thread-safe, so the program is compiled on this thread.
    let jq = match Jq::compile(program) {
        Ok(j) => j,
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    };
    let mut bytes = 0usize;
    if per_line {
        snap.for_each_line(ctl, |_, raw| {
            let text = String::from_utf8_lossy(raw);
            let t = text.trim();
            let json = if t.starts_with('{') || t.starts_with('[') {
                Some(t.to_string())
            } else {
                json_tools::find_json(&text).map(|(s, e, _)| text[s..e].to_string())
            };
            let Some(json) = json else {
                if !t.is_empty() {
                    r.skipped += 1;
                }
                return true;
            };
            r.inputs += 1;
            if keep {
                if jq.matches(&json) {
                    bytes += text.len();
                    r.lines.push(text.into_owned());
                }
            } else {
                match jq.run(&json, 10_000) {
                    Ok(outs) => {
                        for o in outs {
                            bytes += o.len();
                            r.lines.push(o);
                        }
                    }
                    Err(e) if e.starts_with("Not JSON") => r.skipped += 1,
                    Err(e) => {
                        r.error.get_or_insert(e);
                    }
                }
            }
            if r.lines.len() >= 20_000_000 || bytes > 1 << 30 {
                r.truncated = true;
                return false;
            }
            true
        });
    } else {
        let mut doc = String::new();
        snap.for_each_line(ctl, |_, raw| {
            doc.push_str(&String::from_utf8_lossy(raw));
            doc.push('\n');
            doc.len() < 512 << 20
        });
        r.inputs = 1;
        match jq.run(&doc, 1_000_000) {
            Ok(outs) => {
                for o in outs {
                    match serde_json::from_str::<Value>(&o) {
                        Ok(v) => r
                            .lines
                            .extend(json_tools::pretty(&v).lines().map(str::to_string)),
                        Err(_) => r.lines.push(o),
                    }
                }
            }
            Err(e) => r.error = Some(e),
        }
    }
    r
}

fn segment(ui: &mut Ui, label: &str, on: bool, left: bool) -> egui::Response {
    let cr = if left {
        egui::CornerRadius {
            nw: 6,
            sw: 6,
            ne: 0,
            se: 0,
        }
    } else {
        egui::CornerRadius {
            nw: 0,
            sw: 0,
            ne: 6,
            se: 6,
        }
    };
    let b = Button::new(RichText::new(label).size(12.5).color(if on {
        Color32::WHITE
    } else {
        theme::TEXT_DIM
    }))
    .fill(if on {
        theme::ACCENT_DIM
    } else {
        theme::PANEL_BG_2
    })
    .corner_radius(cr);
    ui.add(b)
}

fn child_path(parent: &str, key: &str) -> String {
    let ident = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !key.starts_with(|c: char| c.is_ascii_digit());
    let quoted = serde_json::to_string(key).unwrap_or_default();
    match (parent == ".", ident) {
        (true, true) => format!(".{key}"),
        (true, false) => format!(".[{quoted}]"),
        (false, true) => format!("{parent}.{key}"),
        (false, false) => format!("{parent}[{quoted}]"),
    }
}

/// One node of the collapsible tree (and its children when expanded).
fn tree(
    ui: &mut Ui,
    v: &Value,
    key: Option<&str>,
    path: &str,
    depth: usize,
    toggles: &mut HashMap<String, bool>,
    set_jq: &mut Option<String>,
) {
    let container = v.is_object() || v.is_array();
    let open = container && *toggles.get(path).unwrap_or(&(depth < 2));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(depth as f32 * 14.0);
        if container {
            let (r, resp) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::click());
            let c = r.center();
            let pts = if open {
                vec![
                    c + vec2(-3.5, -1.5),
                    c + vec2(0.0, 2.0),
                    c + vec2(3.5, -1.5),
                ]
            } else {
                vec![
                    c + vec2(-1.5, -3.5),
                    c + vec2(2.0, 0.0),
                    c + vec2(-1.5, 3.5),
                ]
            };
            ui.painter().line(pts, Stroke::new(1.4, theme::TEXT_DIM));
            if resp.clicked() {
                toggles.insert(path.to_string(), !open);
            }
        } else {
            ui.add_space(12.0);
        }
        if let Some(k) = key {
            let kr = ui.add(
                egui::Label::new(RichText::new(k).monospace().size(12.5).color(J_KEY))
                    .sense(Sense::click()),
            );
            if kr.on_hover_text(format!("Filter by {path}")).clicked() {
                *set_jq = Some(path.to_string());
            }
            ui.label(
                RichText::new(":")
                    .monospace()
                    .size(12.5)
                    .color(theme::TEXT_DIM),
            );
        }
        let (text, color) = match v {
            Value::Object(m) => (
                format!("{{{} key{}}}", m.len(), if m.len() == 1 { "" } else { "s" }),
                theme::TEXT_DIM,
            ),
            Value::Array(a) => (
                format!("[{} item{}]", a.len(), if a.len() == 1 { "" } else { "s" }),
                theme::TEXT_DIM,
            ),
            Value::String(s) => {
                let mut t: String = s.chars().take(300).collect();
                if s.chars().count() > 300 {
                    t.push('…');
                }
                (format!("{t:?}"), J_STR)
            }
            Value::Number(n) => (n.to_string(), J_NUM),
            Value::Bool(b) => (b.to_string(), J_LIT),
            Value::Null => ("null".into(), J_LIT),
        };
        let vr = ui.add(
            egui::Label::new(RichText::new(text).monospace().size(12.5).color(color))
                .sense(Sense::click()),
        );
        vr.context_menu(|ui| {
            if ui.button("Copy value").clicked() {
                ui.ctx().copy_text(match v {
                    Value::String(s) => s.clone(),
                    other => json_tools::pretty(other),
                });
                ui.close();
            }
            if ui.button("Copy jq path").clicked() {
                ui.ctx().copy_text(path.to_string());
                ui.close();
            }
            if ui.button("Filter by this path").clicked() {
                *set_jq = Some(path.to_string());
                ui.close();
            }
        });
    });
    if !open {
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter().take(MAX_CHILDREN) {
                tree(
                    ui,
                    child,
                    Some(k),
                    &child_path(path, k),
                    depth + 1,
                    toggles,
                    set_jq,
                );
            }
            if m.len() > MAX_CHILDREN {
                more(ui, depth, m.len() - MAX_CHILDREN);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().take(MAX_CHILDREN).enumerate() {
                let p = if path == "." {
                    format!(".[{i}]")
                } else {
                    format!("{path}[{i}]")
                };
                tree(
                    ui,
                    child,
                    Some(&i.to_string()),
                    &p,
                    depth + 1,
                    toggles,
                    set_jq,
                );
            }
            if a.len() > MAX_CHILDREN {
                more(ui, depth, a.len() - MAX_CHILDREN);
            }
        }
        _ => {}
    }
}

fn more(ui: &mut Ui, depth: usize, n: usize) {
    ui.horizontal(|ui| {
        ui.add_space((depth + 1) as f32 * 14.0 + 16.0);
        ui.label(
            RichText::new(format!("… {} more (use jq to reach them)", fmt_int(n)))
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
    });
}

#[cfg(test)]
impl MammothApp {
    pub(super) fn json_values(&self) -> Option<Vec<Value>> {
        match self.json.cache.as_ref().map(|(_, r)| r) {
            Some(Inspected::Values { values, .. }) => Some(values.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
impl JsonInspector {
    pub(super) fn run_keep_lines_for_test(&mut self, keep: bool) {
        self.run_keep_lines = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::child_path;

    #[test]
    fn jq_paths() {
        assert_eq!(child_path(".", "user"), ".user");
        assert_eq!(child_path(".user", "email"), ".user.email");
        assert_eq!(child_path(".user", "e-mail"), r#".user["e-mail"]"#);
        assert_eq!(child_path(".", "2x"), r#".["2x"]"#);
    }
}
