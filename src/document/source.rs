//! Read-only view of a file on disk plus a *sparse* line index.
//!
//! Big files are memory-mapped, so opening a 100 GB file costs nothing up front.
//! A background thread scans for line breaks and records the byte offset of every
//! `BLOCK`-th line. That keeps the index tiny (~8 bytes per 1024 lines) while any
//! line can still be located quickly: jump to its checkpoint, then decode at most
//! `BLOCK` lines with `memchr`.
//!
//! The indexer runs until it reaches a *target* (a line count and/or byte offset).
//! "Preview mode" is simply a small line target; "Load entire file" raises it to
//! infinity. Lines longer than `MAX_LINE` bytes are split into read-only "soft"
//! segments so a single 1 GB line cannot freeze the UI.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use memmap2::Mmap;

/// Lines per index checkpoint.
pub const BLOCK: usize = 1024;
/// Lines longer than this (in bytes) are split into soft segments.
pub const MAX_LINE: u64 = 64 * 1024;
/// Bytes scanned between index publications.
const CHUNK: u64 = 16 << 20;
/// Newline-counting granularity inside a chunk.
const SUB: u64 = 4096;
/// Files up to this size are read into RAM instead of memory-mapped.
const IN_MEMORY_LIMIT: u64 = 64 << 20;
/// Files up to this size are indexed synchronously when opened.
const SYNC_INDEX_LIMIT: u64 = 16 << 20;
/// Read-ahead granularity and how far ahead of the scanner it may run.
const READAHEAD_CHUNK: usize = 8 << 20;
const READAHEAD_WINDOW: usize = 48 << 20;
/// Number of decoded blocks kept around.
const BLOCK_CACHE: usize = 48;

enum Data {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpan {
    pub start: u64,
    /// End of the line's content (line terminator excluded).
    pub end: u64,
    /// True if this segment was cut because the physical line is too long.
    pub soft: bool,
}

struct Index {
    /// `checkpoints[k]` = byte offset where line `k * BLOCK` starts.
    checkpoints: Vec<u64>,
    /// Line terminators (hard or soft) found so far.
    breaks: usize,
    /// Start offset of line number `breaks`.
    cur: u64,
    /// Where the scanner resumes.
    scanned: u64,
    complete: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub scanned: u64,
    pub total: u64,
    pub lines: usize,
    pub running: bool,
    /// Bytes per second of the current/last indexing run.
    pub rate: f64,
}

pub struct Source {
    path: PathBuf,
    data: Data,
    bom: u64,
    index: Mutex<Index>,
    blocks: Mutex<Vec<(usize, Arc<Vec<LineSpan>>)>>,
    target_lines: AtomicUsize,
    target_offset: AtomicU64,
    running: AtomicBool,
    cancel: AtomicBool,
    clock: Mutex<(Instant, u64, f64)>,
}

impl Source {
    /// Open `path`. The indexer is started with `initial_target` lines as its goal
    /// (`usize::MAX` = index everything).
    pub fn open(
        path: &Path,
        initial_target: usize,
        ctx: &egui::Context,
    ) -> std::io::Result<Arc<Self>> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let data = if len == 0 {
            Data::Owned(Vec::new())
        } else if len <= IN_MEMORY_LIMIT {
            let mut buf = Vec::with_capacity(len as usize);
            (&file).read_to_end(&mut buf)?;
            Data::Owned(buf)
        } else {
            // SAFETY: the file is opened read-only. If another process truncates it
            // while mapped, reads may fault; that is the standard mmap trade-off and
            // Windows prevents truncating mapped files anyway.
            Data::Mapped(unsafe { Mmap::map(&file)? })
        };
        let bytes = match &data {
            Data::Mapped(m) => &m[..],
            Data::Owned(v) => &v[..],
        };
        let bom = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };

        let src = Arc::new(Source {
            path: path.to_path_buf(),
            data,
            bom,
            index: Mutex::new(Index {
                checkpoints: vec![bom],
                breaks: 0,
                cur: bom,
                scanned: bom,
                complete: false,
            }),
            blocks: Mutex::new(Vec::new()),
            target_lines: AtomicUsize::new(initial_target),
            target_offset: AtomicU64::new(0),
            running: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            clock: Mutex::new((Instant::now(), 0, 0.0)),
        });

        if len <= SYNC_INDEX_LIMIT {
            src.scan(ctx);
        }
        src.kick(ctx);
        Ok(src)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            Data::Mapped(m) => &m[..],
            Data::Owned(v) => &v[..],
        }
    }

    pub fn len(&self) -> u64 {
        self.bytes().len() as u64
    }

    pub fn is_mapped(&self) -> bool {
        matches!(self.data, Data::Mapped(_))
    }

    pub fn has_bom(&self) -> bool {
        self.bom > 0
    }

    /// Number of lines that are fully known (and therefore displayable).
    pub fn available_lines(&self) -> usize {
        let ix = self.index.lock().unwrap();
        if ix.complete {
            ix.breaks + 1
        } else {
            ix.breaks
        }
    }

    pub fn is_complete(&self) -> bool {
        self.index.lock().unwrap().complete
    }

    pub fn progress(&self) -> Progress {
        let (scanned, lines) = {
            let ix = self.index.lock().unwrap();
            (
                ix.scanned,
                if ix.complete {
                    ix.breaks + 1
                } else {
                    ix.breaks
                },
            )
        };
        let running = self.running.load(Ordering::SeqCst);
        let rate = {
            let mut clock = self.clock.lock().unwrap();
            if running {
                let secs = clock.0.elapsed().as_secs_f64();
                if secs > 0.05 {
                    clock.2 = scanned.saturating_sub(clock.1) as f64 / secs;
                }
            }
            clock.2
        };
        Progress {
            scanned,
            total: self.len(),
            lines,
            running,
            rate,
        }
    }

    /// Start offset of line `i`. Valid for `i <= breaks` (i.e. up to the first
    /// not-yet-terminated line).
    pub fn line_start(&self, i: usize) -> u64 {
        {
            let ix = self.index.lock().unwrap();
            if i >= ix.breaks {
                return ix.cur;
            }
        }
        self.span(i).start
    }

    /// Span of an available line.
    pub fn span(&self, i: usize) -> LineSpan {
        match self.block(i / BLOCK) {
            Some(b) => b.get(i % BLOCK).copied().unwrap_or(LineSpan {
                start: self.len(),
                end: self.len(),
                soft: false,
            }),
            None => LineSpan {
                start: self.len(),
                end: self.len(),
                soft: false,
            },
        }
    }

    pub fn line_bytes(&self, i: usize) -> &[u8] {
        let s = self.span(i);
        &self.bytes()[s.start as usize..s.end as usize]
    }

    pub fn line_text(&self, i: usize) -> Cow<'_, str> {
        String::from_utf8_lossy(self.line_bytes(i))
    }

    /// Whether the line containing `off` has been indexed.
    pub fn covers_offset(&self, off: u64) -> bool {
        let ix = self.index.lock().unwrap();
        ix.complete || off < ix.cur
    }

    /// Line containing byte offset `off` (must be covered by the index).
    pub fn line_of_offset(&self, off: u64) -> usize {
        let b = {
            let ix = self.index.lock().unwrap();
            ix.checkpoints
                .partition_point(|&c| c <= off)
                .saturating_sub(1)
        };
        let Some(spans) = self.block(b) else {
            return b * BLOCK;
        };
        let i = spans.partition_point(|s| s.start <= off).saturating_sub(1);
        // `off` may sit on a line terminator that belongs to the previous line.
        b * BLOCK + i
    }

    /// Visit the spans of lines `from..to` in order, decoding blocks directly (so a
    /// whole-file pass doesn't evict the UI's block cache). `f` returns false to stop.
    /// Returns false if stopped early.
    pub fn for_each_span(
        &self,
        from: usize,
        to: usize,
        mut f: impl FnMut(usize, LineSpan) -> bool,
    ) -> bool {
        let mut b = from / BLOCK;
        while b * BLOCK < to {
            let Some(start) = self.index.lock().unwrap().checkpoints.get(b).copied() else {
                return true;
            };
            for (k, span) in decode_block(self.bytes(), start).into_iter().enumerate() {
                let i = b * BLOCK + k;
                if i < from {
                    continue;
                }
                if i >= to {
                    return true;
                }
                if !f(i, span) {
                    return false;
                }
            }
            b += 1;
        }
        true
    }

    fn block(&self, b: usize) -> Option<Arc<Vec<LineSpan>>> {
        {
            let mut cache = self.blocks.lock().unwrap();
            if let Some(pos) = cache.iter().position(|(k, _)| *k == b) {
                let entry = cache.remove(pos);
                let spans = entry.1.clone();
                cache.push(entry);
                return Some(spans);
            }
        }
        let start = {
            let ix = self.index.lock().unwrap();
            *ix.checkpoints.get(b)?
        };
        let spans = Arc::new(decode_block(self.bytes(), start));
        let mut cache = self.blocks.lock().unwrap();
        if cache.len() >= BLOCK_CACHE {
            cache.remove(0);
        }
        cache.push((b, spans.clone()));
        Some(spans)
    }

    // ------------------------------------------------------------------
    // Indexer control

    fn needs_work(&self) -> bool {
        if self.cancel.load(Ordering::SeqCst) {
            return false;
        }
        let ix = self.index.lock().unwrap();
        !ix.complete
            && (ix.breaks < self.target_lines.load(Ordering::SeqCst)
                || ix.cur <= self.target_offset.load(Ordering::SeqCst))
    }

    /// Make sure the index reaches at least `lines` lines.
    pub fn want_lines(self: &Arc<Self>, lines: usize, ctx: &egui::Context) {
        self.target_lines.fetch_max(lines, Ordering::SeqCst);
        self.kick(ctx);
    }

    /// Make sure the index covers byte offset `off`.
    pub fn want_offset(self: &Arc<Self>, off: u64, ctx: &egui::Context) {
        self.target_offset.fetch_max(off, Ordering::SeqCst);
        self.kick(ctx);
    }

    pub fn load_all(self: &Arc<Self>, ctx: &egui::Context) {
        self.want_lines(usize::MAX, ctx);
    }

    pub fn pause(&self) {
        self.target_lines.store(0, Ordering::SeqCst);
        self.target_offset.store(0, Ordering::SeqCst);
    }

    pub fn is_loading(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn kick(self: &Arc<Self>, ctx: &egui::Context) {
        if !self.needs_work() {
            return;
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let scanned = self.index.lock().unwrap().scanned;
        *self.clock.lock().unwrap() = (Instant::now(), scanned, 0.0);
        let me = Arc::clone(self);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("mammoth-indexer".into())
            .spawn(move || me.worker(ctx));
        if spawned.is_err() {
            self.running.store(false, Ordering::SeqCst);
        }
    }

    fn worker(self: Arc<Self>, ctx: egui::Context) {
        loop {
            self.scan(&ctx);
            self.running.store(false, Ordering::SeqCst);
            if self.needs_work()
                && self
                    .running
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                continue;
            }
            break;
        }
        ctx.request_repaint();
    }

    /// Ask the indexer to stop without waiting for it.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Stop the indexer and wait for its thread to exit.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        while self.running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn scan(&self, ctx: &egui::Context) {
        let data = self.bytes();
        let len = data.len() as u64;
        let (mut breaks, mut cur, mut pos) = {
            let ix = self.index.lock().unwrap();
            if ix.complete {
                return;
            }
            (ix.breaks, ix.cur, ix.scanned)
        };
        let mut last_repaint = Instant::now();
        let mut cps = Vec::new();

        with_readahead(data, pos as usize, len as usize, self.is_mapped(), |ra| {
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let t_lines = self.target_lines.load(Ordering::Relaxed);
                let t_off = self.target_offset.load(Ordering::Relaxed);
                if breaks >= t_lines && cur > t_off {
                    break;
                }

                let end = (pos + CHUNK).min(len);
                let mut stop = false;
                ra.store(pos as usize, Ordering::Relaxed);

                // Walk the chunk in small blocks. A block whose newlines cannot hit a
                // checkpoint, the stop target or an over-long line is handled with one
                // SIMD count; only the rare remaining blocks are walked newline by newline.
                let mut i = pos;
                'blocks: while i < end {
                    let se = (i + SUB).min(end);
                    let sub = &data[i as usize..se as usize];
                    let cnt = memchr::memchr_iter(b'\n', sub).count();
                    let fast = cnt > 0
                        && breaks + cnt < t_lines
                        && breaks % BLOCK + cnt < BLOCK
                        && i + memchr::memchr(b'\n', sub).unwrap() as u64 - cur <= MAX_LINE;
                    if fast {
                        breaks += cnt;
                        cur = i + memchr::memrchr(b'\n', sub).unwrap() as u64 + 1;
                        i = se;
                        continue;
                    }
                    for k in memchr::memchr_iter(b'\n', sub) {
                        let p = i + k as u64;
                        while p - cur > MAX_LINE {
                            cur = soft_break(data, cur);
                            breaks += 1;
                            if breaks % BLOCK == 0 {
                                cps.push(cur);
                            }
                            if breaks >= t_lines && cur > t_off {
                                stop = true;
                                break 'blocks;
                            }
                        }
                        breaks += 1;
                        cur = p + 1;
                        if breaks % BLOCK == 0 {
                            cps.push(cur);
                        }
                        if breaks >= t_lines && cur > t_off {
                            stop = true;
                            break 'blocks;
                        }
                    }
                    // The pending (unterminated) line may already be too long.
                    while se - cur > MAX_LINE {
                        cur = soft_break(data, cur);
                        breaks += 1;
                        if breaks % BLOCK == 0 {
                            cps.push(cur);
                        }
                        if breaks >= t_lines && cur > t_off {
                            stop = true;
                            break 'blocks;
                        }
                    }
                    i = se;
                }
                pos = if stop { cur } else { end };
                let complete = !stop && end == len;
                {
                    let mut ix = self.index.lock().unwrap();
                    ix.checkpoints.append(&mut cps);
                    ix.breaks = breaks;
                    ix.cur = cur;
                    ix.scanned = pos;
                    ix.complete = complete;
                }
                if complete || stop {
                    break;
                }
                if last_repaint.elapsed() > Duration::from_millis(120) {
                    ctx.request_repaint();
                    last_repaint = Instant::now();
                }
            }
        });
        ctx.request_repaint();
    }
}

/// Ask the OS to start reading `bytes` (part of a memory-mapped file) into RAM in the
/// background, using large concurrent I/O. Scanning code calls this for the chunk
/// *after* the one it is working on, so disk reads overlap with CPU work instead of
/// stalling on one small page fault at a time. A hint only: failures are ignored.
pub fn prefetch(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[repr(C)]
        struct MemoryRange {
            addr: *mut c_void,
            len: usize,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut c_void;
            fn PrefetchVirtualMemory(
                process: *mut c_void,
                count: usize,
                ranges: *const MemoryRange,
                flags: u32,
            ) -> i32;
        }
        let range = MemoryRange {
            addr: bytes.as_ptr() as *mut c_void,
            len: bytes.len(),
        };
        // SAFETY: the range is a live slice of our own address space.
        unsafe {
            PrefetchVirtualMemory(GetCurrentProcess(), 1, &range, 0);
        }
    }
    #[cfg(unix)]
    {
        const PAGE: usize = 4096;
        let start = bytes.as_ptr() as usize;
        let aligned = start & !(PAGE - 1);
        // SAFETY: advisory call on memory we have mapped; the range is page-aligned.
        unsafe {
            libc::madvise(
                aligned as *mut libc::c_void,
                bytes.len() + (start - aligned),
                libc::MADV_WILLNEED,
            );
        }
    }
}

/// Run `f` while a helper thread prefetches `data[lo..hi]` a few chunks ahead of the
/// position `f` reports through the atomic. The OS prefetch call blocks until its I/O
/// is done, so doing it on a separate thread is what lets disk reads overlap with the
/// caller's CPU work. With `enabled == false` no thread is started.
pub fn with_readahead<R>(
    data: &[u8],
    lo: usize,
    hi: usize,
    enabled: bool,
    f: impl FnOnce(&AtomicUsize) -> R,
) -> R {
    let pos = AtomicUsize::new(lo);
    if !enabled || hi.saturating_sub(lo) <= READAHEAD_CHUNK {
        return f(&pos);
    }
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut next = lo;
            while !done.load(Ordering::Relaxed) && next < hi {
                let cur = pos.load(Ordering::Relaxed);
                next = next.max(cur);
                if next >= cur + READAHEAD_WINDOW {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                let e = (next + READAHEAD_CHUNK).min(hi);
                prefetch(&data[next..e]);
                next = e;
            }
        });
        let r = f(&pos);
        done.store(true, Ordering::Relaxed);
        r
    })
}

/// Where to cut a line that exceeds `MAX_LINE`, backing off to a UTF-8 boundary.
fn soft_break(data: &[u8], cur: u64) -> u64 {
    let mut b = cur + MAX_LINE;
    let mut k = 0;
    while k < 3 && (data[b as usize] & 0xC0) == 0x80 {
        b -= 1;
        k += 1;
    }
    if b <= cur { cur + MAX_LINE } else { b }
}

/// Decode up to `BLOCK` lines starting at `start`. Must agree exactly with `scan`.
fn decode_block(data: &[u8], start: u64) -> Vec<LineSpan> {
    let len = data.len() as u64;
    let mut out = Vec::with_capacity(BLOCK);
    let mut cur = start;
    while out.len() < BLOCK {
        let win_end = (cur + MAX_LINE + 1).min(len);
        match memchr::memchr(b'\n', &data[cur as usize..win_end as usize]) {
            Some(i) => {
                let p = cur + i as u64;
                let mut end = p;
                if end > cur && data[end as usize - 1] == b'\r' {
                    end -= 1;
                }
                out.push(LineSpan {
                    start: cur,
                    end,
                    soft: false,
                });
                cur = p + 1;
            }
            None if win_end == len => {
                out.push(LineSpan {
                    start: cur,
                    end: len,
                    soft: false,
                });
                break;
            }
            None => {
                let b = soft_break(data, cur);
                out.push(LineSpan {
                    start: cur,
                    end: b,
                    soft: true,
                });
                cur = b;
            }
        }
    }
    out
}
