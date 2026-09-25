//! Built-in detector modules.
//!
//! Most detectors are just a regex plus a colour, so they share [`RegexDetector`].
//! Patterns use ASCII word boundaries `(?-u:\b)`, which keeps the regex engine on its
//! fast DFA path even on non-ASCII text (important for multi-GB scans).

use std::sync::Arc;

use egui::Color32;

use super::{Module, ModuleKind, email_providers};

/// Maps a group key to a coarser category (e.g. a domain to its provider).
pub type CategoryFn = fn(&str) -> Option<String>;

/// How a detector's hits are grouped in the breakdown view.
pub struct Grouping {
    /// e.g. "Domain".
    pub label: &'static str,
    /// Key for a hit (gets the `(?P<group>…)` capture, if the pattern has one).
    pub key: fn(&str, Option<&str>) -> Option<String>,
    /// Optional coarser grouping, e.g. ("Provider", domain → "Gmail").
    pub category: Option<(&'static str, CategoryFn)>,
}

pub struct RegexDetector {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub pattern: &'static str,
    pub color: Color32,
    pub replacement: &'static str,
    pub enabled: bool,
    pub validate: Option<fn(&str) -> bool>,
    pub group: Option<Grouping>,
}

impl Module for RegexDetector {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Detector
    }
    fn color(&self) -> Color32 {
        self.color
    }
    fn pattern(&self) -> Option<String> {
        Some(self.pattern.to_string())
    }
    fn validate(&self, found: &str) -> bool {
        self.validate.is_none_or(|f| f(found))
    }
    fn replacement(&self) -> Option<String> {
        Some(self.replacement.to_string())
    }
    fn default_enabled(&self) -> bool {
        self.enabled
    }
    fn group_label(&self) -> Option<String> {
        self.group.as_ref().map(|g| g.label.to_string())
    }
    fn group_key(&self, found: &str, captured: Option<&str>) -> Option<String> {
        self.group.as_ref().and_then(|g| (g.key)(found, captured))
    }
    fn category_label(&self) -> Option<String> {
        self.group
            .as_ref()?
            .category
            .map(|(label, _)| label.to_string())
    }
    fn category(&self, key: &str) -> Option<String> {
        self.group.as_ref()?.category.and_then(|(_, f)| f(key))
    }
    fn value_key(&self, found: &str) -> String {
        // Emails and UUIDs are case-insensitive; everything else is kept as written.
        if matches!(self.id, "email" | "uuid") {
            found.to_ascii_lowercase()
        } else {
            found.to_string()
        }
    }
}

pub fn all() -> Vec<Arc<dyn Module>> {
    vec![
        Arc::new(RegexDetector {
            id: "email",
            name: "Email addresses",
            description: "Finds things like jane.doe@example.com",
            pattern: r"(?i)(?-u:\b)[a-z0-9][a-z0-9._%+-]*@(?P<group>[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,24})(?-u:\b)",
            color: Color32::from_rgb(0x3d, 0xd6, 0x8c),
            replacement: "[EMAIL]",
            enabled: true,
            validate: Some(valid_email),
            group: Some(Grouping {
                label: "Domain",
                key: |_, domain| domain.map(str::to_ascii_lowercase),
                category: Some(("Provider", |d| {
                    Some(email_providers::provider(d).to_string())
                })),
            }),
        }),
        Arc::new(RegexDetector {
            id: "url",
            name: "URLs",
            description: "http://, https:// and ftp:// links",
            pattern: r#"(?i)(?-u:\b)(?:https?|ftp)://[^\s<>"'`{}|\\^\[\]]+[^\s<>"'`{}|\\^\[\].,;:!?)]"#,
            color: Color32::from_rgb(0x4f, 0x9d, 0xff),
            replacement: "[URL]",
            enabled: true,
            validate: None,
            group: Some(Grouping {
                label: "Site",
                key: |url, _| url_host(url),
                category: None,
            }),
        }),
        Arc::new(RegexDetector {
            id: "ipv4",
            name: "IPv4 addresses",
            description: "Dotted quads like 192.168.0.1 (octets validated)",
            pattern: r"(?-u:\b)(?:\d{1,3}\.){3}\d{1,3}(?-u:\b)",
            color: Color32::from_rgb(0xf5, 0xa5, 0x24),
            replacement: "[IP]",
            enabled: false,
            validate: Some(valid_ipv4),
            group: Some(Grouping {
                label: "Subnet (/24)",
                key: |ip, _| ip.rsplit_once('.').map(|(net, _)| format!("{net}.0/24")),
                category: Some(("Network", |subnet| Some(ip_network(subnet).to_string()))),
            }),
        }),
        Arc::new(RegexDetector {
            id: "uuid",
            name: "UUIDs / GUIDs",
            description: "8-4-4-4-12 hex identifiers",
            pattern: r"(?-u:\b)[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?-u:\b)",
            color: Color32::from_rgb(0xb3, 0x8b, 0xfa),
            replacement: "[UUID]",
            enabled: false,
            validate: None,
            group: Some(Grouping {
                label: "Version",
                key: |u, _| u.chars().nth(14).map(|v| format!("v{v}")),
                category: None,
            }),
        }),
        Arc::new(RegexDetector {
            id: "credit-card",
            name: "Credit card numbers",
            description: "13–19 digit card numbers that pass the Luhn checksum",
            pattern: r"(?-u:\b)\d(?:[ -]?\d){12,18}(?-u:\b)",
            color: Color32::from_rgb(0xff, 0x5c, 0x7a),
            replacement: "[CARD]",
            enabled: false,
            validate: Some(luhn),
            group: Some(Grouping {
                label: "Card brand",
                key: |c, _| Some(card_brand(c).to_string()),
                category: None,
            }),
        }),
        Arc::new(RegexDetector {
            id: "phone",
            name: "Phone numbers",
            description: "North-American style and +international numbers",
            pattern: r"(?:\+\d{1,3}[ .-]?)?(?:\(\d{3}\)|(?-u:\b)\d{3})[ .-]?\d{3}[ .-]?\d{4}(?-u:\b)",
            color: Color32::from_rgb(0x2e, 0xc4, 0xd6),
            replacement: "[PHONE]",
            enabled: false,
            validate: None,
            group: Some(Grouping {
                label: "Country code",
                key: |p, _| Some(phone_country(p)),
                category: None,
            }),
        }),
        Arc::new(RegexDetector {
            id: "log-level",
            name: "Log levels",
            description: "ERROR / WARN / INFO / DEBUG / TRACE / FATAL markers in logs",
            pattern: r"(?i)(?-u:\b)(?P<group>CRITICAL|FATAL|ERROR|WARNING|WARN|INFO|DEBUG|TRACE)(?-u:\b)",
            color: Color32::from_rgb(0xff, 0x6b, 0x6b),
            replacement: "[LEVEL]",
            enabled: false,
            validate: None,
            group: Some(Grouping {
                label: "Level",
                key: |_, level| level.map(normalize_level),
                category: Some(("Severity", |lvl| Some(severity(lvl).to_string()))),
            }),
        }),
        Arc::new(RegexDetector {
            id: "iso-date",
            name: "Dates & timestamps",
            description: "ISO-8601 dates such as 2024-05-01 or 2024-05-01T13:37:00Z",
            pattern: r"(?-u:\b)\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])(?:[T ]\d{2}:\d{2}(?::\d{2}(?:[.,]\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?)?(?-u:\b)",
            color: Color32::from_rgb(0xe0, 0xc0, 0x5a),
            replacement: "[DATE]",
            enabled: false,
            validate: None,
            group: Some(Grouping {
                label: "Month",
                key: |d, _| d.get(..7).map(str::to_string),
                category: Some(("Year", |m| m.get(..4).map(str::to_string))),
            }),
        }),
    ]
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn ip_network(subnet: &str) -> &'static str {
    let o: Vec<u8> = subnet
        .split('.')
        .take(2)
        .filter_map(|x| x.parse().ok())
        .collect();
    match o.as_slice() {
        [10, _] | [192, 168] => "Private",
        [172, b] if (16..=31).contains(b) => "Private",
        [127, _] => "Loopback",
        [169, 254] => "Link-local",
        [100, b] if (64..=127).contains(b) => "Carrier-grade NAT",
        [a, _] if *a >= 224 => "Multicast / reserved",
        _ => "Public",
    }
}

fn card_brand(card: &str) -> &'static str {
    let d: String = card.chars().filter(char::is_ascii_digit).collect();
    let n = |k: usize| d.get(..k).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
    if d.starts_with('4') {
        "Visa"
    } else if (51..=55).contains(&n(2)) || (2221..=2720).contains(&n(4)) {
        "Mastercard"
    } else if n(2) == 34 || n(2) == 37 {
        "American Express"
    } else if n(4) == 6011 || n(2) == 65 || (644..=649).contains(&n(3)) {
        "Discover"
    } else if (3528..=3589).contains(&n(4)) {
        "JCB"
    } else if n(2) == 36 || n(2) == 38 || (300..=305).contains(&n(3)) {
        "Diners Club"
    } else if n(2) == 62 {
        "UnionPay"
    } else {
        "Other"
    }
}

fn phone_country(p: &str) -> String {
    match p.strip_prefix('+') {
        Some(rest) => format!(
            "+{}",
            rest.chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect::<String>()
        ),
        None => "No country code".into(),
    }
}

fn normalize_level(s: &str) -> String {
    match s.to_ascii_uppercase().as_str() {
        "WARNING" => "WARN".to_string(),
        "CRITICAL" => "FATAL".to_string(),
        other => other.to_string(),
    }
}

fn severity(level: &str) -> &'static str {
    match level {
        "FATAL" | "ERROR" => "Error",
        "WARN" => "Warning",
        "INFO" => "Info",
        "DEBUG" | "TRACE" => "Debug",
        _ => "Other",
    }
}

fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !local.contains("..")
        && !domain.contains("..")
        && !domain.starts_with('-')
        && !local.ends_with('.')
}

fn valid_ipv4(s: &str) -> bool {
    s.split('.')
        .all(|o| o.parse::<u16>().is_ok_and(|v| v <= 255) && !(o.len() > 1 && o.starts_with('0')))
}

fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                if d * 2 > 9 { d * 2 - 9 } else { d * 2 }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(id: &str, text: &str) -> Vec<String> {
        let m = all().into_iter().find(|m| m.id() == id).unwrap();
        let re = regex::Regex::new(&m.pattern().unwrap()).unwrap();
        re.find_iter(text)
            .filter(|x| m.validate(x.as_str()))
            .map(|x| x.as_str().to_string())
            .collect()
    }

    #[test]
    fn detects_emails() {
        assert_eq!(
            hits(
                "email",
                "mail jane.doe+tag@mail.example.co.uk, or bob@x.io."
            ),
            vec!["jane.doe+tag@mail.example.co.uk", "bob@x.io"]
        );
        assert!(hits("email", "not an email: foo@bar").is_empty());
    }

    #[test]
    fn validates_ips_and_cards() {
        assert_eq!(hits("ipv4", "a 10.0.0.1 b 999.1.1.1"), vec!["10.0.0.1"]);
        assert_eq!(
            hits("credit-card", "card 4111 1111 1111 1111 x"),
            vec!["4111 1111 1111 1111"]
        );
        assert!(hits("credit-card", "1234 5678 9012 3456").is_empty());
    }

    #[test]
    fn detects_log_levels_and_normalizes_them() {
        assert_eq!(
            hits("log-level", "2024-01-01 ERROR boom, then WARNING low disk"),
            vec!["ERROR", "WARNING"]
        );
        assert!(hits("log-level", "no errors here, just words").is_empty());
        let m = all().into_iter().find(|m| m.id() == "log-level").unwrap();
        assert_eq!(m.group_key("WARNING", Some("WARNING")).as_deref(), Some("WARN"));
        assert_eq!(m.category("WARN").as_deref(), Some("Warning"));
        assert_eq!(m.category("FATAL").as_deref(), Some("Error"));
    }

    #[test]
    fn detects_urls() {
        assert_eq!(
            hits("url", "see (https://example.com/a?b=c)."),
            vec!["https://example.com/a?b=c"]
        );
    }
}
