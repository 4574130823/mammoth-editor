//! Table mode plumbing: toggling, debounced filter/sort jobs and "Open view as tab".

use std::time::{Duration, Instant};

use super::{Job, JobKind, JobOut, MammothApp, Pending};
use crate::search::{self, Snapshot};
use crate::table::{self, TableState};

impl MammothApp {
    pub(super) fn toggle_table(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some(delim) = crate::csv::delimiter_for(tab.syntax.as_deref()) else {
            self.toast_info("Table view works on CSV, TSV and pipe-separated files — pick one as the syntax in the status bar.");
            return;
        };
        if tab.table.as_ref().is_none_or(|t| t.delim != delim) {
            tab.table = Some(TableState::new(delim));
        }
        tab.table_mode = !tab.table_mode;
        match (tab.table_mode, tab.table.as_mut()) {
            (true, Some(t)) => t.request_focus = true,
            _ => tab.view.request_focus = true,
        }
    }

    /// Start filter/sort jobs once the user has stopped typing for a moment.
    pub(super) fn tick_tables(&mut self, ctx: &egui::Context) {
        let now = self.now;
        let mut start = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Some(st) = tab.table.as_mut() else {
                continue;
            };
            // Lines were added or removed: the view's line numbers are stale.
            if st.view.is_some()
                && st.view_lines != tab.doc.line_count()
                && st.dirty_since.is_none()
                && !st.busy
            {
                st.dirty_since = Some(now);
            }
            if let Some(t) = st.dirty_since {
                if now - t >= 0.35 {
                    st.dirty_since = None;
                    start.push(i);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(60));
                }
            }
        }
        for i in start {
            self.start_table_view(i, ctx);
        }
    }

    pub(super) fn start_table_view(&mut self, i: usize, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(i) else { return };
        let id = tab.id;
        self.cancel_jobs(id, |k| matches!(k, JobKind::TableView { .. }));
        let tab = &mut self.tabs[i];
        let Some(st) = tab.table.as_mut() else { return };
        if !st.constrained() {
            st.view = None;
            st.busy = false;
            return;
        }
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            st.busy = true;
            tab.pending = Some(Pending::TableView);
            self.toast_info(
                "Loading the entire file first so sorting and filtering see every row…",
            );
            return;
        }
        st.generation += 1;
        st.busy = true;
        let (spec, generation) = (st.spec(), st.generation);
        let label = if spec.sort.is_some() {
            "Sorting"
        } else {
            "Filtering"
        };
        let snap = Snapshot::of(&tab.doc);
        let lines = snap.line_count();
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::TableView(table::compute_view(&snap, &spec, ctl), lines)
        });
        let version = tab.doc.version;
        self.jobs.push(Job {
            tab: id,
            kind: JobKind::TableView { generation },
            ctl,
            rx,
            version,
            label,
            started: Instant::now(),
        });
    }

    pub(super) fn start_table_export(&mut self, i: usize, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(i) else { return };
        let Some(st) = &tab.table else { return };
        if !tab.doc.is_fully_loaded() {
            if let Some(src) = &tab.doc.source {
                src.load_all(ctx);
            }
            self.toast_info("Loading the entire file first — try again when it's done.");
            return;
        }
        let (header, view) = (st.header, st.view.clone());
        let snap = Snapshot::of(&tab.doc);
        let (ctl, rx) = search::spawn(snap.scan_size(), ctx, move |ctl| {
            JobOut::Exported(table::export_view(
                &snap,
                header,
                view.as_ref().map(|v| v.as_slice()),
                ctl,
            ))
        });
        let kind = JobKind::ExportView {
            title: format!("{} (view)", tab.doc.title),
            syntax: tab.syntax.clone(),
        };
        let (id, version) = (tab.id, tab.doc.version);
        self.jobs.push(Job {
            tab: id,
            kind,
            ctl,
            rx,
            version,
            label: "Exporting",
            started: Instant::now(),
        });
    }
}
