//! End-to-end tests over real files: indexing, preview mode, editing, saving, searching.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::{Duration, Instant};

use crate::document::source::MAX_LINE;
use crate::document::{Document, EditKind, Pos, Source};
use crate::search::{Hit, JobCtl, Matcher, Query, Snapshot};

fn tmp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mammoth-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn wait_complete(src: &Arc<Source>) {
    let t = Instant::now();
    while !src.is_complete() {
        assert!(t.elapsed() < Duration::from_secs(600), "indexing timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the editor should show for `bytes`: split on \n, strip one trailing \r,
/// and cut lines longer than MAX_LINE into soft segments.
fn expected_lines(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in bytes.split(|&b| b == b'\n') {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let mut rest = raw;
        while rest.len() as u64 > MAX_LINE {
            let (a, b) = rest.split_at(MAX_LINE as usize);
            out.push(String::from_utf8_lossy(a).into_owned());
            rest = b;
        }
        out.push(String::from_utf8_lossy(rest).into_owned());
    }
    out
}

fn doc_lines(doc: &Document) -> Vec<String> {
    (0..doc.line_count())
        .map(|i| doc.line(i).into_owned())
        .collect()
}

fn save_to_vec(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_snapshot()
        .write(&mut out, &AtomicU64::new(0), &AtomicBool::new(false))
        .unwrap();
    out
}

fn ctl() -> JobCtl {
    JobCtl {
        cancel: AtomicBool::new(false),
        done: AtomicU64::new(0),
        total: 1,
    }
}

fn matcher(text: &str, regex: bool) -> Matcher {
    Matcher::new(&Query {
        text: text.into(),
        case_sensitive: false,
        whole_word: false,
        regex,
        module: None,
        group: None,
    })
    .unwrap()
}

fn sample() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..5000 {
        v.extend_from_slice(format!("line {i} contact user{i}@example.com").as_bytes());
        v.extend_from_slice(if i % 3 == 0 { b"\r\n" } else { b"\n" });
    }
    // A single enormous line (soft-split) followed by a final line without newline.
    v.extend(std::iter::repeat_n(b'x', (MAX_LINE * 2 + 123) as usize));
    v.push(b'\n');
    v.extend_from_slice(b"last line");
    v
}

#[test]
fn indexes_lines_exactly() {
    let ctx = egui::Context::default();
    let bytes = sample();
    let path = tmp_file("index.txt", &bytes);
    let src = Source::open(&path, usize::MAX, &ctx).unwrap();
    wait_complete(&src);
    let doc = Document::from_source(src);
    assert_eq!(doc_lines(&doc), expected_lines(&bytes));
    assert!(doc.is_soft(5000) && doc.is_soft(5001) && !doc.is_soft(5002));
}

#[test]
fn big_file_background_index_and_preview() {
    let ctx = egui::Context::default();
    // ~40 MB so the background indexer (not the synchronous path) is used.
    let mut bytes = Vec::new();
    let mut i = 0usize;
    while bytes.len() < 40 << 20 {
        bytes.extend_from_slice(
            format!("{i:09} the quick brown fox jumps over the lazy dog\n").as_bytes(),
        );
        i += 1;
    }
    bytes.extend_from_slice(b"needle-at-the-very-end");
    let total = i + 1;
    let path = tmp_file("big.txt", &bytes);

    // Preview: exactly 1000 lines, then extend.
    let src = Source::open(&path, 1000, &ctx).unwrap();
    let t = Instant::now();
    while src.is_loading() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(src.available_lines(), 1000);
    let doc = Document::from_source(src.clone());
    assert_eq!(doc.line_count(), 1000);
    assert_eq!(
        doc.line(999),
        format!("{:09} the quick brown fox jumps over the lazy dog", 999)
    );

    // A search in preview mode still finds text past the loaded region.
    let snap = Snapshot::of(&doc);
    let hit = snap
        .find_next(&matcher("needle-at", false), 0, 0, &ctl())
        .unwrap();
    let Hit::Beyond { start, .. } = hit else {
        panic!("expected a hit beyond the index, got {hit:?}")
    };
    assert_eq!(start as usize, bytes.len() - "needle-at-the-very-end".len());

    // Saving a partially indexed file must still write every byte.
    assert_eq!(save_to_vec(&doc), bytes);

    src.load_all(&ctx);
    wait_complete(&src);
    assert_eq!(doc.line_count(), total);
    assert_eq!(doc.line(total - 1), "needle-at-the-very-end");
    assert_eq!(
        doc.line(123_456),
        format!("{:09} the quick brown fox jumps over the lazy dog", 123_456)
    );
}

#[test]
fn edit_save_roundtrip() {
    let ctx = egui::Context::default();
    let bytes = sample();
    let path = tmp_file("edit.txt", &bytes);
    let src = Source::open(&path, usize::MAX, &ctx).unwrap();
    wait_complete(&src);
    let mut doc = Document::from_source(src);
    let mut expect = expected_lines(&bytes);
    let sel = (Pos::default(), Pos::default());

    // Insert text in the middle of line 10, join lines 20/21, delete lines 100..200.
    doc.replace(
        Pos::new(10, 4),
        Pos::new(10, 4),
        "INSERTED",
        EditKind::Other,
        sel,
        0.0,
    )
    .unwrap();
    expect[10].insert_str(4, "INSERTED");
    let l20 = doc.line_chars(20);
    doc.replace(
        Pos::new(20, l20),
        Pos::new(21, 0),
        "",
        EditKind::Other,
        sel,
        0.0,
    )
    .unwrap();
    let joined = expect.remove(21);
    expect[20].push_str(&joined);
    doc.replace_lines(99, 199, vec![], sel, sel, 0.0).unwrap();
    expect.drain(99..199);
    assert_eq!(doc_lines(&doc), expect);

    // Soft segments of the huge line are read-only.
    let soft = expect
        .iter()
        .position(|l| l.len() as u64 == MAX_LINE)
        .unwrap();
    assert!(
        doc.replace(
            Pos::new(soft, 1),
            Pos::new(soft, 1),
            "!",
            EditKind::Other,
            sel,
            0.0
        )
        .is_err()
    );

    // Saved bytes: edited lines use the doc's line ending, untouched ones keep theirs.
    let saved = save_to_vec(&doc);
    assert_eq!(expected_lines(&saved), expect);

    // Undo everything and the original bytes come back exactly.
    while doc.can_undo() {
        doc.undo().unwrap();
    }
    assert_eq!(save_to_vec(&doc), bytes);
    assert!(!doc.is_dirty());
}

#[test]
fn find_count_replace() {
    let ctx = egui::Context::default();
    let bytes = sample();
    let path = tmp_file("find.txt", &bytes);
    let src = Source::open(&path, usize::MAX, &ctx).unwrap();
    wait_complete(&src);
    let mut doc = Document::from_source(src);
    let sel = (Pos::default(), Pos::default());
    // An edited line in the middle, to exercise mixed original/added segments.
    doc.replace(
        Pos::new(2500, 0),
        Pos::new(2500, 0),
        "EDITED user9999@example.com ",
        EditKind::Other,
        sel,
        0.0,
    )
    .unwrap();

    let email = crate::modules::builtin()
        .into_iter()
        .find(|m| m.id() == "email")
        .unwrap();
    let m = Matcher::new(&Query {
        text: String::new(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        module: Some(email),
        group: None,
    })
    .unwrap();
    let snap = Snapshot::of(&doc);

    let mut n = 0;
    snap.for_each_match(&m, &ctl(), |_| {
        n += 1;
        true
    });
    assert_eq!(n, 5001);

    // Forward from line 2499's end finds the edited line; its first email is the inserted one.
    match snap.find_next(&m, 2499, 9999, &ctl()).unwrap() {
        Hit::At { line, start, end } => {
            assert_eq!(line, 2500);
            assert_eq!(&doc.line(2500)[start..end], "user9999@example.com");
        }
        h => panic!("{h:?}"),
    }
    // Backward from the start of line 10 finds line 9's email.
    match snap.find_prev(&m, 10, 0, &ctl()).unwrap() {
        Hit::At { line, start, end } => {
            assert_eq!(line, 9);
            assert_eq!(&doc.line(9)[start..end], "user9@example.com");
        }
        h => panic!("{h:?}"),
    }
    // Wrap-around: forward from the last line lands on line 0.
    let last = doc.line_count() - 1;
    assert!(matches!(
        snap.find_next(&m, last, 0, &ctl()),
        Some(Hit::At { line: 0, .. })
    ));

    // Replace All with capture groups over the whole document, then undo.
    let re = matcher(r"user(\d+)@example\.com", true);
    let (pieces, count, _) = snap.replace_all(&re, "<$1>", &ctl()).unwrap();
    assert_eq!(count, 5001);
    let before = doc_lines(&doc);
    doc.replace_prefix(doc.line_count(), pieces, sel, 0.0);
    assert_eq!(doc.line(7), "line 7 contact <7>");
    assert_eq!(doc.line(2500), "EDITED <9999> line 2500 contact <2500>");
    assert_eq!(doc.line_count(), before.len());
    doc.undo().unwrap();
    assert_eq!(doc_lines(&doc), before);
}

/// Benchmark on a real big file:
/// `MAMMOTH_BENCH_FILE=path cargo test --release bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_big_file() {
    let Ok(path) = std::env::var("MAMMOTH_BENCH_FILE") else {
        return;
    };
    let ctx = egui::Context::default();

    let t = Instant::now();
    let src = Source::open(std::path::Path::new(&path), 1000, &ctx).unwrap();
    while src.is_loading() {
        std::thread::yield_now();
    }
    println!(
        "open + preview:        {} lines in {:?} ({:.2} GB file)",
        src.available_lines(),
        t.elapsed(),
        src.len() as f64 / 1e9
    );

    let doc = Document::from_source(src.clone());
    let snap = Snapshot::of(&doc);
    let t = Instant::now();
    let hit = snap.find_next(&matcher("THE-END", false), 0, 0, &ctl());
    let secs = t.elapsed().as_secs_f64();
    println!(
        "find text at EOF:      {hit:?} in {secs:.2}s ({:.2} GB/s, preview mode)",
        src.len() as f64 / secs / 1e9
    );

    let t = Instant::now();
    src.load_all(&ctx);
    wait_complete(&src);
    let secs = t.elapsed().as_secs_f64();
    println!(
        "full line index:       {} lines in {secs:.2}s ({:.2} GB/s)",
        src.available_lines(),
        src.len() as f64 / secs / 1e9
    );

    let n = doc.line_count();
    let t = Instant::now();
    for k in 0..10_000usize {
        std::hint::black_box(doc.line(k.wrapping_mul(2_654_435_761) % n));
    }
    println!("10k random line reads: {:?}", t.elapsed());

    let email = crate::modules::builtin()
        .into_iter()
        .find(|m| m.id() == "email")
        .unwrap();
    let m = Matcher::new(&Query {
        text: String::new(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        module: Some(email),
        group: None,
    })
    .unwrap();
    let snap = Snapshot::of(&doc);
    let t = Instant::now();
    let mut count = 0u64;
    snap.for_each_match(&m, &ctl(), |_| {
        count += 1;
        true
    });
    let secs = t.elapsed().as_secs_f64();
    println!(
        "count emails:          {count} in {secs:.2}s ({:.2} GB/s)",
        src.len() as f64 / secs / 1e9
    );
}

#[test]
fn log_levels_are_coloured() {
    let log = crate::modules::builtin()
        .into_iter()
        .find(|m| m.id() == "log")
        .unwrap();
    let line = "2026-09-01T00:00:00Z [ERROR] key=\"x\" WARN";
    let mut spans = Vec::new();
    log.highlight(line, &mut spans);
    let texts: Vec<&str> = spans.iter().map(|s| &line[s.start..s.end]).collect();
    assert_eq!(
        texts,
        vec!["2026-09-01T00:00:00Z", "[ERROR]", "\"x\"", "WARN"]
    );
    assert!(
        spans.windows(2).all(|w| w[0].end <= w[1].start),
        "spans sorted and disjoint"
    );
}

/// `MAMMOTH_BENCH_FILE=path cargo test --release bench_index -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_index() {
    let Ok(path) = std::env::var("MAMMOTH_BENCH_FILE") else {
        return;
    };
    let ctx = egui::Context::default();
    let src = Source::open(std::path::Path::new(&path), usize::MAX, &ctx).unwrap();
    let t = Instant::now();
    wait_complete(&src);
    let secs = t.elapsed().as_secs_f64();
    println!(
        "index only: {} lines in {secs:.2}s ({:.2} GB/s)",
        src.available_lines(),
        src.len() as f64 / secs / 1e9
    );
}

/// Writes logo previews for eyeballing: `MAMMOTH_PREVIEW_DIR=dir cargo test logo_preview -- --ignored`
#[test]
#[ignore]
fn logo_preview() {
    let Ok(dir) = std::env::var("MAMMOTH_PREVIEW_DIR") else {
        return;
    };
    // Big version plus small ones on a dark and a light background, side by side.
    let sizes = [256usize, 64, 32, 20, 16];
    let (w, h) = (256 + 16 + 64 + 32 + 20 + 16 + 16 * 5, 256 * 2 + 16);
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    for (row, bgc) in [[0x15u8, 0x17, 0x1c], [0xf2, 0xf2, 0xf2]]
        .into_iter()
        .enumerate()
    {
        for y in 0..256 + 8 {
            for x in 0..w {
                img.put_pixel(
                    x as u32,
                    (row * (256 + 8) + y).min(h - 1) as u32,
                    image::Rgba([bgc[0], bgc[1], bgc[2], 255]),
                );
            }
        }
        let mut x0 = 8;
        for &n in &sizes {
            let px = crate::logo::render(n);
            for y in 0..n {
                for x in 0..n {
                    let i = (y * n + x) * 4;
                    let a = px[i + 3] as f32 / 255.0;
                    let oy = row * (256 + 8) + 4 + y;
                    let base = img.get_pixel((x0 + x) as u32, oy as u32).0;
                    let mix = |c: usize| (px[i + c] as f32 * a + base[c] as f32 * (1.0 - a)) as u8;
                    img.put_pixel(
                        (x0 + x) as u32,
                        oy as u32,
                        image::Rgba([mix(0), mix(1), mix(2), 255]),
                    );
                }
            }
            x0 += n + 16;
        }
    }
    img.save(std::path::Path::new(&dir).join("logo_preview.png"))
        .unwrap();
}

#[test]
fn heatmap_buckets_and_mapping() {
    use crate::search::{AxisTarget, HEAT_BUCKETS};
    let ctx = egui::Context::default();
    // 10,000 equal-length lines; ERROR only on lines 1,000..2,000 (10%..20% of the file).
    let mut body = String::new();
    for i in 0..10_000 {
        let word = if (1000..2000).contains(&i) {
            "ERROR"
        } else {
            "fine!"
        };
        body.push_str(&format!("{i:06} {word} xxxxxxxxxxxxxxxxxxxxxxxx\n"));
    }
    let path = tmp_file("heat.txt", body.as_bytes());
    let src = Source::open(&path, usize::MAX, &ctx).unwrap();
    wait_complete(&src);
    let doc = Document::from_source(src);
    let snap = Snapshot::of(&doc);
    let m = matcher("ERROR", false);
    let heat = snap.heatmap(std::slice::from_ref(&m), &ctl());
    assert_eq!(heat.totals[0], 1000);
    let hot: Vec<usize> = (0..HEAT_BUCKETS)
        .filter(|&b| heat.layers[0][b] > 0)
        .collect();
    let (first, last) = (
        hot[0] as f32 / HEAT_BUCKETS as f32,
        *hot.last().unwrap() as f32 / HEAT_BUCKETS as f32,
    );
    assert!(
        (first - 0.10).abs() < 0.005 && (last - 0.20).abs() < 0.005,
        "{first} {last}"
    );
    // Clicking at 15% lands on line ~1,500; line 5,000 sits at ~50%.
    let Some(AxisTarget::Line(l)) = heat.target_at(0.15, &doc) else {
        panic!()
    };
    assert!((1490..=1510).contains(&l), "{l}");
    let f = heat.frac_of_line(5000, &doc).unwrap();
    assert!((f - 0.5).abs() < 0.001, "{f}");

    // Preview mode: the unindexed part is on the axis too; clicking there gives an offset.
    let src = Source::open(&path, 100, &ctx).unwrap();
    while src.is_loading() {
        std::thread::yield_now();
    }
    let doc = Document::from_source(src);
    let heat = Snapshot::of(&doc).heatmap(std::slice::from_ref(&m), &ctl());
    assert_eq!(
        heat.totals[0], 1000,
        "the whole file is scanned even in preview"
    );
    assert!(matches!(
        heat.target_at(0.15, &doc),
        Some(AxisTarget::Offset(_))
    ));
    assert!(
        matches!(heat.target_at(0.001, &doc), Some(AxisTarget::Line(l)) if (5..=15).contains(&l))
    );
}
