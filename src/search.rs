//! Find / Count / Extract / Replace All.
//!
//! Searches run on a background thread over a cheap snapshot of the piece table.
//! Original (unedited) text is searched directly in the memory-mapped bytes, in
//! line-aligned chunks, so a whole-file search runs at disk speed. Matches never
//! span a line break.
//!
//! In preview mode, forward searches keep going past the loaded region into the raw
//! bytes of the file; a hit there is reported as [`Hit::Beyond`] and the UI extends
//! the index up to it.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};

use std::collections::{HashMap, HashSet};

use crate::document::source::{LineSpan, with_readahead};
use crate::document::{Document, Piece, PieceTable, Source};
use crate::modules::{Grouper, Module};

const CHUNK: usize = 8 << 20;

#[derive(Clone)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Search for a detector module's matches instead of `text`.
    pub module: Option<Arc<dyn Module>>,
    /// Only accept module hits in these breakdown groups (e.g. only @gmail.com).
    pub group: Option<GroupFilter>,
}

/// Restricts a module search to hits in certain breakdown groups.
#[derive(Clone)]
pub struct GroupFilter {
    pub grouper: Grouper,
    /// Accept hits whose key is one of these (e.g. domains)…
    pub keys: Option<Arc<HashSet<String>>>,
    /// …and/or whose category is this (e.g. "Gmail").
    pub category: Option<String>,
}

impl GroupFilter {
    pub fn accepts(&self, found: &str) -> bool {
        let Some(key) = self.grouper.key(found) else {
            return false;
        };
        if self.keys.as_ref().is_some_and(|k| !k.contains(&key)) {
            return false;
        }
        match &self.category {
            Some(cat) => self.grouper.category(&key).as_deref() == Some(cat.as_str()),
            None => true,
        }
    }
}

#[derive(Clone)]
pub struct Matcher {
    pub bytes: regex::bytes::Regex,
    pub text: regex::Regex,
    validator: Option<Arc<dyn Module>>,
    group: Option<GroupFilter>,
    /// Expand `$1`-style references in replacements.
    pub expand: bool,
}

impl Matcher {
    pub fn new(q: &Query) -> Result<Self, String> {
        let pattern = match &q.module {
            Some(m) => m
                .pattern()
                .ok_or_else(|| format!("Module “{}” has no pattern", m.name()))?,
            None => {
                if q.text.is_empty() {
                    return Err(String::new());
                }
                let p = if q.regex {
                    q.text.clone()
                } else {
                    regex::escape(&q.text)
                };
                if q.whole_word {
                    format!(r"\b(?:{p})\b")
                } else {
                    p
                }
            }
        };
        let ci = q.module.is_none() && !q.case_sensitive;
        let bytes = regex::bytes::RegexBuilder::new(&pattern)
            .case_insensitive(ci)
            .multi_line(true)
            .crlf(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| short_regex_error(&e.to_string()))?;
        let text = regex::RegexBuilder::new(&pattern)
            .case_insensitive(ci)
            .multi_line(true)
            .crlf(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| short_regex_error(&e.to_string()))?;
        Ok(Self {
            bytes,
            text,
            validator: q.module.clone(),
            group: q.group.clone(),
            expand: q.regex && q.module.is_none(),
        })
    }

    fn accept(&self, hay: &[u8], r: Range<usize>) -> bool {
        if r.is_empty() || memchr::memchr(b'\n', &hay[r.clone()]).is_some() {
            return false;
        }
        if self.validator.is_none() && self.group.is_none() {
            return true;
        }
        let Ok(s) = std::str::from_utf8(&hay[r]) else {
            return false;
        };
        self.validator.as_ref().is_none_or(|v| v.validate(s))
            && self.group.as_ref().is_none_or(|g| g.accepts(s))
    }

    /// All accepted matches in a line (byte ranges), for highlighting.
    pub fn line_matches(&self, line: &str, base: usize, out: &mut Vec<(usize, usize)>) {
        for m in self.text.find_iter(line) {
            if self.accept(line.as_bytes(), m.range()) {
                out.push((base + m.start(), base + m.end()));
            }
        }
    }

    /// Does an accepted match span exactly `start..end` of `line`?
    pub fn matches_exactly(&self, line: &str, start: usize, end: usize) -> bool {
        self.text.find_at(line, start).is_some_and(|m| {
            m.start() == start && m.end() == end && self.accept(line.as_bytes(), m.range())
        })
    }

    /// The replacement text for the match at `start` in `line`.
    pub fn replacement_at(&self, line: &str, start: usize, repl: &str) -> String {
        if !self.expand {
            return repl.to_string();
        }
        match self.text.captures_at(line, start) {
            Some(caps) => {
                let mut out = String::new();
                caps.expand(repl, &mut out);
                out
            }
            None => repl.to_string(),
        }
    }

    /// Replace every accepted match in `line`; returns (new line, replacements).
    pub fn replace_line(&self, line: &str, repl: &str) -> (String, u64) {
        let mut n = 0u64;
        let out = self.text.replace_all(line, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            if !self.accept(line.as_bytes(), m.range()) {
                return m.as_str().to_string();
            }
            n += 1;
            if self.expand {
                let mut s = String::new();
                caps.expand(repl, &mut s);
                s
            } else {
                repl.to_string()
            }
        });
        (out.into_owned(), n)
    }
}

fn short_regex_error(e: &str) -> String {
    e.lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or(e)
        .trim()
        .trim_start_matches("error: ")
        .to_string()
}

// ----------------------------------------------------------------------------
// Snapshots and segments

pub struct Snapshot {
    table: PieceTable,
    source: Option<Arc<Source>>,
    avail: usize,
    /// Byte offset where the not-yet-indexed part of the file begins.
    beyond: Option<u64>,
    pub version: u64,
}

impl Snapshot {
    pub fn of(doc: &Document) -> Self {
        let (avail, beyond) = match &doc.source {
            Some(s) => {
                let complete = s.is_complete();
                let avail = s.available_lines();
                (
                    avail,
                    if complete {
                        None
                    } else {
                        Some(s.line_start(avail))
                    },
                )
            }
            None => (0, None),
        };
        Self {
            table: doc.table().clone(),
            source: doc.source.clone(),
            avail,
            beyond,
            version: doc.version,
        }
    }

    pub fn line_count(&self) -> usize {
        self.table.line_count(self.avail)
    }

    fn segments(&self) -> Vec<Seg> {
        let mut out = Vec::new();
        let mut doc = 0;
        for p in self.table.pieces() {
            match p {
                Piece::Orig { start, len } => out.push(Seg::Orig {
                    doc,
                    orig: *start,
                    len: *len,
                }),
                Piece::Add { lines, start, len } => out.push(Seg::Add {
                    doc,
                    lines: lines.clone(),
                    start: *start,
                    len: *len,
                }),
            }
            doc += p.len();
        }
        let tail = self.table.tail_start();
        if self.avail > tail {
            out.push(Seg::Orig {
                doc,
                orig: tail,
                len: self.avail - tail,
            });
        }
        if let Some(b) = self.beyond {
            out.push(Seg::Beyond { from: b });
        }
        out
    }

    fn src(&self) -> &Source {
        self.source
            .as_deref()
            .expect("original lines without a source")
    }

    /// Total bytes a full scan will touch (for progress bars).
    pub fn scan_size(&self) -> u64 {
        self.source.as_ref().map_or(0, |s| s.len()).max(1)
    }
}

enum Seg {
    Orig {
        doc: usize,
        orig: usize,
        len: usize,
    },
    Add {
        doc: usize,
        lines: Arc<Vec<String>>,
        start: usize,
        len: usize,
    },
    Beyond {
        from: u64,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Hit {
    /// Byte range within a document line.
    At {
        line: usize,
        start: usize,
        end: usize,
    },
    /// A match in the part of the file that is not indexed yet (absolute offsets).
    Beyond { start: u64, end: u64 },
}

pub struct JobCtl {
    pub cancel: AtomicBool,
    pub done: AtomicU64,
    pub total: u64,
}

impl JobCtl {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn fraction(&self) -> f32 {
        (self.done.load(Ordering::Relaxed) as f64 / self.total.max(1) as f64).min(1.0) as f32
    }
}

pub fn spawn<T: Send + 'static>(
    total: u64,
    ctx: &egui::Context,
    f: impl FnOnce(&JobCtl) -> T + Send + 'static,
) -> (Arc<JobCtl>, Receiver<T>) {
    let ctl = Arc::new(JobCtl {
        cancel: AtomicBool::new(false),
        done: AtomicU64::new(0),
        total,
    });
    let (tx, rx) = mpsc::channel();
    let c = ctl.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = f(&c);
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    (ctl, rx)
}

/// Line-aligned forward chunks over `data[lo..hi]`, calling `f(chunk_start, chunk)`.
/// Stops early when `f` returns true.
fn chunks_forward(
    data: &[u8],
    lo: usize,
    hi: usize,
    ctl: &JobCtl,
    mut f: impl FnMut(usize, &[u8]) -> bool,
) -> bool {
    // Large ranges are memory-mapped file data: read ahead on a helper thread so
    // disk I/O overlaps with the regex work.
    with_readahead(data, lo, hi, hi - lo > 4 * CHUNK, |ra| {
        let mut cs = lo;
        while cs < hi {
            if ctl.cancelled() {
                return false;
            }
            ra.store(cs, Ordering::Relaxed);
            let mut ce = (cs + CHUNK).min(hi);
            if ce < hi {
                ce = match memchr::memchr(b'\n', &data[ce..hi]) {
                    Some(i) => ce + i + 1,
                    None => hi,
                };
            }
            if f(cs, &data[cs..ce]) {
                return true;
            }
            ctl.add((ce - cs) as u64);
            cs = ce;
        }
        false
    })
}

fn first_match(m: &Matcher, hay: &[u8], mut at: usize, limit: usize) -> Option<(usize, usize)> {
    while at <= hay.len() {
        let mm = m.bytes.find_at(hay, at)?;
        if mm.start() >= limit {
            return None;
        }
        if m.accept(hay, mm.range()) {
            return Some((mm.start(), mm.end()));
        }
        at = mm.end().max(mm.start() + 1);
    }
    None
}

fn last_match(m: &Matcher, hay: &[u8], limit: usize) -> Option<(usize, usize)> {
    let mut best = None;
    let mut at = 0;
    while at <= hay.len() {
        let Some(mm) = m.bytes.find_at(hay, at) else {
            break;
        };
        if mm.start() >= limit {
            break;
        }
        if m.accept(hay, mm.range()) {
            best = Some((mm.start(), mm.end()));
        }
        at = mm.end().max(mm.start() + 1);
    }
    best
}

impl Snapshot {
    fn orig_hit(&self, doc: usize, orig: usize, abs: (u64, u64)) -> Hit {
        let src = self.src();
        let line = src.line_of_offset(abs.0);
        let span = src.span(line);
        let start = (abs.0 - span.start) as usize;
        Hit::At {
            line: doc + (line - orig),
            start,
            end: start + (abs.1 - abs.0) as usize,
        }
    }

    /// Search forward from (line, byte), wrapping around once.
    pub fn find_next(
        &self,
        m: &Matcher,
        from_line: usize,
        from_byte: usize,
        ctl: &JobCtl,
    ) -> Option<Hit> {
        let segs = self.segments();
        if let Some(h) = self.forward(&segs, m, (from_line, from_byte), None, ctl) {
            return Some(h);
        }
        if ctl.cancelled() {
            return None;
        }
        self.forward(&segs, m, (0, 0), Some((from_line, from_byte)), ctl)
    }

    fn forward(
        &self,
        segs: &[Seg],
        m: &Matcher,
        from: (usize, usize),
        until: Option<(usize, usize)>,
        ctl: &JobCtl,
    ) -> Option<Hit> {
        for seg in segs {
            match seg {
                Seg::Orig { doc, orig, len } => {
                    let (doc, orig, len) = (*doc, *orig, *len);
                    if from.0 >= doc + len {
                        continue;
                    }
                    if let Some(u) = until
                        && u.0 < doc
                    {
                        return None;
                    }
                    let src = self.src();
                    let data = src.bytes();
                    let seg_lo = src.span(orig).start as usize;
                    let seg_hi = src.span(orig + len - 1).end as usize;
                    let (lo, start) = if from.0 >= doc {
                        let s = src.span(orig + (from.0 - doc));
                        (
                            s.start as usize,
                            (s.start as usize + from.1).min(s.end as usize),
                        )
                    } else {
                        (seg_lo, seg_lo)
                    };
                    let limit = match until {
                        Some(u) if u.0 < doc + len => {
                            let s = src.span(orig + (u.0 - doc));
                            (s.start as usize + u.1).min(s.end as usize)
                        }
                        _ => usize::MAX,
                    };
                    let mut found = None;
                    chunks_forward(data, lo, seg_hi, ctl, |cs, hay| {
                        let at = start.saturating_sub(cs);
                        let lim = limit.saturating_sub(cs);
                        if let Some((a, b)) = first_match(m, hay, at, lim) {
                            found = Some(((cs + a) as u64, (cs + b) as u64));
                            return true;
                        }
                        cs + hay.len() >= limit
                    });
                    if let Some(abs) = found {
                        return Some(self.orig_hit(doc, orig, abs));
                    }
                    if limit != usize::MAX {
                        return None;
                    }
                }
                Seg::Add {
                    doc,
                    lines,
                    start,
                    len,
                } => {
                    for k in 0..*len {
                        let line_no = doc + k;
                        if line_no < from.0 {
                            continue;
                        }
                        let text = lines[start + k].as_bytes();
                        let at = if line_no == from.0 {
                            from.1.min(text.len())
                        } else {
                            0
                        };
                        let limit = match until {
                            Some(u) if u.0 == line_no => u.1,
                            Some(u) if u.0 < line_no => return None,
                            _ => usize::MAX,
                        };
                        if let Some((a, b)) = first_match(m, text, at, limit) {
                            return Some(Hit::At {
                                line: line_no,
                                start: a,
                                end: b,
                            });
                        }
                    }
                }
                Seg::Beyond { from: off } => {
                    if until.is_some() {
                        return None;
                    }
                    let src = self.src();
                    let data = src.bytes();
                    let mut found = None;
                    chunks_forward(data, *off as usize, data.len(), ctl, |cs, hay| {
                        if let Some((a, b)) = first_match(m, hay, 0, usize::MAX) {
                            found = Some(((cs + a) as u64, (cs + b) as u64));
                            return true;
                        }
                        false
                    });
                    if let Some((s, e)) = found {
                        return Some(Hit::Beyond { start: s, end: e });
                    }
                }
            }
            if ctl.cancelled() {
                return None;
            }
        }
        None
    }

    /// Search backward from (line, byte) within the loaded lines, wrapping once.
    pub fn find_prev(
        &self,
        m: &Matcher,
        from_line: usize,
        from_byte: usize,
        ctl: &JobCtl,
    ) -> Option<Hit> {
        let segs: Vec<Seg> = self
            .segments()
            .into_iter()
            .filter(|s| !matches!(s, Seg::Beyond { .. }))
            .collect();
        if let Some(h) = self.backward(&segs, m, (from_line, from_byte), None, ctl) {
            return Some(h);
        }
        let end = self.line_count();
        self.backward(&segs, m, (end, 0), Some((from_line, from_byte)), ctl)
    }

    fn backward(
        &self,
        segs: &[Seg],
        m: &Matcher,
        from: (usize, usize),
        until: Option<(usize, usize)>,
        ctl: &JobCtl,
    ) -> Option<Hit> {
        for seg in segs.iter().rev() {
            if ctl.cancelled() {
                return None;
            }
            match seg {
                Seg::Orig { doc, orig, len } => {
                    let (doc, orig, len) = (*doc, *orig, *len);
                    if from.0 < doc {
                        continue;
                    }
                    if let Some(u) = until
                        && u.0 >= doc + len
                    {
                        return None;
                    }
                    let src = self.src();
                    let data = src.bytes();
                    let seg_lo = src.span(orig).start as usize;
                    let (hi, limit) = if from.0 < doc + len {
                        let s = src.span(orig + (from.0 - doc));
                        (
                            s.end as usize,
                            (s.start as usize + from.1).min(s.end as usize),
                        )
                    } else {
                        let e = src.span(orig + len - 1).end as usize;
                        (e, e + 1)
                    };
                    let floor = match until {
                        Some(u) if u.0 >= doc => {
                            let s = src.span(orig + (u.0 - doc));
                            (s.start as usize + u.1).min(s.end as usize)
                        }
                        _ => 0,
                    };
                    let mut ce = hi;
                    while ce > seg_lo {
                        let mut cs = ce.saturating_sub(CHUNK).max(seg_lo);
                        if cs > seg_lo {
                            cs = match memchr::memrchr(b'\n', &data[seg_lo..cs]) {
                                Some(i) => seg_lo + i + 1,
                                None => seg_lo,
                            };
                        }
                        let hay = &data[cs..ce];
                        if let Some((a, b)) = last_match(m, hay, limit.saturating_sub(cs)) {
                            if cs + a >= floor {
                                return Some(self.orig_hit(
                                    doc,
                                    orig,
                                    ((cs + a) as u64, (cs + b) as u64),
                                ));
                            }
                            return None;
                        }
                        ctl.add((ce - cs) as u64);
                        if cs <= floor || ctl.cancelled() {
                            break;
                        }
                        ce = cs;
                    }
                    if until.is_some_and(|u| u.0 >= doc) {
                        return None;
                    }
                }
                Seg::Add {
                    doc,
                    lines,
                    start,
                    len,
                } => {
                    for k in (0..*len).rev() {
                        let line_no = doc + k;
                        if line_no > from.0 {
                            continue;
                        }
                        if let Some(u) = until
                            && line_no < u.0
                        {
                            return None;
                        }
                        let text = lines[start + k].as_bytes();
                        let limit = if line_no == from.0 {
                            from.1
                        } else {
                            usize::MAX
                        };
                        if let Some((a, b)) = last_match(m, text, limit) {
                            if until.is_some_and(|u| u.0 == line_no && a < u.1) {
                                return None;
                            }
                            return Some(Hit::At {
                                line: line_no,
                                start: a,
                                end: b,
                            });
                        }
                    }
                }
                Seg::Beyond { .. } => {}
            }
        }
        None
    }

    /// Visit every accepted match in the whole file (including the unindexed part).
    /// `f` receives the matched bytes and returns false to stop.
    pub fn for_each_match(&self, m: &Matcher, ctl: &JobCtl, mut f: impl FnMut(&[u8]) -> bool) {
        let mut keep_going = true;
        for seg in self.segments() {
            if !keep_going || ctl.cancelled() {
                return;
            }
            let (lo, hi) = match &seg {
                Seg::Orig { orig, len, .. } => {
                    let src = self.src();
                    (
                        src.span(*orig).start as usize,
                        src.span(orig + len - 1).end as usize,
                    )
                }
                Seg::Beyond { from } => (*from as usize, self.src().len() as usize),
                Seg::Add {
                    lines, start, len, ..
                } => {
                    for line in &lines[*start..start + len] {
                        let hay = line.as_bytes();
                        let mut at = 0;
                        while let Some((a, b)) = first_match(m, hay, at, usize::MAX) {
                            if !f(&hay[a..b]) {
                                return;
                            }
                            at = b.max(a + 1);
                        }
                    }
                    continue;
                }
            };
            let data = self.src().bytes();
            chunks_forward(data, lo, hi, ctl, |_, hay| {
                let mut at = 0;
                while let Some((a, b)) = first_match(m, hay, at, usize::MAX) {
                    if !f(&hay[a..b]) {
                        keep_going = false;
                        return true;
                    }
                    at = b.max(a + 1);
                }
                false
            });
        }
    }

    /// Build the replacement piece list for Replace All (requires a fully indexed file).
    /// Returns (pieces covering lines 0..line_count, replacements, skipped soft lines).
    pub fn replace_all(
        &self,
        m: &Matcher,
        repl: &str,
        ctl: &JobCtl,
    ) -> Option<(Vec<Piece>, u64, u64)> {
        let mut out: Vec<Piece> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut total = 0u64;
        let mut skipped = 0u64;
        let flush = |out: &mut Vec<Piece>, pending: &mut Vec<String>| {
            if !pending.is_empty() {
                out.push(Piece::added(std::mem::take(pending)));
            }
        };

        for seg in self.segments() {
            if ctl.cancelled() {
                return None;
            }
            match seg {
                Seg::Beyond { .. } => return None,
                Seg::Add {
                    lines, start, len, ..
                } => {
                    for line in &lines[start..start + len] {
                        let (new, n) = m.replace_line(line, repl);
                        total += n;
                        pending.push(if n > 0 { new } else { line.clone() });
                    }
                }
                Seg::Orig { orig, len, .. } => {
                    let src = self.src();
                    let data = src.bytes();
                    let lo = src.span(orig).start as usize;
                    let hi = src.span(orig + len - 1).end as usize;
                    // Collect the original lines that contain at least one match.
                    let mut hit_lines: Vec<usize> = Vec::new();
                    let mut cur_end = 0u64;
                    chunks_forward(data, lo, hi, ctl, |cs, hay| {
                        let mut at = 0;
                        while let Some((a, b)) = first_match(m, hay, at, usize::MAX) {
                            let abs = (cs + a) as u64;
                            if hit_lines.is_empty() || abs > cur_end {
                                let l = src.line_of_offset(abs);
                                if hit_lines.last() != Some(&l) {
                                    hit_lines.push(l);
                                }
                                cur_end = src.span(l).end;
                            }
                            at = b.max(a + 1);
                        }
                        false
                    });
                    if ctl.cancelled() {
                        return None;
                    }
                    let mut next = orig;
                    for l in hit_lines {
                        if src.span(l).soft {
                            skipped += 1;
                            continue;
                        }
                        let (new, n) = m.replace_line(&src.line_text(l), repl);
                        if n == 0 {
                            continue;
                        }
                        if l > next {
                            flush(&mut out, &mut pending);
                            out.push(Piece::Orig {
                                start: next,
                                len: l - next,
                            });
                        }
                        pending.push(new);
                        total += n;
                        next = l + 1;
                    }
                    if orig + len > next {
                        flush(&mut out, &mut pending);
                        out.push(Piece::Orig {
                            start: next,
                            len: orig + len - next,
                        });
                    }
                }
            }
        }
        flush(&mut out, &mut pending);
        Some((out, total, skipped))
    }
}

/// Result of a breakdown job: hit counts per group key, most frequent first.
pub struct Breakdown {
    pub counts: Vec<(String, u64)>,
    pub total: u64,
    /// Too many distinct keys: the rest were lumped into "(other)".
    pub capped: bool,
    /// The most frequent full matches (e.g. email addresses), most frequent first.
    pub top_values: Vec<(String, u64)>,
    /// How many different full matches there are (a lower bound if `values_capped`).
    pub distinct_values: usize,
    pub values_capped: bool,
}

const MAX_DISTINCT_VALUES: usize = 5_000_000;
const TOP_VALUES: usize = 5_000;

/// Result of a filter job: the matching lines, in file order.
pub struct Filtered {
    /// Doc line numbers of `text`.
    pub lines: Vec<usize>,
    pub text: Vec<String>,
    pub truncated: bool,
}

const MAX_GROUP_KEYS: usize = 2_000_000;
const MAX_FILTER_LINES: usize = 20_000_000;
const MAX_FILTER_BYTES: usize = 1 << 30;

impl Snapshot {
    /// Whether every line of the file is indexed (needed for line-based jobs).
    pub fn is_complete(&self) -> bool {
        self.beyond.is_none()
    }

    /// Text of one line of the snapshot.
    pub fn line(&self, i: usize) -> String {
        match self.table.locate(i) {
            (None, orig) => self.src().line_text(orig).into_owned(),
            (Some(p), off) => match &self.table.pieces()[p] {
                Piece::Orig { start, .. } => self.src().line_text(start + off).into_owned(),
                Piece::Add { lines, start, .. } => lines[start + off].clone(),
            },
        }
    }

    /// Count hits per group (e.g. emails per domain) across the whole file.
    pub fn breakdown(&self, m: &Matcher, grouper: &Grouper, ctl: &JobCtl) -> Breakdown {
        let mut map: HashMap<String, u64> = HashMap::new();
        let mut values: HashMap<String, u64> = HashMap::new();
        let mut values_capped = false;
        let mut total = 0u64;
        let mut capped = false;
        self.for_each_match(m, ctl, |b| {
            let Ok(s) = std::str::from_utf8(b) else {
                return true;
            };
            if let Some(key) = grouper.key(s) {
                let v = grouper.value(s);
                if let Some(c) = values.get_mut(&v) {
                    *c += 1;
                } else if values.len() < MAX_DISTINCT_VALUES {
                    values.insert(v, 1);
                } else {
                    values_capped = true;
                }
                total += 1;
                if let Some(c) = map.get_mut(&key) {
                    *c += 1;
                } else if map.len() < MAX_GROUP_KEYS {
                    map.insert(key, 1);
                } else {
                    capped = true;
                    *map.entry("(other)".into()).or_insert(0) += 1;
                }
            }
            true
        });
        let mut counts: Vec<(String, u64)> = map.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let distinct_values = values.len();
        let mut top_values: Vec<(String, u64)> = values.into_iter().collect();
        // Only the most frequent ones are kept for display.
        if top_values.len() > TOP_VALUES {
            top_values.select_nth_unstable_by(TOP_VALUES, |a, b| b.1.cmp(&a.1));
            top_values.truncate(TOP_VALUES);
        }
        top_values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Breakdown {
            counts,
            total,
            capped,
            top_values,
            distinct_values,
            values_capped,
        }
    }

    /// Every line containing a match. `None` if the file isn't fully indexed.
    /// With `header`, line 0 is always included (for CSV files).
    pub fn filter_lines(&self, m: &Matcher, header: bool, ctl: &JobCtl) -> Option<Filtered> {
        if !self.is_complete() {
            return None;
        }
        let mut out = Filtered {
            lines: Vec::new(),
            text: Vec::new(),
            truncated: false,
        };
        let mut bytes = 0usize;
        let mut push = |out: &mut Filtered, line: usize, text: String| -> bool {
            if out.lines.last() == Some(&line) {
                return true;
            }
            bytes += text.len();
            out.lines.push(line);
            out.text.push(text);
            if out.lines.len() >= MAX_FILTER_LINES || bytes >= MAX_FILTER_BYTES {
                out.truncated = true;
                return false;
            }
            true
        };
        if header && self.line_count() > 0 {
            push(&mut out, 0, self.line(0));
        }
        for seg in self.segments() {
            if ctl.cancelled() || out.truncated {
                break;
            }
            match seg {
                Seg::Beyond { .. } => return None,
                Seg::Add {
                    doc,
                    lines,
                    start,
                    len,
                } => {
                    for (k, line) in lines[start..start + len].iter().enumerate() {
                        if first_match(m, line.as_bytes(), 0, usize::MAX).is_some()
                            && !push(&mut out, doc + k, line.clone())
                        {
                            break;
                        }
                    }
                }
                Seg::Orig { doc, orig, len } => {
                    let src = self.src();
                    let data = src.bytes();
                    let lo = src.span(orig).start as usize;
                    let hi = src.span(orig + len - 1).end as usize;
                    let mut cur: Option<(usize, u64)> = None; // (orig line, its end)
                    chunks_forward(data, lo, hi, ctl, |cs, hay| {
                        let mut at = 0;
                        while let Some((a, b)) = first_match(m, hay, at, usize::MAX) {
                            let abs = (cs + a) as u64;
                            if cur.is_none_or(|(_, end)| abs > end) {
                                let l = src.line_of_offset(abs);
                                cur = Some((l, src.span(l).end));
                                if !push(&mut out, doc + (l - orig), src.line_text(l).into_owned())
                                {
                                    return true;
                                }
                            }
                            at = b.max(a + 1);
                        }
                        false
                    });
                }
            }
        }
        Some(out)
    }

    /// Visit every line in order as raw bytes; `f` returns false to stop.
    /// Returns false if the file isn't fully indexed or the job was cancelled.
    pub fn for_each_line(&self, ctl: &JobCtl, mut f: impl FnMut(usize, &[u8]) -> bool) -> bool {
        if !self.is_complete() {
            return false;
        }
        for seg in self.segments() {
            if ctl.cancelled() {
                return false;
            }
            match seg {
                Seg::Beyond { .. } => return false,
                Seg::Add {
                    doc,
                    lines,
                    start,
                    len,
                } => {
                    for (k, line) in lines[start..start + len].iter().enumerate() {
                        if !f(doc + k, line.as_bytes()) {
                            return true;
                        }
                    }
                }
                Seg::Orig { doc, orig, len } => {
                    let src = self.src();
                    let data = src.bytes();
                    let lo = src.span(orig).start as usize;
                    let hi = src.span(orig + len - 1).end as usize;
                    let mut stopped = false;
                    let mut pending = 0u64;
                    let ok = with_readahead(data, lo, hi, src.is_mapped(), |ra| {
                        src.for_each_span(orig, orig + len, |i, span: LineSpan| {
                            pending += span.end - span.start + 1;
                            if pending > 1 << 20 {
                                ctl.add(pending);
                                pending = 0;
                                ra.store(span.start as usize, std::sync::atomic::Ordering::Relaxed);
                                if ctl.cancelled() {
                                    return false;
                                }
                            }
                            if !f(
                                doc + (i - orig),
                                &data[span.start as usize..span.end as usize],
                            ) {
                                stopped = true;
                                return false;
                            }
                            true
                        })
                    });
                    if stopped {
                        return true;
                    }
                    if !ok {
                        return false;
                    }
                }
            }
        }
        true
    }
}

// ----------------------------------------------------------------------------
// Heatmap

/// Buckets along the file for the heatmap.
pub const HEAT_BUCKETS: usize = 2048;

/// Where hits cluster along the document, per layer (e.g. per detector).
///
/// Positions are on a "document axis": the document's bytes in order, with edited
/// lines and the not-yet-indexed tail included, so it works in preview mode too.
pub struct Heatmap {
    /// `layers[k][bucket]` = hits of layer `k` in that stretch of the document.
    pub layers: Vec<Vec<u32>>,
    pub totals: Vec<u64>,
    pub axis_len: u64,
    segs: Vec<AxisSeg>,
}

#[derive(Clone, Copy)]
struct AxisSeg {
    doc_line: usize,
    lines: usize,
    start: u64,
    len: u64,
    kind: AxisKind,
}

#[derive(Clone, Copy)]
enum AxisKind {
    Orig { orig: usize, lo: u64 },
    Add,
    Beyond { from: u64 },
}

/// Where a click on the heatmap should go.
#[derive(Debug, PartialEq)]
pub enum AxisTarget {
    Line(usize),
    /// A byte offset past the indexed part of the file.
    Offset(u64),
}

impl Snapshot {
    /// Count hits of every matcher in one pass over the file.
    pub fn heatmap(&self, matchers: &[Matcher], ctl: &JobCtl) -> Heatmap {
        let segs = self.segments();
        let mut axis = Vec::with_capacity(segs.len());
        let mut pos = 0u64;
        for seg in &segs {
            let (doc_line, lines, len, kind) = match seg {
                Seg::Orig { doc, orig, len } => {
                    let src = self.src();
                    let lo = src.span(*orig).start;
                    let hi = src.span(orig + len - 1).end;
                    (*doc, *len, hi - lo + 1, AxisKind::Orig { orig: *orig, lo })
                }
                Seg::Add {
                    doc,
                    lines,
                    start,
                    len,
                } => {
                    let bytes = lines[*start..start + len]
                        .iter()
                        .map(|l| l.len() as u64 + 1)
                        .sum();
                    (*doc, *len, bytes, AxisKind::Add)
                }
                Seg::Beyond { from } => (
                    self.line_count(),
                    0,
                    self.src().len() - from,
                    AxisKind::Beyond { from: *from },
                ),
            };
            axis.push(AxisSeg {
                doc_line,
                lines,
                start: pos,
                len,
                kind,
            });
            pos += len;
        }
        let total = pos.max(1);
        let bucket = |p: u64| {
            ((p as u128 * HEAT_BUCKETS as u128 / total as u128) as usize).min(HEAT_BUCKETS - 1)
        };
        let mut layers = vec![vec![0u32; HEAT_BUCKETS]; matchers.len()];
        let mut totals = vec![0u64; matchers.len()];
        let mut hit = |k: usize, p: u64| {
            let b = &mut layers[k][bucket(p)];
            *b = b.saturating_add(1);
            totals[k] += 1;
        };
        for (seg, ax) in segs.iter().zip(&axis) {
            if ctl.cancelled() {
                break;
            }
            match seg {
                Seg::Add {
                    lines, start, len, ..
                } => {
                    let mut off = 0u64;
                    for line in &lines[*start..start + len] {
                        for (k, m) in matchers.iter().enumerate() {
                            let mut at = 0;
                            while let Some((a, b)) = first_match(m, line.as_bytes(), at, usize::MAX)
                            {
                                hit(k, ax.start + off + a as u64);
                                at = b.max(a + 1);
                            }
                        }
                        off += line.len() as u64 + 1;
                    }
                }
                _ => {
                    let (lo, hi) = match ax.kind {
                        AxisKind::Orig { lo, .. } => (lo, lo + ax.len - 1),
                        AxisKind::Beyond { from } => (from, from + ax.len),
                        AxisKind::Add => unreachable!(),
                    };
                    let data = self.src().bytes();
                    chunks_forward(data, lo as usize, hi as usize, ctl, |cs, hay| {
                        for (k, m) in matchers.iter().enumerate() {
                            let mut at = 0;
                            while let Some((a, b)) = first_match(m, hay, at, usize::MAX) {
                                hit(k, ax.start + (cs + a) as u64 - lo);
                                at = b.max(a + 1);
                            }
                        }
                        false
                    });
                }
            }
        }
        Heatmap {
            layers,
            totals,
            axis_len: total,
            segs: axis,
        }
    }
}

impl Heatmap {
    fn seg_at(&self, p: u64) -> Option<&AxisSeg> {
        let i = self.segs.partition_point(|s| s.start <= p).checked_sub(1)?;
        self.segs.get(i)
    }

    /// Where a click at `frac` (0 = top, 1 = bottom) of the heatmap leads.
    pub fn target_at(&self, frac: f32, doc: &Document) -> Option<AxisTarget> {
        let p = ((frac.clamp(0.0, 1.0) as f64) * self.axis_len as f64) as u64;
        let seg = self.seg_at(p.min(self.axis_len.saturating_sub(1)))?;
        let within = p.saturating_sub(seg.start).min(seg.len.saturating_sub(1));
        Some(match seg.kind {
            AxisKind::Orig { orig, lo } => {
                let src = doc.source.as_ref()?;
                let byte = lo + within;
                if !src.covers_offset(byte) {
                    return Some(AxisTarget::Offset(byte));
                }
                let l = src
                    .line_of_offset(byte)
                    .clamp(orig, orig + seg.lines.saturating_sub(1));
                AxisTarget::Line(seg.doc_line + (l - orig))
            }
            AxisKind::Add => {
                let l = (within as u128 * seg.lines as u128 / seg.len.max(1) as u128) as usize;
                AxisTarget::Line(seg.doc_line + l.min(seg.lines.saturating_sub(1)))
            }
            AxisKind::Beyond { from } => AxisTarget::Offset(from + within),
        })
    }

    /// Position of doc `line` on the axis, as a fraction (for the "you are here" box).
    pub fn frac_of_line(&self, line: usize, doc: &Document) -> Option<f32> {
        let p = self.axis_of_line(line, doc)?;
        Some((p as f64 / self.axis_len.max(1) as f64) as f32)
    }

    fn axis_of_line(&self, line: usize, doc: &Document) -> Option<u64> {
        let i = self
            .segs
            .partition_point(|s| s.doc_line <= line)
            .checked_sub(1)?;
        let seg = self.segs[i];
        match seg.kind {
            AxisKind::Orig { orig, lo } if line < seg.doc_line + seg.lines => {
                let src = doc.source.as_ref()?;
                Some(
                    seg.start
                        + src
                            .line_start(orig + (line - seg.doc_line))
                            .saturating_sub(lo),
                )
            }
            AxisKind::Add if line < seg.doc_line + seg.lines => Some(
                seg.start
                    + ((line - seg.doc_line) as u128 * seg.len as u128 / seg.lines.max(1) as u128)
                        as u64,
            ),
            AxisKind::Beyond { from } => {
                // Lines indexed after the heatmap was computed live in this stretch.
                let src = doc.source.as_ref()?;
                let (None, orig) = doc.table().locate(line) else {
                    return None;
                };
                let byte = src.line_start(orig);
                (byte >= from).then(|| seg.start + (byte - from))
            }
            _ => Some(seg.start + seg.len),
        }
    }
}
