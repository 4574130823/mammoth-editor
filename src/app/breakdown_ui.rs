//! Insights panel: what a detector found in the file, grouped and counted.
//! For emails: headline numbers, the provider split (Gmail, Yahoo, Outlook, …) as a
//! stacked bar, then providers, domains and the most frequent addresses, each
//! with Filter / Find / Extract.
//!
//! One click away (the detector's status-bar chip, the title-bar chart button or
//! Ctrl+Shift+E), and computed automatically in the background for the active tab.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use egui::{
    Align, Align2, Button, Color32, ComboBox, FontId, Layout, Rect, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Ui, pos2, vec2,
};

use super::{GroupSel, Job, JobKind, JobOut, MammothApp};
use crate::editor::{fmt_bytes, fmt_int};
use crate::icons::{self, Icon};
use crate::search::{self, Breakdown, Matcher, Query, Snapshot};
use crate::theme;

const ROW_H: f32 = 30.0;
const MAX_CHILDREN: usize = 300;
/// Files up to this size are analysed as soon as the panel shows them.
const AUTO_LIMIT: u64 = 1 << 30;

const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x4f, 0x8c, 0xff),
    Color32::from_rgb(0x3d, 0xd6, 0x8c),
    Color32::from_rgb(0xf5, 0xa5, 0x24),
    Color32::from_rgb(0xb3, 0x8b, 0xfa),
    Color32::from_rgb(0x2e, 0xc4, 0xd6),
    Color32::from_rgb(0xff, 0x7a, 0x90),
    Color32::from_rgb(0xe0, 0xc0, 0x5a),
    Color32::from_rgb(0x8a, 0xb4, 0xf8),
];

/// Recognisable colours for email providers; anything else cycles the palette.
fn category_color(name: &str, i: usize) -> Color32 {
    match name {
        "Gmail" => Color32::from_rgb(0xea, 0x4d, 0x3d),
        "Outlook / Hotmail" => Color32::from_rgb(0x2b, 0x88, 0xe8),
        "Yahoo" => Color32::from_rgb(0x8f, 0x4d, 0xff),
        "iCloud" => Color32::from_rgb(0xb0, 0xb8, 0xc6),
        "Company / other" => Color32::from_rgb(0x3d, 0xd6, 0x8c),
        "Education" => Color32::from_rgb(0xf4, 0xb4, 0x00),
        "Government" => Color32::from_rgb(0xe0, 0x6c, 0x9f),
        "AOL" => Color32::from_rgb(0x00, 0xb4, 0xd8),
        "Proton" => Color32::from_rgb(0xc2, 0x8c, 0xff),
        "Disposable" => Color32::from_rgb(0xa8, 0x72, 0x50),
        _ => PALETTE[i % PALETTE.len()],
    }
}

/// "Domain" → "domains", "Subnet (/24)" → "subnets (/24)", "Status" → "statuses".
fn plural(label: &str) -> String {
    let l = label.to_lowercase();
    let (head, tail) = match l.find(" (") {
        Some(i) => (l[..i].to_string(), l[i..].to_string()),
        None => (l, String::new()),
    };
    let head = if head.ends_with('s') {
        format!("{head}es")
    } else {
        format!("{head}s")
    };
    format!("{head}{tail}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Categories,
    Keys,
    Values,
}

/// What a finished analysis was computed for.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct InsightKey {
    tab: u64,
    module: String,
    version: u64,
}

struct Computed {
    key: InsightKey,
    b: Breakdown,
    /// (category, total, indices into `b.counts`), largest first.
    cats: Vec<(String, u64, Vec<usize>)>,
    label: String,
    cat_label: Option<String>,
    source_title: String,
}

pub(super) struct Insights {
    pub open: bool,
    pub module_id: String,
    view: View,
    filter: String,
    expanded: HashSet<String>,
    result: Option<Computed>,
    running: Option<InsightKey>,
    /// A big file the user explicitly asked to scan: (tab, module).
    requested: Option<(u64, String)>,
    changed_at: Option<f64>,
    filtered: Option<(View, String, Vec<usize>)>,
}

impl Default for Insights {
    fn default() -> Self {
        Self {
            open: false,
            module_id: "email".into(),
            view: View::Categories,
            filter: String::new(),
            expanded: HashSet::new(),
            result: None,
            running: None,
            requested: None,
            changed_at: None,
            filtered: None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum RowAction {
    Find,
    Filter,
    Extract,
}

/// What a row stands for: a breakdown group or one exact value.
pub(super) enum RowTarget {
    Group(GroupSel),
    Value(String),
}

impl MammothApp {
    /// Show the panel for `module_id`. `explicit` also allows scanning big files.
    pub(super) fn open_insights(&mut self, module_id: &str, explicit: bool) {
        let ins = &mut self.insights;
        if ins.module_id != module_id {
            ins.module_id = module_id.to_string();
            ins.expanded.clear();
            ins.filter.clear();
            ins.filtered = None;
            ins.view = View::Categories;
        }
        ins.open = true;
        if explicit
            && let Some(tab) = self.tabs.get(self.active) {
                self.insights.requested = Some((tab.id, module_id.to_string()));
            }
    }

    pub(super) fn toggle_insights(&mut self) {
        if self.insights.open {
            self.insights.open = false;
        } else {
            let id = self.insights.module_id.clone();
            self.open_insights(&id, false);
        }
    }

    /// Keep the analysis in step with the active tab (debounced after edits).
    pub(super) fn tick_insights(&mut self, ctx: &egui::Context) {
        if !self.insights.open || self.tabs.is_empty() {
            return;
        }
        let tab = &self.tabs[self.active];
        let want = InsightKey {
            tab: tab.id,
            module: self.insights.module_id.clone(),
            version: tab.doc.version,
        };
        let ins = &mut self.insights;
        if ins.result.as_ref().is_some_and(|r| r.key == want) || ins.running.as_ref() == Some(&want)
        {
            ins.changed_at = None;
            return;
        }
        let small = tab
            .doc
            .source
            .as_ref()
            .is_none_or(|s| s.len() <= AUTO_LIMIT);
        let asked = ins
            .requested
            .as_ref()
            .is_some_and(|(t, m)| *t == want.tab && *m == want.module);
        if !small && !asked {
            return;
        }
        // Same file and detector, just edited: wait for typing to pause.
        let same = ins
            .result
            .as_ref()
            .is_some_and(|r| r.key.tab == want.tab && r.key.module == want.module);
        let since = *ins.changed_at.get_or_insert(self.now);
        if same && self.now - since < 0.8 {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        }
        ins.changed_at = None;
        self.start_insights(want, ctx);
    }

    fn start_insights(&mut self, key: InsightKey, ctx: &egui::Context) {
        let Some(i) = self.registry.by_id(&key.module) else {
            return;
        };
        let entry = &self.registry.entries[i];
        let Some(grouper) = entry.grouper() else {
            return;
        };
        let q = Query {
            text: String::new(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            module: Some(entry.module.clone()),
            group: None,
        };
        let Ok(m) = Matcher::new(&q) else { return };
        let Some(ti) = self.tab_index(key.tab) else {
            return;
        };
        self.cancel_jobs(key.tab, |k| matches!(k, JobKind::Breakdown(_)));
        let snap = Snapshot::of(&self.tabs[ti].doc);
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Breakdown(snap.breakdown(&m, &grouper, ctl))
        });
        self.insights.running = Some(key.clone());
        let version = key.version;
        self.jobs.push(Job {
            tab: key.tab,
            kind: JobKind::Breakdown(key),
            ctl,
            rx,
            version,
            label: "Analyzing",
            started: Instant::now(),
        });
    }

    /// A cancelled job must not block the next one from starting.
    pub(super) fn insights_job_cancelled(&mut self, key: &InsightKey) {
        if self.insights.running.as_ref() == Some(key) {
            self.insights.running = None;
        }
    }

    /// The total for a detector in the active tab, if it's been analysed.
    pub(super) fn insights_total(&self, module: &str) -> Option<u64> {
        let r = self.insights.result.as_ref()?;
        let tab = self.tabs.get(self.active)?;
        (r.key.tab == tab.id && r.key.module == module).then_some(r.b.total)
    }

    pub(super) fn finish_breakdown(&mut self, key: InsightKey, b: Breakdown) {
        if self.insights.running.as_ref() == Some(&key) {
            self.insights.running = None;
        }
        let Some(i) = self.registry.by_id(&key.module) else {
            return;
        };
        let Some(grouper) = self.registry.entries[i].grouper() else {
            return;
        };
        let mut cats = Vec::new();
        if grouper.category_label.is_some() {
            let mut map: HashMap<String, (u64, Vec<usize>)> = HashMap::new();
            for (i, (k, c)) in b.counts.iter().enumerate() {
                let e = map
                    .entry(grouper.category(k).unwrap_or_else(|| "Other".into()))
                    .or_default();
                e.0 += c;
                e.1.push(i);
            }
            cats = map.into_iter().map(|(k, (c, i))| (k, c, i)).collect();
            cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        let source_title = self
            .tab_index(key.tab)
            .map(|t| self.tabs[t].doc.title.clone())
            .unwrap_or_default();
        let ins = &mut self.insights;
        ins.filtered = None;
        ins.result = Some(Computed {
            key,
            b,
            cats,
            label: grouper.label.clone(),
            cat_label: grouper.category_label.clone(),
            source_title,
        });
    }

    pub(super) fn run_row_action(
        &mut self,
        target: RowTarget,
        act: RowAction,
        ctx: &egui::Context,
    ) {
        let Some(tab) = self
            .insights
            .result
            .as_ref()
            .and_then(|r| self.tab_index(r.key.tab))
        else {
            self.toast_info("That file is no longer open.");
            return;
        };
        self.active = tab;
        match target {
            RowTarget::Group(sel) => {
                self.find.module = Some(sel.module.clone());
                self.find.group = Some(sel);
            }
            RowTarget::Value(v) => {
                self.find.module = None;
                self.find.group = None;
                self.find.query = v;
                self.find.regex = false;
                self.find.word = false;
                self.find.case = false;
            }
        }
        self.find.open = true;
        match act {
            RowAction::Find => self.start_find(false, ctx),
            RowAction::Filter => self.start_filter(ctx),
            RowAction::Extract => self.start_extract(ctx),
        }
    }

    pub(super) fn insights_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut action: Option<(RowTarget, RowAction)> = None;
        let mut export: Option<bool> = None;
        let mut close = false;
        let mut pick: Option<String> = None;
        let mut scan = false;

        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Chart, theme::ACCENT);
            ui.label(
                RichText::new("Insights")
                    .size(17.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Close, "Close (Ctrl+Shift+E)").clicked() {
                    close = true;
                }
            });
        });
        let choices: Vec<(String, String)> = self
            .registry
            .detectors()
            .filter(|(_, e)| e.grouper().is_some())
            .map(|(_, e)| (e.module.id().to_string(), e.module.name().to_string()))
            .collect();
        let current = choices
            .iter()
            .find(|(id, _)| *id == self.insights.module_id)
            .map_or("—".to_string(), |c| c.1.clone());
        ComboBox::from_id_salt("insights-module")
            .selected_text(current)
            .width(200.0)
            .show_ui(ui, |ui| {
                for (id, name) in &choices {
                    if ui
                        .selectable_label(*id == self.insights.module_id, name)
                        .clicked()
                    {
                        pick = Some(id.clone());
                    }
                }
            });
        ui.add_space(6.0);

        let Some(tab) = self.tabs.get(self.active) else {
            ui.label(RichText::new("Open a file to see what's in it.").color(theme::TEXT_DIM));
            return;
        };
        let (tab_id, version, size) = (
            tab.id,
            tab.doc.version,
            tab.doc.source.as_ref().map_or(0, |s| s.len()),
        );
        let progress = self
            .jobs
            .iter()
            .find(|j| j.tab == tab_id && matches!(j.kind, JobKind::Breakdown(_)))
            .map(|j| j.ctl.fraction());
        let ins = &mut self.insights;
        let fits = ins
            .result
            .as_ref()
            .is_some_and(|r| r.key.tab == tab_id && r.key.module == ins.module_id);

        if fits {
            let stale = ins
                .result
                .as_ref()
                .is_some_and(|r| r.key.version != version);
            if stale {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0));
                    let pct = progress.map_or(String::new(), |p| format!(" {:.0}%", p * 100.0));
                    ui.label(
                        RichText::new(format!("Updating after your edits…{pct}"))
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            }
            let (a, e) = result_ui(ui, ins);
            action = a;
            export = e;
        } else if let Some(p) = progress {
            scanning(ui, p);
        } else if size > AUTO_LIMIT {
            ui.label(
                RichText::new(format!(
                    "This file is {}. Analysing it reads the whole file once.",
                    fmt_bytes(size)
                ))
                .color(theme::TEXT_DIM),
            );
            ui.add_space(4.0);
            let b = Button::new(RichText::new("Scan file").color(Color32::WHITE).strong())
                .fill(theme::ACCENT);
            if ui.add(b).clicked() {
                scan = true;
            }
        } else {
            scanning(ui, 0.0);
        }

        if close {
            self.insights.open = false;
        }
        if let Some(id) = pick {
            self.open_insights(&id, false);
        }
        if scan {
            self.insights.requested = Some((tab_id, self.insights.module_id.clone()));
        }
        if let Some(as_tab) = export {
            self.export_insights(as_tab, ctx);
        }
        if let Some((target, act)) = action {
            self.run_row_action(target, act, ctx);
        }
    }

    fn export_insights(&mut self, as_tab: bool, ctx: &egui::Context) {
        let Some(r) = &self.insights.result else {
            return;
        };
        let csv = to_csv(r, self.insights.view);
        if as_tab {
            let name = self
                .registry
                .by_id(&r.key.module)
                .map_or("breakdown".into(), |i| {
                    self.registry.entries[i].module.name().to_lowercase()
                });
            let title = format!("{name} — {}.csv", r.source_title);
            let lines = csv.lines().map(str::to_string).collect();
            self.push_tab(
                crate::document::Document::from_lines(title, lines),
                Some("csv".into()),
            );
        } else {
            ctx.copy_text(csv);
            self.toast_ok("Copied as CSV.");
        }
    }
}

fn scanning(ui: &mut Ui, p: f32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0).color(theme::ACCENT));
        ui.label(
            RichText::new(format!("Scanning the whole file… {:.0}%", p * 100.0))
                .color(theme::TEXT_DIM),
        );
    });
    ui.add(
        egui::ProgressBar::new(p)
            .desired_height(4.0)
            .fill(theme::ACCENT),
    );
}

/// A big headline number with a caption.
fn stat(ui: &mut Ui, value: String, label: &str) {
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.label(RichText::new(value).size(22.0).strong().color(theme::TEXT));
    ui.add(egui::Label::new(RichText::new(label).size(12.0).color(theme::TEXT_DIM)).truncate());
}

fn pct(c: u64, total: u64) -> String {
    let p = c as f64 * 100.0 / total.max(1) as f64;
    if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 0.1 {
        format!("{p:.1}%")
    } else {
        "<0.1%".into()
    }
}

fn result_ui(ui: &mut Ui, ins: &mut Insights) -> (Option<(RowTarget, RowAction)>, Option<bool>) {
    let mut action = None;
    let mut export = None;
    let r = ins.result.as_ref().unwrap();
    let is_email = r.key.module == "email";
    let b = &r.b;
    let total = b.total.max(1);

    ui.label(
        RichText::new(&r.source_title)
            .size(12.0)
            .color(theme::TEXT_DIM),
    );
    ui.add_space(4.0);
    if b.total == 0 {
        ui.add_space(10.0);
        ui.label(
            RichText::new("Nothing found in this file.")
                .size(15.0)
                .color(theme::TEXT_DIM),
        );
        return (None, None);
    }

    // ---- headline numbers
    let uniq = format!(
        "{}{}",
        fmt_int(b.distinct_values),
        if b.values_capped { "+" } else { "" }
    );
    ui.columns(3, |cols| {
        stat(
            &mut cols[0],
            fmt_int(b.total as usize),
            if is_email { "emails found" } else { "matches" },
        );
        stat(
            &mut cols[1],
            uniq,
            if is_email {
                "unique addresses"
            } else {
                "unique"
            },
        );
        stat(&mut cols[2], fmt_int(b.counts.len()), &plural(&r.label));
    });
    if b.capped {
        let note = format!(
            "So many different {} that the rarest are grouped as “(other)”.",
            plural(&r.label)
        );
        ui.label(RichText::new(note).size(12.0).color(theme::WARN));
    }
    ui.add_space(8.0);

    // ---- the split, as one stacked bar + legend
    let parts: Vec<(String, u64, Color32)> = if r.cats.is_empty() {
        let mut v: Vec<(String, u64, Color32)> = b
            .counts
            .iter()
            .take(7)
            .enumerate()
            .map(|(i, (k, c))| (k.clone(), *c, PALETTE[i % PALETTE.len()]))
            .collect();
        let rest: u64 = b.counts.iter().skip(7).map(|(_, c)| c).sum();
        if rest > 0 {
            v.push(("everything else".into(), rest, theme::LINENO));
        }
        v
    } else {
        r.cats
            .iter()
            .enumerate()
            .map(|(i, (n, c, _))| (n.clone(), *c, category_color(n, i)))
            .collect()
    };
    let (bar, bar_resp) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::click());
    let mut x = bar.left();
    let mut hovered_part = None;
    for (i, (_, c, color)) in parts.iter().enumerate() {
        let w = (bar.width() * *c as f32 / total as f32)
            .max(2.0)
            .min(bar.right() - x);
        let seg = Rect::from_min_size(pos2(x, bar.top()), vec2(w, bar.height()));
        ui.painter().rect_filled(seg, 0.0, *color);
        if bar_resp.hover_pos().is_some_and(|p| seg.contains(p)) {
            hovered_part = Some(i);
            ui.painter().rect_stroke(
                seg,
                0.0,
                Stroke::new(1.5, Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }
        x += w;
    }
    // Round the ends by masking the corners with the panel colour.
    ui.painter().rect_stroke(
        bar.expand(2.0),
        7.0,
        Stroke::new(4.0, theme::PANEL_BG),
        egui::StrokeKind::Inside,
    );
    if let Some(i) = hovered_part {
        let (name, c, _) = &parts[i];
        let text = format!("{name}: {} ({})", fmt_int(*c as usize), pct(*c, total));
        let clicked = bar_resp.clicked();
        bar_resp.on_hover_text(text);
        if clicked && !r.cats.is_empty() {
            let name = parts[i].0.clone();
            ins.view = View::Categories;
            ins.expanded.insert(name);
        }
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(14.0, 4.0);
        for (name, c, color) in parts.iter().take(10) {
            let name_g =
                ui.painter()
                    .layout_no_wrap(name.clone(), FontId::proportional(12.5), theme::TEXT);
            let pct_g = ui.painter().layout_no_wrap(
                pct(*c, total),
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
            let size = vec2(
                13.0 + name_g.size().x + 5.0 + pct_g.size().x,
                name_g.size().y.max(16.0),
            );
            let (r, _) = ui.allocate_exact_size(size, Sense::hover());
            let p = ui.painter();
            p.circle_filled(pos2(r.left() + 4.0, r.center().y), 4.0, *color);
            let nw = name_g.size().x;
            p.galley(
                pos2(r.left() + 13.0, r.center().y - name_g.size().y / 2.0),
                name_g,
                theme::TEXT,
            );
            p.galley(
                pos2(r.left() + 18.0 + nw, r.center().y - pct_g.size().y / 2.0),
                pct_g,
                theme::TEXT_DIM,
            );
        }
    });
    ui.add_space(10.0);

    // ---- views
    let mut views = Vec::new();
    if let Some(cl) = &r.cat_label {
        views.push((View::Categories, plural(cl)));
    }
    views.push((View::Keys, plural(&r.label)));
    views.push((
        View::Values,
        if is_email {
            "top addresses".into()
        } else {
            "top values".into()
        },
    ));
    if !views.iter().any(|(v, _)| *v == ins.view) {
        ins.view = views[0].0;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let n = views.len();
        for (i, (v, label)) in views.iter().enumerate() {
            let on = ins.view == *v;
            let cr = egui::CornerRadius {
                nw: if i == 0 { 6 } else { 0 },
                sw: if i == 0 { 6 } else { 0 },
                ne: if i + 1 == n { 6 } else { 0 },
                se: if i + 1 == n { 6 } else { 0 },
            };
            let mut text = label.clone();
            text[..1].make_ascii_uppercase();
            let b = Button::new(RichText::new(text).size(12.5).color(if on {
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
            if ui.add(b).clicked() {
                ins.view = *v;
            }
        }
    });
    ui.add_space(6.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 26.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            if ui
                .small_button("Open as table")
                .on_hover_text("This list as a CSV tab")
                .clicked()
            {
                export = Some(true);
            }
            if ui
                .small_button("Copy")
                .on_hover_text("Copy this list as CSV")
                .clicked()
            {
                export = Some(false);
            }
            // Whatever width is left, never more (a wider row would make the panel grow).
            ui.add(
                TextEdit::singleline(&mut ins.filter)
                    .hint_text("Search…")
                    .desired_width(ui.available_width()),
            );
        },
    );
    ui.add_space(4.0);

    // ---- list
    let filter = ins.filter.to_lowercase();
    let view = ins.view;
    {
        let b = &ins.result.as_ref().unwrap().b;
        if ins
            .filtered
            .as_ref()
            .is_none_or(|(v, f, _)| *v != view || *f != filter)
        {
            let src: &Vec<(String, u64)> = if view == View::Values {
                &b.top_values
            } else {
                &b.counts
            };
            let idx = (0..src.len())
                .filter(|&i| filter.is_empty() || src[i].0.to_lowercase().contains(&filter))
                .collect();
            ins.filtered = Some((view, filter.clone(), idx));
        }
    }
    let keys = ins.filtered.as_ref().unwrap().2.clone();
    let r = ins.result.as_ref().unwrap();
    let b = &r.b;
    let module = r.key.module.clone();
    let key_sel = |key: &str| GroupSel {
        module: module.clone(),
        label: if is_email {
            format!("@{key}")
        } else {
            key.to_string()
        },
        keys: Some(vec![key.to_string()]),
        category: None,
    };
    let cat_of: HashMap<usize, Color32> = r
        .cats
        .iter()
        .enumerate()
        .flat_map(|(ci, (n, _, members))| members.iter().map(move |&m| (m, category_color(n, ci))))
        .collect();

    match view {
        View::Categories => {
            let max = r.cats.first().map_or(1, |c| c.1);
            let visible: HashSet<usize> = keys.iter().copied().collect();
            let mut toggle = None;
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (ci, (cat, count, members)) in r.cats.iter().enumerate() {
                        let shown: Vec<usize> = members
                            .iter()
                            .copied()
                            .filter(|i| visible.contains(i))
                            .collect();
                        if shown.is_empty()
                            && !(filter.is_empty() || cat.to_lowercase().contains(&filter))
                        {
                            continue;
                        }
                        let open =
                            ins.expanded.contains(cat) || (!filter.is_empty() && !shown.is_empty());
                        let color = category_color(cat, ci);
                        let rr = row(ui, cat, *count, total, max, 0.0, Some(open), color, true);
                        if rr.toggle {
                            toggle = Some(cat.clone());
                        }
                        if let Some(a) = rr.action {
                            let sel = GroupSel {
                                module: module.clone(),
                                label: cat.clone(),
                                keys: None,
                                category: Some(cat.clone()),
                            };
                            action = Some((RowTarget::Group(sel), a));
                        }
                        if open {
                            let list = if filter.is_empty() { members } else { &shown };
                            for &i in list.iter().take(MAX_CHILDREN) {
                                let (k, c) = &b.counts[i];
                                let rr =
                                    row(ui, k, *c, total, max, 22.0, None, color, k != "(other)");
                                if let Some(a) = rr.action {
                                    action = Some((RowTarget::Group(key_sel(k)), a));
                                }
                            }
                            if list.len() > MAX_CHILDREN {
                                let more = format!(
                                    "   … {} more under “{}”",
                                    fmt_int(list.len() - MAX_CHILDREN),
                                    plural(&r.label)
                                );
                                ui.label(RichText::new(more).size(12.0).color(theme::TEXT_DIM));
                            }
                        }
                    }
                });
            if let Some(cat) = toggle
                && !ins.expanded.remove(&cat) {
                    ins.expanded.insert(cat);
                }
        }
        View::Keys => {
            let max = b.counts.first().map_or(1, |c| c.1);
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, keys.len(), |ui, range| {
                    for &i in &keys[range] {
                        let (k, c) = &b.counts[i];
                        let color = cat_of.get(&i).copied().unwrap_or(PALETTE[0]);
                        let rr = row(ui, k, *c, total, max, 0.0, None, color, k != "(other)");
                        if let Some(a) = rr.action {
                            action = Some((RowTarget::Group(key_sel(k)), a));
                        }
                    }
                });
        }
        View::Values => {
            let max = b.top_values.first().map_or(1, |c| c.1);
            // Duplicates first: how many values appear more than once.
            let dups = b.top_values.iter().take_while(|(_, c)| *c > 1).count();
            let what = if is_email { "address" } else { "value" };
            let (note, color) = match dups {
                0 => (
                    format!("No duplicates: every {what} appears once."),
                    theme::OK,
                ),
                n => {
                    let more = if n == b.top_values.len() { "+" } else { "" };
                    let noun = match (is_email, n == 1) {
                        (true, true) => "address appears",
                        (true, false) => "addresses appear",
                        (false, true) => "value appears",
                        (false, false) => "values appear",
                    };
                    (
                        format!(
                            "{}{more} {noun} more than once — they're at the top.",
                            fmt_int(n)
                        ),
                        theme::WARN,
                    )
                }
            };
            ui.label(RichText::new(note).size(12.5).color(color));
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, keys.len(), |ui, range| {
                    for &i in &keys[range] {
                        let (v, c) = &b.top_values[i];
                        let rr = row(ui, v, *c, total, max, 0.0, None, theme::ACCENT, true);
                        if let Some(a) = rr.action {
                            action = Some((RowTarget::Value(v.clone()), a));
                        }
                    }
                });
        }
    }
    (action, export)
}

struct RowResponse {
    toggle: bool,
    action: Option<RowAction>,
}

/// One list row: dot or chevron, name over a data bar, count and share — and
/// Filter / Find / Extract buttons while hovered.
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    name: &str,
    count: u64,
    total: u64,
    max: u64,
    indent: f32,
    expanded: Option<bool>,
    color: Color32,
    actions: bool,
) -> RowResponse {
    let mut out = RowResponse {
        toggle: false,
        action: None,
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let hot = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    if hot {
        p.rect_filled(rect, 6.0, theme::PANEL_BG_2);
    }
    let right_cols = 190.0;
    let bar_w =
        (rect.width() - right_cols - 30.0 - indent).max(0.0) * (count as f32 / max.max(1) as f32);
    p.rect_filled(
        Rect::from_min_size(
            pos2(rect.left() + 22.0 + indent, rect.top() + 5.0),
            vec2(bar_w, ROW_H - 10.0),
        ),
        4.0,
        color.gamma_multiply(if indent > 0.0 { 0.14 } else { 0.24 }),
    );
    let x = rect.left() + indent;
    match expanded {
        Some(open) => {
            let c = pos2(x + 9.0, rect.center().y);
            let pts = if open {
                vec![
                    c + vec2(-4.0, -2.0),
                    c + vec2(0.0, 2.5),
                    c + vec2(4.0, -2.0),
                ]
            } else {
                vec![
                    c + vec2(-2.0, -4.0),
                    c + vec2(2.5, 0.0),
                    c + vec2(-2.0, 4.0),
                ]
            };
            p.line(pts, Stroke::new(1.5, theme::TEXT_DIM));
            if resp.clicked() {
                out.toggle = true;
            }
        }
        None => {
            p.circle_filled(pos2(x + 10.0, rect.center().y), 3.5, color);
        }
    }
    let (font, text_color) = if indent > 0.0 {
        (FontId::proportional(13.0), theme::TEXT_DIM)
    } else {
        (FontId::proportional(14.0), theme::TEXT)
    };
    p.with_clip_rect(Rect::from_min_max(
        rect.min,
        pos2(rect.right() - right_cols, rect.bottom()),
    ))
    .text(
        pos2(x + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        font,
        text_color,
    );

    if actions && hot {
        let mut ax = rect.right() - 6.0;
        for (label, act, tip) in [
            ("Extract", RowAction::Extract, "Copy these into a new tab"),
            ("Find", RowAction::Find, "Jump to the next one"),
            (
                "Filter",
                RowAction::Filter,
                "Every line with these, in a new tab",
            ),
        ] {
            let g = p.layout_no_wrap(
                label.to_string(),
                FontId::proportional(12.5),
                Color32::WHITE,
            );
            let r = Rect::from_min_size(
                pos2(ax - g.size().x - 12.0, rect.top() + 4.0),
                vec2(g.size().x + 12.0, ROW_H - 8.0),
            );
            ax = r.left() - 4.0;
            let lr = ui
                .interact(r, resp.id.with(label), Sense::click())
                .on_hover_text(tip);
            let fill = if lr.hovered() {
                theme::ACCENT
            } else if label == "Filter" {
                theme::ACCENT_DIM
            } else {
                theme::BORDER
            };
            ui.painter().rect_filled(r, 5.0, fill);
            ui.painter().galley(
                r.min + vec2(6.0, (r.height() - g.size().y) / 2.0),
                g,
                Color32::WHITE,
            );
            if lr.clicked() {
                out.action = Some(act);
            }
        }
    } else {
        p.text(
            pos2(rect.right() - 62.0, rect.center().y),
            Align2::RIGHT_CENTER,
            fmt_int(count as usize),
            FontId::monospace(12.5),
            theme::TEXT,
        );
        p.text(
            pos2(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            pct(count, total),
            FontId::monospace(12.0),
            theme::TEXT_DIM,
        );
    }
    out
}

fn to_csv(r: &Computed, view: View) -> String {
    let total = r.b.total.max(1) as f64;
    let esc = |s: &str| {
        if s.contains([',', '"', '\n']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let mut out = String::new();
    match view {
        View::Categories => {
            let cat = r.cat_label.clone().unwrap_or_default().to_lowercase();
            out.push_str(&format!("{cat},{},count,percent\n", r.label.to_lowercase()));
            for (name, _, members) in &r.cats {
                for &i in members {
                    let (k, c) = &r.b.counts[i];
                    out.push_str(&format!(
                        "{},{},{c},{:.2}\n",
                        esc(name),
                        esc(k),
                        *c as f64 * 100.0 / total
                    ));
                }
            }
        }
        View::Keys => {
            out.push_str(&format!("{},count,percent\n", r.label.to_lowercase()));
            for (k, c) in &r.b.counts {
                out.push_str(&format!(
                    "{},{c},{:.2}\n",
                    esc(k),
                    *c as f64 * 100.0 / total
                ));
            }
        }
        View::Values => {
            out.push_str("value,count\n");
            for (v, c) in &r.b.top_values {
                out.push_str(&format!("{},{c}\n", esc(v)));
            }
        }
    }
    out
}

#[cfg(test)]
type InsightSummary = (u64, Vec<(String, u64)>, Vec<(String, u64)>, usize);

#[cfg(test)]
impl MammothApp {
    /// (total, counts per key, counts per category, distinct values)
    pub(super) fn insights_for_test(&self) -> Option<InsightSummary> {
        let r = self.insights.result.as_ref()?;
        let cats = r.cats.iter().map(|(c, n, _)| (c.clone(), *n)).collect();
        Some((r.b.total, r.b.counts.clone(), cats, r.b.distinct_values))
    }

    pub(super) fn insights_expand_for_test(&mut self, cat: &str) {
        self.insights.expanded.insert(cat.to_string());
    }

    pub(super) fn insights_view_for_test(&mut self, i: usize) {
        self.insights.view = [View::Categories, View::Keys, View::Values][i];
    }
}

#[cfg(test)]
mod tests {
    use super::plural;

    #[test]
    fn plurals() {
        assert_eq!(plural("Domain"), "domains");
        assert_eq!(plural("Subnet (/24)"), "subnets (/24)");
        assert_eq!(plural("Status"), "statuses");
        assert_eq!(plural("Card brand"), "card brands");
    }
}
