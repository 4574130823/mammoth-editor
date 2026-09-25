//! Detector modules defined in `.toml` files, so anyone can add one without Rust.
//!
//! ```toml
//! name        = "JWT tokens"
//! description = "JSON Web Tokens"
//! pattern     = '''\beyJ[\w-]+\.eyJ[\w-]+\.[\w-]+'''
//! color       = "#e5c07b"
//! replacement = "[JWT]"   # optional, used by "Mask all"
//! enabled     = true      # optional, on by default
//! group_label = "Kind"    # optional: name of a (?P<group>…) capture for breakdowns
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::Color32;
use serde::Deserialize;

use super::{Module, ModuleKind, parse_hex_color};

#[derive(Deserialize)]
struct UserModuleFile {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    description: String,
    pattern: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    replacement: Option<String>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    case_insensitive: bool,
    /// Name for the `(?P<group>…)` capture in breakdowns, e.g. "Status code".
    #[serde(default)]
    group_label: Option<String>,
}

fn yes() -> bool {
    true
}

struct UserDetector {
    id: String,
    name: String,
    description: String,
    pattern: String,
    color: Color32,
    replacement: Option<String>,
    enabled: bool,
    group_label: Option<String>,
}

impl Module for UserDetector {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn kind(&self) -> ModuleKind {
        ModuleKind::Detector
    }
    fn color(&self) -> Color32 {
        self.color
    }
    fn pattern(&self) -> Option<String> {
        Some(self.pattern.clone())
    }
    fn replacement(&self) -> Option<String> {
        self.replacement.clone()
    }
    fn default_enabled(&self) -> bool {
        self.enabled
    }
    fn group_label(&self) -> Option<String> {
        self.group_label.clone()
    }
}

pub fn module_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        dirs.push(d.join("modules"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("modules"));
    }
    if let Some(cfg) = config_dir() {
        dirs.push(cfg.join("modules"));
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let key = std::fs::canonicalize(d).unwrap_or_else(|_| d.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    dirs
}

pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("Mammoth"))
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(x).join("mammoth"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("mammoth"))
    }
}

/// Loaded modules with their source file, plus human-readable load errors.
pub type Loaded = (Vec<(Arc<dyn Module>, PathBuf)>, Vec<String>);

pub fn load_all() -> Loaded {
    let mut modules = Vec::new();
    let mut errors = Vec::new();
    for dir in module_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            })
            .collect();
        files.sort();
        for path in files {
            match load_one(&path) {
                Ok(m) => modules.push((m, path)),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
    (modules, errors)
}

pub fn load_one(path: &Path) -> Result<Arc<dyn Module>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let f: UserModuleFile = toml::from_str(&text).map_err(|e| e.to_string())?;
    let id = f.id.unwrap_or_else(|| {
        format!(
            "user:{}",
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )
    });
    let color = f
        .color
        .as_deref()
        .map(|c| parse_hex_color(c).ok_or_else(|| format!("bad color {c:?} (use \"#rrggbb\")")))
        .transpose()?
        .unwrap_or(Color32::from_rgb(0xe5, 0xc0, 0x7b));
    let pattern = if f.case_insensitive {
        format!("(?i){}", f.pattern)
    } else {
        f.pattern
    };
    Ok(Arc::new(UserDetector {
        id,
        name: f.name,
        description: f.description,
        pattern,
        color,
        replacement: f.replacement,
        enabled: f.enabled,
        group_label: f.group_label,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(m: &Arc<dyn Module>, text: &str) -> Vec<String> {
        let re = regex::Regex::new(&m.pattern().unwrap()).unwrap();
        re.find_iter(text).map(|x| x.as_str().to_string()).collect()
    }

    #[test]
    fn shipped_example_modules_work() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("modules");
        let jwt = load_one(&dir.join("jwt.toml")).unwrap();
        let tok =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(
            hits(&jwt, &format!("Authorization: Bearer {tok}")),
            vec![tok]
        );
        let aws = load_one(&dir.join("aws-keys.toml")).unwrap();
        assert_eq!(
            hits(&aws, "key=AKIAIOSFODNN7EXAMPLE, x"),
            vec!["AKIAIOSFODNN7EXAMPLE"]
        );
        assert_eq!(aws.id(), "user:aws-keys");
    }

    #[test]
    fn example_template_parses() {
        let p = std::env::temp_dir().join(format!("mammoth-example-{}.toml", std::process::id()));
        std::fs::write(&p, crate::app::EXAMPLE_MODULE).unwrap();
        let m = load_one(&p).unwrap();
        assert_eq!(
            hits(&m, "#rust and @ferris, not a#b"),
            vec!["#rust", "@ferris"]
        );
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn toml_module_breakdown() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("modules/http-status.toml");
        let m = load_one(&path).unwrap();
        let entry = crate::modules::ModuleEntry::for_test(m);
        let g = entry
            .grouper()
            .expect("a (?P<group>) capture enables breakdowns");
        assert_eq!(g.label, "Status");
        let line = r#"1.2.3.4 - - [25/Sep/2026:10:00:00 +0000] "GET / HTTP/1.1" 503 12"#;
        let mut hits = Vec::new();
        entry.detect(line, &mut hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(g.key(&line[hits[0].0..hits[0].1]).as_deref(), Some("503"));
    }
}
