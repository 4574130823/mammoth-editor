//! Detects base64 text that decodes to a Discord-style snowflake ID (a 64-bit
//! integer whose top 42 bits are milliseconds since 2015-01-01T00:00:00Z) and shows
//! the date it encodes via the breakdown view. IDs whose date falls outside
//! 2015-2099 are treated as false positives and rejected, not just hidden.

use std::sync::Arc;

use egui::Color32;

use super::{Module, ModuleKind};

const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000; // 2015-01-01T00:00:00Z
const MIN_YEAR: i64 = 2015;
const MAX_YEAR: i64 = 2099;

pub struct Snowflake;

impl Module for Snowflake {
    fn id(&self) -> &str {
        "b64-snowflake"
    }
    fn name(&self) -> &str {
        "Base64 Snowflake IDs"
    }
    fn description(&self) -> &str {
        "Base64 text that decodes to a Discord-style snowflake ID; shows the date it encodes (2015-2099)"
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Detector
    }
    fn color(&self) -> Color32 {
        Color32::from_rgb(0xd6, 0x7a, 0xe0)
    }
    fn pattern(&self) -> Option<String> {
        Some(r"(?-u:\b)[A-Za-z0-9+/_-]{20,28}(?-u:\b)={0,2}".to_string())
    }
    fn validate(&self, found: &str) -> bool {
        decoded_date(found).is_some()
    }
    fn replacement(&self) -> Option<String> {
        Some("[SNOWFLAKE]".to_string())
    }
    fn group_label(&self) -> Option<String> {
        Some("Decoded date".to_string())
    }
    fn group_key(&self, found: &str, _captured: Option<&str>) -> Option<String> {
        decoded_date(found).map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
    }
    fn category_label(&self) -> Option<String> {
        Some("Year".to_string())
    }
    fn category(&self, key: &str) -> Option<String> {
        key.get(..4).map(str::to_string)
    }
}

pub fn all() -> Vec<Arc<dyn Module>> {
    vec![Arc::new(Snowflake)]
}

/// Decodes `found` as base64, reads the bytes as an ASCII decimal snowflake ID, and
/// returns its embedded (year, month, day) if it falls within [`MIN_YEAR`, `MAX_YEAR`].
/// Requiring the decoded bytes to be a plausible-length all-digit string (rather than
/// accepting any 8 decoded bytes as a raw integer) is what keeps this from lighting up
/// on ordinary base64 blobs: a random byte string decoding to pure ASCII digits is rare.
fn decoded_date(found: &str) -> Option<(i64, u32, u32)> {
    let bytes = decode_base64(found)?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if !(15..=20).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id: u64 = text.parse().ok()?;
    let ms = DISCORD_EPOCH_MS + (id >> 22) as i64;
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    (MIN_YEAR..=MAX_YEAR).contains(&y).then_some((y, m, d))
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// A permissive base64 decoder: accepts both the standard and URL-safe alphabets and
/// either padded or unpadded input, since a match in the wild could be either.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let mut vals = [0u8; 4];
        for (v, &c) in vals.iter_mut().zip(chunk) {
            *v = b64_val(c)?;
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if chunk.len() > 2 {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if chunk.len() > 3 {
            out.push((vals[2] << 6) | vals[3]);
        }
    }
    Some(out)
}

/// Days since the Unix epoch to a civil (year, month, day).
/// Howard Hinnant's `civil_from_days`: <http://howardhinnant.github.io/date_algorithms.html>.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64_std(bytes: &[u8]) -> String {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            out.push(ALPHA[(b[0] >> 2) as usize] as char);
            out.push(ALPHA[(((b[0] & 0x3) << 4) | (b[1] >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 {
                ALPHA[(((b[1] & 0xf) << 2) | (b[2] >> 6)) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHA[(b[2] & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    fn hits(text: &str) -> Vec<String> {
        let m = Snowflake;
        let re = regex::Regex::new(&m.pattern().unwrap()).unwrap();
        re.find_iter(text)
            .filter(|x| m.validate(x.as_str()))
            .map(|x| x.as_str().to_string())
            .collect()
    }

    #[test]
    fn decodes_a_real_looking_snowflake() {
        // Discord's own docs example ID, minted 2016-04-30.
        let token = b64_std(b"175928847299117063");
        assert_eq!(decoded_date(&token), Some((2016, 4, 30)));
    }

    #[test]
    fn finds_the_token_in_surrounding_text() {
        let token = b64_std(b"175928847299117063");
        assert_eq!(
            hits(&format!("cursor={token}&limit=50")),
            vec![token.clone()]
        );
        let m = Snowflake;
        assert_eq!(
            m.group_key(&token, None).as_deref(),
            Some("2016-04-30")
        );
        assert_eq!(m.category("2016-04-30").as_deref(), Some("2016"));
    }

    #[test]
    fn rejects_dates_past_2099() {
        // u64::MAX >> 22 is a ~139-year ms offset from the 2015 epoch, past 2099.
        let token = b64_std(b"18446744073709551615");
        assert!(decoded_date(&token).is_none());
    }

    #[test]
    fn rejects_non_base64_and_non_numeric_payloads() {
        assert!(decoded_date("not-a-real-base64-string!!").is_none());
        // Valid base64, but decodes to text, not a decimal snowflake.
        assert!(decoded_date(&b64_std(b"hello world this is text")).is_none());
    }
}
