//! JSON helpers: find JSON inside a line (e.g. after a log prefix), pretty-print it,
//! and run jq programs on it (via `jaq`, a jq implementation in Rust).

use serde_json::Value;

/// Find a JSON object or array in `text`: the whole line, or embedded after a
/// prefix like `2024-01-01 INFO {"user":…}`. Returns (start, end, value).
pub fn find_json(text: &str) -> Option<(usize, usize, Value)> {
    for (i, _) in text.match_indices(['{', '[']).take(24) {
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(v)) = stream.next()
            && (v.is_object() || v.is_array())
        {
            return Some((i, i + stream.byte_offset(), v));
        }
    }
    None
}

/// Parse a whole document as one JSON value.
pub fn parse_document(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

pub fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

pub fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// A compiled jq program.
pub struct Jq {
    filter: jaq_core::compile::Filter<jaq_core::Native<jaq_core::data::JustLut<jaq_json::Val>>>,
}

impl Jq {
    pub fn compile(code: &str) -> Result<Jq, String> {
        use jaq_core::load::{Arena, File, Loader};
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs());
        let loader = Loader::new(defs);
        let arena = Arena::default();
        let modules = loader
            .load(&arena, File { code, path: () })
            .map_err(|errs| {
                format!(
                    "Syntax error in jq program ({} problem{})",
                    errs.len(),
                    if errs.len() == 1 { "" } else { "s" }
                )
            })?;
        let filter = jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errs| {
                let names: Vec<String> = errs
                    .iter()
                    .flat_map(|(_, es)| es.iter().map(|(name, _)| name.to_string()))
                    .take(3)
                    .collect();
                format!("Unknown name in jq program: {}", names.join(", "))
            })?;
        Ok(Jq { filter })
    }

    /// Run on one JSON text. Each output is returned as compact JSON.
    pub fn run(&self, json: &str, limit: usize) -> Result<Vec<String>, String> {
        use jaq_core::{Ctx, Vars, unwrap_valr};
        let input =
            jaq_json::read::parse_single(json.as_bytes()).map_err(|e| format!("Not JSON: {e}"))?;
        let ctx =
            Ctx::<jaq_core::data::JustLut<jaq_json::Val>>::new(&self.filter.lut, Vars::new([]));
        let mut out = Vec::new();
        for r in self
            .filter
            .id
            .run((ctx, input))
            .map(unwrap_valr)
            .take(limit)
        {
            match r {
                Ok(v) => out.push(v.to_string()),
                Err(e) => return Err(format!("jq error: {e}")),
            }
        }
        Ok(out)
    }

    /// Does the program produce at least one value other than `false`/`null`?
    /// (What `select(...)` style filters mean for "keep matching lines".)
    pub fn matches(&self, json: &str) -> bool {
        self.run(json, 1)
            .is_ok_and(|o| o.first().is_some_and(|v| v != "false" && v != "null"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_json_after_log_prefix() {
        let line = r#"2026-09-25T10:00:00Z [ERROR] payment failed {"user":{"email":"a@b.co"},"amount":12.5} (retrying)"#;
        let (s, e, v) = find_json(line).unwrap();
        assert_eq!(&line[s..e], r#"{"user":{"email":"a@b.co"},"amount":12.5}"#);
        assert_eq!(v["user"]["email"], "a@b.co");
        assert!(find_json("[INFO ] no json here").is_none());
        assert!(find_json("[1, 2, 3]").is_some());
    }

    #[test]
    fn keeps_key_order_and_big_numbers() {
        let v = parse_document(r#"{"z":1,"a":12345678901234567890123,"m":0.10}"#).unwrap();
        assert_eq!(
            compact(&v),
            r#"{"z":1,"a":12345678901234567890123,"m":0.10}"#
        );
    }

    #[test]
    fn runs_jq() {
        let jq = Jq::compile(".items[] | select(.ok) | .name").unwrap();
        let out = jq.run(r#"{"items":[{"name":"a","ok":true},{"name":"b","ok":false},{"name":"c","ok":true}]}"#, 100).unwrap();
        assert_eq!(out, vec![r#""a""#, r#""c""#]);
        assert!(Jq::compile(".foo | ").is_err());
        assert!(Jq::compile("nope_not_a_function").is_err());
        let sel = Jq::compile("select(.status >= 500)").unwrap();
        assert!(sel.matches(r#"{"status":503}"#));
        assert!(!sel.matches(r#"{"status":200}"#));
        assert_eq!(
            Jq::compile("keys")
                .unwrap()
                .run(r#"{"b":1,"a":2}"#, 10)
                .unwrap(),
            vec![r#"["a","b"]"#]
        );
    }
}
