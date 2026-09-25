//! Font choices: the platform's own fonts by default, egui's bundled fonts, or any
//! `.ttf` / `.otf` / `.ttc` file (installed or picked by hand).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum FontChoice {
    /// The platform's usual font (Segoe UI / Cascadia Mono on Windows).
    #[default]
    System,
    /// egui's bundled fonts (Ubuntu / Hack).
    BuiltIn,
    /// A specific font file (`index` selects a face inside a `.ttc` collection).
    File {
        path: PathBuf,
        index: u32,
        name: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Interface,
    Editor,
}

impl Role {
    fn mono(self) -> bool {
        self == Role::Editor
    }
}

impl FontChoice {
    pub fn label(&self, role: Role) -> String {
        match self {
            FontChoice::System => match system_default(role) {
                Some((name, _)) => format!("System default · {name}"),
                None => format!("System default · {}", builtin_name(role)),
            },
            FontChoice::BuiltIn => format!("Built-in · {}", builtin_name(role)),
            FontChoice::File { name, .. } => name.clone(),
        }
    }
}

fn builtin_name(role: Role) -> &'static str {
    if role.mono() { "Hack" } else { "Ubuntu" }
}

/// Folders that hold installed fonts on this platform.
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        dirs.push(
            env("WINDIR")
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
                .join("Fonts"),
        );
        if let Some(local) = env("LOCALAPPDATA") {
            dirs.push(local.join(r"Microsoft\Windows\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/Library/Fonts".into());
        if let Some(h) = env("HOME") {
            dirs.push(h.join("Library/Fonts"));
        }
    } else {
        dirs.push("/usr/share/fonts".into());
        dirs.push("/usr/local/share/fonts".into());
        if let Some(h) = env("HOME") {
            dirs.push(h.join(".local/share/fonts"));
            dirs.push(h.join(".fonts"));
        }
    }
    dirs
}

/// The font "System default" resolves to, if it is installed.
pub fn system_default(role: Role) -> Option<(&'static str, PathBuf)> {
    let candidates: &[(&str, &str)] = match (cfg!(windows), cfg!(target_os = "macos"), role.mono())
    {
        (true, _, true) => &[
            ("Cascadia Mono", "CascadiaMono.ttf"),
            ("Consolas", "consola.ttf"),
        ],
        (true, _, false) => &[("Segoe UI", "segoeui.ttf")],
        (_, true, true) => &[("Menlo", "Menlo.ttc")],
        (_, true, false) => &[("Helvetica Neue", "HelveticaNeue.ttc")],
        (_, _, true) => &[
            ("DejaVu Sans Mono", "truetype/dejavu/DejaVuSansMono.ttf"),
            ("DejaVu Sans Mono", "TTF/DejaVuSansMono.ttf"),
        ],
        (_, _, false) => &[
            ("Noto Sans", "truetype/noto/NotoSans-Regular.ttf"),
            ("DejaVu Sans", "truetype/dejavu/DejaVuSans.ttf"),
        ],
    };
    for (name, file) in candidates {
        for dir in font_dirs() {
            let p = dir.join(file);
            if p.is_file() {
                return Some((name, p));
            }
        }
    }
    None
}

/// One installed font family (represented by its most "regular" face).
#[derive(Clone, Debug)]
pub struct FontInfo {
    pub family: String,
    pub path: PathBuf,
    pub index: u32,
    pub mono: bool,
}

impl FontInfo {
    pub fn choice(&self) -> FontChoice {
        FontChoice::File {
            path: self.path.clone(),
            index: self.index,
            name: self.family.clone(),
        }
    }
}

/// Scan the installed fonts. Slow-ish (reads font headers); call off the UI thread.
pub fn scan() -> Vec<FontInfo> {
    let mut files = Vec::new();
    for dir in font_dirs() {
        collect(&dir, 0, &mut files);
    }
    let mut best: HashMap<String, (u32, FontInfo)> = HashMap::new();
    for path in files {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        // SAFETY: read-only mapping of a font file; only headers are touched.
        let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else {
            continue;
        };
        let faces = ttf_parser::fonts_in_collection(&map).unwrap_or(1).min(32);
        for index in 0..faces {
            let Ok(face) = ttf_parser::Face::parse(&map, index) else {
                continue;
            };
            // Skip symbol fonts that can't show ordinary text.
            if face.glyph_index('a').is_none() || face.glyph_index('0').is_none() {
                continue;
            }
            let Some(family) = family_name(&face) else {
                continue;
            };
            let score = (face.weight().to_number() as i32 - 400).unsigned_abs()
                + if face.is_italic() || face.is_oblique() {
                    1000
                } else {
                    0
                };
            let info = FontInfo {
                family: family.clone(),
                path: path.clone(),
                index,
                mono: is_mono(&face),
            };
            let key = family.to_lowercase();
            if best.get(&key).is_none_or(|(s, _)| score < *s) {
                best.insert(key, (score, info));
            }
        }
    }
    let mut out: Vec<FontInfo> = best.into_values().map(|(_, f)| f).collect();
    out.sort_by_key(|f| f.family.to_lowercase());
    out
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                collect(&p, depth + 1, out);
            }
        } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        }) {
            out.push(p);
        }
    }
}

fn family_name(face: &ttf_parser::Face) -> Option<String> {
    use ttf_parser::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    for id in [TYPOGRAPHIC_FAMILY, FAMILY] {
        let mut fallback = None;
        for name in face.names() {
            if name.name_id != id {
                continue;
            }
            if let Some(s) = name.to_string().filter(|s| !s.trim().is_empty()) {
                // Prefer US English, else whatever decodes first.
                if name.language_id == 0x0409 {
                    return Some(s);
                }
                fallback.get_or_insert(s);
            }
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    None
}

fn is_mono(face: &ttf_parser::Face) -> bool {
    if face.is_monospaced() {
        return true;
    }
    let adv = |c| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g));
    matches!((adv('i'), adv('W'), adv('0')), (Some(a), Some(b), Some(c)) if a == b && b == c)
}

/// Build egui font definitions for the chosen fonts. The bundled fonts stay behind
/// them as fallbacks, so symbols and emoji keep working.
pub fn definitions(interface: &FontChoice, editor: &FontChoice) -> (FontDefinitions, Vec<String>) {
    let mut defs = FontDefinitions::default();
    let mut errors = Vec::new();
    for (choice, role, family, key) in [
        (
            interface,
            Role::Interface,
            FontFamily::Proportional,
            "mammoth-ui",
        ),
        (
            editor,
            Role::Editor,
            FontFamily::Monospace,
            "mammoth-editor",
        ),
    ] {
        let source = match choice {
            FontChoice::BuiltIn => None,
            FontChoice::System => system_default(role).map(|(_, p)| (p, 0)),
            FontChoice::File { path, index, .. } => Some((path.clone(), *index)),
        };
        let Some((path, index)) = source else {
            continue;
        };
        match load(&path, index) {
            Ok(data) => {
                defs.font_data.insert(key.to_string(), Arc::new(data));
                defs.families
                    .entry(family)
                    .or_default()
                    .insert(0, key.to_string());
            }
            Err(e) => errors.push(format!("Could not load font {}: {e}", path.display())),
        }
    }
    (defs, errors)
}

fn load(path: &Path, index: u32) -> Result<FontData, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    ttf_parser::Face::parse(&bytes, index).map_err(|e| format!("not a usable font ({e})"))?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    Ok(data)
}

/// Inspect a font file picked by hand: its family name and whether it's monospaced.
pub fn describe_file(path: &Path) -> Result<FontInfo, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let face =
        ttf_parser::Face::parse(&bytes, 0).map_err(|e| format!("not a usable font ({e})"))?;
    let family = family_name(&face).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Ok(FontInfo {
        family,
        path: path.to_path_buf(),
        index: 0,
        mono: is_mono(&face),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_installed_fonts() {
        let fonts = scan();
        if fonts.is_empty() {
            return; // headless CI box without fonts
        }
        assert!(
            fonts
                .windows(2)
                .all(|w| w[0].family.to_lowercase() <= w[1].family.to_lowercase())
        );
        if cfg!(windows) {
            let consolas = fonts
                .iter()
                .find(|f| f.family == "Consolas")
                .expect("Consolas installed");
            assert!(consolas.mono);
            let segoe = fonts
                .iter()
                .find(|f| f.family == "Segoe UI")
                .expect("Segoe UI installed");
            assert!(!segoe.mono);
            // The regular face represents the family, not bold/italic.
            assert!(
                segoe
                    .path
                    .file_name()
                    .unwrap()
                    .eq_ignore_ascii_case("segoeui.ttf"),
                "{:?}",
                segoe.path
            );
        }
    }

    #[test]
    fn builds_definitions() {
        let (defs, errors) = definitions(&FontChoice::System, &FontChoice::System);
        assert!(errors.is_empty(), "{errors:?}");
        if system_default(Role::Editor).is_some() {
            assert_eq!(defs.families[&FontFamily::Monospace][0], "mammoth-editor");
        }
        let (defs, _) = definitions(&FontChoice::BuiltIn, &FontChoice::BuiltIn);
        assert!(!defs.font_data.contains_key("mammoth-ui"));
        let bad = FontChoice::File {
            path: "nope.ttf".into(),
            index: 0,
            name: "Nope".into(),
        };
        let (_, errors) = definitions(&bad, &FontChoice::BuiltIn);
        assert_eq!(errors.len(), 1);
    }
}
