//! Application shell: tabs, find/replace bar, status bar and the background jobs
//! (search, count, extract, replace-all, save). The window chrome, modules drawer,
//! command palette and settings live in the submodules.

mod breakdown_ui;
mod commands;
mod convert_ui;
mod heatmap_ui;
mod json_ui;
mod modules_ui;
mod palette;
mod settings_ui;
mod table_ui;
#[cfg(test)]
mod tests;
mod titlebar;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use egui::{
    Align, Button, CentralPanel, Color32, ComboBox, CornerRadius, FontId, Frame, Id, Key,
    KeyboardShortcut, Layout, Margin, Modifiers, Panel, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, ViewportCommand, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use crate::document::{Document, EditKind, Piece, Pos, Source, char_to_byte};
use crate::editor::{self, Command, EditorView, Env, Reveal, fmt_bytes, fmt_int};
use crate::fonts::{FontChoice, FontInfo};
use crate::icons::{self, Icon};
use crate::modules::Registry;
use crate::search::{
    self, Breakdown, Filtered, GroupFilter, Hit, JobCtl, Matcher, Query, Snapshot,
};
use crate::theme;

const MB: u64 = 1024 * 1024;

/// Colours cycled through for ad-hoc "Highlight word/selection" terms.
const HIGHLIGHT_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xff, 0xd5, 0x4f),
    Color32::from_rgb(0x4f, 0x9d, 0xff),
    Color32::from_rgb(0x3d, 0xd6, 0x8c),
    Color32::from_rgb(0xff, 0x7a, 0x90),
    Color32::from_rgb(0xb3, 0x8b, 0xfa),
    Color32::from_rgb(0xf5, 0xa5, 0x24),
    Color32::from_rgb(0x2e, 0xc4, 0xd6),
    Color32::from_rgb(0xe0, 0xc0, 0x5a),
];

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub font_size: f32,
    pub tab_width: usize,
    pub preview_threshold_mb: u64,
    pub preview_lines: usize,
    pub highlight_line: bool,
    /// Heatmap strip beside the scrollbar.
    pub heatmap: bool,
    pub line_spacing: f32,
    pub ui_font: FontChoice,
    pub editor_font: FontChoice,
    /// Modules panel docked at the side instead of floating over the editor.
    pub modules_pinned: bool,
    pub extract_unique: bool,
    pub module_states: Vec<(String, bool)>,
    pub recent: Vec<PathBuf>,
    /// Bookmarked line numbers per file path, most recently touched first.
    pub bookmarks: Vec<(PathBuf, Vec<usize>)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            tab_width: 4,
            preview_threshold_mb: 1024,
            preview_lines: 1000,
            highlight_line: true,
            heatmap: true,
            line_spacing: 1.5,
            ui_font: FontChoice::System,
            editor_font: FontChoice::System,
            modules_pinned: false,
            extract_unique: true,
            module_states: Vec::new(),
            recent: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

pub struct Tab {
    id: u64,
    pub doc: Document,
    pub view: EditorView,
    syntax: Option<String>,
    pending_hit: Option<(u64, u64)>,
    pending_goto: Option<usize>,
    /// Work waiting for the whole file to be indexed.
    pending: Option<Pending>,
    /// Set on tabs produced by "Filter lines".
    origin: Option<FilterOrigin>,
    /// Table view state for CSV / TSV files, and whether it's showing.
    table: Option<crate::table::TableState>,
    table_mode: bool,
    heat: heatmap_ui::HeatState,
    was_loading: bool,
    load_started: Option<Instant>,
    close_after_save: bool,
    /// Bookmarked line numbers, marked in the gutter (F2 / Shift+F2 to jump).
    bookmarks: BTreeSet<usize>,
    /// Ad-hoc highlighted terms ("Highlight word/selection"), each its own colour.
    highlights: Vec<(String, Color32)>,
}

impl Tab {
    fn new(id: u64, doc: Document, syntax: Option<String>) -> Self {
        Self {
            id,
            doc,
            view: EditorView::focused(),
            syntax,
            pending_hit: None,
            pending_goto: None,
            pending: None,
            origin: None,
            table: None,
            table_mode: false,
            heat: Default::default(),
            was_loading: false,
            load_started: None,
            close_after_save: false,
            bookmarks: BTreeSet::new(),
            highlights: Vec::new(),
        }
    }

    fn is_partial(&self) -> bool {
        !self.doc.is_fully_loaded()
    }

    /// Source line numbers for the gutter of an unedited filter tab.
    fn origin_lines(&self) -> Option<&[usize]> {
        self.origin
            .as_ref()
            .filter(|o| o.version == self.doc.version)
            .map(|o| o.lines.as_slice())
    }

    fn is_delimited(&self) -> bool {
        matches!(self.syntax.as_deref(), Some("csv" | "tsv" | "psv"))
    }
}

/// Work that needs every line indexed; it runs as soon as loading finishes.
enum Pending {
    ReplaceAll,
    TableView,
    Jq,
    Convert {
        spec: crate::convert::Spec,
        path: Option<PathBuf>,
    },
    Filter {
        matcher: Box<Matcher>,
        label: String,
    },
}

/// Where the lines of a filtered tab came from.
struct FilterOrigin {
    source: u64,
    source_title: String,
    /// Source line of each line in the filtered tab.
    lines: Arc<Vec<usize>>,
    /// Document version when created; after edits the mapping is dropped.
    version: u64,
    label: String,
    source_lines: usize,
}

/// A module search narrowed to some breakdown groups, e.g. only Gmail addresses.
#[derive(Clone, Debug, PartialEq)]
struct GroupSel {
    module: String,
    /// Shown to the user, e.g. "Gmail" or "@example.com".
    label: String,
    keys: Option<Vec<String>>,
    category: Option<String>,
}

#[derive(PartialEq, Clone)]
struct FindKey(String, bool, bool, bool, Option<String>, Option<GroupSel>);

#[derive(Default)]
struct FindState {
    open: bool,
    replace_open: bool,
    query: String,
    replace: String,
    case: bool,
    word: bool,
    regex: bool,
    module: Option<String>,
    group: Option<GroupSel>,
    focus: bool,
    last_count: Option<(u64, bool)>,
    cache: Option<(FindKey, Result<Matcher, String>)>,
}

impl FindState {
    fn matcher(&mut self, reg: &Registry) -> Result<Matcher, String> {
        let key = FindKey(
            self.query.clone(),
            self.case,
            self.word,
            self.regex,
            self.module.clone(),
            self.group.clone(),
        );
        if let Some((k, r)) = &self.cache
            && *k == key
        {
            return r.clone();
        }
        if self
            .group
            .as_ref()
            .is_some_and(|g| Some(&g.module) != self.module.as_ref())
        {
            self.group = None;
        }
        let entry = self
            .module
            .as_ref()
            .and_then(|id| reg.by_id(id))
            .map(|i| &reg.entries[i]);
        let group = self.group.as_ref().and_then(|g| {
            Some(GroupFilter {
                grouper: entry?.grouper()?,
                keys: g
                    .keys
                    .as_ref()
                    .map(|k| Arc::new(k.iter().cloned().collect())),
                category: g.category.clone(),
            })
        });
        let r = if self.module.is_some() && entry.is_none() {
            Err("That module is no longer loaded.".into())
        } else {
            Matcher::new(&Query {
                text: self.query.clone(),
                case_sensitive: self.case,
                whole_word: self.word,
                regex: self.regex,
                module: entry.map(|e| e.module.clone()),
                group,
            })
        };
        self.last_count = None;
        self.cache = Some((key, r.clone()));
        r
    }

    fn describe(&self, reg: &Registry) -> String {
        match (
            self.module.as_ref().and_then(|id| reg.by_id(id)),
            &self.group,
        ) {
            (Some(i), Some(g)) => format!("{} ({})", reg.entries[i].module.name(), g.label),
            (Some(i), None) => reg.entries[i].module.name().to_string(),
            (None, _) => format!("“{}”", self.query),
        }
    }
}

enum JobKind {
    Find,
    Count,
    Extract {
        title: String,
    },
    ReplaceAll,
    Save {
        path: PathBuf,
        tmp: PathBuf,
    },
    Breakdown(breakdown_ui::InsightKey),
    Filter {
        label: String,
        syntax: Option<String>,
        source_title: String,
    },
    TableView {
        generation: u64,
    },
    ExportView {
        title: String,
        syntax: Option<String>,
    },
    Jq {
        title: String,
        keep: bool,
    },
    Heatmap {
        key: String,
        version: u64,
        layers: Vec<(String, Color32)>,
    },
    Convert {
        title: String,
        syntax: Option<String>,
    },
}

enum JobOut {
    Found(Option<Hit>),
    Counted(u64, bool),
    Extracted(Vec<String>, bool, u64),
    Replaced(Option<(Vec<Piece>, u64, u64)>, u64, usize),
    Saved(Result<(), String>),
    Breakdown(Breakdown),
    Filtered(Option<Filtered>, usize),
    TableView(Result<Vec<usize>, String>, usize),
    Exported(Option<Vec<String>>),
    Jq(json_ui::JqResult),
    Heatmap(crate::search::Heatmap),
    Converted(
        Result<(crate::convert::Stats, Option<Vec<u8>>), String>,
        Option<PathBuf>,
    ),
}

struct Job {
    tab: u64,
    kind: JobKind,
    ctl: Arc<JobCtl>,
    rx: Receiver<JobOut>,
    version: u64,
    label: &'static str,
    started: Instant,
}

struct Toast {
    text: String,
    color: Color32,
    until: f64,
}

#[derive(PartialEq)]
enum Dialog {
    None,
    GoTo { text: String, focus: bool },
    ConfirmClose(u64),
    ConfirmQuit,
}

#[derive(Clone, Debug, PartialEq)]
enum ModuleAction {
    Breakdown(String),
    Find(String),
    Count(String),
    Extract(String),
    Mask(String),
}

pub struct MammothApp {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    untitled: u32,
    registry: Registry,
    settings: Settings,
    find: FindState,
    jobs: Vec<Job>,
    toast: Option<Toast>,
    dialog: Dialog,
    show_settings: bool,
    show_help: bool,
    allow_quit: bool,
    last_title: String,
    now: f64,
    /// Theme and fonts are applied on the first frame (to whichever context runs us).
    setup_done: bool,
    logo: icons::Logo,
    modules_open: bool,
    modules_button_rect: egui::Rect,
    module_filter: String,
    palette: Option<palette::PaletteState>,
    font_picker: Option<settings_ui::FontPicker>,
    installed_fonts: Option<Vec<FontInfo>>,
    font_scan: Option<Receiver<Vec<FontInfo>>>,
    insights: breakdown_ui::Insights,
    json: json_ui::JsonInspector,
    convert: Option<convert_ui::ConvertDialog>,
}

impl MammothApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, "settings"))
            .unwrap_or_default();
        Self::create(&cc.egui_ctx, settings, files)
    }

    fn create(ctx: &egui::Context, settings: Settings, files: Vec<PathBuf>) -> Self {
        let mut registry = Registry::load();
        registry.apply_states(&settings.module_states);
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            untitled: 0,
            registry,
            settings,
            find: FindState::default(),
            jobs: Vec::new(),
            toast: None,
            dialog: Dialog::None,
            show_settings: false,
            show_help: false,
            allow_quit: false,
            last_title: String::new(),
            now: 0.0,
            setup_done: false,
            logo: icons::Logo::default(),
            modules_open: false,
            modules_button_rect: egui::Rect::NOTHING,
            module_filter: String::new(),
            palette: None,
            font_picker: None,
            installed_fonts: None,
            font_scan: None,
            insights: breakdown_ui::Insights::default(),
            json: json_ui::JsonInspector::default(),
            convert: None,
        };
        for e in app.registry.load_errors.clone() {
            app.toast_err(format!("Module error: {e}"));
        }
        for f in files {
            app.open_path(f, ctx);
        }
        app
    }

    // ------------------------------------------------------------------
    // Helpers

    fn toast(&mut self, text: impl Into<String>, color: Color32) {
        let text = text.into();
        let secs = 3.0 + text.len() as f64 / 30.0;
        self.toast = Some(Toast {
            text,
            color,
            until: self.now + secs.min(10.0),
        });
    }
    fn toast_ok(&mut self, text: impl Into<String>) {
        self.toast(text, theme::OK);
    }
    fn toast_info(&mut self, text: impl Into<String>) {
        self.toast(text, theme::TEXT);
    }
    fn toast_err(&mut self, text: impl Into<String>) {
        self.toast(text, theme::ERROR);
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn active_tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn push_tab(&mut self, doc: Document, syntax: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab::new(id, doc, syntax));
        self.active = self.tabs.len() - 1;
    }

    fn new_tab(&mut self) {
        self.untitled += 1;
        let doc = Document::new_empty(format!("Untitled-{}", self.untitled));
        self.push_tab(doc, None);
    }

    fn add_recent(&mut self, path: &Path) {
        let r = &mut self.settings.recent;
        r.retain(|p| p != path);
        r.insert(0, path.to_path_buf());
        r.truncate(12);
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(files) = rfd::FileDialog::new().set_title("Open").pick_files() {
            for f in files {
                self.open_path(f, ctx);
            }
        }
    }

    pub fn open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        let path = std::fs::canonicalize(&path)
            .map(strip_verbatim)
            .unwrap_or(path);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.doc.path.as_deref() == Some(&path))
        {
            self.active = i;
            return;
        }
        if path.is_dir() {
            self.toast_err(format!("{} is a folder.", path.display()));
            return;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let preview = size > self.settings.preview_threshold_mb.max(1) * MB;
        let target = if preview {
            self.settings.preview_lines.max(1)
        } else {
            usize::MAX
        };
        match Source::open(&path, target, ctx) {
            Ok(src) => {
                let doc = Document::from_source(src);
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let syntax = self
                    .registry
                    .syntax_for_extension(&ext)
                    .map(|i| self.registry.entries[i].module.id().to_string());
                // Replace a pristine, empty "Untitled" tab.
                if self.tabs.len() == 1 {
                    let t = &self.tabs[0].doc;
                    if t.path.is_none()
                        && !t.is_dirty()
                        && t.line_count() == 1
                        && t.line(0).is_empty()
                    {
                        self.tabs.clear();
                    }
                }
                self.push_tab(doc, syntax);
                if let Some((_, marks)) = self.settings.bookmarks.iter().find(|(p, _)| *p == path)
                {
                    self.tabs.last_mut().unwrap().bookmarks = marks.iter().copied().collect();
                }
                self.add_recent(&path);
                if preview {
                    self.toast(
                        format!(
                            "Preview mode: showing the first {} lines of this {} file. Click “Load entire file” (Ctrl+L) to load the rest.",
                            fmt_int(self.settings.preview_lines),
                            fmt_bytes(size)
                        ),
                        theme::ACCENT,
                    );
                }
            }
            Err(e) => self.toast_err(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn request_close(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if tab.doc.is_dirty() {
            self.active = idx;
            self.dialog = Dialog::ConfirmClose(tab.id);
        } else {
            self.close_tab(idx);
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(idx);
        for j in &self.jobs {
            if j.tab == tab.id && !matches!(j.kind, JobKind::Save { .. }) {
                j.ctl.cancel.store(true, Ordering::SeqCst);
            }
        }
        if let Some(src) = &tab.doc.source {
            src.cancel();
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if idx < self.active {
            self.active -= 1;
        }
    }

    fn load_entire(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get(self.active)
            && let Some(src) = &tab.doc.source
            && !src.is_complete()
        {
            src.load_all(ctx);
        }
    }

    // ------------------------------------------------------------------
    // Saving

    fn save_tab(&mut self, idx: usize, save_as: bool, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if tab.doc.busy.is_some() {
            return;
        }
        let path = if save_as || tab.doc.path.is_none() {
            let mut dlg = rfd::FileDialog::new()
                .set_title("Save as")
                .set_file_name(&tab.doc.title);
            if let Some(dir) = tab.doc.path.as_ref().and_then(|p| p.parent()) {
                dlg = dlg.set_directory(dir);
            }
            match dlg.save_file() {
                Some(p) => p,
                None => return,
            }
        } else {
            tab.doc.path.clone().unwrap()
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = path.with_file_name(format!(".{name}.mammoth-tmp"));
        let snap = tab.doc.save_snapshot();
        let total = snap.estimate();
        let (tab_id, version) = (tab.id, tab.doc.version);
        self.tabs[idx].doc.busy = Some("saving".into());
        let tmp2 = tmp.clone();
        let (ctl, rx) = search::spawn(total, ctx, move |ctl| {
            let r = (|| -> std::io::Result<()> {
                let f = std::fs::File::create(&tmp2)?;
                let mut w = std::io::BufWriter::with_capacity(4 << 20, f);
                snap.write(&mut w, &ctl.done, &ctl.cancel)?;
                let f = w.into_inner().map_err(|e| e.into_error())?;
                f.sync_all()
            })();
            drop(snap);
            if r.is_err() {
                let _ = std::fs::remove_file(&tmp2);
            }
            JobOut::Saved(r.map_err(|e| e.to_string()))
        });
        self.jobs.push(Job {
            tab: tab_id,
            kind: JobKind::Save { path, tmp },
            ctl,
            rx,
            version,
            label: "Saving",
            started: Instant::now(),
        });
    }

    fn finish_save(
        &mut self,
        tab_id: u64,
        path: PathBuf,
        tmp: PathBuf,
        result: Result<(), String>,
        ctx: &egui::Context,
    ) {
        let Some(idx) = self.tab_index(tab_id) else {
            if result.is_ok() {
                let _ = replace_file(&tmp, &path);
            }
            return;
        };
        self.tabs[idx].doc.busy = None;
        if let Err(e) = result {
            self.tabs[idx].close_after_save = false;
            self.toast_err(format!("Save failed: {e}"));
            return;
        }
        let preview_lines = self.settings.preview_lines;
        let tab = &mut self.tabs[idx];
        let mapped_same = tab
            .doc
            .source
            .as_ref()
            .is_some_and(|s| s.is_mapped() && same_file(s.path(), &path));
        let res = if mapped_same {
            // The old file is memory-mapped; Windows won't let us replace it until
            // every reference to the mapping is gone.
            for j in &self.jobs {
                if j.tab == tab_id {
                    j.ctl.cancel.store(true, Ordering::SeqCst);
                }
            }
            let src = tab.doc.source.take().unwrap();
            let was_complete = src.is_complete();
            let avail = src.available_lines();
            src.stop();
            let t0 = Instant::now();
            while Arc::strong_count(&src) > 1 && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(2));
            }
            drop(src);
            let r = replace_file(&tmp, &path);
            let target = if was_complete {
                usize::MAX
            } else {
                avail.max(preview_lines)
            };
            match Source::open(&path, target, ctx) {
                Ok(new_src) => {
                    if r.is_ok() {
                        tab.doc.reset_to_source(new_src);
                    } else {
                        tab.doc.source = Some(new_src);
                    }
                    r
                }
                Err(e) => Err(format!(
                    "{} Reopening failed: {e}",
                    r.err().unwrap_or_default()
                )),
            }
        } else {
            replace_file(&tmp, &path)
        };
        match res {
            Ok(()) => {
                tab.doc.mark_saved(&path);
                let close = tab.close_after_save;
                let msg = format!("Saved {}", path.display());
                if close {
                    self.close_tab(idx);
                }
                self.add_recent(&path);
                self.toast_ok(msg);
            }
            Err(e) => {
                tab.close_after_save = false;
                self.toast_err(e);
            }
        }
    }

    // ------------------------------------------------------------------
    // Find / replace

    fn open_find(&mut self, replace: bool) {
        self.find.open = true;
        self.find.replace_open |= replace;
        self.find.focus = true;
        if let Some(tab) = self.tabs.get(self.active) {
            let (s, e) = tab.view.selection();
            if s != e
                && s.line == e.line
                && self.find.module.is_none()
                && let Some(t) = tab.doc.text_range(s, e, 1000)
            {
                self.find.query = t;
            }
        }
    }

    fn matcher_or_toast(&mut self) -> Option<Matcher> {
        match self.find.matcher(&self.registry) {
            Ok(m) => Some(m),
            Err(e) => {
                if !self.find.open {
                    self.open_find(false);
                } else if !e.is_empty() {
                    self.toast_err(e);
                }
                None
            }
        }
    }

    fn cancel_jobs(&self, tab: u64, pred: impl Fn(&JobKind) -> bool) {
        for j in &self.jobs {
            if j.tab == tab && pred(&j.kind) {
                j.ctl.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    fn start_find(&mut self, backward: bool, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            return;
        }
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let tab = &self.tabs[self.active];
        if tab.doc.line_count() == 0 {
            return;
        }
        self.cancel_jobs(tab.id, |k| matches!(k, JobKind::Find));
        let (s, e) = tab.view.selection();
        let from = if backward { s } else { e };
        let byte = char_to_byte(&tab.doc.line(from.line), from.col);
        let snap = Snapshot::of(&tab.doc);
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Found(if backward {
                snap.find_prev(&m, from.line, byte, ctl)
            } else {
                snap.find_next(&m, from.line, byte, ctl)
            })
        });
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::Find,
            ctl,
            rx,
            version,
            label: "Searching",
            started: Instant::now(),
        });
    }

    fn start_count(&mut self, ctx: &egui::Context) {
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        self.cancel_jobs(tab.id, |k| matches!(k, JobKind::Count));
        let snap = Snapshot::of(&tab.doc);
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            let mut n = 0u64;
            snap.for_each_match(&m, ctl, |_| {
                n += 1;
                true
            });
            JobOut::Counted(n, ctl.cancel.load(Ordering::SeqCst))
        });
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::Count,
            ctl,
            rx,
            version,
            label: "Counting",
            started: Instant::now(),
        });
    }

    fn start_extract(&mut self, ctx: &egui::Context) {
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let what = self.find.describe(&self.registry);
        let unique = self.settings.extract_unique;
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let title = format!("{what} — from {}", tab.doc.title);
        let snap = Snapshot::of(&tab.doc);
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            let mut items = Vec::new();
            let mut seen = HashSet::new();
            let mut bytes = 0usize;
            let mut total = 0u64;
            let mut truncated = false;
            snap.for_each_match(&m, ctl, |b| {
                total += 1;
                let s = String::from_utf8_lossy(b).into_owned();
                if unique && !seen.insert(s.clone()) {
                    return true;
                }
                bytes += s.len();
                items.push(s);
                if items.len() >= 20_000_000 || bytes > (1 << 30) {
                    truncated = true;
                    return false;
                }
                true
            });
            JobOut::Extracted(items, truncated, total)
        });
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::Extract { title },
            ctl,
            rx,
            version,
            label: "Extracting",
            started: Instant::now(),
        });
    }

    fn replace_one(&mut self, ctx: &egui::Context) {
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let repl = self.find.replace.clone();
        let now = self.now;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let (s, e) = tab.view.selection();
        if s != e && s.line == e.line {
            let line = tab.doc.line(s.line).into_owned();
            let (sb, eb) = (char_to_byte(&line, s.col), char_to_byte(&line, e.col));
            if m.matches_exactly(&line, sb, eb) {
                let r = m.replacement_at(&line, sb, &repl);
                let sel = (tab.view.anchor, tab.view.cursor);
                match tab.doc.replace(s, e, &r, EditKind::Other, sel, now) {
                    Ok(p) => tab.view.select(p, p, Reveal::Nearest),
                    Err(err) => {
                        self.toast_err(err);
                        return;
                    }
                }
            }
        }
        self.start_find(false, ctx);
    }

    fn start_replace_all(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let repl = self.find.replace.clone();
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        if tab.doc.busy.is_some() {
            return;
        }
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            tab.pending = Some(Pending::ReplaceAll);
            self.toast_info(
                "Loading the entire file first — Replace All will run as soon as it's done.",
            );
            return;
        }
        tab.doc.busy = Some("replacing".into());
        let snap = Snapshot::of(&tab.doc);
        let (version, lines) = (snap.version, snap.line_count());
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Replaced(snap.replace_all(&m, &repl, ctl), version, lines)
        });
        let id = tab.id;
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::ReplaceAll,
            ctl,
            rx,
            version,
            label: "Replacing",
            started: Instant::now(),
        });
    }

    fn run_module_action(&mut self, action: ModuleAction, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            self.toast_info("Open a file first.");
            return;
        }
        let id = match &action {
            ModuleAction::Breakdown(id)
            | ModuleAction::Find(id)
            | ModuleAction::Count(id)
            | ModuleAction::Extract(id)
            | ModuleAction::Mask(id) => id.clone(),
        };
        if let ModuleAction::Breakdown(_) = action {
            self.open_insights(&id, true);
            return;
        }
        self.find.module = Some(id.clone());
        self.find.group = None;
        self.find.open = true;
        match action {
            ModuleAction::Breakdown(_) => {}
            ModuleAction::Find(_) => self.start_find(false, ctx),
            ModuleAction::Count(_) => self.start_count(ctx),
            ModuleAction::Extract(_) => self.start_extract(ctx),
            ModuleAction::Mask(_) => {
                if let Some(i) = self.registry.by_id(&id) {
                    self.find.replace = self.registry.entries[i]
                        .module
                        .replacement()
                        .unwrap_or_else(|| "[REDACTED]".into());
                }
                self.find.replace_open = true;
                self.toast_info("Check the replacement text, then press “Replace all”.");
            }
        }
    }

    // ------------------------------------------------------------------
    // Per-frame bookkeeping

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.jobs.len() {
            match self.jobs[i].rx.try_recv() {
                Ok(out) => {
                    let job = self.jobs.remove(i);
                    self.finish_job(job, out, ctx);
                }
                Err(TryRecvError::Empty) => i += 1,
                Err(TryRecvError::Disconnected) => {
                    let job = self.jobs.remove(i);
                    if let Some(t) = self.tab_index(job.tab) {
                        self.tabs[t].doc.busy = None;
                    }
                    self.toast_err(format!("{} failed unexpectedly.", job.label));
                }
            }
        }
    }

    fn finish_job(&mut self, job: Job, out: JobOut, ctx: &egui::Context) {
        let cancelled = job.ctl.cancel.load(Ordering::SeqCst);
        let idx = self.tab_index(job.tab);
        match (job.kind, out) {
            (JobKind::Save { path, tmp }, JobOut::Saved(r)) => {
                self.finish_save(job.tab, path, tmp, r, ctx)
            }
            (JobKind::Find, JobOut::Found(hit)) => {
                let Some(idx) = idx else { return };
                if cancelled {
                    return;
                }
                let what = self.find.describe(&self.registry);
                let tab = &mut self.tabs[idx];
                if tab.doc.version != job.version {
                    return;
                }
                match hit {
                    Some(Hit::At { line, start, end }) if line < tab.doc.line_count() => {
                        let a = tab.doc.byte_to_col(line, start);
                        let b = tab.doc.byte_to_col(line, end);
                        tab.view
                            .select(Pos::new(line, a), Pos::new(line, b), Reveal::Center);
                        if let (true, Some(t)) = (tab.table_mode, tab.table.as_mut()) {
                            t.select_line(&tab.doc, line, start);
                        }
                        if self.active == idx && !self.find.open {
                            tab.view.request_focus = true;
                        }
                    }
                    Some(Hit::Beyond { start, end }) => {
                        tab.pending_hit = Some((start, end));
                        if let Some(src) = &tab.doc.source {
                            src.want_offset(end, ctx);
                        }
                        self.toast_info("Found a match past the loaded part — loading up to it…");
                    }
                    _ => self.toast(format!("No matches for {what}."), theme::WARN),
                }
            }
            (JobKind::Count, JobOut::Counted(n, was_cancelled)) => {
                if was_cancelled {
                    return;
                }
                let what = self.find.describe(&self.registry);
                self.find.last_count = Some((n, true));
                let secs = job.started.elapsed().as_secs_f64();
                self.toast_ok(format!(
                    "{} matches for {what} ({secs:.1}s).",
                    fmt_int(n as usize)
                ));
            }
            (JobKind::Extract { title }, JobOut::Extracted(items, truncated, total)) => {
                if cancelled {
                    return;
                }
                if items.is_empty() {
                    self.toast(
                        format!(
                            "Nothing to extract: no matches for {}.",
                            self.find.describe(&self.registry)
                        ),
                        theme::WARN,
                    );
                    return;
                }
                let n = items.len();
                self.push_tab(Document::from_lines(title, items), None);
                let mut msg = format!(
                    "Extracted {} ({} total matches).",
                    fmt_int(n),
                    fmt_int(total as usize)
                );
                if truncated {
                    msg.push_str(" Stopped early: result limit reached.");
                }
                self.toast_ok(msg);
            }
            (JobKind::ReplaceAll, JobOut::Replaced(res, version, lines)) => {
                let Some(idx) = idx else { return };
                let now = self.now;
                let tab = &mut self.tabs[idx];
                tab.doc.busy = None;
                match res {
                    Some((pieces, n, skipped)) if tab.doc.version == version => {
                        if n > 0 {
                            let sel = (tab.view.anchor, tab.view.cursor);
                            tab.doc.replace_prefix(lines, pieces, sel, now);
                        }
                        let mut msg = format!("Replaced {} occurrences.", fmt_int(n as usize));
                        if skipped > 0 {
                            msg.push_str(&format!(
                                " Skipped {skipped} read-only segments of huge lines."
                            ));
                        }
                        self.toast_ok(msg);
                    }
                    Some(_) => self.toast_err(
                        "The document changed during Replace All; nothing was replaced.",
                    ),
                    None if cancelled => self.toast_info("Replace All cancelled."),
                    None => self.toast_err("Replace All could not run on a partially loaded file."),
                }
            }
            (JobKind::TableView { generation }, JobOut::TableView(res, lines)) => {
                let Some(idx) = idx else { return };
                let Some(st) = self.tabs[idx].table.as_mut() else {
                    return;
                };
                if st.generation != generation {
                    return; // a newer filter/sort superseded this one
                }
                st.busy = false;
                match res {
                    Ok(view) => {
                        st.view = Some(Arc::new(view));
                        st.view_lines = lines;
                        st.sel.0 = 0;
                    }
                    Err(e) if !cancelled => self.toast_err(e),
                    Err(_) => {}
                }
            }
            (JobKind::ExportView { title, syntax }, JobOut::Exported(res)) => {
                if let Some(lines) = res.filter(|l| !l.is_empty()) {
                    let n = lines.len();
                    self.push_tab(Document::from_lines(title, lines), syntax);
                    self.toast_ok(format!(
                        "Opened {} lines as a new tab. Save it with Ctrl+S.",
                        fmt_int(n)
                    ));
                }
            }
            (JobKind::Jq { title, keep }, JobOut::Jq(r)) => {
                if cancelled {
                    return;
                }
                if r.lines.is_empty() {
                    match r.error {
                        Some(e) => self.toast_err(e),
                        None => self.toast(
                            format!(
                                "jq produced no output ({} JSON inputs).",
                                fmt_int(r.inputs as usize)
                            ),
                            theme::WARN,
                        ),
                    }
                    return;
                }
                let syntax = if keep {
                    idx.and_then(|i| self.tabs[i].syntax.clone())
                } else {
                    Some("json".into())
                };
                let n = r.lines.len();
                self.push_tab(Document::from_lines(title, r.lines), syntax);
                let mut msg = format!(
                    "{} result lines from {} JSON inputs.",
                    fmt_int(n),
                    fmt_int(r.inputs as usize)
                );
                if r.skipped > 0 {
                    msg.push_str(&format!(
                        " {} lines weren't JSON.",
                        fmt_int(r.skipped as usize)
                    ));
                }
                if r.truncated {
                    msg.push_str(" Stopped early: result limit reached.");
                }
                match r.error {
                    Some(e) => self.toast(format!("{msg} Some inputs failed: {e}"), theme::WARN),
                    None => self.toast_ok(msg),
                }
            }
            (
                JobKind::Heatmap {
                    key,
                    version,
                    layers,
                },
                JobOut::Heatmap(map),
            ) => {
                if !cancelled {
                    self.finish_heatmap(job.tab, key, version, layers, map);
                }
            }
            (JobKind::Convert { title, syntax }, JobOut::Converted(res, path)) => {
                if !cancelled {
                    self.finish_convert(title, syntax, res, path, ctx);
                } else if let Some(p) = path {
                    let _ = std::fs::remove_file(p);
                }
            }
            (JobKind::Breakdown(key), JobOut::Breakdown(b)) => {
                if cancelled {
                    self.insights_job_cancelled(&key);
                } else {
                    self.finish_breakdown(key, b);
                }
            }
            (
                JobKind::Filter {
                    label,
                    syntax,
                    source_title,
                },
                JobOut::Filtered(res, source_lines),
            ) => {
                if cancelled {
                    return;
                }
                let Some(f) = res else {
                    self.toast_err(
                        "Filtering needs the whole file loaded; try again once loading finishes.",
                    );
                    return;
                };
                let header_only = f.lines.len() == 1
                    && f.lines[0] == 0
                    && matches!(syntax.as_deref(), Some("csv" | "tsv" | "psv"));
                if f.lines.is_empty() || header_only {
                    self.toast(format!("No lines match {label}."), theme::WARN);
                    return;
                }
                let n = f.lines.len();
                let truncated = f.truncated;
                self.push_tab(
                    Document::from_lines(format!("{label} — {source_title}"), f.text),
                    syntax,
                );
                let tab = self.tabs.last_mut().unwrap();
                tab.origin = Some(FilterOrigin {
                    source: job.tab,
                    source_title,
                    lines: Arc::new(f.lines),
                    version: tab.doc.version,
                    label,
                    source_lines,
                });
                let mut msg = format!("{} of {} lines match.", fmt_int(n), fmt_int(source_lines));
                if truncated {
                    msg.push_str(" Stopped early: result limit reached.");
                }
                self.toast_ok(msg);
            }
            _ => {}
        }
    }

    fn start_filter(&mut self, ctx: &egui::Context) {
        let Some(m) = self.matcher_or_toast() else {
            return;
        };
        let label = self.find.describe(&self.registry);
        self.start_filter_with(self.active, m, label, ctx);
    }

    /// Open a new tab with every line of tab `idx` that `m` matches.
    fn start_filter_with(&mut self, idx: usize, m: Matcher, label: String, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            tab.pending = Some(Pending::Filter {
                matcher: Box::new(m),
                label,
            });
            self.toast_info(
                "Loading the entire file first — the filter will run as soon as it's done.",
            );
            return;
        }
        let header = tab.is_delimited();
        let snap = Snapshot::of(&tab.doc);
        let total = snap.line_count();
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Filtered(snap.filter_lines(&m, header, ctl), total)
        });
        let kind = JobKind::Filter {
            label,
            syntax: tab.syntax.clone(),
            source_title: tab.doc.title.clone(),
        };
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind,
            ctl,
            rx,
            version,
            label: "Filtering",
            started: Instant::now(),
        });
    }

    /// Go from a line of a filtered tab back to the same line in its source.
    fn jump_to_source(&mut self, idx: usize, line: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let Some(origin) = &tab.origin else { return };
        let Some(&src_line) = tab.origin_lines().and_then(|l| l.get(line)) else {
            self.toast_info(
                "This filtered copy was edited, so its lines no longer map to the source.",
            );
            return;
        };
        let Some(si) = self.tab_index(origin.source) else {
            self.toast_info(format!("{} is no longer open.", origin.source_title));
            return;
        };
        self.active = si;
        let t = &mut self.tabs[si];
        let end = Pos::new(src_line, t.doc.line_chars(src_line));
        t.view.select(Pos::new(src_line, 0), end, Reveal::Center);
        t.view.request_focus = true;
    }

    // ------------------------------------------------------------------
    // Bookmarks, error navigation and ad-hoc highlights

    fn toggle_bookmark(&mut self) {
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let line = tab.view.cursor.line;
        if !tab.bookmarks.remove(&line) {
            tab.bookmarks.insert(line);
        }
        self.sync_bookmarks(idx);
    }

    /// Mirrors a tab's bookmarks into `settings.bookmarks` so they survive a reload.
    /// Only tabs backed by a real file are tracked (untitled buffers are ephemeral).
    fn sync_bookmarks(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let Some(path) = tab.doc.path.clone() else {
            return;
        };
        let marks: Vec<usize> = tab.bookmarks.iter().copied().collect();
        let list = &mut self.settings.bookmarks;
        list.retain(|(p, _)| *p != path);
        if !marks.is_empty() {
            list.insert(0, (path, marks));
            list.truncate(300);
        }
    }

    fn jump_bookmark(&mut self, backward: bool) {
        let Some(idx) = (!self.tabs.is_empty()).then_some(self.active) else {
            return;
        };
        if self.tabs[idx].bookmarks.is_empty() {
            self.toast_info("No bookmarks in this file. Press Ctrl+F2 to add one.");
            return;
        }
        let tab = &mut self.tabs[idx];
        let cur = tab.view.cursor.line;
        let target = if backward {
            tab.bookmarks
                .range(..cur)
                .next_back()
                .or_else(|| tab.bookmarks.iter().next_back())
        } else {
            tab.bookmarks
                .range(cur + 1..)
                .next()
                .or_else(|| tab.bookmarks.iter().next())
        };
        if let Some(&line) = target {
            let end = Pos::new(line, tab.doc.line_chars(line));
            tab.view.select(Pos::new(line, 0), end, Reveal::Center);
            tab.view.request_focus = true;
        }
    }

    /// Jump to the next/previous hit of the built-in log-level detector, restricted
    /// to `keys` (e.g. `["ERROR", "FATAL"]`). Reuses the same find machinery as
    /// clicking "Find next" on a detector chip, scoped to a breakdown group.
    fn jump_to_level(&mut self, keys: &[&str], label: &str, backward: bool, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            return;
        }
        self.find.module = Some("log-level".into());
        self.find.group = Some(GroupSel {
            module: "log-level".into(),
            label: label.to_string(),
            keys: Some(keys.iter().map(|s| s.to_string()).collect()),
            category: None,
        });
        self.find.open = true;
        self.start_find(backward, ctx);
    }

    /// Toggles an ad-hoc highlight for the current selection (or the word under the
    /// cursor, if there's no selection), cycling through `HIGHLIGHT_COLORS`.
    fn toggle_highlight_at_cursor(&mut self) {
        let Some(idx) = (!self.tabs.is_empty()).then_some(self.active) else {
            return;
        };
        let tab = &self.tabs[idx];
        let (s, e) = tab.view.selection();
        let (a, b) = if s != e && s.line == e.line {
            (s, e)
        } else {
            editor::word_at(&tab.doc, tab.view.cursor)
        };
        if a == b {
            return;
        }
        let Some(term) = tab.doc.text_range(a, b, 500).filter(|t| !t.is_empty()) else {
            return;
        };
        let tab = &mut self.tabs[idx];
        if let Some(pos) = tab
            .highlights
            .iter()
            .position(|(t, _)| t.eq_ignore_ascii_case(&term))
        {
            tab.highlights.remove(pos);
            return;
        }
        if tab.highlights.len() >= HIGHLIGHT_COLORS.len() {
            self.toast_info(format!(
                "Up to {} highlighted terms at once — clear one first.",
                HIGHLIGHT_COLORS.len()
            ));
            return;
        }
        let color = HIGHLIGHT_COLORS[tab.highlights.len()];
        tab.highlights.push((term, color));
    }

    fn clear_highlights(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.highlights.clear();
        }
    }

    fn filter_banner(&mut self, ui: &mut Ui) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(o) = &tab.origin else { return };
        let mapped = tab.origin_lines().is_some();
        let mut jump = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(
                RichText::new("FILTERED")
                    .strong()
                    .size(11.5)
                    .color(theme::OK),
            );
            ui.label(
                RichText::new(format!(
                    "{} of {} lines of {} · {}",
                    fmt_int(o.lines.len()),
                    fmt_int(o.source_lines),
                    o.source_title,
                    o.label
                ))
                .color(theme::TEXT),
            );
            if mapped {
                ui.label(
                    RichText::new(
                        "Line numbers are from the original file. Double-click one to jump there.",
                    )
                    .size(12.5)
                    .color(theme::TEXT_DIM),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(mapped, Button::new("Jump to source"))
                    .clicked()
                {
                    jump = true;
                }
            });
        });
        ui.add_space(6.0);
        if jump {
            let line = self.tabs[self.active].view.cursor.line;
            self.jump_to_source(self.active, line);
        }
    }

    fn tick_tabs(&mut self, ctx: &egui::Context) {
        let mut msgs = Vec::new();
        let mut replace_all = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Some(src) = tab.doc.source.clone() else {
                continue;
            };
            let loading = src.is_loading();
            if loading && !tab.was_loading {
                tab.load_started = Some(Instant::now());
            }
            if !loading
                && tab.was_loading
                && src.is_complete()
                && let Some(t) = tab.load_started.take()
            {
                let secs = t.elapsed().as_secs_f64();
                if secs > 1.0 {
                    msgs.push(format!(
                        "{} fully loaded: {} lines in {secs:.1}s.",
                        tab.doc.title,
                        fmt_int(src.available_lines())
                    ));
                }
            }
            tab.was_loading = loading;

            if let Some((s, e)) = tab.pending_hit
                && src.covers_offset(e.saturating_sub(1).max(s))
            {
                tab.pending_hit = None;
                let orig = src.line_of_offset(s);
                if let Some(line) = tab.doc.tail_doc_line(orig) {
                    let base = src.span(orig).start;
                    let a = tab.doc.byte_to_col(line, (s - base) as usize);
                    let b = tab.doc.byte_to_col(line, (e - base) as usize);
                    tab.view
                        .select(Pos::new(line, a), Pos::new(line, b), Reveal::Center);
                }
            }
            if let Some(line) = tab.pending_goto {
                if line < tab.doc.line_count() {
                    tab.pending_goto = None;
                    tab.view
                        .select(Pos::new(line, 0), Pos::new(line, 0), Reveal::Center);
                } else if src.is_complete() {
                    tab.pending_goto = None;
                    let p = tab.doc.end_pos();
                    tab.view.select(p, p, Reveal::Center);
                } else {
                    src.want_lines(tab.doc.orig_lines_needed(line), ctx);
                }
            }
            if tab.pending.is_some() && src.is_complete() && tab.doc.busy.is_none() {
                replace_all.push((i, tab.pending.take().unwrap()));
            }
        }
        for m in msgs {
            self.toast_ok(m);
        }
        for (i, pending) in replace_all {
            match pending {
                Pending::ReplaceAll => self.start_replace_all(i, ctx),
                Pending::TableView => self.start_table_view(i, ctx),
                Pending::Jq => self.start_jq_run(i, ctx),
                Pending::Convert { spec, path } => self.start_convert(i, spec, path, ctx),
                Pending::Filter { matcher, label } => {
                    self.start_filter_with(i, *matcher, label, ctx)
                }
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use commands::Cmd;
        let cmd = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        // Most specific first: shortcuts ignore extra Shift/Alt when matching.
        let table: [(Modifiers, Key, Cmd); 30] = [
            (cs, Key::S, Cmd::SaveAs),
            (cmd, Key::S, Cmd::Save),
            (cmd, Key::N, Cmd::NewFile),
            (cmd, Key::O, Cmd::Open),
            (cmd, Key::W, Cmd::CloseTab),
            (cmd, Key::F, Cmd::Find),
            (cmd, Key::H, Cmd::Replace),
            (Modifiers::SHIFT, Key::F3, Cmd::FindPrev),
            (Modifiers::NONE, Key::F3, Cmd::FindNext),
            (cs, Key::F3, Cmd::ClearHighlights),
            (cmd, Key::F3, Cmd::ToggleHighlight),
            (cmd, Key::F2, Cmd::ToggleBookmark),
            (Modifiers::SHIFT, Key::F2, Cmd::PrevBookmark),
            (Modifiers::NONE, Key::F2, Cmd::NextBookmark),
            (Modifiers::ALT, Key::F4, Cmd::Exit),
            (Modifiers::SHIFT, Key::F4, Cmd::PrevError),
            (Modifiers::NONE, Key::F4, Cmd::NextError),
            (cmd, Key::G, Cmd::GoToLine),
            (cmd, Key::L, Cmd::LoadEntire),
            (cs, Key::M, Cmd::ToggleModules),
            (cs, Key::P, Cmd::Palette),
            (cs, Key::T, Cmd::ToggleTable),
            (cs, Key::J, Cmd::ToggleJson),
            (cs, Key::E, Cmd::ToggleInsights),
            (cmd, Key::Comma, Cmd::Settings),
            (cmd, Key::Equals, Cmd::ZoomIn),
            (cmd, Key::Plus, Cmd::ZoomIn),
            (cmd, Key::Minus, Cmd::ZoomOut),
            (cmd, Key::Num0, Cmd::ZoomReset),
            (Modifiers::NONE, Key::F1, Cmd::Shortcuts),
        ];
        for (m, k, c) in table {
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k))) {
                self.run(c, ctx);
            }
        }
        if !self.tabs.is_empty() {
            let n = self.tabs.len();
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cs, Key::Tab))) {
                self.active = (self.active + n - 1) % n;
            } else if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Tab))) {
                self.active = (self.active + 1) % n;
            }
        }
        let z = ctx.input(|i| i.zoom_delta());
        if z != 1.0 {
            self.zoom(z);
        }
        // Esc closes the innermost thing: palette/drawer handle their own, then the find bar.
        let overlay =
            self.palette.is_some() || (self.modules_open && !self.settings.modules_pinned);
        if self.find.open && !overlay && ctx.input(|i| i.key_pressed(Key::Escape)) {
            let has_sel = self
                .tabs
                .get(self.active)
                .is_some_and(|t| t.view.has_selection());
            if !has_sel {
                self.close_find();
            }
        }
    }

    fn close_find(&mut self) {
        self.find.open = false;
        for j in &self.jobs {
            if matches!(
                j.kind,
                JobKind::Find | JobKind::Count | JobKind::Extract { .. }
            ) {
                j.ctl.cancel.store(true, Ordering::SeqCst);
            }
        }
        if let Some(t) = self.active_tab() {
            t.view.request_focus = true;
        }
    }

    fn zoom(&mut self, f: f32) {
        self.settings.font_size = (self.settings.font_size * f).clamp(7.0, 48.0);
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            if !p.as_os_str().is_empty() {
                self.open_path(p, ctx);
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_quit {
            if self.tabs.iter().any(|t| t.doc.is_dirty()) {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.dialog = Dialog::ConfirmQuit;
            } else {
                for t in &self.tabs {
                    if let Some(s) = &t.doc.source {
                        s.cancel();
                    }
                }
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.tabs.get(self.active) {
            Some(t) => format!(
                "{}{} — Mammoth",
                if t.doc.is_dirty() { "● " } else { "" },
                t.doc.title
            ),
            None => "Mammoth".to_string(),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ------------------------------------------------------------------
    // UI pieces

    fn run_editor_command(&mut self, c: Command, ctx: &egui::Context) {
        let now = self.now;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.view.request_focus = true;
            if let Err(e) = editor::run_command(ctx, &mut tab.doc, &mut tab.view, c, now) {
                self.toast_err(e);
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut activate = None;
        let mut close = None;
        let mut new = false;
        ScrollArea::horizontal()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let active = i == self.active;
                        let font = FontId::proportional(13.5);
                        let color = if active { theme::TEXT } else { theme::TEXT_DIM };
                        let galley =
                            ui.painter()
                                .layout_no_wrap(tab.doc.title.clone(), font, color);
                        let w = (galley.size().x + 50.0).clamp(96.0, 320.0);
                        let (rect, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
                        let p = ui.painter();
                        let hovered = resp.hovered();
                        let bg = if active {
                            theme::EDITOR_BG
                        } else if hovered {
                            theme::PANEL_BG_2
                        } else {
                            theme::APP_BG
                        };
                        p.rect_filled(
                            rect,
                            CornerRadius {
                                nw: 8,
                                ne: 8,
                                sw: 0,
                                se: 0,
                            },
                            bg,
                        );
                        if active {
                            p.hline(
                                rect.x_range().shrink(6.0),
                                rect.top() + 1.0,
                                Stroke::new(2.0, theme::ACCENT),
                            );
                        }
                        let text_pos =
                            pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0);
                        p.with_clip_rect(
                            rect.shrink2(vec2(6.0, 0.0)).with_max_x(rect.right() - 30.0),
                        )
                        .galley(text_pos, galley, color);

                        let cr = egui::Rect::from_center_size(
                            pos2(rect.right() - 17.0, rect.center().y),
                            vec2(18.0, 18.0),
                        );
                        let cresp = ui.interact(cr, Id::new(("tab-close", tab.id)), Sense::click());
                        let p = ui.painter();
                        if cresp.hovered() {
                            p.rect_filled(cr, 4.0, theme::BORDER);
                        }
                        let busy = tab.doc.busy.is_some()
                            || tab.doc.source.as_ref().is_some_and(|s| s.is_loading());
                        if busy && !cresp.hovered() {
                            let t = ui.input(|i| i.time) as f32;
                            let a = t * 6.0;
                            let c = cr.center();
                            p.circle_stroke(c, 5.0, Stroke::new(1.5, theme::BORDER));
                            p.line_segment(
                                [c, c + vec2(a.cos(), a.sin()) * 5.0],
                                Stroke::new(1.5, theme::ACCENT),
                            );
                        } else if tab.doc.is_dirty() && !cresp.hovered() {
                            p.circle_filled(cr.center(), 4.0, theme::LINENO_ACTIVE);
                        } else if hovered || active || cresp.hovered() {
                            let c = cr.center();
                            let s = Stroke::new(
                                1.4,
                                if cresp.hovered() {
                                    Color32::WHITE
                                } else {
                                    theme::TEXT_DIM
                                },
                            );
                            p.line_segment([c - vec2(4.0, 4.0), c + vec2(4.0, 4.0)], s);
                            p.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);
                        }
                        if cresp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            activate = Some(i);
                        }
                        if let Some(path) = &tab.doc.path {
                            resp.on_hover_text(path.display().to_string());
                        }
                    }
                    let plus = ui.add(
                        Button::new(RichText::new("+").size(17.0).color(theme::TEXT_DIM))
                            .frame(false),
                    );
                    if plus.on_hover_text("New tab (Ctrl+N)").clicked() {
                        new = true;
                    }
                });
            });
        if let Some(i) = activate {
            self.active = i;
            self.tabs[i].view.request_focus = true;
        }
        if let Some(i) = close {
            self.request_close(i);
        }
        if new {
            self.new_tab();
        }
    }

    /// Small removable chips for ad-hoc highlighted terms (Ctrl+F3).
    fn highlight_chips(&mut self, ui: &mut Ui) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.highlights.is_empty() {
            return;
        }
        let mut remove = None;
        for (i, (term, color)) in tab.highlights.iter().enumerate() {
            let label = RichText::new(format!("{term}  ×"))
                .size(12.0)
                .color(Color32::WHITE);
            let b = Button::new(label)
                .fill(color.gamma_multiply(0.35))
                .corner_radius(10.0);
            if ui
                .add(b)
                .on_hover_text("Highlighted — click to remove")
                .clicked()
            {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            self.tabs[self.active].highlights.remove(i);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut toggle_table = false;
        let mut toggle_json = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let small = |t: String| RichText::new(t).size(12.5).color(theme::TEXT_DIM);
            if let Some(toast) = &self.toast {
                if toast.until > self.now {
                    ui.label(RichText::new(&toast.text).size(12.5).color(toast.color));
                } else {
                    self.toast = None;
                }
            }
            if self.toast.is_none()
                && let Some(tab) = self.tabs.get(self.active)
            {
                let c = tab.view.cursor;
                let mut s = format!("Ln {}, Col {}", fmt_int(c.line + 1), c.col + 1);
                if let (true, Some(t)) = (tab.table_mode, &tab.table) {
                    let rows = t.row_count(&tab.doc);
                    s = format!(
                        "Row {} of {}, column {}",
                        fmt_int((t.sel.0 + 1).min(rows)),
                        fmt_int(rows),
                        t.sel.1 + 1
                    );
                }
                let (a, b) = tab.view.selection();
                if a != b {
                    if a.line == b.line {
                        s.push_str(&format!("  ({} selected)", b.col - a.col));
                    } else {
                        s.push_str(&format!(
                            "  ({} lines selected)",
                            fmt_int(b.line - a.line + 1)
                        ));
                    }
                }
                ui.label(small(s));
            }
            if !self.tabs.is_empty() {
                ui.spacing_mut().item_spacing.x = 6.0;
                self.detector_chips(ui, ctx);
                self.highlight_chips(ui);
                ui.spacing_mut().item_spacing.x = 14.0;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for job in &self.jobs {
                    if job.started.elapsed() < Duration::from_millis(150) {
                        continue;
                    }
                    if icons::button(ui, Icon::Close, "Cancel").clicked() {
                        job.ctl.cancel.store(true, Ordering::SeqCst);
                    }
                    let frac = job.ctl.fraction();
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(90.0)
                            .desired_height(6.0)
                            .fill(theme::ACCENT),
                    );
                    ui.label(small(format!("{}… {:.0}%", job.label, frac * 100.0)));
                }
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                let doc = &tab.doc;
                let lines = doc.line_count();
                let partial = !doc.is_fully_loaded();
                ui.label(small(format!(
                    "{}{} lines",
                    fmt_int(lines),
                    if partial { "+" } else { "" }
                )))
                .on_hover_text(if partial {
                    "More lines exist beyond what is loaded."
                } else {
                    "Total lines"
                });
                if let Some(src) = &doc.source {
                    ui.label(small(fmt_bytes(src.len())));
                }
                if !tab.bookmarks.is_empty() {
                    ui.label(RichText::new(format!("★ {}", tab.bookmarks.len()))
                        .size(12.5)
                        .color(theme::BOOKMARK))
                    .on_hover_text(
                        "Bookmarks — F2 / Shift+F2 to jump, Ctrl+F2 to toggle on this line",
                    );
                }
                ui.label(small(doc.eol.label().to_string()));
                ui.label(small(if doc.bom {
                    "UTF-8 BOM".into()
                } else {
                    "UTF-8".into()
                }));

                let current = tab
                    .syntax
                    .as_ref()
                    .and_then(|id| self.registry.by_id(id))
                    .map(|i| self.registry.entries[i].module.name().to_string());
                ComboBox::from_id_salt("syntax")
                    .selected_text(small(current.unwrap_or_else(|| "Plain text".into())))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut tab.syntax, None, "Plain text");
                        for (_, e) in self.registry.syntaxes() {
                            let id = Some(e.module.id().to_string());
                            ui.selectable_value(&mut tab.syntax, id, e.module.name());
                        }
                    });
                if doc.busy.is_some() {
                    ui.label(RichText::new("read-only").size(12.5).color(theme::WARN));
                }
                if matches!(tab.syntax.as_deref(), Some("json" | "log")) {
                    let on = self.json.open;
                    let b = Button::new(RichText::new("{ } JSON").size(12.0).color(if on {
                        Color32::WHITE
                    } else {
                        theme::TEXT_DIM
                    }))
                    .fill(if on {
                        theme::ACCENT_DIM
                    } else {
                        theme::PANEL_BG_2
                    });
                    if ui
                        .add(b)
                        .on_hover_text("JSON inspector (Ctrl+Shift+J)")
                        .clicked()
                    {
                        toggle_json = true;
                    }
                }
                if tab.is_delimited() {
                    let table = tab.table_mode;
                    let seg = |ui: &mut Ui, text: &str, on: bool| {
                        let b = Button::new(RichText::new(text).size(12.0).color(if on {
                            Color32::WHITE
                        } else {
                            theme::TEXT_DIM
                        }))
                        .fill(if on {
                            theme::ACCENT_DIM
                        } else {
                            theme::PANEL_BG_2
                        })
                        .min_size(vec2(46.0, 20.0));
                        ui.add(b)
                    };
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if seg(ui, "Table", table)
                        .on_hover_text("Table view (Ctrl+Shift+T)")
                        .clicked()
                        && !table
                    {
                        toggle_table = true;
                    }
                    if seg(ui, "Text", !table).clicked() && table {
                        toggle_table = true;
                    }
                    ui.spacing_mut().item_spacing.x = 14.0;
                }
            });
        });
        if toggle_table {
            self.toggle_table();
        }
        if toggle_json {
            self.toggle_json();
        }
    }

    fn banner(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(src) = tab.doc.source.clone() else {
            return;
        };
        let p = src.progress();
        let pct = p.scanned as f64 / p.total.max(1) as f64;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if p.running {
                ui.add(egui::Spinner::new().size(14.0).color(theme::ACCENT));
                let mut text = format!(
                    "Loading…  {:.1}%  ·  {} lines",
                    pct * 100.0,
                    fmt_int(p.lines)
                );
                if p.rate > 0.0 {
                    let eta = (p.total - p.scanned) as f64 / p.rate;
                    text.push_str(&format!(
                        "  ·  {}/s  ·  {} left",
                        fmt_bytes(p.rate as u64),
                        fmt_secs(eta)
                    ));
                }
                ui.label(RichText::new(text).color(theme::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Pause").clicked() {
                        src.pause();
                    }
                });
            } else {
                ui.label(
                    RichText::new("PREVIEW")
                        .strong()
                        .size(11.5)
                        .color(theme::ACCENT),
                );
                ui.label(
                    RichText::new(format!(
                        "{} lines of this {} file are loaded ({:.1}%).",
                        fmt_int(p.lines),
                        fmt_bytes(p.total),
                        pct * 100.0
                    ))
                    .color(theme::TEXT),
                );
                ui.label(
                    RichText::new("Find, Count and Extract still scan the whole file.")
                        .color(theme::TEXT_DIM)
                        .size(12.5),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = Button::new(
                        RichText::new("Load entire file")
                            .color(Color32::WHITE)
                            .strong(),
                    )
                    .fill(theme::ACCENT)
                    .corner_radius(6.0);
                    if ui.add(b).on_hover_text("Ctrl+L").clicked() {
                        src.load_all(ctx);
                    }
                    if ui.button("+100k lines").clicked() {
                        src.want_lines(p.lines + 100_000, ctx);
                    }
                });
            }
        });
        let r = ui.available_rect_before_wrap();
        let bar = egui::Rect::from_min_size(pos2(r.left(), r.top() + 4.0), vec2(r.width(), 3.0));
        ui.painter().rect_filled(bar, 2.0, theme::BORDER);
        ui.painter().rect_filled(
            bar.with_max_x(bar.left() + bar.width() * pct as f32),
            2.0,
            theme::ACCENT,
        );
        ui.add_space(8.0);
    }

    fn find_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let matcher = self.find.matcher(&self.registry);
        let mut do_find: Option<bool> = None;
        let mut do_count = false;
        let mut do_extract = false;
        let mut do_filter = false;
        let mut do_replace = false;
        let mut do_replace_all = false;
        let mut close = false;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let toggle_btn = |ui: &mut Ui, on: &mut bool, label: &str, tip: &str| {
                let text = RichText::new(label).monospace().color(if *on {
                    Color32::WHITE
                } else {
                    theme::TEXT_DIM
                });
                let b = Button::new(text)
                    .fill(if *on {
                        theme::ACCENT_DIM
                    } else {
                        Color32::TRANSPARENT
                    })
                    .min_size(vec2(28.0, 24.0));
                if ui.add(b).on_hover_text(tip).clicked() {
                    *on = !*on;
                }
            };
            let arrow = if self.find.replace_open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            if icons::button(ui, arrow, "Toggle replace (Ctrl+H)").clicked() {
                self.find.replace_open = !self.find.replace_open;
            }

            let module_mode = self.find.module.is_some();
            let hint = if module_mode {
                "searching with a module"
            } else {
                "Find"
            };
            let mut te = TextEdit::singleline(&mut self.find.query)
                .hint_text(hint)
                .desired_width(280.0)
                .font(egui::TextStyle::Monospace);
            if matcher.as_ref().is_err_and(|e| !e.is_empty()) && !module_mode {
                te = te.text_color(theme::ERROR);
            }
            let r = ui.add_enabled(!module_mode, te);
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                do_find = Some(ui.input(|i| i.modifiers.shift));
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
            if !module_mode {
                toggle_btn(ui, &mut self.find.case, "Aa", "Match case");
                toggle_btn(ui, &mut self.find.word, "ab", "Whole word");
                toggle_btn(ui, &mut self.find.regex, ".*", "Regular expression");
            }

            let current = self
                .find
                .module
                .as_ref()
                .and_then(|id| self.registry.by_id(id))
                .map(|i| self.registry.entries[i].module.name().to_string());
            ComboBox::from_id_salt("find-module")
                .selected_text(current.unwrap_or_else(|| "Text".into()))
                .width(150.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.find.module, None, "Text");
                    for (_, e) in self.registry.detectors() {
                        if e.error.is_none() {
                            let id = Some(e.module.id().to_string());
                            ui.selectable_value(&mut self.find.module, id, e.module.name());
                        }
                    }
                })
                .response
                .on_hover_text("Search with a detector module instead of text");
            if let Some(g) = &self.find.group {
                let chip = Button::new(
                    RichText::new(format!("{}  ×", g.label))
                        .size(12.5)
                        .color(Color32::WHITE),
                )
                .fill(theme::ACCENT_DIM)
                .corner_radius(10.0);
                if ui
                    .add(chip)
                    .on_hover_text("Only these matches — click to search all of them")
                    .clicked()
                {
                    self.find.group = None;
                }
            }

            ui.separator();
            if icons::button(ui, Icon::Up, "Find previous (Shift+F3 / Shift+Enter)").clicked() {
                do_find = Some(true);
            }
            if icons::button(ui, Icon::Down, "Find next (F3 / Enter)").clicked() {
                do_find = Some(false);
            }
            if ui
                .button("Count")
                .on_hover_text("Count matches in the whole file")
                .clicked()
            {
                do_count = true;
            }
            if ui
                .button("Extract")
                .on_hover_text("Copy every match into a new tab")
                .clicked()
            {
                do_extract = true;
            }
            if ui
                .button("Filter")
                .on_hover_text("Open every line that matches in a new tab")
                .clicked()
            {
                do_filter = true;
            }
            ui.checkbox(
                &mut self.settings.extract_unique,
                RichText::new("unique").size(12.5),
            )
            .on_hover_text("Extract each distinct match only once");

            match &matcher {
                Err(e) if !e.is_empty() => {
                    ui.label(RichText::new(e).color(theme::ERROR).size(12.5));
                }
                _ => {
                    if let Some((n, _)) = self.find.last_count {
                        ui.label(
                            RichText::new(format!("{} matches", fmt_int(n as usize)))
                                .color(theme::TEXT_DIM)
                                .size(12.5),
                        );
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Close, "Close (Esc)").clicked() {
                    close = true;
                }
            });
        });
        if self.find.replace_open {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(24.0);
                let r = ui.add(
                    TextEdit::singleline(&mut self.find.replace)
                        .hint_text("Replace with")
                        .desired_width(280.0)
                        .font(egui::TextStyle::Monospace),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    do_replace = true;
                    r.request_focus();
                }
                if ui
                    .button("Replace")
                    .on_hover_text("Replace the selected match and find the next one")
                    .clicked()
                {
                    do_replace = true;
                }
                if ui
                    .button("Replace all")
                    .on_hover_text("Replace every match in the file (undoable)")
                    .clicked()
                {
                    do_replace_all = true;
                }
                if self.find.regex && self.find.module.is_none() {
                    ui.label(
                        RichText::new("Use $1, $2… for capture groups")
                            .color(theme::TEXT_DIM)
                            .size(12.0),
                    );
                }
            });
        }
        ui.add_space(2.0);

        if close {
            self.close_find();
        }
        if let Some(back) = do_find {
            self.start_find(back, ctx);
        }
        if do_count {
            self.start_count(ctx);
        }
        if do_extract {
            self.start_extract(ctx);
        }
        if do_filter {
            self.start_filter(ctx);
        }
        if do_replace {
            self.replace_one(ctx);
        }
        if do_replace_all {
            self.start_replace_all(self.active, ctx);
        }
    }

    fn welcome(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut open: Option<PathBuf> = None;
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.18).max(20.0));
            let (r, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
            self.logo.paint(ui, r);
            ui.add_space(6.0);
            ui.label(
                RichText::new("Mammoth")
                    .size(34.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                RichText::new("Open 100 GB text, CSV, JSON and log files instantly.")
                    .size(15.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                let w = 150.0 * 2.0 + 10.0;
                ui.add_space((ui.available_width() - w) / 2.0);
                let open_btn = Button::new(
                    RichText::new("Open file…")
                        .color(Color32::WHITE)
                        .strong()
                        .size(15.0),
                )
                .fill(theme::ACCENT)
                .min_size(vec2(150.0, 36.0));
                if ui.add(open_btn).on_hover_text("Ctrl+O").clicked() {
                    self.open_dialog(ctx);
                }
                if ui
                    .add(
                        Button::new(RichText::new("New file").size(15.0))
                            .min_size(vec2(150.0, 36.0)),
                    )
                    .clicked()
                {
                    self.new_tab();
                }
            });
            ui.add_space(10.0);
            ui.label(
                RichText::new("…or drop files anywhere in this window")
                    .color(theme::TEXT_DIM)
                    .size(13.0),
            );
            if !self.settings.recent.is_empty() {
                ui.add_space(26.0);
                ui.label(
                    RichText::new("RECENT")
                        .size(11.5)
                        .color(theme::LINENO_ACTIVE)
                        .strong(),
                );
                ui.add_space(4.0);
                for p in self.settings.recent.iter().take(8) {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let r =
                        ui.add(Button::new(RichText::new(name).color(theme::ACCENT)).frame(false));
                    if r.on_hover_text(p.display().to_string()).clicked() {
                        open = Some(p.clone());
                    }
                }
            }
        });
        if let Some(p) = open {
            self.open_path(p, ctx);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        match &mut self.dialog {
            Dialog::None => {}
            Dialog::GoTo { text, focus } => {
                let mut go: Option<String> = None;
                let mut cancel = false;
                let max = self.tabs.get(self.active).map_or(0, |t| t.doc.line_count());
                let partial = self.tabs.get(self.active).is_some_and(|t| t.is_partial());
                egui::Modal::new(Id::new("goto")).show(ctx, |ui| {
                    ui.set_width(320.0);
                    ui.label(RichText::new("Go to line").strong().size(16.0));
                    ui.add_space(4.0);
                    let hint = format!(
                        "1 – {}{}   (line or line:column)",
                        fmt_int(max),
                        if partial { "+" } else { "" }
                    );
                    let r = ui.add(
                        TextEdit::singleline(text)
                            .hint_text(hint)
                            .desired_width(f32::INFINITY),
                    );
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        go = Some(text.clone());
                    }
                    if partial {
                        ui.label(
                            RichText::new("Lines past the loaded part will be loaded on demand.")
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Go").clicked() {
                            go = Some(text.clone());
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            cancel = true;
                        }
                    });
                });
                if let Some(t) = go {
                    self.dialog = Dialog::None;
                    self.goto(&t, ctx);
                } else if cancel {
                    self.dialog = Dialog::None;
                }
            }
            Dialog::ConfirmClose(id) => {
                let id = *id;
                let title = self
                    .tab_index(id)
                    .map(|i| self.tabs[i].doc.title.clone())
                    .unwrap_or_default();
                let mut choice = 0;
                egui::Modal::new(Id::new("confirm-close")).show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        RichText::new(format!("Save changes to “{title}”?"))
                            .strong()
                            .size(16.0),
                    );
                    ui.label(
                        RichText::new("Your changes will be lost if you don't save them.")
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                Button::new(RichText::new("Save").color(Color32::WHITE))
                                    .fill(theme::ACCENT),
                            )
                            .clicked()
                        {
                            choice = 1;
                        }
                        if ui.button("Don't save").clicked() {
                            choice = 2;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            choice = 3;
                        }
                    });
                });
                if choice != 0 {
                    self.dialog = Dialog::None;
                }
                if let Some(i) = self.tab_index(id) {
                    match choice {
                        1 => {
                            self.tabs[i].close_after_save = true;
                            self.save_tab(i, false, ctx);
                        }
                        2 => self.close_tab(i),
                        _ => {}
                    }
                }
            }
            Dialog::ConfirmQuit => {
                let n = self.tabs.iter().filter(|t| t.doc.is_dirty()).count();
                let mut choice = 0;
                egui::Modal::new(Id::new("confirm-quit")).show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        RichText::new(format!(
                            "{n} unsaved document{}",
                            if n == 1 { "" } else { "s" }
                        ))
                        .strong()
                        .size(16.0),
                    );
                    ui.label(
                        RichText::new("Quit anyway and discard the changes?")
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                Button::new(
                                    RichText::new("Quit without saving").color(Color32::WHITE),
                                )
                                .fill(theme::ERROR),
                            )
                            .clicked()
                        {
                            choice = 1;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            choice = 2;
                        }
                    });
                });
                if choice == 1 {
                    self.allow_quit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                if choice != 0 {
                    self.dialog = Dialog::None;
                }
            }
        }

        if self.show_help {
            let mut open = true;
            egui::Window::new("Keyboard shortcuts")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    let rows = [
                        ("Ctrl+O / Ctrl+N", "Open / new file"),
                        ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
                        ("Ctrl+W, Ctrl+Tab", "Close tab, next tab"),
                        ("Ctrl+L", "Load entire file (preview mode)"),
                        ("Ctrl+F / Ctrl+H", "Find / replace"),
                        ("F3 / Shift+F3", "Find next / previous"),
                        ("Ctrl+G", "Go to line"),
                        ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
                        ("Ctrl+D", "Duplicate line"),
                        ("Ctrl+Shift+K", "Delete line"),
                        ("Alt+Up / Alt+Down", "Move line"),
                        ("Tab / Shift+Tab", "Indent / outdent"),
                        ("Ctrl+wheel, Ctrl+= / -", "Zoom"),
                        ("Ctrl+Shift+M", "Toggle modules panel"),
                    ];
                    egui::Grid::new("keys")
                        .num_columns(2)
                        .spacing([24.0, 6.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for (k, v) in rows {
                                ui.label(RichText::new(k).monospace().color(theme::ACCENT));
                                ui.label(v);
                                ui.end_row();
                            }
                        });
                });
            self.show_help = open;
        }
    }

    fn goto(&mut self, text: &str, ctx: &egui::Context) {
        let mut parts = text.trim().split([':', ',']);
        let line = parts
            .next()
            .and_then(|s| s.trim().replace(['_', ','], "").parse::<usize>().ok());
        let col = parts
            .next()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(1);
        let Some(line) = line else {
            self.toast_err("Enter a line number.");
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let target = line.max(1) - 1;
        tab.view.request_focus = true;
        if target < tab.doc.line_count() {
            let p = tab.doc.clamp(Pos::new(target, col.max(1) - 1));
            tab.view.select(p, p, Reveal::Center);
        } else if let Some(src) = tab.doc.source.clone().filter(|s| !s.is_complete()) {
            tab.pending_goto = Some(target);
            src.want_lines(tab.doc.orig_lines_needed(target), ctx);
            self.toast_info(format!("Loading up to line {}…", fmt_int(line)));
        } else {
            let p = tab.doc.end_pos();
            tab.view.select(p, p, Reveal::Center);
        }
    }
}

impl eframe::App for MammothApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.settings.module_states = self.registry.states();
        eframe::set_value(storage, "settings", &self.settings);
    }
}

impl MammothApp {
    /// One UI frame (separate from `eframe::App` so it can run headless in tests).
    fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.setup_done {
            self.setup_done = true;
            theme::apply(&ctx);
            ctx.options_mut(|o| o.zoom_with_keyboard = false);
            self.apply_fonts(&ctx);
        }
        self.now = ctx.input(|i| i.time);
        self.handle_drops(&ctx);
        self.handle_close_request(&ctx);
        let overlay_open =
            self.dialog != Dialog::None || self.font_picker.is_some() || self.convert.is_some();
        if !overlay_open {
            self.shortcuts(&ctx);
        }
        self.poll_jobs(&ctx);
        self.tick_tabs(&ctx);
        self.tick_tables(&ctx);
        self.tick_heatmap(&ctx);
        self.tick_insights(&ctx);
        self.update_title(&ctx);

        titlebar::resize_handles(&ctx);
        Panel::top("title-bar")
            .exact_size(titlebar::TITLE_BAR_HEIGHT)
            .frame(Frame::NONE.fill(theme::APP_BG))
            .show_separator_line(false)
            .show(ui, |ui| self.title_bar(ui, &ctx));
        if !self.tabs.is_empty() {
            Panel::top("tabs")
                .frame(Frame::NONE.fill(theme::APP_BG).inner_margin(Margin {
                    left: 6,
                    right: 6,
                    top: 4,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(ui, |ui| self.tab_bar(ui));
        }
        Panel::bottom("status")
            .frame(
                Frame::NONE
                    .fill(theme::APP_BG)
                    .inner_margin(Margin::symmetric(12, 4)),
            )
            .show_separator_line(false)
            .show(ui, |ui| self.status_bar(ui, &ctx));
        if self.modules_open && self.settings.modules_pinned {
            Panel::right("modules-pinned")
                .resizable(true)
                .default_size(330.0)
                .min_size(260.0)
                .max_size(ctx.content_rect().width() * 0.6)
                .frame(
                    Frame::NONE
                        .fill(theme::PANEL_BG)
                        .inner_margin(Margin::symmetric(14, 12)),
                )
                .show(ui, |ui| self.modules_side_panel(ui, &ctx));
        }
        if self.insights.open && !self.tabs.is_empty() {
            Panel::right("insights")
                .resizable(true)
                .default_size(420.0)
                .min_size(330.0)
                .max_size(ctx.content_rect().width() * 0.6)
                .frame(
                    Frame::NONE
                        .fill(theme::PANEL_BG)
                        .inner_margin(Margin::symmetric(14, 12)),
                )
                .show(ui, |ui| self.insights_panel(ui, &ctx));
        }
        if self.json.open && !self.tabs.is_empty() {
            Panel::right("json-inspector")
                .resizable(true)
                .default_size(440.0)
                .min_size(300.0)
                .max_size(ctx.content_rect().width() * 0.6)
                .frame(
                    Frame::NONE
                        .fill(theme::PANEL_BG)
                        .inner_margin(Margin::symmetric(14, 12)),
                )
                .show(ui, |ui| self.json_panel(ui, &ctx));
        }
        if self.find.open && !self.tabs.is_empty() {
            Panel::top("find")
                .frame(
                    Frame::NONE
                        .fill(theme::PANEL_BG)
                        .inner_margin(Margin::symmetric(10, 6)),
                )
                .show(ui, |ui| self.find_bar(ui, &ctx));
        }
        if self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.origin.is_some())
        {
            Panel::top("filter-banner")
                .frame(Frame::NONE.fill(theme::PANEL_BG_2).inner_margin(Margin {
                    left: 12,
                    right: 12,
                    top: 6,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(ui, |ui| self.filter_banner(ui));
        }
        if self.tabs.get(self.active).is_some_and(|t| t.is_partial()) {
            Panel::top("banner")
                .frame(Frame::NONE.fill(theme::PANEL_BG_2).inner_margin(Margin {
                    left: 12,
                    right: 12,
                    top: 6,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(ui, |ui| self.banner(ui, &ctx));
        }

        let mut status = None;
        let mut activate: Option<usize> = None;
        let mut table_out: Option<crate::table::Output> = None;
        let mut strip: Option<(egui::Rect, (usize, usize))> = None;
        let side_strip = if self.settings.heatmap {
            heatmap_ui::STRIP_W
        } else {
            0.0
        };
        let central = CentralPanel::default()
            .frame(Frame::NONE.fill(theme::EDITOR_BG))
            .show(ui, |ui| {
                if self.tabs.is_empty() {
                    self.welcome(ui, &ctx);
                    return;
                }
                let find_m = if self.find.open {
                    self.find.matcher(&self.registry).ok()
                } else {
                    None
                };
                let tab = &mut self.tabs[self.active];
                let syntax = tab
                    .syntax
                    .as_ref()
                    .and_then(|id| self.registry.by_id(id))
                    .map(|i| self.registry.entries[i].module.as_ref());
                let env = Env {
                    registry: &self.registry,
                    syntax,
                    find: find_m.as_ref(),
                    font_size: self.settings.font_size,
                    line_spacing: self.settings.line_spacing,
                    tab_width: self.settings.tab_width,
                    highlight_line: self.settings.highlight_line,
                    line_numbers: tab
                        .origin
                        .as_ref()
                        .filter(|o| o.version == tab.doc.version)
                        .map(|o| o.lines.as_slice()),
                    now: self.now,
                    side_strip,
                    highlights: &tab.highlights,
                    bookmarks: &tab.bookmarks,
                };
                if let (true, Some(st)) = (tab.table_mode, tab.table.as_mut()) {
                    let tenv = crate::table::Env {
                        registry: &self.registry,
                        font_size: self.settings.font_size,
                        now: self.now,
                    };
                    let out =
                        crate::table::show(ui, Id::new(("table", tab.id)), &mut tab.doc, st, &tenv);
                    status = out.status.clone();
                    table_out = Some(out);
                    return;
                }
                let out = editor::show(
                    ui,
                    Id::new(("editor", tab.id)),
                    &mut tab.doc,
                    &mut tab.view,
                    &env,
                );
                status = out.status;
                activate = out.activate_line;
                strip = out.strip.map(|r| (r, out.visible));
            })
            .response
            .rect;
        if let Some(s) = status {
            self.toast(s, theme::WARN);
        }
        if let Some(line) = activate.filter(|_| {
            self.tabs
                .get(self.active)
                .is_some_and(|t| t.origin.is_some())
        }) {
            self.jump_to_source(self.active, line);
        }
        if let Some((rect, visible)) = strip {
            self.heatmap_strip(ui, rect, visible, &ctx);
        }
        if let Some(out) = table_out {
            if out.to_text {
                self.toggle_table();
            }
            if out.open_view_as_tab {
                self.start_table_export(self.active, &ctx);
            }
        }

        self.modules_drawer(&ctx, central);
        self.palette_ui(&ctx);
        self.dialogs(&ctx);
        self.convert_dialog(&ctx);
        self.settings_window(&ctx);
        titlebar::window_border(&ctx);

        let loading = self
            .tabs
            .iter()
            .any(|t| t.doc.source.as_ref().is_some_and(|s| s.is_loading()));
        if !self.jobs.is_empty() || loading || self.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

// ----------------------------------------------------------------------------
// Small widgets and utilities

fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(30.0, 16.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let p = ui.painter();
    let bg = if *on { theme::ACCENT } else { theme::BORDER };
    p.rect_filled(rect, 8.0, bg);
    let x = egui::lerp(rect.left() + 8.0..=rect.right() - 8.0, t);
    p.circle_filled(pos2(x, rect.center().y), 6.0, Color32::WHITE);
    resp
}

fn fmt_secs(s: f64) -> String {
    if !s.is_finite() {
        return "—".into();
    }
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `C:\foo` instead of `\\?\C:\foo` on Windows.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

fn replace_file(tmp: &Path, dst: &Path) -> Result<(), String> {
    match std::fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(e1) => match std::fs::copy(tmp, dst) {
            Ok(_) => {
                let _ = std::fs::remove_file(tmp);
                Ok(())
            }
            Err(e2) => Err(format!(
                "Could not replace {} ({e1}; {e2}). Your changes are safe in {}.",
                dst.display(),
                tmp.display()
            )),
        },
    }
}

fn user_modules_dir() -> Option<PathBuf> {
    crate::modules::module_dirs().pop()
}

fn open_in_file_manager(dir: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(dir).spawn();
}

pub(crate) const EXAMPLE_MODULE: &str = r##"# Example Mammoth detector module.
# Edit the fields below, save, then press reload in the Modules panel.
#
# pattern     – a Rust regex (https://docs.rs/regex). Use ''' quotes so backslashes work.
# color       – highlight colour as "#rrggbb"
# replacement – what "Mask…" replaces each hit with (optional)
# enabled     – highlight by default (optional, default true)

name        = "Hashtags & mentions"
description = "Social-media style #tags and @handles"
pattern     = '''(?-u:\B)[#@][A-Za-z0-9_]{2,32}(?-u:\b)'''
color       = "#ff9e64"
replacement = "[TAG]"
enabled     = true
"##;

fn create_example_module() -> std::io::Result<PathBuf> {
    let dir = user_modules_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("example-hashtags.toml");
    if !path.exists() {
        std::fs::write(&path, EXAMPLE_MODULE)?;
    }
    Ok(path)
}
