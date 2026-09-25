//! Heatmap strip beside the scrollbar: where each enabled detector (plus errors in
//! logs, plus the current search) clusters across the whole file. Click to jump.

use std::time::Instant;

use egui::{Color32, Id, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::{Job, JobKind, JobOut, MammothApp};
use crate::document::Pos;
use crate::editor::{Reveal, fmt_bytes, fmt_int};
use crate::search::{self, AxisTarget, HEAT_BUCKETS, Heatmap, Matcher, Query, Snapshot};
use crate::theme;

/// Width of the strip, in points.
pub(super) const STRIP_W: f32 = 16.0;
/// Files up to this size are (re)scanned automatically; bigger ones on request.
const AUTO_SCAN_LIMIT: u64 = 256 << 20;
const MAX_LANES: usize = 4;
const ERRORS_PATTERN: &str = r"(?-u:\b)(?:FATAL|CRITICAL|ERROR|SEVERE|PANIC|Exception)(?-u:\b)";

#[derive(Default)]
pub(super) struct HeatState {
    map: Option<Heatmap>,
    /// Name and colour of each layer in `map`.
    layers: Vec<(String, Color32)>,
    /// Layer configuration and document version `map` was computed for.
    key: String,
    version: u64,
    /// Key of the scan in progress.
    running: Option<String>,
    /// For big files: the user asked for a scan.
    requested: bool,
    changed_at: Option<f64>,
}

impl HeatState {
    /// Hits for the layer called `name` in the current map, if there is one.
    pub(super) fn total_for(&self, name: &str) -> Option<u64> {
        let map = self.map.as_ref()?;
        self.layers
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| map.totals[i])
    }
}

impl MammothApp {
    /// A cheap description of what the heatmap should currently show.
    fn heat_key(&self, idx: usize) -> String {
        let tab = &self.tabs[idx];
        let mut key: Vec<String> = self
            .registry
            .detectors()
            .filter(|(_, e)| e.enabled)
            .map(|(_, e)| e.module.id().to_string())
            .collect();
        if tab.syntax.as_deref() == Some("log") {
            key.push("errors".into());
        }
        if self.find.open && (!self.find.query.is_empty() || self.find.module.is_some()) {
            key.push(format!(
                "find:{}:{:?}:{}{}{}",
                self.find.query, self.find.group, self.find.case, self.find.word, self.find.regex
            ));
        }
        key.join("|")
    }

    fn heat_layers(&mut self, idx: usize) -> Vec<(String, Color32, Matcher)> {
        let mut v = Vec::new();
        for (_, e) in self.registry.detectors().filter(|(_, e)| e.enabled) {
            let q = Query {
                text: String::new(),
                case_sensitive: false,
                whole_word: false,
                regex: false,
                module: Some(e.module.clone()),
                group: None,
            };
            if let Ok(m) = Matcher::new(&q) {
                v.push((e.module.name().to_string(), e.module.color(), m));
            }
        }
        if self.tabs[idx].syntax.as_deref() == Some("log") {
            let q = Query {
                text: ERRORS_PATTERN.into(),
                case_sensitive: true,
                whole_word: false,
                regex: true,
                module: None,
                group: None,
            };
            if let Ok(m) = Matcher::new(&q) {
                v.push(("Errors".into(), theme::ERROR, m));
            }
        }
        if self.find.open
            && let Ok(m) = self.find.matcher(&self.registry)
        {
            v.push((
                format!("Search: {}", self.find.describe(&self.registry)),
                theme::WARN,
                m,
            ));
        }
        v
    }

    /// Keep the active tab's heatmap current (debounced).
    pub(super) fn tick_heatmap(&mut self, ctx: &egui::Context) {
        if !self.settings.heatmap || self.tabs.is_empty() {
            return;
        }
        let idx = self.active;
        if self.tabs[idx].table_mode {
            return;
        }
        let key = self.heat_key(idx);
        let now = self.now;
        let tab = &mut self.tabs[idx];
        let small = tab
            .doc
            .source
            .as_ref()
            .is_none_or(|s| s.len() <= AUTO_SCAN_LIMIT);
        let st = &mut tab.heat;
        let fresh = st.map.is_some() && st.key == key && st.version == tab.doc.version;
        if fresh || st.running.as_deref() == Some(key.as_str()) || (!small && !st.requested) {
            st.changed_at = None;
            return;
        }
        let since = *st.changed_at.get_or_insert(now);
        // Scan right away the first time; after changes, wait until typing pauses.
        if st.map.is_some() && now - since < 0.6 {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
            return;
        }
        st.changed_at = None;
        self.start_heatmap(idx, key, ctx);
    }

    fn start_heatmap(&mut self, idx: usize, key: String, ctx: &egui::Context) {
        let layers = self.heat_layers(idx);
        let tab = &mut self.tabs[idx];
        let id = tab.id;
        tab.heat.requested = false;
        if layers.is_empty() {
            tab.heat.map = None;
            tab.heat.key = key;
            tab.heat.version = tab.doc.version;
            return;
        }
        tab.heat.running = Some(key.clone());
        let snap = Snapshot::of(&tab.doc);
        let version = tab.doc.version;
        let names: Vec<(String, Color32)> =
            layers.iter().map(|(n, c, _)| (n.clone(), *c)).collect();
        let matchers: Vec<Matcher> = layers.into_iter().map(|(_, _, m)| m).collect();
        self.cancel_jobs(id, |k| matches!(k, JobKind::Heatmap { .. }));
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Heatmap(snap.heatmap(&matchers, ctl))
        });
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::Heatmap {
                key,
                version,
                layers: names,
            },
            ctl,
            rx,
            version,
            label: "Mapping",
            started: Instant::now(),
        });
    }

    pub(super) fn finish_heatmap(
        &mut self,
        tab: u64,
        key: String,
        version: u64,
        layers: Vec<(String, Color32)>,
        map: Heatmap,
    ) {
        let Some(i) = self.tab_index(tab) else { return };
        let st = &mut self.tabs[i].heat;
        if st.running.as_deref() == Some(key.as_str()) {
            st.running = None;
        }
        *st = HeatState {
            map: Some(map),
            layers,
            key,
            version,
            running: st.running.take(),
            requested: false,
            changed_at: None,
        };
    }

    /// Paint the strip and handle hover / clicks on it.
    pub(super) fn heatmap_strip(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        visible: (usize, usize),
        ctx: &egui::Context,
    ) {
        let idx = self.active;
        let key = self.heat_key(idx);
        let progress = self
            .jobs
            .iter()
            .find(|j| j.tab == self.tabs[idx].id && matches!(j.kind, JobKind::Heatmap { .. }))
            .map(|j| j.ctl.fraction());
        let tab = &mut self.tabs[idx];
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, Color32::from_rgb(0x11, 0x13, 0x17));
        p.vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::BORDER),
        );
        let resp = ui.interact(rect, Id::new(("heatmap", tab.id)), Sense::click_and_drag());
        let inner = Rect::from_min_max(
            pos2(rect.left() + 2.0, rect.top() + 2.0),
            pos2(rect.right() - 1.0, rect.bottom() - 2.0),
        );
        let size = tab.doc.source.as_ref().map_or(0, |s| s.len());
        let mut rescan = false;

        if let Some(frac) = progress {
            let h = inner.height() * frac;
            p.rect_filled(
                Rect::from_min_size(inner.min, vec2(inner.width(), h)),
                0.0,
                theme::ACCENT.gamma_multiply(0.25),
            );
        }

        let Some(map) = &tab.heat.map else {
            let tip = if progress.is_some() {
                "Mapping where matches are in the file…".to_string()
            } else if key.is_empty() {
                "Heatmap: turn on a detector in Modules (or open Find) to see where it matches across the file.".to_string()
            } else {
                format!(
                    "Click to map where matches are across this {} file (reads it once).",
                    fmt_bytes(size)
                )
            };
            if progress.is_none() && !key.is_empty() {
                for k in 0..3 {
                    p.circle_filled(
                        pos2(inner.center().x, inner.top() + 12.0 + k as f32 * 7.0),
                        1.8,
                        theme::LINENO,
                    );
                }
            }
            let clicked = resp.clicked();
            resp.on_hover_text(tip);
            if clicked && progress.is_none() && !key.is_empty() {
                tab.heat.requested = true;
            }
            return;
        };

        let stale = tab.heat.key != key || tab.heat.version != tab.doc.version;
        // Only layers that matched something get a lane; extra ones share the last lane.
        let active: Vec<usize> = (0..map.layers.len())
            .filter(|&k| map.totals[k] > 0)
            .collect();
        let lanes = active.len().min(MAX_LANES);
        let lane_w = inner.width() / lanes.max(1) as f32;
        let rows = ((inner.height() / 2.0) as usize).max(1);
        let dim = if stale { 0.45 } else { 1.0 };
        for lane in 0..lanes {
            let members: Vec<usize> = if lane + 1 == lanes {
                active[lane..].to_vec()
            } else {
                vec![active[lane]]
            };
            let sums: Vec<u64> = (0..rows)
                .map(|r| {
                    let (b0, b1) = (
                        r * HEAT_BUCKETS / rows,
                        ((r + 1) * HEAT_BUCKETS / rows).max(r * HEAT_BUCKETS / rows + 1),
                    );
                    members
                        .iter()
                        .map(|&k| {
                            map.layers[k][b0..b1.min(HEAT_BUCKETS)]
                                .iter()
                                .map(|&c| c as u64)
                                .sum::<u64>()
                        })
                        .sum()
                })
                .collect();
            let max = sums.iter().copied().max().unwrap_or(0).max(1) as f32;
            let color = tab
                .heat
                .layers
                .get(active[lane])
                .map_or(theme::ACCENT, |l| l.1);
            let x = inner.left() + lane as f32 * lane_w;
            for (r, &s) in sums.iter().enumerate() {
                if s == 0 {
                    continue;
                }
                // A gentle power curve keeps dense spots clearly brighter than sparse ones.
                let intensity = (s as f32 / max).powf(0.55);
                let y = inner.top() + r as f32 * inner.height() / rows as f32;
                let cell = Rect::from_min_size(
                    pos2(x, y),
                    vec2(
                        (lane_w - 1.0).max(1.0),
                        (inner.height() / rows as f32).max(2.0),
                    ),
                );
                p.rect_filled(
                    cell,
                    0.0,
                    color.gamma_multiply((0.10 + 0.62 * intensity) * dim),
                );
            }
        }
        // "You are here".
        if let (Some(f0), Some(f1)) = (
            map.frac_of_line(visible.0, &tab.doc),
            map.frac_of_line(visible.1, &tab.doc),
        ) {
            let y0 = inner.top() + f0 * inner.height();
            let y1 = (inner.top() + f1 * inner.height()).max(y0 + 3.0);
            let r = Rect::from_min_max(pos2(rect.left() + 1.0, y0), pos2(rect.right(), y1));
            p.rect_filled(r, 1.0, Color32::from_white_alpha(22));
            p.rect_stroke(
                r,
                1.0,
                Stroke::new(1.0, Color32::from_white_alpha(110)),
                egui::StrokeKind::Inside,
            );
        }

        // Hover: what's here.
        let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
        let frac = pointer.map(|pp| ((pp.y - inner.top()) / inner.height()).clamp(0.0, 1.0));
        let target = frac.and_then(|f| map.target_at(f, &tab.doc));
        let totals: Vec<(String, Color32, u64)> = tab
            .heat
            .layers
            .iter()
            .zip(&map.totals)
            .map(|((n, c), t)| (n.clone(), *c, *t))
            .collect();
        let here: Vec<u64> = frac.map_or_else(Vec::new, |f| {
            let b = (f * HEAT_BUCKETS as f32) as usize;
            let (b0, b1) = (b.saturating_sub(8), (b + 8).min(HEAT_BUCKETS));
            map.layers
                .iter()
                .map(|l| l[b0..b1].iter().map(|&c| c as u64).sum())
                .collect()
        });
        let clicked = resp.clicked();
        let dragged = resp.dragged();
        let mut hide = false;
        let resp = resp.on_hover_ui(|ui| {
            ui.set_max_width(320.0);
            match &target {
                Some(AxisTarget::Line(l)) => {
                    ui.label(RichText::new(format!("Around line {}", fmt_int(l + 1))).strong());
                }
                Some(AxisTarget::Offset(_)) => {
                    ui.label(
                        RichText::new("Past the loaded part — click to load up to here").strong(),
                    );
                }
                None => {}
            }
            for (i, (name, color, total)) in totals.iter().enumerate() {
                ui.label(
                    RichText::new(format!(
                        "● {name}: {} here · {} in the file",
                        fmt_int(here.get(i).copied().unwrap_or(0) as usize),
                        fmt_int(*total as usize)
                    ))
                    .color(*color),
                );
            }
            if stale {
                ui.label(
                    RichText::new("Out of date — right-click → Rescan now").color(theme::WARN),
                );
            }
        });
        resp.context_menu(|ui| {
            if ui.button("Rescan now").clicked() {
                rescan = true;
                ui.close();
            }
            if ui.button("Hide heatmap").clicked() {
                hide = true;
                ui.close();
            }
        });

        match target {
            Some(AxisTarget::Line(l)) if clicked || dragged => {
                tab.view
                    .select(Pos::new(l, 0), Pos::new(l, 0), Reveal::Center);
                tab.view.request_focus = true;
            }
            Some(AxisTarget::Offset(o)) if clicked => {
                tab.pending_hit = Some((o, o));
                if let Some(src) = &tab.doc.source {
                    src.want_offset(o, ctx);
                }
            }
            _ => {}
        }
        if rescan {
            tab.heat.requested = true;
            tab.heat.version = u64::MAX; // force it to count as out of date
        }
        if hide {
            self.settings.heatmap = false;
            self.toast_info("Heatmap hidden — turn it back on from the View menu.");
        }
    }
}

#[cfg(test)]
impl HeatState {
    pub(super) fn map_for_test(&self) -> Option<(Vec<String>, Vec<u64>)> {
        let map = self.map.as_ref()?;
        Some((
            self.layers.iter().map(|(n, _)| n.clone()).collect(),
            map.totals.clone(),
        ))
    }
}
