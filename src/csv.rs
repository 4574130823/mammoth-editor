//! Minimal, lossless CSV field handling for the table view.
//!
//! Records are single lines. Editing a cell rewrites only that field's bytes, so
//! every other value keeps its exact original text: no reformatted dates, no
//! dropped leading zeros, no scientific notation.

use std::borrow::Cow;

/// Delimiter for a delimited syntax module id.
pub fn delimiter_for(syntax: Option<&str>) -> Option<u8> {
    match syntax? {
        "csv" => Some(b','),
        "tsv" => Some(b'\t'),
        "psv" => Some(b'|'),
        _ => None,
    }
}

/// Byte ranges of each field (quotes included), split on `delim` outside quotes.
pub fn field_spans(line: &str, delim: u8) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, &c) in line.as_bytes().iter().enumerate() {
        if c == b'"' {
            quoted = !quoted;
        } else if c == delim && !quoted {
            out.push((start, i));
            start = i + 1;
        }
    }
    out.push((start, line.len()));
    out
}

/// The value of a raw field: surrounding quotes removed, `""` unescaped.
pub fn value(raw: &str) -> Cow<'_, str> {
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        Cow::Owned(raw[1..raw.len() - 1].replace("\"\"", "\""))
    } else {
        Cow::Borrowed(raw)
    }
}

/// Field `col` of `line`, unquoted ("" if the row is shorter).
pub fn field(line: &str, delim: u8, col: usize) -> Cow<'_, str> {
    match field_spans(line, delim).get(col) {
        Some(&(a, b)) => value(&line[a..b]),
        None => Cow::Borrowed(""),
    }
}

/// Encode a value for a field, quoting only when needed (or when it was quoted).
pub fn encode(value: &str, delim: u8, keep_quotes: bool) -> String {
    let needs = keep_quotes || value.contains(delim as char) || value.contains(['"', '\n', '\r']);
    if needs {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// `line` with field `col` set to `value`; other bytes are untouched. Short rows
/// are padded with empty fields.
pub fn set_field(line: &str, delim: u8, col: usize, value: &str) -> String {
    let spans = field_spans(line, delim);
    match spans.get(col) {
        Some(&(a, b)) => {
            let was_quoted = line[a..b].starts_with('"');
            format!(
                "{}{}{}",
                &line[..a],
                encode(value, delim, was_quoted),
                &line[b..]
            )
        }
        None => {
            let mut s = line.to_string();
            for _ in spans.len()..=col {
                s.push(delim as char);
            }
            s.push_str(&encode(value, delim, false));
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_unquotes() {
        let line = r#"1,"Doe, Jane","say ""hi""",007"#;
        let spans = field_spans(line, b',');
        assert_eq!(spans.len(), 4);
        assert_eq!(field(line, b',', 1), "Doe, Jane");
        assert_eq!(field(line, b',', 2), r#"say "hi""#);
        assert_eq!(field(line, b',', 3), "007");
        assert_eq!(field(line, b',', 9), "");
    }

    #[test]
    fn edits_only_the_target_field() {
        let line = r#"00123,"Doe, Jane",2024-01-02,1e10"#;
        assert_eq!(
            set_field(line, b',', 1, "Roe, Rick"),
            r#"00123,"Roe, Rick",2024-01-02,1e10"#
        );
        assert_eq!(
            set_field(line, b',', 2, "tomorrow"),
            r#"00123,"Doe, Jane",tomorrow,1e10"#
        );
        assert_eq!(
            set_field(line, b',', 3, r#"a"b"#),
            r#"00123,"Doe, Jane",2024-01-02,"a""b""#
        );
        assert_eq!(set_field("a,b", b',', 3, "d"), "a,b,,d");
        assert_eq!(set_field("a\tb", b'\t', 0, "x,y"), "x,y\tb");
    }
}
