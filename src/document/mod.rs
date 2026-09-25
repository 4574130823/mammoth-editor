//! The editable document: a line-based piece table layered over a read-only [`Source`].
//!
//! Every edit is expressed as "replace doc lines `a..b` with these pieces". Untouched
//! lines are never copied: an `Orig` piece just references a range of original lines,
//! and everything past the last piece is an implicit *tail* of original lines whose
//! length grows while the background indexer runs. That is what lets you edit line 3
//! of a 100 GB file while the rest of it is still being scanned.

pub mod source;

use std::borrow::Cow;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use source::Source;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    /// Column in chars (not bytes).
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
        }
    }
    pub fn platform() -> Self {
        if cfg!(windows) { Eol::CrLf } else { Eol::Lf }
    }
}

#[derive(Clone, Debug)]
pub enum Piece {
    /// `len` original lines starting at original line `start`.
    Orig { start: usize, len: usize },
    /// Lines that were typed/pasted/replaced.
    Add {
        lines: Arc<Vec<String>>,
        start: usize,
        len: usize,
    },
}

impl Piece {
    pub fn added(lines: Vec<String>) -> Piece {
        let len = lines.len();
        Piece::Add {
            lines: Arc::new(lines),
            start: 0,
            len,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Piece::Orig { len, .. } | Piece::Add { len, .. } => *len,
        }
    }

    fn slice(&self, from: usize, to: usize) -> Piece {
        match self {
            Piece::Orig { start, .. } => Piece::Orig {
                start: start + from,
                len: to - from,
            },
            Piece::Add { lines, start, .. } => Piece::Add {
                lines: lines.clone(),
                start: start + from,
                len: to - from,
            },
        }
    }

    fn try_merge(&self, next: &Piece) -> Option<Piece> {
        match (self, next) {
            (Piece::Orig { start: a, len: la }, Piece::Orig { start: b, len: lb })
                if a + la == *b =>
            {
                Some(Piece::Orig {
                    start: *a,
                    len: la + lb,
                })
            }
            (
                Piece::Add {
                    lines: x,
                    start: a,
                    len: la,
                },
                Piece::Add {
                    lines: y,
                    start: b,
                    len: lb,
                },
            ) => {
                if Arc::ptr_eq(x, y) && a + la == *b {
                    Some(Piece::Add {
                        lines: x.clone(),
                        start: *a,
                        len: la + lb,
                    })
                } else if la + lb <= 512 {
                    let mut v = Vec::with_capacity(la + lb);
                    v.extend_from_slice(&x[*a..a + la]);
                    v.extend_from_slice(&y[*b..b + lb]);
                    Some(Piece::added(v))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

pub enum Loc<'a> {
    Orig(usize),
    Add(&'a str),
}

#[derive(Clone, Default)]
pub struct PieceTable {
    list: Vec<Piece>,
    /// `ends[i]` = doc line just after piece `i`.
    ends: Vec<usize>,
    /// Original lines `tail_start..` follow the pieces.
    tail_start: usize,
}

impl PieceTable {
    fn fixed_len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    pub fn line_count(&self, avail: usize) -> usize {
        self.fixed_len() + avail.saturating_sub(self.tail_start)
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.list
    }

    pub fn tail_start(&self) -> usize {
        self.tail_start
    }

    fn rebuild(&mut self) {
        let mut out: Vec<Piece> = Vec::with_capacity(self.list.len());
        for p in self.list.drain(..) {
            if p.len() == 0 {
                continue;
            }
            if let Some(last) = out.last()
                && let Some(m) = last.try_merge(&p)
            {
                *out.last_mut().unwrap() = m;
                continue;
            }
            out.push(p);
        }
        self.list = out;
        self.ends.clear();
        let mut acc = 0;
        for p in &self.list {
            acc += p.len();
            self.ends.push(acc);
        }
    }

    pub fn locate(&self, line: usize) -> (Option<usize>, usize) {
        // Returns (piece index or None for the tail, offset within it).
        let fixed = self.fixed_len();
        if line >= fixed {
            return (None, self.tail_start + (line - fixed));
        }
        let i = self.ends.partition_point(|&e| e <= line);
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        (Some(i), line - start)
    }

    fn loc(&self, line: usize) -> Loc<'_> {
        match self.locate(line) {
            (None, orig) => Loc::Orig(orig),
            (Some(i), off) => match &self.list[i] {
                Piece::Orig { start, .. } => Loc::Orig(start + off),
                Piece::Add { lines, start, .. } => Loc::Add(&lines[start + off]),
            },
        }
    }

    fn materialize(&mut self, upto: usize) {
        let fixed = self.fixed_len();
        if upto > fixed {
            let n = upto - fixed;
            self.list.push(Piece::Orig {
                start: self.tail_start,
                len: n,
            });
            self.ends.push(fixed + n);
            self.tail_start += n;
        }
    }

    fn split_at(&mut self, line: usize) -> usize {
        let i = self.ends.partition_point(|&e| e <= line);
        if i == self.list.len() {
            return i;
        }
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        if start == line {
            return i;
        }
        let off = line - start;
        let p = self.list[i].clone();
        let l = p.len();
        self.list[i] = p.slice(0, off);
        self.list.insert(i + 1, p.slice(off, l));
        self.ends.insert(i, start + off);
        i + 1
    }

    /// Replace doc lines `a..b` with `new`, returning the removed pieces.
    pub fn splice(&mut self, a: usize, b: usize, new: Vec<Piece>) -> Vec<Piece> {
        self.materialize(b);
        let i = self.split_at(a);
        let j = self.split_at(b);
        let removed: Vec<Piece> = self.list.splice(i..j, new).collect();
        self.rebuild();
        removed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Other,
}

pub type Selection = (Pos, Pos); // (anchor, cursor)

struct Edit {
    at: usize,
    removed: Vec<Piece>,
    inserted: usize,
}

struct Group {
    id: u64,
    edits: Vec<Edit>,
    before: Selection,
    after: Selection,
    kind: EditKind,
    time: f64,
}

pub const SOFT_LINE_MSG: &str =
    "That part of the file is a segment of a huge line (>64 KB) and is read-only.";

pub struct Document {
    pub source: Option<Arc<Source>>,
    pub path: Option<PathBuf>,
    pub title: String,
    table: PieceTable,
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_group: u64,
    state_id: u64,
    saved_id: u64,
    /// Bumped on every change (used to invalidate background jobs).
    pub version: u64,
    pub eol: Eol,
    pub bom: bool,
    /// When set, the document is temporarily read-only (e.g. while saving).
    pub busy: Option<String>,
}

impl Document {
    pub fn new_empty(title: String) -> Self {
        Self::from_lines(title, vec![String::new()])
    }

    pub fn from_lines(title: String, lines: Vec<String>) -> Self {
        let mut table = PieceTable::default();
        table.splice(
            0,
            0,
            vec![Piece::added(if lines.is_empty() {
                vec![String::new()]
            } else {
                lines
            })],
        );
        Self {
            source: None,
            path: None,
            title,
            table,
            undo: Vec::new(),
            redo: Vec::new(),
            next_group: 1,
            state_id: 0,
            saved_id: 0,
            version: 0,
            eol: Eol::platform(),
            bom: false,
            busy: None,
        }
    }

    pub fn from_source(src: Arc<Source>) -> Self {
        let path = src.path().to_path_buf();
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bom = src.has_bom();
        let mut doc = Self {
            source: Some(src),
            path: Some(path),
            title,
            table: PieceTable::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            next_group: 1,
            state_id: 0,
            saved_id: 0,
            version: 0,
            eol: Eol::Lf,
            bom,
            busy: None,
        };
        doc.detect_eol();
        doc
    }

    fn detect_eol(&mut self) {
        if let Some(src) = &self.source {
            let data = src.bytes();
            let probe = &data[..data.len().min(1 << 20)];
            self.eol = match memchr::memchr(b'\n', probe) {
                Some(i) if i > 0 && probe[i - 1] == b'\r' => Eol::CrLf,
                Some(_) => Eol::Lf,
                None => Eol::platform(),
            };
        }
    }

    /// Replace the backing file after it was rewritten on disk (clears undo history).
    pub fn reset_to_source(&mut self, src: Arc<Source>) {
        self.source = Some(src);
        self.table = PieceTable::default();
        self.undo.clear();
        self.redo.clear();
        self.state_id = 0;
        self.saved_id = 0;
        self.version += 1;
    }

    pub fn mark_saved(&mut self, path: &Path) {
        self.saved_id = self.state_id;
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.title = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.state_id != self.saved_id
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn table(&self) -> &PieceTable {
        &self.table
    }

    pub fn available(&self) -> usize {
        self.source.as_ref().map_or(0, |s| s.available_lines())
    }

    /// Everything of the file is visible (no preview / partially indexed state).
    pub fn is_fully_loaded(&self) -> bool {
        self.source.as_ref().is_none_or(|s| s.is_complete())
    }

    pub fn line_count(&self) -> usize {
        self.table.line_count(self.available())
    }

    pub fn line(&self, i: usize) -> Cow<'_, str> {
        match self.table.loc(i) {
            Loc::Add(s) => Cow::Borrowed(s),
            Loc::Orig(o) => match &self.source {
                Some(src) => src.line_text(o),
                None => Cow::Borrowed(""),
            },
        }
    }

    pub fn raw_line(&self, i: usize) -> &[u8] {
        match self.table.loc(i) {
            Loc::Add(s) => s.as_bytes(),
            Loc::Orig(o) => self.source.as_ref().map_or(&[][..], |s| s.line_bytes(o)),
        }
    }

    /// Doc line of an original line that lies in the untouched tail, if it does.
    pub fn tail_doc_line(&self, orig: usize) -> Option<usize> {
        let t = self.table.tail_start();
        (orig >= t).then(|| self.table.fixed_len() + orig - t)
    }

    /// How many original lines must be indexed for doc line `line` to exist.
    pub fn orig_lines_needed(&self, line: usize) -> usize {
        (line + 1 + self.table.tail_start()).saturating_sub(self.table.fixed_len())
    }

    pub fn is_soft(&self, i: usize) -> bool {
        match self.table.loc(i) {
            Loc::Add(_) => false,
            Loc::Orig(o) => self.source.as_ref().is_some_and(|s| s.span(o).soft),
        }
    }

    /// Convert a byte column in the raw line to a char column.
    pub fn byte_to_col(&self, line: usize, byte: usize) -> usize {
        let raw = self.raw_line(line);
        let byte = byte.min(raw.len());
        String::from_utf8_lossy(&raw[..byte]).chars().count()
    }

    pub fn line_chars(&self, i: usize) -> usize {
        self.line(i).chars().count()
    }

    pub fn end_pos(&self) -> Pos {
        let n = self.line_count();
        if n == 0 {
            return Pos::default();
        }
        Pos::new(n - 1, self.line_chars(n - 1))
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let n = self.line_count();
        if n == 0 {
            return Pos::default();
        }
        let line = p.line.min(n - 1);
        Pos::new(line, p.col.min(self.line_chars(line)))
    }

    /// Text between two positions. Returns `None` if it would exceed `limit` bytes.
    pub fn text_range(&self, a: Pos, b: Pos, limit: usize) -> Option<String> {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for i in a.line..=b.line {
            let line = self.line(i);
            let s = if i == a.line {
                char_to_byte(&line, a.col)
            } else {
                0
            };
            let e = if i == b.line {
                char_to_byte(&line, b.col)
            } else {
                line.len()
            };
            out.push_str(&line[s.min(e)..e]);
            if i != b.line {
                out.push_str(self.eol.as_str());
            }
            if out.len() > limit {
                return None;
            }
        }
        Some(out)
    }

    fn check_editable(&self) -> Result<(), String> {
        match &self.busy {
            Some(why) => Err(format!("Document is read-only while {why}.")),
            None if self.line_count() == 0 => Err("Still loading…".into()),
            None => Ok(()),
        }
    }

    /// Replace the text between `start` and `end` with `text`. Returns the position
    /// right after the inserted text.
    pub fn replace(
        &mut self,
        start: Pos,
        end: Pos,
        text: &str,
        kind: EditKind,
        before: Selection,
        now: f64,
    ) -> Result<Pos, String> {
        self.check_editable()?;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let (start, end) = (self.clamp(start), self.clamp(end));
        if self.is_soft(start.line) || self.is_soft(end.line) {
            return Err(SOFT_LINE_MSG.into());
        }
        let normalized;
        let text = if text.contains('\r') {
            normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            normalized.as_str()
        } else {
            text
        };

        let first = self.line(start.line);
        let last = self.line(end.line);
        let prefix = &first[..char_to_byte(&first, start.col)];
        let suffix = &last[char_to_byte(&last, end.col)..];
        let mut joined = String::with_capacity(prefix.len() + text.len() + suffix.len());
        joined.push_str(prefix);
        joined.push_str(text);
        let caret_byte = joined.len();
        joined.push_str(suffix);

        let before_caret = &joined[..caret_byte];
        let newlines = before_caret.matches('\n').count();
        let caret_col = match before_caret.rfind('\n') {
            Some(i) => before_caret[i + 1..].chars().count(),
            None => before_caret.chars().count(),
        };
        let lines: Vec<String> = joined.split('\n').map(str::to_owned).collect();
        let after = Pos::new(start.line + newlines, caret_col);
        self.apply(
            start.line,
            end.line + 1,
            vec![Piece::added(lines)],
            kind,
            before,
            (after, after),
            now,
        );
        Ok(after)
    }

    /// Replace whole lines `a..b` with `lines`.
    pub fn replace_lines(
        &mut self,
        a: usize,
        b: usize,
        lines: Vec<String>,
        before: Selection,
        after: Selection,
        now: f64,
    ) -> Result<(), String> {
        self.check_editable()?;
        let n = self.line_count();
        let b = b.min(n);
        // Deleting whole segments of a huge line is fine; rewriting them is not.
        if !lines.is_empty() && (a..b).any(|i| self.is_soft(i)) {
            return Err(SOFT_LINE_MSG.into());
        }
        // Never leave the document without a single line.
        let lines = if lines.is_empty() && a == 0 && b == n {
            vec![String::new()]
        } else {
            lines
        };
        let pieces = if lines.is_empty() {
            vec![]
        } else {
            vec![Piece::added(lines)]
        };
        self.apply(a, b, pieces, EditKind::Other, before, after, now);
        Ok(())
    }

    /// Swap lines `0..len` for a whole new set of pieces (used by Replace All).
    pub fn replace_prefix(&mut self, len: usize, pieces: Vec<Piece>, before: Selection, now: f64) {
        self.apply(0, len, pieces, EditKind::Other, before, before, now);
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        a: usize,
        b: usize,
        new: Vec<Piece>,
        kind: EditKind,
        before: Selection,
        after: Selection,
        now: f64,
    ) {
        let inserted: usize = new.iter().map(Piece::len).sum();
        let removed = self.table.splice(a, b, new);
        self.version += 1;
        self.redo.clear();

        if let Some(top) = self.undo.last_mut() {
            let mergeable = kind == EditKind::Typing
                && top.kind == EditKind::Typing
                && top.id == self.state_id
                && top.id != self.saved_id
                && now - top.time < 1.5
                && top.edits.len() == 1
                && top.edits[0].at == a
                && top.edits[0].inserted == b - a;
            if mergeable {
                top.edits[0].inserted = inserted;
                top.after = after;
                top.time = now;
                return;
            }
        }
        let id = self.next_group;
        self.next_group += 1;
        self.undo.push(Group {
            id,
            edits: vec![Edit {
                at: a,
                removed,
                inserted,
            }],
            before,
            after,
            kind,
            time: now,
        });
        self.state_id = id;
        if self.undo.len() > 10_000 {
            self.undo.remove(0);
        }
    }

    fn revert(&mut self, g: Group) -> Group {
        let mut inv = Vec::with_capacity(g.edits.len());
        for e in g.edits.into_iter().rev() {
            let removed_len: usize = e.removed.iter().map(Piece::len).sum();
            let now_removed = self.table.splice(e.at, e.at + e.inserted, e.removed);
            inv.push(Edit {
                at: e.at,
                removed: now_removed,
                inserted: removed_len,
            });
        }
        self.version += 1;
        Group { edits: inv, ..g }
    }

    pub fn undo(&mut self) -> Result<Option<Selection>, String> {
        if let Some(why) = &self.busy {
            return Err(format!("Document is read-only while {why}."));
        }
        let Some(g) = self.undo.pop() else {
            return Ok(None);
        };
        let g = self.revert(g);
        let sel = g.before;
        self.redo.push(g);
        self.state_id = self.undo.last().map_or(0, |g| g.id);
        Ok(Some(sel))
    }

    pub fn redo(&mut self) -> Result<Option<Selection>, String> {
        if let Some(why) = &self.busy {
            return Err(format!("Document is read-only while {why}."));
        }
        let Some(g) = self.redo.pop() else {
            return Ok(None);
        };
        let g = self.revert(g);
        let sel = g.after;
        self.state_id = g.id;
        // Never merge new typing into a redone group.
        let mut g = g;
        g.kind = EditKind::Other;
        self.undo.push(g);
        Ok(Some(sel))
    }

    pub fn save_snapshot(&self) -> SaveSnapshot {
        SaveSnapshot {
            table: self.table.clone(),
            source: self.source.clone(),
            eol: self.eol,
            bom: self.bom,
        }
    }
}

/// Everything needed to write the document from a background thread.
pub struct SaveSnapshot {
    pub table: PieceTable,
    pub source: Option<Arc<Source>>,
    pub eol: Eol,
    pub bom: bool,
}

impl SaveSnapshot {
    /// Rough number of bytes that will be written (for progress reporting).
    pub fn estimate(&self) -> u64 {
        self.source.as_ref().map_or(0, |s| s.len()) + 1
    }

    pub fn write(
        &self,
        w: &mut impl Write,
        progress: &AtomicU64,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let eol = self.eol.as_str().as_bytes();
        let mut need_sep = false;
        let mut written = 0u64;
        let mut put = |w: &mut dyn Write, bytes: &[u8]| -> io::Result<()> {
            for chunk in bytes.chunks(4 << 20) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
                w.write_all(chunk)?;
                written += chunk.len() as u64;
                progress.store(written, Ordering::Relaxed);
            }
            Ok(())
        };

        // Separator before the next piece. Between two original lines that were
        // adjacent in the file we copy the file's own terminator, so untouched
        // regions stay byte-identical even in files with mixed line endings.
        let src = self.source.as_deref();
        let separator = |prev_orig: Option<usize>, next_orig: Option<usize>| -> &[u8] {
            match (src, prev_orig, next_orig) {
                (Some(s), Some(p), Some(n)) if n == p + 1 => {
                    &s.bytes()[s.span(p).end as usize..s.line_start(n) as usize]
                }
                _ => eol,
            }
        };

        if self.bom {
            put(w, &[0xEF, 0xBB, 0xBF])?;
        }
        let mut prev_orig: Option<usize> = None;
        for piece in self.table.pieces() {
            match piece {
                Piece::Orig { start, len } => {
                    let src = src.expect("original lines without a source");
                    if need_sep {
                        put(w, separator(prev_orig, Some(*start)))?;
                    }
                    let first = src.span(*start);
                    let last = src.span(start + len - 1);
                    put(w, &src.bytes()[first.start as usize..last.end as usize])?;
                    need_sep = !last.soft;
                    prev_orig = Some(start + len - 1);
                }
                Piece::Add { lines, start, len } => {
                    if need_sep {
                        put(w, eol)?;
                    }
                    for (k, line) in lines[*start..start + len].iter().enumerate() {
                        if k > 0 {
                            put(w, eol)?;
                        }
                        put(w, line.as_bytes())?;
                    }
                    need_sep = true;
                    prev_orig = None;
                }
            }
        }
        if let Some(src) = src {
            let tail = self.table.tail_start();
            let has_tail = !(src.is_complete() && tail >= src.available_lines());
            if has_tail {
                if need_sep {
                    put(w, separator(prev_orig, Some(tail)))?;
                }
                let from = src.line_start(tail) as usize;
                put(w, &src.bytes()[from..])?;
            }
        }
        w.flush()
    }
}

pub fn char_to_byte(s: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(doc: &Document) -> String {
        (0..doc.line_count())
            .map(|i| doc.line(i).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn edit_undo_redo() {
        let mut d = Document::from_lines("t".into(), vec!["hello".into(), "world".into()]);
        let sel = (Pos::default(), Pos::default());
        let p = d
            .replace(
                Pos::new(0, 5),
                Pos::new(0, 5),
                " there\nnew",
                EditKind::Other,
                sel,
                0.0,
            )
            .unwrap();
        assert_eq!(p, Pos::new(1, 3));
        assert_eq!(text(&d), "hello there\nnew\nworld");
        d.replace(
            Pos::new(0, 2),
            Pos::new(2, 1),
            "",
            EditKind::Other,
            sel,
            0.0,
        )
        .unwrap();
        assert_eq!(text(&d), "heorld");
        d.undo().unwrap();
        assert_eq!(text(&d), "hello there\nnew\nworld");
        d.undo().unwrap();
        assert_eq!(text(&d), "hello\nworld");
        d.redo().unwrap();
        d.redo().unwrap();
        assert_eq!(text(&d), "heorld");
    }

    #[test]
    fn typing_merges() {
        let mut d = Document::new_empty("t".into());
        let sel = (Pos::default(), Pos::default());
        let mut p = Pos::default();
        for c in ["a", "b", "c"] {
            p = d.replace(p, p, c, EditKind::Typing, sel, 0.0).unwrap();
        }
        assert_eq!(text(&d), "abc");
        d.undo().unwrap();
        assert_eq!(text(&d), "");
        assert!(!d.is_dirty());
    }
}
