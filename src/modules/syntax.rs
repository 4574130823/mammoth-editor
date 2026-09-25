//! Built-in syntax colouring modules: rainbow CSV/TSV columns, JSON and log files.
//! All of them work line-by-line so they stay O(visible lines) on huge files.

use std::sync::Arc;

use egui::Color32;

use super::{Module, ModuleKind, Span};

pub fn all() -> Vec<Arc<dyn Module>> {
    vec![
        Arc::new(Delimited {
            id: "csv",
            name: "CSV (rainbow columns)",
            delim: b',',
            exts: &["csv"],
        }),
        Arc::new(Delimited {
            id: "tsv",
            name: "TSV (rainbow columns)",
            delim: b'\t',
            exts: &["tsv", "tab"],
        }),
        Arc::new(Delimited {
            id: "psv",
            name: "Pipe-separated (rainbow columns)",
            delim: b'|',
            exts: &["psv"],
        }),
        Arc::new(Json),
        Arc::new(Log),
    ]
}

const RAINBOW: [Color32; 8] = [
    Color32::from_rgb(0xe6, 0xe6, 0xe6),
    Color32::from_rgb(0x6c, 0xb6, 0xff),
    Color32::from_rgb(0x7e, 0xe0, 0x9a),
    Color32::from_rgb(0xff, 0xc6, 0x6d),
    Color32::from_rgb(0xd7, 0x9b, 0xff),
    Color32::from_rgb(0x5e, 0xe0, 0xd8),
    Color32::from_rgb(0xff, 0x8f, 0x8f),
    Color32::from_rgb(0xc8, 0xd8, 0x6a),
];
const PUNCT: Color32 = Color32::from_rgb(0x6b, 0x72, 0x80);

struct Delimited {
    id: &'static str,
    name: &'static str,
    delim: u8,
    exts: &'static [&'static str],
}

impl Module for Delimited {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Colours each column differently (quote-aware)."
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Syntax
    }
    fn auto_enable(&self, ext: &str) -> bool {
        self.exts.contains(&ext)
    }
    fn highlight(&self, line: &str, out: &mut Vec<Span>) {
        let b = line.as_bytes();
        let mut col = 0usize;
        let mut start = 0usize;
        let mut in_quotes = false;
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if c == b'"' {
                in_quotes = !in_quotes;
            } else if c == self.delim && !in_quotes {
                if i > start {
                    out.push(Span {
                        start,
                        end: i,
                        color: RAINBOW[col % RAINBOW.len()],
                    });
                }
                out.push(Span {
                    start: i,
                    end: i + 1,
                    color: PUNCT,
                });
                col += 1;
                start = i + 1;
            }
            i += 1;
        }
        if b.len() > start {
            out.push(Span {
                start,
                end: b.len(),
                color: RAINBOW[col % RAINBOW.len()],
            });
        }
    }
}

struct Json;

const J_KEY: Color32 = Color32::from_rgb(0x7a, 0xb8, 0xff);
const J_STR: Color32 = Color32::from_rgb(0x98, 0xd4, 0x8a);
const J_NUM: Color32 = Color32::from_rgb(0xf0, 0xa4, 0x6c);
const J_LIT: Color32 = Color32::from_rgb(0xd0, 0x8b, 0xf5);
const J_PUNCT: Color32 = Color32::from_rgb(0x8a, 0x93, 0xa3);

impl Module for Json {
    fn id(&self) -> &str {
        "json"
    }
    fn name(&self) -> &str {
        "JSON / JSON Lines"
    }
    fn description(&self) -> &str {
        "Keys, strings, numbers and literals."
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Syntax
    }
    fn auto_enable(&self, ext: &str) -> bool {
        matches!(
            ext,
            "json" | "jsonl" | "ndjson" | "geojson" | "jsonc" | "har"
        )
    }
    fn highlight(&self, line: &str, out: &mut Vec<Span>) {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            match c {
                b'"' => {
                    let start = i;
                    i += 1;
                    while i < b.len() {
                        match b[i] {
                            b'\\' => i += 2,
                            b'"' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                    let end = i.min(b.len());
                    // A string followed by ':' is an object key.
                    let mut k = end;
                    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                        k += 1;
                    }
                    let color = if k < b.len() && b[k] == b':' {
                        J_KEY
                    } else {
                        J_STR
                    };
                    out.push(Span { start, end, color });
                }
                b'-' | b'0'..=b'9' => {
                    let start = i;
                    i += 1;
                    while i < b.len()
                        && matches!(b[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                    {
                        i += 1;
                    }
                    out.push(Span {
                        start,
                        end: i,
                        color: J_NUM,
                    });
                }
                b't' | b'f' | b'n' => {
                    let rest = &b[i..];
                    let lit = [&b"true"[..], b"false", b"null"]
                        .into_iter()
                        .find(|w| rest.starts_with(w));
                    if let Some(w) = lit {
                        out.push(Span {
                            start: i,
                            end: i + w.len(),
                            color: J_LIT,
                        });
                        i += w.len();
                    } else {
                        i += 1;
                    }
                }
                b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                    out.push(Span {
                        start: i,
                        end: i + 1,
                        color: J_PUNCT,
                    });
                    i += 1;
                }
                _ => i += 1,
            }
        }
        // Guard against slicing inside a multi-byte char (escaped bytes at the end).
        out.retain(|s| {
            line.is_char_boundary(s.start) && line.is_char_boundary(s.end.min(line.len()))
        });
    }
}

struct Log;

impl Module for Log {
    fn id(&self) -> &str {
        "log"
    }
    fn name(&self) -> &str {
        "Log files"
    }
    fn description(&self) -> &str {
        "Severity levels, timestamps, [brackets] and quoted text."
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Syntax
    }
    fn auto_enable(&self, ext: &str) -> bool {
        matches!(ext, "log" | "out" | "err" | "trace")
    }
    fn highlight(&self, line: &str, out: &mut Vec<Span>) {
        use std::sync::OnceLock;
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            regex::Regex::new(concat!(
                r"(?P<err>(?:\[[ \t]*)?(?-u:\b)(?:FATAL|CRITICAL|CRIT|ERROR|ERR|SEVERE|PANIC|EXCEPTION|Exception|Error)(?-u:\b)(?:[ \t]*\])?)",
                r"|(?P<warn>(?:\[[ \t]*)?(?-u:\b)(?:WARNING|WARN|Warning)(?-u:\b)(?:[ \t]*\])?)",
                r"|(?P<info>(?:\[[ \t]*)?(?-u:\b)(?:INFO|NOTICE|Info)(?-u:\b)(?:[ \t]*\])?)",
                r"|(?P<debug>(?:\[[ \t]*)?(?-u:\b)(?:DEBUG|TRACE|VERBOSE|Debug)(?-u:\b)(?:[ \t]*\])?)",
                r"|(?P<time>\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?|(?-u:\b)\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?-u:\b))",
                r"|(?P<br>\[[^\]\n]{1,64}\])",
                r#"|(?P<str>"[^"\n]{0,512}")"#,
            ))
            .unwrap()
        });
        for caps in re.captures_iter(line) {
            let (m, color) = if let Some(m) = caps.name("err") {
                (m, Color32::from_rgb(0xff, 0x6b, 0x6b))
            } else if let Some(m) = caps.name("warn") {
                (m, Color32::from_rgb(0xff, 0xc8, 0x57))
            } else if let Some(m) = caps.name("info") {
                (m, Color32::from_rgb(0x4f, 0xb4, 0xff))
            } else if let Some(m) = caps.name("debug") {
                (m, Color32::from_rgb(0x6f, 0x78, 0x88))
            } else if let Some(m) = caps.name("time") {
                (m, Color32::from_rgb(0x9a, 0xa6, 0xd8))
            } else if let Some(m) = caps.name("br") {
                (m, Color32::from_rgb(0xc3, 0x9a, 0xf0))
            } else if let Some(m) = caps.name("str") {
                (m, Color32::from_rgb(0x98, 0xd4, 0x8a))
            } else {
                continue;
            };
            out.push(Span {
                start: m.start(),
                end: m.end(),
                color,
            });
        }
    }
}
