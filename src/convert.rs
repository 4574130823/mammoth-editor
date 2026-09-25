//! Streaming format conversion: CSV / TSV / PSV ↔ JSON Lines / JSON array.
//!
//! Everything streams line by line from the document to a writer, so a 50 GB CSV
//! becomes JSON Lines without being loaded into memory. JSON → CSV reads the input
//! twice: once to discover every column, once to write the rows.

use std::collections::HashSet;
use std::io::Write;

use serde_json::Value;

use crate::csv;
use crate::search::{JobCtl, Snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Delimited text with this delimiter.
    Delimited(u8),
    JsonLines,
    JsonArray,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Delimited(b',') => "CSV",
            Format::Delimited(b'\t') => "TSV",
            Format::Delimited(b'|') => "Pipe-separated",
            Format::Delimited(_) => "Delimited",
            Format::JsonLines => "JSON Lines",
            Format::JsonArray => "JSON array",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Delimited(b'\t') => "tsv",
            Format::Delimited(b'|') => "psv",
            Format::Delimited(_) => "csv",
            Format::JsonLines => "jsonl",
            Format::JsonArray => "json",
        }
    }

    /// Guess the format from a tab's syntax module.
    pub fn from_syntax(syntax: Option<&str>) -> Option<Format> {
        match syntax? {
            "json" => Some(Format::JsonLines),
            s => csv::delimiter_for(Some(s)).map(Format::Delimited),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub from: Format,
    pub to: Format,
    /// Delimited input: the first row holds column names.
    pub header: bool,
    /// Delimited → JSON: write numbers and true/false as JSON types (text otherwise).
    pub typed: bool,
    /// JSON → delimited: nested objects become `user.email` style columns.
    pub flatten: bool,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub records: u64,
    pub skipped: u64,
    pub columns: usize,
}

/// Convert the snapshot's text according to `spec`, writing to `out`.
pub fn convert(
    snap: &Snapshot,
    spec: &Spec,
    out: &mut dyn Write,
    ctl: &JobCtl,
) -> Result<Stats, String> {
    if !snap.is_complete() {
        return Err("The whole file must be loaded first.".into());
    }
    let r = match (spec.from, spec.to) {
        (Format::Delimited(d), Format::Delimited(d2)) => {
            delimited_to_delimited(snap, d, d2, out, ctl)
        }
        (Format::Delimited(d), to) => {
            delimited_to_json(snap, d, spec, to == Format::JsonArray, out, ctl)
        }
        (from, Format::Delimited(d)) => json_to_delimited(snap, from, d, spec.flatten, out, ctl),
        (from, to) => json_to_json(snap, from, to == Format::JsonArray, out, ctl),
    };
    if ctl.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    r
}

fn io(e: std::io::Error) -> String {
    format!("Write failed: {e}")
}

/// Column names from a header row: trimmed, with blanks and duplicates made unique.
fn column_names(header: &str, delim: u8) -> Vec<String> {
    let mut seen = HashSet::new();
    csv::field_spans(header, delim)
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let base = csv::value(&header[a..b]).trim().to_string();
            let base = if base.is_empty() {
                format!("column_{}", i + 1)
            } else {
                base
            };
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

/// Is `s` exactly a JSON number (so it can be written verbatim, without reformatting)?
/// Values with leading zeros like `007` are not, and stay text.
fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// One CSV row as a JSON object, written by hand so numbers keep their exact text
/// (`12.50` stays `12.50`, `1e3` stays `1e3`).
fn row_json(names: &[String], line: &str, delim: u8, typed: bool) -> String {
    let mut s = String::with_capacity(line.len() * 2 + 16);
    s.push('{');
    for (c, &(a, b)) in csv::field_spans(line, delim).iter().enumerate() {
        if c > 0 {
            s.push(',');
        }
        let v = csv::value(&line[a..b]);
        match names.get(c) {
            Some(k) => s.push_str(&json_string(k)),
            None => s.push_str(&json_string(&format!("column_{}", c + 1))),
        }
        s.push(':');
        if typed && (v == "true" || v == "false" || is_json_number(&v)) {
            s.push_str(&v);
        } else {
            s.push_str(&json_string(&v));
        }
    }
    s.push('}');
    s
}

fn delimited_to_json(
    snap: &Snapshot,
    delim: u8,
    spec: &Spec,
    array: bool,
    out: &mut dyn Write,
    ctl: &JobCtl,
) -> Result<Stats, String> {
    let mut stats = Stats::default();
    let mut names: Vec<String> = Vec::new();
    let mut err = None;
    if array {
        out.write_all(b"[\n").map_err(io)?;
    }
    snap.for_each_line(ctl, |i, bytes| {
        let line = String::from_utf8_lossy(bytes);
        if i == 0 && spec.header {
            names = column_names(&line, delim);
            stats.columns = names.len();
            return true;
        }
        if line.is_empty() {
            return true;
        }
        let json = row_json(&names, &line, delim, spec.typed);
        let sep: &[u8] = if array && stats.records > 0 {
            b",\n"
        } else {
            b""
        };
        let end: &[u8] = if array { b"" } else { b"\n" };
        if let Err(e) = out
            .write_all(sep)
            .and_then(|_| out.write_all(json.as_bytes()))
            .and_then(|_| out.write_all(end))
        {
            err = Some(io(e));
            return false;
        }
        stats.records += 1;
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    if array {
        out.write_all(b"\n]\n").map_err(io)?;
    }
    Ok(stats)
}

/// Flatten nested objects into dotted keys; arrays stay as values.
fn flatten_into(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, child) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_into(&key, child, out);
            }
        }
        _ => out.push((prefix.to_string(), v.clone())),
    }
}

fn record_fields(v: &Value, flatten: bool) -> Option<Vec<(String, Value)>> {
    let obj = v.as_object()?;
    let mut fields = Vec::with_capacity(obj.len());
    for (k, child) in obj {
        if flatten {
            flatten_into(k, child, &mut fields);
        } else {
            fields.push((k.clone(), child.clone()));
        }
    }
    Some(fields)
}

fn cell_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// JSON records from the input: one per line (JSON Lines), or the items of a single
/// top-level array.
fn for_each_record(
    snap: &Snapshot,
    from: Format,
    ctl: &JobCtl,
    skipped: &mut u64,
    mut f: impl FnMut(Value) -> bool,
) -> Result<(), String> {
    match from {
        Format::JsonArray => {
            let mut text = String::new();
            let mut too_big = false;
            snap.for_each_line(ctl, |_, b| {
                text.push_str(&String::from_utf8_lossy(b));
                text.push('\n');
                too_big = text.len() > 1 << 30;
                !too_big
            });
            if too_big {
                return Err(
                    "A single JSON array over 1 GB is too big; convert it to JSON Lines first."
                        .into(),
                );
            }
            match serde_json::from_str::<Value>(&text)
                .map_err(|e| format!("Not valid JSON: {e}"))?
            {
                Value::Array(items) => {
                    for item in items {
                        if !f(item) {
                            break;
                        }
                    }
                }
                other => {
                    f(other);
                }
            }
        }
        _ => {
            snap.for_each_line(ctl, |_, b| {
                let t = String::from_utf8_lossy(b);
                let t = t.trim();
                if t.is_empty() {
                    return true;
                }
                match serde_json::from_str::<Value>(t) {
                    Ok(v) => f(v),
                    Err(_) => {
                        *skipped += 1;
                        true
                    }
                }
            });
        }
    }
    Ok(())
}

fn json_to_delimited(
    snap: &Snapshot,
    from: Format,
    delim: u8,
    flatten: bool,
    out: &mut dyn Write,
    ctl: &JobCtl,
) -> Result<Stats, String> {
    // Pass 1: every column, in first-seen order.
    let mut columns: Vec<String> = Vec::new();
    let mut known: HashSet<String> = HashSet::new();
    let mut skipped = 0;
    for_each_record(snap, from, ctl, &mut skipped, |v| {
        for (k, _) in record_fields(&v, flatten).unwrap_or_default() {
            if known.insert(k.clone()) {
                columns.push(k);
            }
        }
        true
    })?;
    let d = (delim as char).to_string();
    let header: Vec<String> = columns
        .iter()
        .map(|c| csv::encode(c, delim, false))
        .collect();
    writeln!(out, "{}", header.join(&d)).map_err(io)?;
    let index: std::collections::HashMap<&str, usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i))
        .collect();

    // Pass 2: the rows.
    let mut stats = Stats {
        columns: columns.len(),
        ..Default::default()
    };
    let mut err = None;
    let mut skipped2 = 0;
    for_each_record(snap, from, ctl, &mut skipped2, |v| {
        let Some(fields) = record_fields(&v, flatten) else {
            stats.skipped += 1;
            return true;
        };
        let mut row = vec![String::new(); columns.len()];
        for (k, val) in fields {
            if let Some(&i) = index.get(k.as_str()) {
                row[i] = csv::encode(&cell_text(&val), delim, false);
            }
        }
        if let Err(e) = writeln!(out, "{}", row.join(&d)) {
            err = Some(io(e));
            return false;
        }
        stats.records += 1;
        true
    })?;
    stats.skipped += skipped;
    err.map_or(Ok(stats), Err)
}

fn json_to_json(
    snap: &Snapshot,
    from: Format,
    array: bool,
    out: &mut dyn Write,
    ctl: &JobCtl,
) -> Result<Stats, String> {
    let mut stats = Stats::default();
    if from == Format::JsonLines && array {
        // Keep each record's original text exactly; only check that it is JSON.
        let mut err = None;
        out.write_all(b"[\n").map_err(io)?;
        snap.for_each_line(ctl, |_, b| {
            let t = String::from_utf8_lossy(b);
            let t = t.trim();
            if t.is_empty() {
                return true;
            }
            if serde_json::from_str::<serde::de::IgnoredAny>(t).is_err() {
                stats.skipped += 1;
                return true;
            }
            let sep: &[u8] = if stats.records > 0 { b",\n" } else { b"" };
            if let Err(e) = out.write_all(sep).and_then(|_| out.write_all(t.as_bytes())) {
                err = Some(io(e));
                return false;
            }
            stats.records += 1;
            true
        });
        out.write_all(b"\n]\n").map_err(io)?;
        return err.map_or(Ok(stats), Err);
    }
    let mut skipped = 0;
    let mut err = None;
    if array {
        out.write_all(b"[\n").map_err(io)?;
    }
    for_each_record(snap, from, ctl, &mut skipped, |v| {
        let json = serde_json::to_string(&v).unwrap_or_default();
        let r = if array {
            let sep: &[u8] = if stats.records > 0 { b",\n" } else { b"" };
            out.write_all(sep)
                .and_then(|_| out.write_all(json.as_bytes()))
        } else {
            writeln!(out, "{json}")
        };
        if let Err(e) = r {
            err = Some(io(e));
            return false;
        }
        stats.records += 1;
        true
    })?;
    if array {
        out.write_all(b"\n]\n").map_err(io)?;
    }
    stats.skipped = skipped;
    err.map_or(Ok(stats), Err)
}

fn delimited_to_delimited(
    snap: &Snapshot,
    from: u8,
    to: u8,
    out: &mut dyn Write,
    ctl: &JobCtl,
) -> Result<Stats, String> {
    let mut stats = Stats::default();
    let mut err = None;
    let d = (to as char).to_string();
    snap.for_each_line(ctl, |_, bytes| {
        let line = String::from_utf8_lossy(bytes);
        if line.is_empty() {
            return true;
        }
        let row: Vec<String> = csv::field_spans(&line, from)
            .iter()
            .map(|&(a, b)| csv::encode(&csv::value(&line[a..b]), to, false))
            .collect();
        stats.columns = stats.columns.max(row.len());
        if let Err(e) = writeln!(out, "{}", row.join(&d)) {
            err = Some(io(e));
            return false;
        }
        stats.records += 1;
        true
    });
    err.map_or(Ok(stats), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn run(text: &str, spec: Spec) -> (String, Stats) {
        let lines = text.lines().map(str::to_string).collect();
        let doc = Document::from_lines("t".into(), lines);
        let snap = Snapshot::of(&doc);
        let ctl = JobCtl {
            cancel: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: 1,
        };
        let mut out = Vec::new();
        let stats = convert(&snap, &spec, &mut out, &ctl).unwrap();
        (String::from_utf8(out).unwrap(), stats)
    }

    fn spec(from: Format, to: Format) -> Spec {
        Spec {
            from,
            to,
            header: true,
            typed: true,
            flatten: true,
        }
    }

    #[test]
    fn json_numbers() {
        for ok in [
            "0",
            "-1",
            "12.50",
            "1e10",
            "-0.5E-3",
            "12345678901234567890",
        ] {
            assert!(is_json_number(ok), "{ok}");
        }
        for bad in ["007", "1.", ".5", "+1", "1e", "", "12a", "0x10"] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }

    #[test]
    fn csv_to_json_lines_keeps_values_exact() {
        let csv = "id,name,zip,price,ok,\n1,\"Doe, Jane\",00123,12.50,true,x\n2,Bob,,1e3,no,\n";
        let (out, stats) = run(csv, spec(Format::Delimited(b','), Format::JsonLines));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            r#"{"id":1,"name":"Doe, Jane","zip":"00123","price":12.50,"ok":true,"column_6":"x"}"#
        );
        assert_eq!(
            lines[1],
            r#"{"id":2,"name":"Bob","zip":"","price":1e3,"ok":"no","column_6":""}"#
        );
        assert_eq!(stats.records, 2);
        let (untyped, _) = run(
            csv,
            Spec {
                typed: false,
                ..spec(Format::Delimited(b','), Format::JsonLines)
            },
        );
        assert!(untyped.starts_with(r#"{"id":"1","#));
    }

    #[test]
    fn csv_to_json_array() {
        let (out, _) = run(
            "a,b\n1,2\n3,4\n",
            spec(Format::Delimited(b','), Format::JsonArray),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!([{"a":1,"b":2},{"a":3,"b":4}]));
    }

    #[test]
    fn json_lines_to_csv_unions_and_flattens() {
        let jsonl = "{\"id\":1,\"user\":{\"email\":\"a@x.io\"},\"tags\":[\"x\",\"y\"]}\nnot json\n{\"id\":2,\"note\":\"hi, there\"}\n";
        let (out, stats) = run(jsonl, spec(Format::JsonLines, Format::Delimited(b',')));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "id,user.email,tags,note");
        assert_eq!(lines[1], r#"1,a@x.io,"[""x"",""y""]","#);
        assert_eq!(lines[2], r#"2,,,"hi, there""#);
        assert_eq!(stats.records, 2);
        assert_eq!(stats.skipped, 1);
    }

    #[test]
    fn json_array_to_lines_and_tsv() {
        let (out, _) = run(
            "[\n {\"a\": 1},\n {\"a\": 2}\n]",
            spec(Format::JsonArray, Format::JsonLines),
        );
        assert_eq!(out, "{\"a\":1}\n{\"a\":2}\n");
        let (tsv, _) = run(
            "x,y\n\"a\tb\",c\n",
            spec(Format::Delimited(b','), Format::Delimited(b'\t')),
        );
        assert_eq!(tsv, "x\ty\n\"a\tb\"\tc\n");
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use crate::document::Document;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    #[test]
    fn json_lines_to_array_keeps_text() {
        let doc = Document::from_lines(
            "t".into(),
            vec![
                r#"{"n":1e3,"s":"x"}"#.into(),
                "junk".into(),
                r#"[1,2]"#.into(),
            ],
        );
        let snap = Snapshot::of(&doc);
        let ctl = JobCtl {
            cancel: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: 1,
        };
        let mut out = Vec::new();
        let spec = Spec {
            from: Format::JsonLines,
            to: Format::JsonArray,
            header: true,
            typed: true,
            flatten: true,
        };
        let stats = convert(&snap, &spec, &mut out, &ctl).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[\n{\"n\":1e3,\"s\":\"x\"},\n[1,2]\n]\n"
        );
        assert_eq!((stats.records, stats.skipped), (2, 1));
    }
}
