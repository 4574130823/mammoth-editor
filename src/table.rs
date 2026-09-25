//! Table view for CSV / TSV / PSV files: a virtualised grid with a sticky header,
//! click-to-sort columns, per-column filters and in-place cell editing.
//!
//! Sorting and filtering never rewrite the file. They produce a *view*, a list of
//! line numbers in display order, computed over the whole file in the background.
//! Cell edits go through the normal document (undoable) and only touch that field.

use std::cmp::Ordering;
use std::sync::Arc;

use egui::{
    Align, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Id, Key, Layout, Rect,
    RichText, Sense, Stroke, TextEdit, Ui, UiBuilder, pos2, vec2,
};

use crate::csv;
use crate::document::{Document, Pos};
use crate::editor::{digits, fmt_int};
use crate::modules::Registry;
use crate::search::{JobCtl, Snapshot};
use crate::theme;

const SCROLLBAR: f32 = 12.0;
const MAX_SORT_ROWS: usize = 20_000_000;
const MAX_VIEW_ROWS: usize = 50_000_000;
const COLUMN_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xe6, 0xe6, 0xe6),
    Color32::from_rgb(0x6c, 0xb6, 0xff),
    Color32::from_rgb(0x7e, 0xe0, 0x9a),
    Color32::from_rgb(0xff, 0xc6, 0x6d),
    Color32::from_rgb(0xd7, 0x9b, 0xff),
    Color32::from_rgb(0x5e, 0xe0, 0xd8),
    Color32::from_rgb(0xff, 0x8f, 0x8f),
    Color32::from_rgb(0xc8, 0xd8, 0x6a),
];

pub struct TableState {
    pub delim: u8,
    /// The first line holds column names.
    pub header: bool,
    widths: Vec<f32>,
    top: usize,
    y_off: f32,
    scroll_x: f32,
    /// Selected (view row, column).
    pub sel: (usize, usize),
    edit: Option<CellEdit>,
    /// Doc lines in display order (without the header); `None` = file order.
    pub view: Option<Arc<Vec<usize>>>,
    /// Line count the view was computed for (edits that add/remove lines invalidate it).
    pub view_lines: usize,
    pub sort: Option<(usize, bool)>,
    pub filters: Vec<String>,
    /// When filters/sort last changed; the app debounces view jobs on this.
    pub dirty_since: Option<f64>,
    pub busy: bool,
    /// Bumped per view job so stale results can be dropped.
    pub generation: u64,
    pub request_focus: bool,
    reveal: bool,
    resizing: Option<(usize, f32)>,
}

struct CellEdit {
    line: usize,
    col: usize,
    text: String,
    fresh: bool,
}

pub struct Env<'a> {
    pub registry: &'a Registry,
    pub font_size: f32,
    pub now: f64,
}

#[derive(Default)]
pub struct Output {
    pub status: Option<String>,
    pub open_view_as_tab: bool,
    pub to_text: bool,
}

impl TableState {
    pub fn new(delim: u8) -> Self {
        Self {
            delim,
            header: true,
            widths: Vec::new(),
            top: 0,
            y_off: 0.0,
            scroll_x: 0.0,
            sel: (0, 0),
            edit: None,
            view: None,
            view_lines: 0,
            sort: None,
            filters: Vec::new(),
            dirty_since: None,
            busy: false,
            generation: 0,
            request_focus: true,
            reveal: false,
            resizing: None,
        }
    }

    /// Whether a sort or any filter is active.
    pub fn constrained(&self) -> bool {
        self.sort.is_some() || self.filters.iter().any(|f| !f.trim().is_empty())
    }

    pub fn row_count(&self, doc: &Document) -> usize {
        match &self.view {
            Some(v) => v.len(),
            None => {
                let n = doc.line_count();
                let mut rows = n.saturating_sub(self.header as usize);
                if rows > 0 && doc.line(n - 1).is_empty() {
                    rows -= 1; // trailing newline
                }
                rows
            }
        }
    }

    pub fn row_line(&self, row: usize) -> usize {
        match &self.view {
            Some(v) => v[row],
            None => row + self.header as usize,
        }
    }

    /// Everything the background view job needs.
    pub fn spec(&self) -> ViewSpec {
        ViewSpec {
            delim: self.delim,
            header: self.header,
            filters: self
                .filters
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.trim().is_empty())
                .map(|(i, f)| (i, f.clone()))
                .collect(),
            sort: self.sort,
        }
    }

    /// Select the cell holding `byte` of doc `line` (after a Find, say).
    pub fn select_line(&mut self, doc: &Document, line: usize, byte: usize) {
        let row = match &self.view {
            Some(v) => v.iter().position(|&l| l == line),
            None => line.checked_sub(self.header as usize),
        };
        if let Some(row) = row {
            let text = doc.line(line);
            let col = csv::field_spans(&text, self.delim)
                .iter()
                .position(|&(_, b)| byte <= b)
                .unwrap_or(0);
            self.sel = (row, col);
            self.reveal = true;
        }
    }
}

// ----------------------------------------------------------------------------
// Background view computation

pub struct ViewSpec {
    pub delim: u8,
    pub header: bool,
    pub filters: Vec<(usize, String)>,
    pub sort: Option<(usize, bool)>,
}

enum Pred {
    Contains(String),
    NotContains(String),
    Equals(String),
    Empty,
    NotEmpty,
    Cmp(Ordering, bool, f64),
    Regex(regex::Regex),
}

impl Pred {
    /// Filter syntax: `text` contains · `!text` doesn't contain · `=text` equals ·
    /// `=` empty · `!` not empty · `>10` `>=10` `<10` `<=10` numeric · `/re/` regex.
    fn parse(f: &str) -> Result<Pred, String> {
        let f = f.trim();
        if let Some(re) = f
            .strip_prefix('/')
            .and_then(|r| r.strip_suffix('/'))
            .filter(|r| !r.is_empty())
        {
            return regex::RegexBuilder::new(re)
                .case_insensitive(true)
                .build()
                .map(Pred::Regex)
                .map_err(|e| format!("Bad regex filter: {e}"));
        }
        for (prefix, ord, eq) in [
            (">=", Ordering::Greater, true),
            ("<=", Ordering::Less, true),
            (">", Ordering::Greater, false),
            ("<", Ordering::Less, false),
        ] {
            if let Some(n) = f.strip_prefix(prefix)
                && let Ok(n) = n.trim().parse::<f64>()
            {
                return Ok(Pred::Cmp(ord, eq, n));
            }
        }
        Ok(match f {
            "=" => Pred::Empty,
            "!" => Pred::NotEmpty,
            _ if f.starts_with('=') => Pred::Equals(f[1..].to_lowercase()),
            _ if f.starts_with('!') => Pred::NotContains(f[1..].to_lowercase()),
            _ => Pred::Contains(f.to_lowercase()),
        })
    }

    fn test(&self, v: &str) -> bool {
        match self {
            Pred::Contains(s) => v.to_lowercase().contains(s),
            Pred::NotContains(s) => !v.to_lowercase().contains(s),
            Pred::Equals(s) => v.trim().to_lowercase() == *s,
            Pred::Empty => v.trim().is_empty(),
            Pred::NotEmpty => !v.trim().is_empty(),
            Pred::Cmp(ord, eq, n) => v
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(|x| x.partial_cmp(n))
                .is_some_and(|o| o == *ord || (*eq && o == Ordering::Equal)),
            Pred::Regex(re) => re.is_match(v),
        }
    }
}

enum SortKey {
    Num(f64),
    Text(String),
}

fn sort_key(v: &str) -> Option<SortKey> {
    let t = v.trim();
    if t.is_empty() {
        return None;
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Some(SortKey::Num(n)),
        _ => Some(SortKey::Text(t.to_lowercase())),
    }
}

fn cmp_keys(a: &SortKey, b: &SortKey) -> Ordering {
    match (a, b) {
        (SortKey::Num(x), SortKey::Num(y)) => x.total_cmp(y),
        (SortKey::Num(_), SortKey::Text(_)) => Ordering::Less,
        (SortKey::Text(_), SortKey::Num(_)) => Ordering::Greater,
        (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
    }
}

/// Filter and sort the whole file. `Ok(lines)` in display order.
pub fn compute_view(snap: &Snapshot, spec: &ViewSpec, ctl: &JobCtl) -> Result<Vec<usize>, String> {
    if !snap.is_complete() {
        return Err("The whole file must be loaded first.".into());
    }
    let preds: Vec<(usize, Pred)> = spec
        .filters
        .iter()
        .map(|(c, f)| Pred::parse(f).map(|p| (*c, p)))
        .collect::<Result<_, _>>()?;
    let mut rows: Vec<usize> = Vec::new();
    let mut keys: Vec<Option<SortKey>> = Vec::new();
    let mut too_many = false;
    snap.for_each_line(ctl, |line_no, bytes| {
        if (spec.header && line_no == 0) || bytes.is_empty() {
            return true;
        }
        let text = String::from_utf8_lossy(bytes);
        if preds
            .iter()
            .any(|(c, p)| !p.test(&csv::field(&text, spec.delim, *c)))
        {
            return true;
        }
        rows.push(line_no);
        if let Some((c, _)) = spec.sort {
            keys.push(sort_key(&csv::field(&text, spec.delim, c)));
        }
        if rows.len() >= MAX_VIEW_ROWS || (spec.sort.is_some() && rows.len() > MAX_SORT_ROWS) {
            too_many = true;
            return false;
        }
        true
    });
    if ctl.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    if too_many {
        return Err(format!(
            "More than {} rows match — add a filter to narrow it down first.",
            fmt_int(if spec.sort.is_some() {
                MAX_SORT_ROWS
            } else {
                MAX_VIEW_ROWS
            })
        ));
    }
    if let Some((_, desc)) = spec.sort {
        let mut order: Vec<usize> = (0..rows.len()).collect();
        // Empty cells always go last; ties keep file order (stable sort).
        order.sort_by(|&a, &b| match (&keys[a], &keys[b]) {
            (Some(x), Some(y)) => {
                let o = cmp_keys(x, y);
                if desc { o.reverse() } else { o }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
        rows = order.into_iter().map(|i| rows[i]).collect();
    }
    Ok(rows)
}

/// The header plus the rows of `view` (or all rows), for "Open view as tab".
pub fn export_view(
    snap: &Snapshot,
    header: bool,
    view: Option<&[usize]>,
    ctl: &JobCtl,
) -> Option<Vec<String>> {
    let mut out = Vec::new();
    match view {
        None => {
            snap.for_each_line(ctl, |_, b| {
                out.push(String::from_utf8_lossy(b).into_owned());
                out.len() < MAX_VIEW_ROWS
            });
        }
        Some(view) => {
            let pos: std::collections::HashMap<usize, usize> =
                view.iter().enumerate().map(|(i, &l)| (l, i)).collect();
            let mut rows: Vec<String> = vec![String::new(); view.len()];
            let mut head = None;
            snap.for_each_line(ctl, |l, b| {
                if header && l == 0 {
                    head = Some(String::from_utf8_lossy(b).into_owned());
                } else if let Some(&i) = pos.get(&l) {
                    rows[i] = String::from_utf8_lossy(b).into_owned();
                }
                true
            });
            out.extend(head);
            out.extend(rows);
        }
    }
    (!ctl.cancel.load(std::sync::atomic::Ordering::Relaxed)).then_some(out)
}

// ----------------------------------------------------------------------------
// The widget

struct Metrics {
    font: FontId,
    cw: f32,
    row_h: f32,
}

fn column_name(doc: &Document, st: &TableState, col: usize) -> String {
    if st.header && doc.line_count() > 0 {
        let h = csv::field(&doc.line(0), st.delim, col).trim().to_string();
        if !h.is_empty() {
            return h;
        }
    }
    format!("Column {}", col + 1)
}

fn measure(doc: &Document, st: &mut TableState, m: &Metrics) {
    let mut chars: Vec<usize> = Vec::new();
    let rows = st.row_count(doc);
    let mut sample: Vec<usize> = (0..rows.min(200)).map(|r| st.row_line(r)).collect();
    if st.header && doc.line_count() > 0 {
        sample.push(0);
    }
    for line in sample {
        let text = doc.line(line);
        for (c, &(a, b)) in csv::field_spans(&text, st.delim).iter().enumerate() {
            let n = csv::value(&text[a..b]).chars().count();
            if c >= chars.len() {
                chars.resize(c + 1, 0);
            }
            chars[c] = chars[c].max(n);
        }
    }
    st.widths = chars
        .iter()
        .map(|&n| (n as f32 * m.cw + 24.0).clamp(64.0, 380.0))
        .collect();
    if st.widths.is_empty() {
        st.widths.push(120.0);
    }
}

pub fn show(ui: &mut Ui, id: Id, doc: &mut Document, st: &mut TableState, env: &Env) -> Output {
    let mut out = Output::default();
    let font = FontId::monospace(env.font_size);
    let (cw, glyph_h) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let m = Metrics {
        font,
        cw,
        row_h: (glyph_h * 1.75).round(),
    };

    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());
    if st.widths.is_empty() && doc.line_count() > 0 {
        measure(doc, st, &m);
    }
    let rows = st.row_count(doc);
    if rows > 0 {
        st.sel.0 = st.sel.0.min(rows - 1);
    }
    let ncols = st.widths.len();
    st.sel.1 = st.sel.1.min(ncols.saturating_sub(1));
    if st.filters.len() < ncols {
        st.filters.resize(ncols, String::new());
    }

    // ---- layout
    let toolbar = Rect::from_min_size(outer.min, vec2(outer.width(), 36.0));
    let head = Rect::from_min_size(
        pos2(outer.left(), toolbar.bottom()),
        vec2(outer.width() - SCROLLBAR, m.row_h + 6.0),
    );
    let filt = Rect::from_min_size(
        pos2(outer.left(), head.bottom()),
        vec2(head.width(), m.row_h + 4.0),
    );
    let body = Rect::from_min_max(
        pos2(outer.left(), filt.bottom()),
        pos2(outer.right() - SCROLLBAR, outer.bottom() - SCROLLBAR),
    );
    let max_line = st
        .view
        .as_ref()
        .and_then(|v| v.iter().max().copied())
        .unwrap_or(doc.line_count());
    let num_w = (digits(max_line + 1) as f32 + 1.0) * cw + 14.0;
    let cells_left = body.left() + num_w;
    let visible_rows = ((body.height() / m.row_h).floor() as usize).max(1);
    let max_top = rows.saturating_sub(visible_rows.saturating_sub(1).max(1));
    let total_w: f32 = st.widths.iter().sum();
    let col_x =
        |st: &TableState, c: usize| cells_left - st.scroll_x + st.widths[..c].iter().sum::<f32>();

    toolbar_ui(ui, toolbar, doc, st, rows, &mut out);

    // ---- focus & keyboard
    let body_resp = ui.interact(body, id, Sense::click_and_drag());
    if body_resp.clicked() || st.request_focus {
        ui.memory_mut(|mem| mem.request_focus(id));
        st.request_focus = false;
    }
    let focused = ui.memory(|mem| mem.has_focus(id));
    if focused {
        ui.memory_mut(|mem| {
            mem.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        if st.edit.is_none() && rows > 0 {
            for ev in ui.input(|i| i.events.clone()) {
                if let Err(e) = handle_key(ui.ctx(), doc, st, &ev, rows, visible_rows, env.now) {
                    out.status = Some(e);
                }
            }
        }
    }

    // ---- scrolling
    if ui.rect_contains_pointer(outer) {
        let d = ui.input(|i| i.smooth_scroll_delta());
        if d.y != 0.0 {
            let total = st.y_off - d.y;
            let lines = (total / m.row_h).floor();
            let t = (st.top as i64 + lines as i64).clamp(0, max_top as i64);
            st.y_off = if t == 0 && lines < 0.0 || t as usize == max_top {
                0.0
            } else {
                total - lines * m.row_h
            };
            st.top = t as usize;
        }
        if d.x != 0.0 {
            st.scroll_x =
                (st.scroll_x - d.x).clamp(0.0, (total_w - (body.width() - num_w) + 40.0).max(0.0));
        }
    }
    if st.reveal && rows > 0 {
        st.reveal = false;
        let r = st.sel.0;
        if r < st.top {
            st.top = r;
            st.y_off = 0.0;
        } else if r >= st.top + visible_rows {
            st.top = (r + 1).saturating_sub(visible_rows);
            st.y_off = 0.0;
        }
        let x0 = st.widths[..st.sel.1].iter().sum::<f32>();
        let x1 = x0 + st.widths[st.sel.1];
        let view_w = body.width() - num_w;
        if x0 < st.scroll_x {
            st.scroll_x = x0;
        } else if x1 > st.scroll_x + view_w {
            st.scroll_x = x1 - view_w;
        }
    }
    st.top = st.top.min(max_top);

    // ---- mouse on cells
    let pointer = ui.input(|i| i.pointer.interact_pos());
    if let Some(p) = pointer
        && (body_resp.clicked() || body_resp.double_clicked())
    {
        let row = st.top + ((p.y - body.top() + st.y_off) / m.row_h).floor().max(0.0) as usize;
        let mut x = cells_left - st.scroll_x;
        let mut col = None;
        for (c, w) in st.widths.iter().enumerate() {
            if p.x >= x && p.x < x + w {
                col = Some(c);
            }
            x += w;
        }
        if let (true, Some(col)) = (row < rows, col) {
            st.sel = (row, col);
            if body_resp.double_clicked() {
                begin_edit(doc, st, None);
            }
        }
    }

    let painter = ui.painter_at(outer);
    painter.rect_filled(
        Rect::from_min_max(toolbar.left_bottom(), outer.max),
        0.0,
        theme::EDITOR_BG,
    );

    // ---- body
    let body_p = painter.with_clip_rect(Rect::from_min_max(pos2(cells_left, body.top()), body.max));
    let num_p = painter.with_clip_rect(Rect::from_min_max(
        body.min,
        pos2(cells_left, body.bottom()),
    ));
    num_p.rect_filled(
        Rect::from_min_max(body.min, pos2(cells_left, body.bottom())),
        0.0,
        theme::GUTTER_BG,
    );
    let detectors: Vec<_> = env
        .registry
        .detectors()
        .filter(|(_, e)| e.enabled)
        .map(|(_, e)| e)
        .collect();
    let mut hits = Vec::new();
    let last = (st.top + visible_rows + 2).min(rows);
    let first_col = (0..ncols)
        .find(|&c| col_x(st, c) + st.widths[c] >= cells_left)
        .unwrap_or(ncols);
    for (i, row) in (st.top..last).enumerate() {
        let y = body.top() + i as f32 * m.row_h - st.y_off;
        let line = st.row_line(row);
        let text = doc.line(line);
        let spans = csv::field_spans(&text, st.delim);
        let row_rect = Rect::from_min_size(pos2(cells_left, y), vec2(body.width(), m.row_h));
        if row == st.sel.0 {
            body_p.rect_filled(row_rect, 0.0, theme::CURRENT_LINE);
        }
        num_p.text(
            pos2(cells_left - 8.0, y + m.row_h / 2.0),
            Align2::RIGHT_CENTER,
            fmt_int(line + 1),
            FontId::monospace(env.font_size * 0.85),
            if row == st.sel.0 {
                theme::LINENO_ACTIVE
            } else {
                theme::LINENO
            },
        );
        for c in first_col..ncols {
            let x = col_x(st, c);
            if x > body.right() {
                break;
            }
            let cell = Rect::from_min_size(pos2(x, y), vec2(st.widths[c], m.row_h));
            let Some(&(a, b)) = spans.get(c) else {
                continue;
            };
            let value = csv::value(&text[a..b]);
            let mut color = theme::TEXT;
            if value.len() < 1024 {
                for d in &detectors {
                    hits.clear();
                    d.detect(&value, &mut hits);
                    if !hits.is_empty() {
                        color = d.module.color();
                        break;
                    }
                }
            }
            let numeric = !value.is_empty() && value.trim().parse::<f64>().is_ok();
            let (anchor, px) = if numeric {
                (Align2::RIGHT_CENTER, cell.right() - 8.0)
            } else {
                (Align2::LEFT_CENTER, cell.left() + 8.0)
            };
            let shown: String = value
                .chars()
                .take(400)
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect();
            body_p
                .with_clip_rect(cell.shrink2(vec2(4.0, 0.0)).intersect(body))
                .text(
                    pos2(px, cell.center().y),
                    anchor,
                    shown,
                    m.font.clone(),
                    color,
                );
        }
        body_p.hline(
            row_rect.x_range(),
            y + m.row_h - 0.5,
            Stroke::new(1.0, Color32::from_white_alpha(6)),
        );
    }
    // Column separators.
    for c in first_col..ncols {
        let x = col_x(st, c) + st.widths[c];
        if x > body.right() {
            break;
        }
        body_p.vline(
            x,
            body.y_range(),
            Stroke::new(1.0, Color32::from_white_alpha(10)),
        );
    }
    // Selected cell outline.
    if rows > 0 && st.sel.0 >= st.top && st.sel.0 < last && ncols > 0 {
        let y = body.top() + (st.sel.0 - st.top) as f32 * m.row_h - st.y_off;
        let cell = Rect::from_min_size(
            pos2(col_x(st, st.sel.1), y),
            vec2(st.widths[st.sel.1], m.row_h),
        );
        body_p.rect_stroke(
            cell.shrink(1.0),
            2.0,
            Stroke::new(1.5, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        let result = st.edit.as_mut().and_then(|e| edit_cell(ui, e, cell, &m));
        if let Some(r) = result {
            finish_edit(doc, st, r, env.now, &mut out);
        }
    } else if st.edit.is_some() {
        st.edit = None;
    }
    if rows == 0 {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            if st.constrained() {
                "No rows match these filters."
            } else {
                "No rows."
            },
            FontId::proportional(14.0),
            theme::TEXT_DIM,
        );
    }

    // ---- header
    painter.rect_filled(head, 0.0, theme::PANEL_BG_2);
    let head_p = painter.with_clip_rect(Rect::from_min_max(pos2(cells_left, head.top()), head.max));
    painter.text(
        pos2(body.left() + 8.0, head.center().y),
        Align2::LEFT_CENTER,
        "#",
        FontId::proportional(12.0),
        theme::LINENO_ACTIVE,
    );
    for c in first_col..ncols {
        let x = col_x(st, c);
        if x > head.right() {
            break;
        }
        let cell = Rect::from_min_size(pos2(x, head.top()), vec2(st.widths[c], head.height()));
        let name = column_name(doc, st, c);
        head_p
            .with_clip_rect(cell.shrink2(vec2(6.0, 0.0)).intersect(head))
            .text(
                pos2(cell.left() + 8.0, cell.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(env.font_size * 0.95),
                COLUMN_COLORS[c % COLUMN_COLORS.len()],
            );
        if let Some((sc, desc)) = st.sort.filter(|(sc, _)| *sc == c) {
            let _ = sc;
            let t = pos2(cell.right() - 12.0, cell.center().y);
            let pts = if desc {
                vec![
                    t + vec2(-4.0, -2.0),
                    t + vec2(4.0, -2.0),
                    t + vec2(0.0, 3.0),
                ]
            } else {
                vec![t + vec2(-4.0, 2.0), t + vec2(4.0, 2.0), t + vec2(0.0, -3.0)]
            };
            head_p.add(egui::Shape::convex_polygon(
                pts,
                theme::ACCENT,
                Stroke::NONE,
            ));
        }
        head_p.vline(
            cell.right(),
            cell.y_range(),
            Stroke::new(1.0, theme::BORDER),
        );
        // Click to sort (asc → desc → off), drag the right edge to resize.
        let grip = Rect::from_min_max(
            pos2(cell.right() - 4.0, cell.top()),
            pos2(cell.right() + 4.0, cell.bottom()),
        );
        let gr = ui.interact(grip, id.with(("grip", c)), Sense::drag());
        if gr.hovered() || gr.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if gr.drag_started() {
            st.resizing = Some((c, st.widths[c]));
        }
        if let (Some((rc, _)), true) = (st.resizing, gr.dragged())
            && rc == c
        {
            st.widths[c] = (st.widths[c] + gr.drag_delta().x).clamp(40.0, 1200.0);
        }
        if gr.drag_stopped() {
            st.resizing = None;
        }
        let hr = ui.interact(
            Rect::from_min_max(cell.min, pos2(grip.left(), cell.bottom())),
            id.with(("head", c)),
            Sense::click(),
        );
        if hr.clicked() {
            st.sort = match st.sort {
                Some((sc, false)) if sc == c => Some((c, true)),
                Some((sc, true)) if sc == c => None,
                _ => Some((c, false)),
            };
            st.dirty_since = Some(0.0);
        }
        hr.on_hover_text("Click to sort");
    }
    painter.hline(
        head.x_range(),
        head.bottom() - 0.5,
        Stroke::new(1.0, theme::BORDER),
    );

    // ---- filter row
    painter.rect_filled(filt, 0.0, theme::PANEL_BG);
    painter.text(
        pos2(body.left() + 8.0, filt.center().y),
        Align2::LEFT_CENTER,
        "filter",
        FontId::proportional(10.5),
        theme::LINENO,
    );
    let clip = Rect::from_min_max(pos2(cells_left, filt.top()), filt.max);
    let mut fui = ui.new_child(
        UiBuilder::new()
            .max_rect(filt)
            .layout(Layout::left_to_right(Align::Center)),
    );
    fui.set_clip_rect(clip);
    for c in first_col..ncols {
        let x = col_x(st, c);
        if x > filt.right() {
            break;
        }
        let cell = Rect::from_min_size(pos2(x, filt.top()), vec2(st.widths[c], filt.height()))
            .shrink2(vec2(3.0, 3.0));
        let te = TextEdit::singleline(&mut st.filters[c])
            .id(id.with(("filter", c)))
            .hint_text("…")
            .font(FontId::proportional(12.5))
            .margin(vec2(6.0, 2.0));
        if fui.put(cell, te).on_hover_text(FILTER_HELP).changed() {
            st.dirty_since = Some(env.now);
        }
    }
    painter.hline(
        filt.x_range(),
        filt.bottom() - 0.5,
        Stroke::new(1.0, theme::BORDER),
    );

    // ---- scrollbars
    let vbar = Rect::from_min_max(
        pos2(outer.right() - SCROLLBAR, head.top()),
        pos2(outer.right(), outer.bottom() - SCROLLBAR),
    );
    painter.rect_filled(vbar, 0.0, theme::SCROLL_TRACK);
    if max_top > 0 {
        let track = vbar.shrink(3.0);
        let th = (track.height() * visible_rows as f32 / (max_top + visible_rows) as f32)
            .clamp(28.0_f32.min(track.height()), track.height());
        let ty = track.top() + (st.top as f32 / max_top as f32) * (track.height() - th);
        let thumb = Rect::from_min_size(pos2(track.left(), ty), vec2(track.width(), th));
        let vr = ui.interact(vbar, id.with("vbar"), Sense::click_and_drag());
        if let (Some(p), true) = (pointer, vr.dragged() || vr.clicked()) {
            let f =
                ((p.y - track.top() - th / 2.0) / (track.height() - th).max(1.0)).clamp(0.0, 1.0);
            st.top = (f as f64 * max_top as f64).round() as usize;
            st.y_off = 0.0;
        }
        painter.rect_filled(
            thumb,
            4.0,
            if vr.hovered() || vr.dragged() {
                theme::SCROLL_THUMB_HOT
            } else {
                theme::SCROLL_THUMB
            },
        );
    }
    let hbar = Rect::from_min_max(
        pos2(cells_left, outer.bottom() - SCROLLBAR),
        pos2(outer.right() - SCROLLBAR, outer.bottom()),
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(outer.left(), hbar.top()), outer.max),
        0.0,
        theme::SCROLL_TRACK,
    );
    let view_w = body.width() - num_w;
    if total_w > view_w {
        let track = hbar.shrink(3.0);
        let max_x = total_w - view_w + 40.0;
        let tw = (track.width() * view_w / total_w).clamp(28.0, track.width());
        let tx = track.left() + (st.scroll_x / max_x).clamp(0.0, 1.0) * (track.width() - tw);
        let thumb = Rect::from_min_size(pos2(tx, track.top()), vec2(tw, track.height()));
        let hr = ui.interact(hbar, id.with("hbar"), Sense::click_and_drag());
        if let (Some(p), true) = (pointer, hr.dragged() || hr.clicked()) {
            let f =
                ((p.x - track.left() - tw / 2.0) / (track.width() - tw).max(1.0)).clamp(0.0, 1.0);
            st.scroll_x = f * max_x;
        }
        painter.rect_filled(
            thumb,
            4.0,
            if hr.hovered() || hr.dragged() {
                theme::SCROLL_THUMB_HOT
            } else {
                theme::SCROLL_THUMB
            },
        );
    }
    out
}

const FILTER_HELP: &str = "Filter this column:\n\
    text — contains (any case)\n\
    !text — doesn't contain\n\
    =text — equals · = empty · ! not empty\n\
    >10  >=10  <10  <=10 — numbers\n\
    /regex/ — regular expression";

fn toolbar_ui(
    ui: &mut Ui,
    rect: Rect,
    doc: &Document,
    st: &mut TableState,
    rows: usize,
    out: &mut Output,
) {
    ui.painter().rect_filled(rect, 0.0, theme::PANEL_BG);
    let mut tb = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 4.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    tb.spacing_mut().item_spacing.x = 10.0;
    tb.label(
        RichText::new("TABLE")
            .strong()
            .size(11.5)
            .color(theme::ACCENT),
    );
    let before = st.header;
    tb.checkbox(&mut st.header, "First row is the header");
    if before != st.header {
        st.widths.clear();
        st.view = None;
        if st.constrained() {
            st.dirty_since = Some(0.0);
        }
    }
    let total = {
        let n = doc.line_count();
        let mut r = n.saturating_sub(st.header as usize);
        if r > 0 && doc.line(n - 1).is_empty() {
            r -= 1;
        }
        r
    };
    let text = if st.view.is_some() {
        format!("{} of {} rows", fmt_int(rows), fmt_int(total))
    } else {
        format!("{} rows", fmt_int(rows))
    };
    tb.label(RichText::new(text).color(theme::TEXT_DIM));
    if st.busy {
        tb.add(egui::Spinner::new().size(13.0).color(theme::ACCENT));
    }
    if let Some((c, desc)) = st.sort {
        tb.label(
            RichText::new(format!(
                "sorted by {}, {}",
                column_name(doc, st, c),
                if desc { "descending" } else { "ascending" }
            ))
            .color(theme::TEXT_DIM),
        );
    }
    tb.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .button("Text view")
            .on_hover_text("Back to plain text (Ctrl+Shift+T)")
            .clicked()
        {
            out.to_text = true;
        }
        if ui
            .button("Open view as tab")
            .on_hover_text("These rows, in this order, as a new file")
            .clicked()
        {
            out.open_view_as_tab = true;
        }
        if st.constrained() && ui.button("Clear filters & sort").clicked() {
            st.filters.iter_mut().for_each(String::clear);
            st.sort = None;
            st.view = None;
            st.dirty_since = None;
        }
    });
}

fn current_value(doc: &Document, st: &TableState) -> Option<(usize, String)> {
    if st.row_count(doc) == 0 {
        return None;
    }
    let line = st.row_line(st.sel.0);
    Some((
        line,
        csv::field(&doc.line(line), st.delim, st.sel.1).into_owned(),
    ))
}

fn begin_edit(doc: &Document, st: &mut TableState, initial: Option<String>) {
    if let Some((line, value)) = current_value(doc, st) {
        if doc.is_soft(line) {
            return;
        }
        st.edit = Some(CellEdit {
            line,
            col: st.sel.1,
            text: initial.unwrap_or(value),
            fresh: true,
        });
    }
}

fn set_cell(
    doc: &mut Document,
    st: &TableState,
    line: usize,
    col: usize,
    value: &str,
    now: f64,
) -> Result<(), String> {
    let old = doc.line(line).into_owned();
    let new = csv::set_field(&old, st.delim, col, value);
    if new != old {
        let p = (Pos::new(line, 0), Pos::new(line, 0));
        doc.replace_lines(line, line + 1, vec![new], p, p, now)?;
    }
    Ok(())
}

/// How an inline edit ended: commit (and where to move next) or cancel.
struct EditEnd {
    commit: bool,
    dr: isize,
    dc: isize,
}

/// The inline editor over `cell`. Returns how the edit ended, once it has.
fn edit_cell(ui: &mut Ui, e: &mut CellEdit, cell: Rect, m: &Metrics) -> Option<EditEnd> {
    let id = Id::new(("cell-edit", e.line, e.col));
    let te = TextEdit::singleline(&mut e.text)
        .id(id)
        .font(m.font.clone())
        .margin(vec2(7.0, 4.0))
        .desired_width(cell.width());
    let r = ui.put(cell, te);
    if e.fresh {
        r.request_focus();
        if let Some(mut state) = TextEdit::load_state(ui.ctx(), id) {
            let end = egui::text::CCursor::new(e.text.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        }
        e.fresh = false;
        return None;
    }
    if !r.lost_focus() {
        return None;
    }
    let (esc, enter, tab, shift) = ui.input(|i| {
        (
            i.key_pressed(Key::Escape),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Tab),
            i.modifiers.shift,
        )
    });
    let back = if shift { -1 } else { 1 };
    Some(match (esc, enter, tab) {
        (true, _, _) => EditEnd {
            commit: false,
            dr: 0,
            dc: 0,
        },
        (_, true, _) => EditEnd {
            commit: true,
            dr: back,
            dc: 0,
        },
        (_, _, true) => EditEnd {
            commit: true,
            dr: 0,
            dc: back,
        },
        _ => EditEnd {
            commit: true,
            dr: 0,
            dc: 0,
        },
    })
}

fn finish_edit(doc: &mut Document, st: &mut TableState, end: EditEnd, now: f64, out: &mut Output) {
    let EditEnd { commit, dr, dc } = end;
    let Some(e) = st.edit.take() else { return };
    if commit {
        if let Err(err) = set_cell(doc, st, e.line, e.col, &e.text, now) {
            out.status = Some(err);
        }
        let rows = st.row_count(doc);
        st.sel.0 = (st.sel.0 as isize + dr).clamp(0, rows.saturating_sub(1) as isize) as usize;
        st.sel.1 =
            (st.sel.1 as isize + dc).clamp(0, st.widths.len().saturating_sub(1) as isize) as usize;
        st.reveal = true;
    }
    st.request_focus = true;
}

fn handle_key(
    ctx: &egui::Context,
    doc: &mut Document,
    st: &mut TableState,
    ev: &Event,
    rows: usize,
    page: usize,
    now: f64,
) -> Result<(), String> {
    let ncols = st.widths.len().max(1);
    let mv = |st: &mut TableState, dr: isize, dc: isize| {
        st.sel.0 = (st.sel.0 as isize + dr).clamp(0, rows as isize - 1) as usize;
        st.sel.1 = (st.sel.1 as isize + dc).clamp(0, ncols as isize - 1) as usize;
        st.reveal = true;
    };
    match ev {
        Event::Text(t) if !t.is_empty() => begin_edit(doc, st, Some(t.clone())),
        Event::Copy | Event::Cut => {
            if let Some((line, v)) = current_value(doc, st) {
                ctx.copy_text(v);
                if matches!(ev, Event::Cut) {
                    set_cell(doc, st, line, st.sel.1, "", now)?;
                }
            }
        }
        Event::Paste(t) => {
            if let Some((line, _)) = current_value(doc, st) {
                let v = t.lines().next().unwrap_or("");
                set_cell(doc, st, line, st.sel.1, v, now)?;
            }
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let ctrl = modifiers.command || modifiers.ctrl;
            match key {
                Key::ArrowUp => mv(st, -1, 0),
                Key::ArrowDown => mv(st, 1, 0),
                Key::ArrowLeft => mv(st, 0, -1),
                Key::ArrowRight => mv(st, 0, 1),
                Key::Tab => mv(st, 0, if modifiers.shift { -1 } else { 1 }),
                Key::PageUp => mv(st, -(page as isize), 0),
                Key::PageDown => mv(st, page as isize, 0),
                Key::Home if ctrl => mv(st, -(rows as isize), -(ncols as isize)),
                Key::End if ctrl => mv(st, rows as isize, ncols as isize),
                Key::Home => mv(st, 0, -(ncols as isize)),
                Key::End => mv(st, 0, ncols as isize),
                Key::Enter | Key::F2 => begin_edit(doc, st, None),
                Key::Delete | Key::Backspace => {
                    if let Some((line, _)) = current_value(doc, st) {
                        set_cell(doc, st, line, st.sel.1, "", now)?;
                    }
                }
                Key::Z if ctrl && modifiers.shift => {
                    doc.redo()?;
                }
                Key::Z if ctrl => {
                    doc.undo()?;
                }
                Key::Y if ctrl => {
                    doc.redo()?;
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_syntax() {
        let t = |f: &str, v: &str| Pred::parse(f).unwrap().test(v);
        assert!(t("gmail", "Jane@GMAIL.com"));
        assert!(!t("!gmail", "jane@gmail.com"));
        assert!(t("=pro", " PRO "));
        assert!(!t("=pro", "proton"));
        assert!(t("=", "  "));
        assert!(t("!", "x"));
        assert!(t(">10", "10.5") && !t(">10", "10") && t(">=10", "10"));
        assert!(t("<0", "-3") && !t("<0", "abc"));
        assert!(t("/^\\d{3}-/", "555-1234"));
        assert!(Pred::parse("/(/").is_err());
    }

    #[test]
    fn sort_keys_numbers_before_text() {
        let mut v = vec!["10", "9", "apple", "", "Banana", "-1"];
        v.sort_by(|a, b| match (sort_key(a), sort_key(b)) {
            (Some(x), Some(y)) => cmp_keys(&x, &y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
        assert_eq!(v, vec!["-1", "9", "10", "apple", "Banana", ""]);
    }
}
