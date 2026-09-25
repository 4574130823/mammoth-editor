//! The text view: a fully virtualised, custom-painted editor widget.
//!
//! Nothing here ever touches more than the lines on screen, and scrolling is tracked
//! as (top line, pixel offset) instead of a float pixel position, so it stays exact
//! even for files with billions of lines.

use egui::{
    Align2, Color32, CursorIcon, Event, EventFilter, FontId, Id, Key, Painter, Pos2, Rect, Sense,
    Stroke, Ui, pos2, vec2,
};

use crate::document::{Document, EditKind, Pos, char_to_byte};
use crate::modules::{Module, Registry, Span};
use crate::search::Matcher;
use crate::theme;

const PAD: f32 = 10.0;
const SCROLLBAR: f32 = 12.0;
const COPY_LIMIT: usize = 256 << 20;
/// Line-based commands (duplicate, move, indent) refuse larger selections.
const LINE_CMD_LIMIT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Reveal {
    #[default]
    None,
    Nearest,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum Drag {
    #[default]
    None,
    Select,
    VBar(f32),
    HBar(f32),
}

#[derive(Default)]
pub struct EditorView {
    pub cursor: Pos,
    pub anchor: Pos,
    pub top: usize,
    pub y_off: f32,
    pub scroll_x: f32,
    want_vcol: Option<usize>,
    pub reveal: Reveal,
    drag: Drag,
    clicks: u32,
    last_click: (f64, Pos),
    blink_epoch: f64,
    /// Fully visible rows (updated every frame).
    pub rows: usize,
    pub request_focus: bool,
}

impl EditorView {
    pub fn focused() -> Self {
        Self {
            request_focus: true,
            ..Self::default()
        }
    }

    pub fn selection(&self) -> (Pos, Pos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    pub fn select(&mut self, anchor: Pos, cursor: Pos, reveal: Reveal) {
        self.anchor = anchor;
        self.cursor = cursor;
        self.want_vcol = None;
        self.reveal = reveal;
    }

    fn set_cursor(&mut self, p: Pos, extend: bool) {
        self.cursor = p;
        if !extend {
            self.anchor = p;
        }
        self.reveal = Reveal::Nearest;
    }

    fn sel_state(&self) -> (Pos, Pos) {
        (self.anchor, self.cursor)
    }
}

/// Everything the editor needs from the rest of the app for one frame.
pub struct Env<'a> {
    pub registry: &'a Registry,
    pub syntax: Option<&'a dyn Module>,
    pub find: Option<&'a Matcher>,
    pub font_size: f32,
    /// Row height as a multiple of the font's line height.
    pub line_spacing: f32,
    pub tab_width: usize,
    pub highlight_line: bool,
    /// Line numbers to show in the gutter instead of 1, 2, 3… (filtered views).
    pub line_numbers: Option<&'a [usize]>,
    /// Width reserved left of the scrollbar for the app's heatmap strip (0 = none).
    pub side_strip: f32,
    pub now: f64,
    /// Ad-hoc highlighted terms (from "Highlight word/selection"), each its own colour.
    pub highlights: &'a [(String, Color32)],
    /// Bookmarked line numbers, marked in the gutter.
    pub bookmarks: &'a std::collections::BTreeSet<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Undo,
    Redo,
    SelectAll,
    Copy,
    Cut,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    Indent,
    Outdent,
}

// ----------------------------------------------------------------------------
// Text helpers

fn char_width(ch: char, col: usize, tab: usize) -> usize {
    if ch == '\t' { tab - col % tab } else { 1 }
}

pub fn vcol_of(s: &str, col: usize, tab: usize) -> usize {
    let mut v = 0;
    for ch in s.chars().take(col) {
        v += char_width(ch, v, tab);
    }
    v
}

/// Char index closest to visual column `x` (fractional).
fn col_at(s: &str, x: f32, tab: usize) -> usize {
    let mut v = 0usize;
    for (i, ch) in s.chars().enumerate() {
        let w = char_width(ch, v, tab);
        if x < v as f32 + w as f32 / 2.0 {
            return i;
        }
        v += w;
    }
    s.chars().count()
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn word_left(doc: &Document, p: Pos) -> Pos {
    if p.col == 0 {
        return if p.line > 0 {
            Pos::new(p.line - 1, doc.line_chars(p.line - 1))
        } else {
            p
        };
    }
    let chars: Vec<char> = doc.line(p.line).chars().collect();
    let mut i = p.col.min(chars.len());
    while i > 0 && class(chars[i - 1]) == Class::Space {
        i -= 1;
    }
    if i > 0 {
        let k = class(chars[i - 1]);
        while i > 0 && class(chars[i - 1]) == k {
            i -= 1;
        }
    }
    Pos::new(p.line, i)
}

fn word_right(doc: &Document, p: Pos) -> Pos {
    let chars: Vec<char> = doc.line(p.line).chars().collect();
    if p.col >= chars.len() {
        return if p.line + 1 < doc.line_count() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        };
    }
    let mut i = p.col;
    let k = class(chars[i]);
    while i < chars.len() && class(chars[i]) == k {
        i += 1;
    }
    while i < chars.len() && class(chars[i]) == Class::Space {
        i += 1;
    }
    Pos::new(p.line, i)
}

pub(crate) fn word_at(doc: &Document, p: Pos) -> (Pos, Pos) {
    let chars: Vec<char> = doc.line(p.line).chars().collect();
    if chars.is_empty() {
        return (p, p);
    }
    let i = p.col.min(chars.len() - 1);
    let k = class(chars[i]);
    let mut s = i;
    while s > 0 && class(chars[s - 1]) == k {
        s -= 1;
    }
    let mut e = i;
    while e < chars.len() && class(chars[e]) == k {
        e += 1;
    }
    (Pos::new(p.line, s), Pos::new(p.line, e))
}

fn leading_ws(s: &str) -> &str {
    &s[..s.len() - s.trim_start_matches([' ', '\t']).len()]
}

/// ASCII case-insensitive substring search, for ad-hoc highlight terms.
fn find_ci(hay: &[u8], needle: &[u8], out: &mut Vec<(usize, usize)>) {
    if needle.is_empty() || needle.len() > hay.len() {
        return;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
}

// ----------------------------------------------------------------------------
// Editing operations

fn edit(
    doc: &mut Document,
    view: &mut EditorView,
    text: &str,
    kind: EditKind,
    now: f64,
) -> Result<(), String> {
    let (s, e) = view.selection();
    let p = doc.replace(s, e, text, kind, view.sel_state(), now)?;
    view.set_cursor(p, false);
    view.want_vcol = None;
    Ok(())
}

fn delete_range(
    doc: &mut Document,
    view: &mut EditorView,
    a: Pos,
    b: Pos,
    kind: EditKind,
    now: f64,
) -> Result<(), String> {
    let p = doc.replace(a, b, "", kind, view.sel_state(), now)?;
    view.set_cursor(p, false);
    view.want_vcol = None;
    Ok(())
}

fn selected_lines(view: &EditorView) -> (usize, usize) {
    let (s, e) = view.selection();
    let last = if e.col == 0 && e.line > s.line {
        e.line - 1
    } else {
        e.line
    };
    (s.line, last)
}

fn limited_lines(view: &EditorView) -> Result<(usize, usize), String> {
    let (first, last) = selected_lines(view);
    if last - first >= LINE_CMD_LIMIT {
        return Err(format!(
            "That works on up to {} lines at a time.",
            fmt_int(LINE_CMD_LIMIT)
        ));
    }
    Ok((first, last))
}

pub fn run_command(
    ctx: &egui::Context,
    doc: &mut Document,
    view: &mut EditorView,
    cmd: Command,
    now: f64,
) -> Result<(), String> {
    match cmd {
        Command::Undo => {
            if let Some((a, c)) = doc.undo()? {
                view.select(doc.clamp(a), doc.clamp(c), Reveal::Nearest);
            }
        }
        Command::Redo => {
            if let Some((a, c)) = doc.redo()? {
                view.select(doc.clamp(a), doc.clamp(c), Reveal::Nearest);
            }
        }
        Command::SelectAll => {
            view.anchor = Pos::default();
            view.cursor = doc.end_pos();
        }
        Command::Copy | Command::Cut => {
            let (s, e) = view.selection();
            let (a, b) = if s == e {
                // No selection: whole line.
                let l = view.cursor.line;
                if l + 1 < doc.line_count() {
                    (Pos::new(l, 0), Pos::new(l + 1, 0))
                } else {
                    (Pos::new(l, 0), doc.end_pos())
                }
            } else {
                (s, e)
            };
            let too_big = || "Selection is too large for the clipboard (limit 256 MB).".to_string();
            if b.line - a.line > 4_000_000 {
                return Err(too_big());
            }
            let Some(mut text) = doc.text_range(a, b, COPY_LIMIT) else {
                return Err(too_big());
            };
            if s == e && !text.ends_with('\n') {
                text.push_str(doc.eol.as_str());
            }
            ctx.copy_text(text);
            if matches!(cmd, Command::Cut) {
                if s == e {
                    let l = view.cursor.line;
                    let after = (Pos::new(l, 0), Pos::new(l, 0));
                    doc.replace_lines(l, l + 1, vec![], view.sel_state(), after, now)?;
                    let p = doc.clamp(Pos::new(l, 0));
                    view.select(p, p, Reveal::Nearest);
                } else {
                    delete_range(doc, view, a, b, EditKind::Other, now)?;
                }
            }
        }
        Command::DuplicateLine => {
            let (first, last) = limited_lines(view)?;
            let lines: Vec<String> = (first..=last).map(|i| doc.line(i).into_owned()).collect();
            let n = lines.len();
            let mut both = lines.clone();
            both.extend(lines);
            let (a, c) = (view.anchor, view.cursor);
            let shifted = (Pos::new(a.line + n, a.col), Pos::new(c.line + n, c.col));
            doc.replace_lines(first, last + 1, both, view.sel_state(), shifted, now)?;
            view.select(shifted.0, shifted.1, Reveal::Nearest);
        }
        Command::DeleteLine => {
            let (first, last) = selected_lines(view);
            let p = Pos::new(first, 0);
            doc.replace_lines(first, last + 1, vec![], view.sel_state(), (p, p), now)?;
            let p = doc.clamp(p);
            view.select(p, p, Reveal::Nearest);
        }
        Command::MoveLineUp | Command::MoveLineDown => {
            let (first, last) = limited_lines(view)?;
            let up = matches!(cmd, Command::MoveLineUp);
            if (up && first == 0) || (!up && last + 1 >= doc.line_count()) {
                return Ok(());
            }
            let (a0, b0) = if up {
                (first - 1, last + 1)
            } else {
                (first, last + 2)
            };
            let mut lines: Vec<String> = (a0..b0).map(|i| doc.line(i).into_owned()).collect();
            if up {
                lines.rotate_left(1);
            } else {
                lines.rotate_right(1);
            }
            let d: isize = if up { -1 } else { 1 };
            let mv = |p: Pos| Pos::new((p.line as isize + d) as usize, p.col);
            let after = (mv(view.anchor), mv(view.cursor));
            doc.replace_lines(a0, b0, lines, view.sel_state(), after, now)?;
            view.select(after.0, after.1, Reveal::Nearest);
        }
        Command::Indent | Command::Outdent => {
            let (first, last) = limited_lines(view)?;
            let outdent = matches!(cmd, Command::Outdent);
            let lines: Vec<String> = (first..=last)
                .map(|i| {
                    let l = doc.line(i);
                    if outdent {
                        if let Some(rest) = l.strip_prefix('\t') {
                            rest.to_string()
                        } else {
                            let n = l.len() - l.trim_start_matches(' ').len();
                            l[n.min(4)..].to_string()
                        }
                    } else if l.is_empty() {
                        String::new()
                    } else {
                        format!("\t{l}")
                    }
                })
                .collect();
            let end_len = lines.last().map_or(0, |l| l.chars().count());
            let after = (Pos::new(first, 0), Pos::new(last, end_len));
            doc.replace_lines(first, last + 1, lines, view.sel_state(), after, now)?;
            view.select(after.0, after.1, Reveal::Nearest);
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// The widget

struct Metrics {
    cw: f32,
    row_h: f32,
    text_y: f32,
    font: FontId,
}

/// Show the editor. Returns a status message (e.g. why an edit was refused).
/// What happened in the editor this frame.
#[derive(Default)]
pub struct Output {
    /// Why an edit was refused, if it was.
    pub status: Option<String>,
    /// A line number was double-clicked in the gutter.
    pub activate_line: Option<usize>,
    /// Where the heatmap strip goes, when `Env::side_strip` asked for one.
    pub strip: Option<Rect>,
    /// First and last visible line.
    pub visible: (usize, usize),
}

pub fn show(ui: &mut Ui, id: Id, doc: &mut Document, view: &mut EditorView, env: &Env) -> Output {
    let mut status: Option<String> = None;
    let mut activate_line = None;
    let font = FontId::monospace(env.font_size);
    let (cw, glyph_h) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = (glyph_h * env.line_spacing.clamp(1.0, 3.0)).round();
    let m = Metrics {
        cw,
        row_h,
        text_y: ((row_h - glyph_h) / 2.0).round(),
        font,
    };
    let tab = env.tab_width.max(1);

    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());

    let max_number = env
        .line_numbers
        .and_then(|m| m.last())
        .map_or(doc.line_count(), |n| n + 1);
    let digits = digits(max_number.max(1)).max(4);
    let gutter_w = (digits as f32 + 1.5) * cw + 16.0;
    let strip_w = env.side_strip.max(0.0);
    let text_rect = Rect::from_min_max(
        pos2(outer.left() + gutter_w, outer.top()),
        pos2(
            outer.right() - SCROLLBAR - strip_w,
            outer.bottom() - SCROLLBAR,
        ),
    );
    let gutter_rect = Rect::from_min_max(outer.min, pos2(text_rect.left(), text_rect.bottom()));
    let vbar = Rect::from_min_max(
        pos2(outer.right() - SCROLLBAR, outer.top()),
        pos2(outer.right(), text_rect.bottom()),
    );
    let strip = (strip_w > 0.0).then(|| {
        Rect::from_min_max(
            pos2(text_rect.right(), outer.top()),
            pos2(vbar.left(), text_rect.bottom()),
        )
    });
    let hbar = Rect::from_min_max(
        pos2(text_rect.left(), text_rect.bottom()),
        pos2(text_rect.right(), outer.bottom()),
    );
    view.rows = ((text_rect.height() / row_h).floor() as usize).max(1);

    // ---------------- focus ----------------
    let resp = ui.interact(text_rect.union(gutter_rect), id, Sense::click_and_drag());
    if resp.clicked() || resp.drag_started() || view.request_focus {
        ui.memory_mut(|mem| mem.request_focus(id));
        view.request_focus = false;
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
    }
    if resp.hovered() && ui.rect_contains_pointer(text_rect) {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }

    // ---------------- keyboard ----------------
    if focused {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            if let Err(e) = handle_event(ui.ctx(), doc, view, &ev, env.now, tab) {
                status = Some(e);
            }
            if !matches!(ev, Event::PointerMoved(_) | Event::MouseMoved(_)) {
                view.blink_epoch = env.now;
            }
        }
    }

    let line_count = doc.line_count();
    let max_top = line_count.saturating_sub(view.rows.saturating_sub(1).max(1));
    view.cursor = doc.clamp(view.cursor);
    view.anchor = doc.clamp(view.anchor);

    // ---------------- scrolling ----------------
    if ui.rect_contains_pointer(outer) {
        let d = ui.input(|i| i.smooth_scroll_delta());
        if d.y != 0.0 {
            scroll_by(view, -d.y, row_h, max_top);
        }
        if d.x != 0.0 {
            view.scroll_x = (view.scroll_x - d.x).max(0.0);
        }
    }

    // ---------------- mouse selection ----------------
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let (primary_pressed, primary_down, shift) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.modifiers.shift,
        )
    });
    if let Some(pp) = pointer {
        if primary_pressed && resp.hovered() {
            let p = pos_at(doc, view, &m, text_rect, pp, tab);
            let now = env.now;
            if now - view.last_click.0 < 0.4 && view.last_click.1.line == p.line {
                view.clicks += 1;
            } else {
                view.clicks = 1;
            }
            view.last_click = (now, p);
            view.drag = Drag::Select;
            view.want_vcol = None;
            let in_gutter = pp.x < text_rect.left();
            if in_gutter && view.clicks == 2 {
                activate_line = Some(p.line);
            }
            if in_gutter || view.clicks >= 3 {
                let next = if p.line + 1 < line_count {
                    Pos::new(p.line + 1, 0)
                } else {
                    Pos::new(p.line, doc.line_chars(p.line))
                };
                if shift && in_gutter {
                    view.cursor = next;
                } else {
                    view.select(Pos::new(p.line, 0), next, Reveal::None);
                }
            } else if view.clicks == 2 {
                let (a, b) = word_at(doc, p);
                view.select(a, b, Reveal::None);
            } else if shift {
                view.cursor = p;
            } else {
                view.select(p, p, Reveal::None);
            }
        } else if view.drag == Drag::Select && primary_down && view.clicks == 1 {
            // Auto-scroll when dragging past the edges.
            if pp.y < text_rect.top() {
                scroll_by(
                    view,
                    -(text_rect.top() - pp.y).min(row_h * 3.0),
                    row_h,
                    max_top,
                );
                ui.ctx().request_repaint();
            } else if pp.y > text_rect.bottom() {
                scroll_by(
                    view,
                    (pp.y - text_rect.bottom()).min(row_h * 3.0),
                    row_h,
                    max_top,
                );
                ui.ctx().request_repaint();
            }
            let p = pos_at(doc, view, &m, text_rect, pp, tab);
            if p != view.cursor {
                view.cursor = p;
                view.reveal = Reveal::None;
                if pp.x > text_rect.right() - cw || pp.x < text_rect.left() + cw {
                    view.reveal = Reveal::Nearest;
                }
            }
        }
    }
    if !primary_down && view.drag == Drag::Select {
        view.drag = Drag::None;
    }

    // ---------------- scrollbars ----------------
    let vresp = ui.interact(vbar, id.with("vbar"), Sense::click_and_drag());
    let track = vbar.shrink2(vec2(3.0, 3.0));
    let total_rows = (max_top + view.rows).max(1) as f64;
    let thumb_h = ((track.height() as f64 * view.rows as f64 / total_rows) as f32)
        .clamp(28.0_f32.min(track.height()), track.height());
    let frac = if max_top == 0 {
        0.0
    } else {
        ((view.top as f64 + (view.y_off / row_h) as f64) / max_top as f64).min(1.0)
    };
    let thumb_y = track.top() + (frac as f32) * (track.height() - thumb_h);
    let thumb = Rect::from_min_size(pos2(track.left(), thumb_y), vec2(track.width(), thumb_h));
    if let Some(pp) = pointer {
        if primary_pressed && vresp.hovered() {
            let grab = if thumb.contains(pp) {
                pp.y - thumb.top()
            } else {
                thumb_h / 2.0
            };
            view.drag = Drag::VBar(grab);
        }
        if let Drag::VBar(grab) = view.drag {
            let span = (track.height() - thumb_h).max(1.0);
            let f = ((pp.y - grab - track.top()) / span).clamp(0.0, 1.0) as f64;
            view.top = ((f * max_top as f64).round() as usize).min(max_top);
            view.y_off = 0.0;
        }
    }
    if !primary_down && matches!(view.drag, Drag::VBar(_) | Drag::HBar(_)) {
        view.drag = Drag::None;
    }

    // ---------------- reveal cursor ----------------
    let text_w = text_rect.width();
    match view.reveal {
        Reveal::None => {}
        Reveal::Nearest | Reveal::Center => {
            let c = view.cursor;
            if view.reveal == Reveal::Center {
                if c.line < view.top || c.line + 1 >= view.top + view.rows {
                    view.top = c.line.saturating_sub(view.rows / 3).min(max_top);
                    view.y_off = 0.0;
                }
            } else if c.line < view.top || (c.line == view.top && view.y_off > 0.0) {
                view.top = c.line;
                view.y_off = 0.0;
            } else if c.line >= view.top + view.rows {
                view.top = c.line + 1 - view.rows;
                view.y_off = 0.0;
            }
            let x = vcol_of(&doc.line(c.line), c.col, tab) as f32 * cw;
            let margin = (cw * 6.0).min(text_w / 3.0);
            if x < view.scroll_x + margin {
                view.scroll_x = (x - margin).max(0.0);
            } else if x > view.scroll_x + text_w - PAD - margin {
                view.scroll_x = x - text_w + PAD + margin;
            }
            view.reveal = Reveal::None;
        }
    }
    view.top = view.top.min(line_count.saturating_sub(1));

    // ---------------- paint ----------------
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, 0.0, theme::EDITOR_BG);
    painter.rect_filled(gutter_rect, 0.0, theme::GUTTER_BG);

    let text_p = painter.with_clip_rect(text_rect);
    let gutter_p = painter.with_clip_rect(gutter_rect);
    let (sel_s, sel_e) = view.selection();
    let has_sel = sel_s != sel_e;
    let x0 = text_rect.left() + PAD - view.scroll_x;
    let vc0 = ((view.scroll_x - PAD) / cw).floor().max(0.0) as usize;
    let vc1 = vc0 + (text_w / cw).ceil() as usize + 2;
    let blink_on = ((env.now - view.blink_epoch) % 1.1) < 0.65;
    let mut caret_rect: Option<Rect> = None;
    let mut content_cols = 0usize;

    let mut fg: Vec<Span> = Vec::new();
    let mut marks: Vec<(usize, usize, Color32)> = Vec::new();
    let mut finds: Vec<(usize, usize)> = Vec::new();
    let mut det: Vec<(usize, usize)> = Vec::new();
    let mut hl: Vec<(usize, usize)> = Vec::new();

    let last = (view.top + view.rows + 2).min(line_count);
    for (row, li) in (view.top..last).enumerate() {
        let y = text_rect.top() + row as f32 * row_h - view.y_off;
        let line = doc.line(li);
        let is_cur = li == view.cursor.line;

        if is_cur && env.highlight_line && !has_sel {
            let r = Rect::from_min_size(
                pos2(outer.left(), y),
                vec2(text_rect.right() - outer.left(), row_h),
            );
            painter
                .with_clip_rect(text_rect.union(gutter_rect))
                .rect_filled(r, 0.0, theme::CURRENT_LINE);
        }

        // Decorations. Very long lines are only analysed around the visible window.
        let (win_s, win_e) = if line.len() > 8192 {
            let a = byte_at_vcol(&line, vc0, tab).saturating_sub(1024);
            let b = (byte_at_vcol(&line, vc1, tab) + 1024).min(line.len());
            (floor_boundary(&line, a), floor_boundary(&line, b))
        } else {
            (0, line.len())
        };
        let window = &line[win_s..win_e];

        fg.clear();
        if let Some(sx) = env.syntax {
            sx.highlight(&line, &mut fg);
        }
        marks.clear();
        for (_, entry) in env.registry.detectors() {
            if entry.enabled {
                det.clear();
                entry.detect(window, &mut det);
                let c = entry.module.color();
                marks.extend(det.iter().map(|&(s, e)| (s + win_s, e + win_s, c)));
            }
        }
        for (term, color) in env.highlights {
            if term.is_empty() {
                continue;
            }
            hl.clear();
            find_ci(window.as_bytes(), term.as_bytes(), &mut hl);
            marks.extend(hl.iter().map(|&(s, e)| (s + win_s, e + win_s, *color)));
        }
        finds.clear();
        if let Some(f) = env.find {
            f.line_matches(window, win_s, &mut finds);
        }
        let sel = if has_sel && li >= sel_s.line && li <= sel_e.line {
            let s = if li == sel_s.line {
                char_to_byte(&line, sel_s.col)
            } else {
                0
            };
            let e = if li == sel_e.line {
                char_to_byte(&line, sel_e.col)
            } else {
                line.len()
            };
            Some((s, e, li < sel_e.line))
        } else {
            None
        };

        let total_cols = paint_line(
            &text_p,
            &line,
            pos2(x0, y),
            &m,
            tab,
            (vc0, vc1),
            &LineDeco {
                fg: &fg,
                marks: &marks,
                finds: &finds,
                sel,
            },
        );
        content_cols = content_cols.max(total_cols);

        // Gutter.
        let num_color = if is_cur {
            theme::LINENO_ACTIVE
        } else {
            theme::LINENO
        };
        gutter_p.text(
            pos2(gutter_rect.right() - 12.0, y + m.text_y),
            Align2::RIGHT_TOP,
            fmt_int(
                env.line_numbers
                    .and_then(|m| m.get(li))
                    .map_or(li + 1, |n| n + 1),
            ),
            m.font.clone(),
            num_color,
        );
        if doc.is_soft(li) {
            let r = Rect::from_min_size(
                pos2(gutter_rect.right() - 5.0, y + 3.0),
                vec2(2.0, row_h - 6.0),
            );
            gutter_p.rect_filled(r, 1.0, theme::WARN);
        }
        if env.bookmarks.contains(&li) {
            gutter_p.circle_filled(pos2(gutter_rect.left() + 7.0, y + row_h / 2.0), 3.0, theme::BOOKMARK);
        }

        // Cursor.
        if is_cur && focused {
            let cx = x0 + vcol_of(&line, view.cursor.col, tab) as f32 * cw;
            let r = Rect::from_min_size(pos2(cx - 1.0, y + 2.0), vec2(2.0, row_h - 4.0));
            caret_rect = Some(r);
            if blink_on && cx >= text_rect.left() - 1.0 {
                text_p.rect_filled(r, 1.0, theme::CURSOR);
            }
        }
    }

    // "More lines not loaded yet" hint.
    if last == line_count && !doc.is_fully_loaded() {
        let y = text_rect.top() + (last - view.top) as f32 * row_h - view.y_off;
        if y < text_rect.bottom() {
            let loading = doc.source.as_ref().is_some_and(|s| s.is_loading());
            let msg = if loading {
                "… loading more lines …"
            } else {
                "— end of preview · use “Load entire file” (Ctrl+L) to see the rest —"
            };
            text_p.text(
                pos2(text_rect.left() + PAD, y + m.text_y),
                Align2::LEFT_TOP,
                msg,
                FontId::proportional(env.font_size * 0.95),
                theme::TEXT_DIM,
            );
        }
    }

    painter.vline(
        gutter_rect.right() - 0.5,
        gutter_rect.y_range(),
        Stroke::new(1.0, theme::BORDER),
    );

    // Vertical scrollbar.
    painter.rect_filled(vbar, 0.0, theme::SCROLL_TRACK);
    let hot = vresp.hovered() || matches!(view.drag, Drag::VBar(_));
    if max_top > 0 {
        painter.rect_filled(
            thumb,
            4.0,
            if hot {
                theme::SCROLL_THUMB_HOT
            } else {
                theme::SCROLL_THUMB
            },
        );
    }
    // Cursor marker on the scrollbar.
    if line_count > 1 {
        let cy = track.top()
            + track.height() * (view.cursor.line as f64 / (line_count - 1) as f64) as f32;
        painter.hline(
            track.x_range(),
            cy,
            Stroke::new(2.0, theme::CURSOR.gamma_multiply(0.7)),
        );
    }

    // Horizontal scrollbar.
    painter.rect_filled(hbar, 0.0, theme::SCROLL_TRACK);
    let content_w = content_cols as f32 * cw + PAD * 2.0 + cw * 4.0;
    let max_x = (content_w - text_w).max(0.0).max(view.scroll_x);
    let hresp = ui.interact(hbar, id.with("hbar"), Sense::click_and_drag());
    if max_x > 0.0 {
        let htrack = hbar.shrink2(vec2(3.0, 3.0));
        let tw = (htrack.width() * text_w / (max_x + text_w))
            .clamp(28.0_f32.min(htrack.width()), htrack.width());
        let tx = htrack.left() + (view.scroll_x / max_x) * (htrack.width() - tw);
        let hthumb = Rect::from_min_size(pos2(tx, htrack.top()), vec2(tw, htrack.height()));
        if let Some(pp) = pointer {
            if primary_pressed && hresp.hovered() {
                let grab = if hthumb.contains(pp) {
                    pp.x - hthumb.left()
                } else {
                    tw / 2.0
                };
                view.drag = Drag::HBar(grab);
            }
            if let Drag::HBar(grab) = view.drag {
                let span = (htrack.width() - tw).max(1.0);
                let f = ((pp.x - grab - htrack.left()) / span).clamp(0.0, 1.0);
                view.scroll_x = f * max_x;
            }
        }
        let hot = hresp.hovered() || matches!(view.drag, Drag::HBar(_));
        painter.rect_filled(
            hthumb,
            4.0,
            if hot {
                theme::SCROLL_THUMB_HOT
            } else {
                theme::SCROLL_THUMB
            },
        );
    }
    painter.rect_filled(
        Rect::from_min_max(pos2(vbar.left(), hbar.top()), outer.max),
        0.0,
        theme::SCROLL_TRACK,
    );

    if focused && ui.memory(|mem| mem.owns_ime_events(id)) {
        // Lets the OS place IME candidate windows (CJK input) at the caret.
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        let caret = caret_rect.unwrap_or(text_rect);
        ui.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: to_global * text_rect,
                cursor_rect: to_global * caret,
                should_interrupt_composition: false,
            })
        });
    }

    if focused {
        let next_blink = 0.55 - ((env.now - view.blink_epoch) % 0.55);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(next_blink.max(0.05)));
    }
    let last_visible = (view.top + view.rows).min(doc.line_count().saturating_sub(1));
    Output {
        status,
        activate_line,
        strip,
        visible: (view.top, last_visible),
    }
}

fn scroll_by(view: &mut EditorView, dy: f32, row_h: f32, max_top: usize) {
    let total = view.y_off + dy;
    let lines = (total / row_h).floor();
    let mut top = view.top as i64 + lines as i64;
    let mut off = total - lines * row_h;
    if top < 0 {
        top = 0;
        off = 0.0;
    }
    if top as usize >= max_top {
        top = max_top as i64;
        off = 0.0;
    }
    view.top = top as usize;
    view.y_off = off;
}

fn pos_at(
    doc: &Document,
    view: &EditorView,
    m: &Metrics,
    text_rect: Rect,
    p: Pos2,
    tab: usize,
) -> Pos {
    let n = doc.line_count();
    if n == 0 {
        return Pos::default();
    }
    let row = ((p.y - text_rect.top() + view.y_off) / m.row_h).floor();
    let line = if row < 0.0 {
        view.top.saturating_sub((-row) as usize)
    } else {
        view.top + row as usize
    };
    let line = line.min(n - 1);
    let x = ((p.x - text_rect.left() - PAD + view.scroll_x) / m.cw).max(0.0);
    Pos::new(line, col_at(&doc.line(line), x, tab))
}

fn byte_at_vcol(s: &str, vcol: usize, tab: usize) -> usize {
    let mut v = 0;
    for (b, ch) in s.char_indices() {
        if v >= vcol {
            return b;
        }
        v += char_width(ch, v, tab);
    }
    s.len()
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

struct LineDeco<'a> {
    fg: &'a [Span],
    marks: &'a [(usize, usize, Color32)],
    finds: &'a [(usize, usize)],
    /// Selected byte range, and whether the selection continues past the line end.
    sel: Option<(usize, usize, bool)>,
}

#[derive(PartialEq, Clone, Copy)]
struct Attr {
    fg: Color32,
    bg: Option<Color32>,
    underline: Option<Color32>,
}

/// Paint the visible part of one line; returns the line's width in columns.
fn paint_line(
    p: &Painter,
    line: &str,
    origin: Pos2,
    m: &Metrics,
    tab: usize,
    (vc0, vc1): (usize, usize),
    d: &LineDeco,
) -> usize {
    let mut col = 0usize;
    let mut fi = 0usize;
    let mut run = String::new();
    let mut run_col = 0usize;
    let mut run_cols = 0usize;
    let mut run_attr: Option<Attr> = None;
    let mut run_ascii = true;

    let flush = |text: &mut String, start_col: usize, cols: usize, attr: Option<Attr>| {
        let Some(a) = attr else { return };
        let x = origin.x + start_col as f32 * m.cw;
        let w = cols as f32 * m.cw;
        if let Some(bg) = a.bg {
            p.rect_filled(
                Rect::from_min_size(pos2(x, origin.y), vec2(w, m.row_h)),
                0.0,
                bg,
            );
        }
        if let Some(u) = a.underline {
            let y = origin.y + m.row_h - 3.0;
            p.hline(x..=x + w, y, Stroke::new(1.5, u));
        }
        if !text.trim().is_empty() {
            p.text(
                pos2(x, origin.y + m.text_y),
                Align2::LEFT_TOP,
                text.as_str(),
                m.font.clone(),
                a.fg,
            );
        }
        text.clear();
    };

    let mut broke = None;
    for (b, ch) in line.char_indices() {
        let w = char_width(ch, col, tab);
        if col + w <= vc0 {
            col += w;
            continue;
        }
        if col >= vc1 {
            broke = Some(b);
            break;
        }
        while fi < d.fg.len() && d.fg[fi].end <= b {
            fi += 1;
        }
        let fg = if fi < d.fg.len() && d.fg[fi].start <= b {
            d.fg[fi].color
        } else {
            theme::TEXT
        };
        let in_sel = d.sel.is_some_and(|(s, e, _)| s <= b && b < e);
        let in_find = d.finds.iter().any(|&(s, e)| s <= b && b < e);
        let mark = d
            .marks
            .iter()
            .find(|&&(s, e, _)| s <= b && b < e)
            .map(|m| m.2);
        let bg = if in_sel {
            Some(theme::SELECTION)
        } else if in_find {
            Some(theme::FIND_HIT)
        } else {
            mark.map(|c| c.gamma_multiply(0.18))
        };
        let attr = Attr {
            fg: mark.filter(|_| !in_sel).unwrap_or(fg),
            bg,
            underline: mark,
        };
        let ascii = ch.is_ascii();
        if run_attr != Some(attr) || !ascii || !run_ascii {
            flush(&mut run, run_col, run_cols, run_attr);
            run_col = col;
            run_cols = 0;
            run_attr = Some(attr);
        }
        run_ascii = ascii;
        if ch == '\t' {
            run.extend(std::iter::repeat_n(' ', w));
        } else if ch.is_control() {
            run.push('·');
        } else {
            run.push(ch);
        }
        run_cols += w;
        col += w;
    }
    flush(&mut run, run_col, run_cols, run_attr);

    let total = match broke {
        Some(b) => col + line[b..].chars().count(),
        None => col,
    };
    // Show the selected line break as a small block.
    if let Some((_, _, true)) = d.sel
        && broke.is_none()
        && col + 1 > vc0
    {
        let x = origin.x + col as f32 * m.cw;
        p.rect_filled(
            Rect::from_min_size(pos2(x, origin.y), vec2(m.cw * 0.6, m.row_h)),
            0.0,
            theme::SELECTION,
        );
    }
    total
}

fn handle_event(
    ctx: &egui::Context,
    doc: &mut Document,
    view: &mut EditorView,
    ev: &Event,
    now: f64,
    tab: usize,
) -> Result<(), String> {
    match ev {
        Event::Text(t) => {
            if t.is_empty() {
                return Ok(());
            }
            let kind = if t.chars().count() == 1 && !view.has_selection() {
                EditKind::Typing
            } else {
                EditKind::Other
            };
            edit(doc, view, t, kind, now)
        }
        Event::Paste(t) => edit(doc, view, t, EditKind::Other, now),
        Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => {
            edit(doc, view, t, EditKind::Other, now)
        }
        Event::Copy => run_command(ctx, doc, view, Command::Copy, now),
        Event::Cut => run_command(ctx, doc, view, Command::Cut, now),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let shift = modifiers.shift;
            let ctrl = modifiers.command || modifiers.ctrl;
            let alt = modifiers.alt;
            let n = doc.line_count();
            if n == 0 {
                return Ok(());
            }
            let c = view.cursor;
            match key {
                Key::ArrowLeft => {
                    if view.has_selection() && !shift {
                        let (s, _) = view.selection();
                        view.set_cursor(s, false);
                    } else {
                        let p = if ctrl {
                            word_left(doc, c)
                        } else if c.col > 0 {
                            Pos::new(c.line, c.col - 1)
                        } else if c.line > 0 {
                            Pos::new(c.line - 1, doc.line_chars(c.line - 1))
                        } else {
                            c
                        };
                        view.set_cursor(p, shift);
                    }
                    view.want_vcol = None;
                }
                Key::ArrowRight => {
                    if view.has_selection() && !shift {
                        let (_, e) = view.selection();
                        view.set_cursor(e, false);
                    } else {
                        let len = doc.line_chars(c.line);
                        let p = if ctrl {
                            word_right(doc, c)
                        } else if c.col < len {
                            Pos::new(c.line, c.col + 1)
                        } else if c.line + 1 < n {
                            Pos::new(c.line + 1, 0)
                        } else {
                            c
                        };
                        view.set_cursor(p, shift);
                    }
                    view.want_vcol = None;
                }
                Key::ArrowUp | Key::ArrowDown if alt => {
                    let cmd = if *key == Key::ArrowUp {
                        Command::MoveLineUp
                    } else {
                        Command::MoveLineDown
                    };
                    run_command(ctx, doc, view, cmd, now)?;
                }
                Key::ArrowUp | Key::ArrowDown if ctrl => {
                    // Scroll without moving the cursor.
                    if *key == Key::ArrowUp {
                        view.top = view.top.saturating_sub(1);
                    } else {
                        view.top = (view.top + 1).min(n.saturating_sub(1));
                    }
                    view.y_off = 0.0;
                }
                Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown => {
                    let page = view.rows.saturating_sub(1).max(1) as isize;
                    let delta = match key {
                        Key::ArrowUp => -1,
                        Key::ArrowDown => 1,
                        Key::PageUp => -page,
                        _ => page,
                    };
                    let vcol = view
                        .want_vcol
                        .unwrap_or_else(|| vcol_of(&doc.line(c.line), c.col, tab));
                    let target = c.line as isize + delta;
                    let p = if target < 0 {
                        Pos::new(0, 0)
                    } else if target as usize >= n {
                        Pos::new(n - 1, doc.line_chars(n - 1))
                    } else {
                        let l = target as usize;
                        Pos::new(l, col_at(&doc.line(l), vcol as f32, tab))
                    };
                    if matches!(key, Key::PageUp | Key::PageDown) {
                        view.top = (view.top as isize + delta)
                            .clamp(0, n.saturating_sub(1) as isize)
                            as usize;
                    }
                    view.set_cursor(p, shift);
                    view.want_vcol = Some(vcol);
                }
                Key::Home => {
                    let p = if ctrl {
                        Pos::new(0, 0)
                    } else {
                        let line = doc.line(c.line);
                        let indent = leading_ws(&line).chars().count();
                        Pos::new(c.line, if c.col == indent { 0 } else { indent })
                    };
                    view.set_cursor(p, shift);
                    view.want_vcol = None;
                }
                Key::End => {
                    let p = if ctrl {
                        doc.end_pos()
                    } else {
                        Pos::new(c.line, doc.line_chars(c.line))
                    };
                    view.set_cursor(p, shift);
                    view.want_vcol = None;
                }
                Key::Backspace => {
                    if view.has_selection() {
                        return edit(doc, view, "", EditKind::Other, now);
                    }
                    let (start, kind) = if ctrl {
                        (word_left(doc, c), EditKind::Other)
                    } else if c.col > 0 {
                        (Pos::new(c.line, c.col - 1), EditKind::Typing)
                    } else if c.line > 0 {
                        (
                            Pos::new(c.line - 1, doc.line_chars(c.line - 1)),
                            EditKind::Other,
                        )
                    } else {
                        return Ok(());
                    };
                    delete_range(doc, view, start, c, kind, now)?;
                }
                Key::Delete => {
                    if shift && !view.has_selection() {
                        return run_command(ctx, doc, view, Command::DeleteLine, now);
                    }
                    if view.has_selection() {
                        return edit(doc, view, "", EditKind::Other, now);
                    }
                    let len = doc.line_chars(c.line);
                    let end = if ctrl {
                        word_right(doc, c)
                    } else if c.col < len {
                        Pos::new(c.line, c.col + 1)
                    } else if c.line + 1 < n {
                        Pos::new(c.line + 1, 0)
                    } else {
                        return Ok(());
                    };
                    delete_range(doc, view, c, end, EditKind::Other, now)?;
                }
                Key::Enter => {
                    let line = doc.line(c.line);
                    let indent: String = leading_ws(&line).chars().take(c.col).collect();
                    drop(line);
                    edit(doc, view, &format!("\n{indent}"), EditKind::Other, now)?;
                }
                Key::Tab if !ctrl && !alt => {
                    let (s, e) = view.selection();
                    if shift {
                        run_command(ctx, doc, view, Command::Outdent, now)?;
                    } else if s.line != e.line {
                        run_command(ctx, doc, view, Command::Indent, now)?;
                    } else {
                        edit(doc, view, "\t", EditKind::Typing, now)?;
                    }
                }
                Key::Escape => {
                    if view.has_selection() {
                        view.anchor = view.cursor;
                    }
                }
                Key::A if ctrl => run_command(ctx, doc, view, Command::SelectAll, now)?,
                Key::Z if ctrl && shift => run_command(ctx, doc, view, Command::Redo, now)?,
                Key::Z if ctrl => run_command(ctx, doc, view, Command::Undo, now)?,
                Key::Y if ctrl => run_command(ctx, doc, view, Command::Redo, now)?,
                Key::D if ctrl => run_command(ctx, doc, view, Command::DuplicateLine, now)?,
                Key::K if ctrl && shift => run_command(ctx, doc, view, Command::DeleteLine, now)?,
                Key::CloseBracket if ctrl => run_command(ctx, doc, view, Command::Indent, now)?,
                Key::OpenBracket if ctrl => run_command(ctx, doc, view, Command::Outdent, now)?,
                _ => {}
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d + (d - 1) / 3
}

pub fn fmt_int(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
