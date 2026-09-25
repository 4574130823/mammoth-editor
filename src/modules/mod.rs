//! Mammoth's module (plugin) system.
//!
//! A module is anything implementing [`Module`]. There are two flavours:
//!
//! * **Detectors** find things in text (emails, URLs, IPs, secrets…). They supply a
//!   regex via [`Module::pattern`] and optionally a [`Module::validate`] check. The
//!   editor then highlights every hit, and the find bar can use the detector as its
//!   search term, which gives you Find Next / Count / Extract / Replace All for it.
//! * **Syntax** modules colour a line ([`Module::highlight`]), e.g. rainbow CSV
//!   columns or JSON tokens. One of them is active per tab, picked by file extension.
//!
//! To add a module in Rust, implement the trait and register it in [`builtin`].
//! To add a detector without recompiling, drop a `.toml` file in a `modules` folder
//! (see `user.rs` and the README).

mod detectors;
mod email_providers;
mod snowflake;
mod syntax;
mod user;

use std::path::PathBuf;
use std::sync::Arc;

use egui::Color32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    Detector,
    Syntax,
}

/// A coloured byte range within one line.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub color: Color32,
}

pub trait Module: Send + Sync {
    /// Stable identifier (used to remember on/off state).
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn description(&self) -> &str {
        ""
    }
    fn kind(&self) -> ModuleKind;
    /// Highlight colour for detectors.
    fn color(&self) -> Color32 {
        Color32::from_rgb(0x4f, 0x8c, 0xff)
    }

    // ---- Detectors ----

    /// Regex (Rust `regex` syntax) that finds candidate matches.
    fn pattern(&self) -> Option<String> {
        None
    }
    /// Extra check applied to every regex hit (e.g. a Luhn checksum).
    fn validate(&self, _found: &str) -> bool {
        true
    }
    /// Suggested replacement when masking/redacting hits, e.g. `[EMAIL]`.
    fn replacement(&self) -> Option<String> {
        None
    }
    /// Whether the detector is on by default.
    fn default_enabled(&self) -> bool {
        false
    }

    // ---- Breakdowns (optional) ----
    //
    // A detector can group its hits, e.g. emails by domain, for the breakdown view.
    // By default the key is whatever a `(?P<group>…)` capture in the pattern matched.

    /// What a group is called, e.g. "Domain". `None` = no breakdown (unless the
    /// pattern has a `group` capture, which is then labelled "Group").
    fn group_label(&self) -> Option<String> {
        None
    }
    /// The group a hit belongs to. `captured` is the `(?P<group>…)` text, if any.
    fn group_key(&self, _found: &str, captured: Option<&str>) -> Option<String> {
        captured.map(str::to_string)
    }
    /// Optional coarser grouping of keys, e.g. "Provider" (gmail.com → Gmail).
    fn category_label(&self) -> Option<String> {
        None
    }
    fn category(&self, _key: &str) -> Option<String> {
        None
    }
    /// How a full match is counted in "top values", e.g. lowercased emails so
    /// `Jane@Gmail.com` and `jane@gmail.com` are the same address.
    fn value_key(&self, found: &str) -> String {
        found.to_string()
    }

    // ---- Syntax ----

    /// Colour a single line. Spans must be sorted and non-overlapping.
    fn highlight(&self, _line: &str, _out: &mut Vec<Span>) {}
    /// Should this syntax be picked automatically for a file with this extension?
    fn auto_enable(&self, _ext: &str) -> bool {
        false
    }
}

pub struct ModuleEntry {
    pub module: Arc<dyn Module>,
    pub enabled: bool,
    /// Compiled pattern for detectors.
    pub regex: Option<regex::Regex>,
    pub error: Option<String>,
    /// Loaded from a user TOML file (path shown in the UI).
    pub origin: Option<PathBuf>,
}

impl ModuleEntry {
    fn new(module: Arc<dyn Module>, origin: Option<PathBuf>) -> Self {
        let (regex, error) = match module.pattern() {
            Some(p) => match regex::RegexBuilder::new(&p).size_limit(64 << 20).build() {
                Ok(r) => (Some(r), None),
                Err(e) => (None, Some(e.to_string())),
            },
            None => (None, None),
        };
        let enabled = module.default_enabled() && error.is_none();
        Self {
            module,
            enabled,
            regex,
            error,
            origin,
        }
    }

    #[cfg(test)]
    pub fn for_test(module: Arc<dyn Module>) -> Self {
        Self::new(module, None)
    }

    pub fn is_detector(&self) -> bool {
        self.module.kind() == ModuleKind::Detector
    }

    /// A cheap, thread-safe handle for computing breakdown keys, if supported.
    pub fn grouper(&self) -> Option<Grouper> {
        let regex = self.regex.clone()?;
        let has_capture = regex.capture_names().any(|n| n == Some("group"));
        let label = self
            .module
            .group_label()
            .or_else(|| has_capture.then(|| "Group".to_string()))?;
        Some(Grouper {
            regex,
            module: self.module.clone(),
            label,
            category_label: self.module.category_label(),
        })
    }

    /// Run the detector over `text`, pushing byte ranges of accepted hits.
    pub fn detect(&self, text: &str, out: &mut Vec<(usize, usize)>) {
        if let Some(re) = &self.regex {
            for m in re.find_iter(text) {
                if !m.is_empty() && self.module.validate(m.as_str()) {
                    out.push((m.start(), m.end()));
                }
            }
        }
    }
}

/// Computes the breakdown key (and optional category) of a detector hit.
#[derive(Clone)]
pub struct Grouper {
    regex: regex::Regex,
    module: Arc<dyn Module>,
    pub label: String,
    pub category_label: Option<String>,
}

impl Grouper {
    pub fn key(&self, found: &str) -> Option<String> {
        let captured = self
            .regex
            .captures(found)
            .and_then(|c| c.name("group"))
            .map(|m| m.as_str());
        self.module.group_key(found, captured)
    }

    pub fn category(&self, key: &str) -> Option<String> {
        self.module.category(key)
    }

    pub fn value(&self, found: &str) -> String {
        self.module.value_key(found)
    }
}

pub struct Registry {
    pub entries: Vec<ModuleEntry>,
    pub load_errors: Vec<String>,
}

impl Registry {
    pub fn load() -> Self {
        let mut entries: Vec<ModuleEntry> = builtin()
            .into_iter()
            .map(|m| ModuleEntry::new(m, None))
            .collect();
        let (user, load_errors) = user::load_all();
        for (m, path) in user {
            // A user module with the same id overrides a built-in one.
            entries.retain(|e| e.module.id() != m.id());
            entries.push(ModuleEntry::new(m, Some(path)));
        }
        Self {
            entries,
            load_errors,
        }
    }

    /// Reload (keeps on/off state of modules that still exist).
    pub fn reload(&mut self) {
        let states: Vec<(String, bool)> = self
            .entries
            .iter()
            .map(|e| (e.module.id().to_string(), e.enabled))
            .collect();
        *self = Self::load();
        self.apply_states(&states);
    }

    pub fn apply_states(&mut self, states: &[(String, bool)]) {
        for (id, on) in states {
            if let Some(e) = self.entries.iter_mut().find(|e| e.module.id() == id) {
                e.enabled = *on && e.error.is_none();
            }
        }
    }

    pub fn states(&self) -> Vec<(String, bool)> {
        self.entries
            .iter()
            .map(|e| (e.module.id().to_string(), e.enabled))
            .collect()
    }

    pub fn detectors(&self) -> impl Iterator<Item = (usize, &ModuleEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_detector())
    }

    pub fn syntaxes(&self) -> impl Iterator<Item = (usize, &ModuleEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.module.kind() == ModuleKind::Syntax)
    }

    pub fn syntax_for_extension(&self, ext: &str) -> Option<usize> {
        let ext = ext.to_ascii_lowercase();
        self.syntaxes()
            .find(|(_, e)| e.module.auto_enable(&ext))
            .map(|(i, _)| i)
    }

    pub fn by_id(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.module.id() == id)
    }
}

/// All modules compiled into Mammoth. Add yours here.
pub fn builtin() -> Vec<Arc<dyn Module>> {
    let mut v: Vec<Arc<dyn Module>> = Vec::new();
    v.extend(detectors::all());
    v.extend(snowflake::all());
    v.extend(syntax::all());
    v
}

/// Folders scanned for user `.toml` modules.
pub fn module_dirs() -> Vec<PathBuf> {
    user::module_dirs()
}

pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        3 => {
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            Some(Color32::from_rgb(
                (r * 17) as u8,
                (g * 17) as u8,
                (b * 17) as u8,
            ))
        }
        _ => None,
    }
}
