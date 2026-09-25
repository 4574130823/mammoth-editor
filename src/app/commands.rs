//! Every user-facing action in one place. Menus, keyboard shortcuts and the command
//! palette all go through [`Cmd`], so labels, shortcuts and enabled-state never drift.

use std::path::PathBuf;

use egui::ViewportCommand;

use super::{Dialog, MammothApp, ModuleAction};
use crate::editor::Command;
use crate::fonts::Role;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Cmd {
    NewFile,
    Open,
    OpenRecent(PathBuf),
    Save,
    SaveAs,
    CloseTab,
    LoadEntire,
    Exit,
    Edit(Command),
    Find,
    Replace,
    FindNext,
    FindPrev,
    CountMatches,
    ExtractMatches,
    FilterLines,
    GoToLine,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    NextError,
    PrevError,
    ToggleHighlight,
    ClearHighlights,
    ToggleModules,
    PinModules,
    ReloadModules,
    CreateModule,
    OpenModulesFolder,
    ToggleDetector(String),
    Module(ModuleAction),
    Syntax(Option<String>),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleLineHighlight,
    ToggleTable,
    ToggleJson,
    ToggleInsights,
    ToggleHeatmap,
    Convert(Option<crate::convert::Format>),
    JsonPretty,
    JsonMinify,
    RunJq,
    Settings,
    ChooseFont(Role),
    Shortcuts,
    Palette,
}

impl Cmd {
    pub(super) fn shortcut(&self) -> &'static str {
        match self {
            Cmd::NewFile => "Ctrl+N",
            Cmd::Open => "Ctrl+O",
            Cmd::Save => "Ctrl+S",
            Cmd::SaveAs => "Ctrl+Shift+S",
            Cmd::CloseTab => "Ctrl+W",
            Cmd::LoadEntire => "Ctrl+L",
            Cmd::Exit => "Alt+F4",
            Cmd::Edit(c) => match c {
                Command::Undo => "Ctrl+Z",
                Command::Redo => "Ctrl+Y",
                Command::Cut => "Ctrl+X",
                Command::Copy => "Ctrl+C",
                Command::SelectAll => "Ctrl+A",
                Command::DuplicateLine => "Ctrl+D",
                Command::DeleteLine => "Ctrl+Shift+K",
                Command::MoveLineUp => "Alt+Up",
                Command::MoveLineDown => "Alt+Down",
                Command::Indent => "Tab",
                Command::Outdent => "Shift+Tab",
            },
            Cmd::Find => "Ctrl+F",
            Cmd::Replace => "Ctrl+H",
            Cmd::FindNext => "F3",
            Cmd::FindPrev => "Shift+F3",
            Cmd::GoToLine => "Ctrl+G",
            Cmd::ToggleBookmark => "Ctrl+F2",
            Cmd::NextBookmark => "F2",
            Cmd::PrevBookmark => "Shift+F2",
            Cmd::NextError => "F4",
            Cmd::PrevError => "Shift+F4",
            Cmd::ToggleHighlight => "Ctrl+F3",
            Cmd::ClearHighlights => "Ctrl+Shift+F3",
            Cmd::ToggleModules => "Ctrl+Shift+M",
            Cmd::ZoomIn => "Ctrl+=",
            Cmd::ZoomOut => "Ctrl+-",
            Cmd::ZoomReset => "Ctrl+0",
            Cmd::Settings => "Ctrl+,",
            Cmd::Shortcuts => "F1",
            Cmd::Palette => "Ctrl+Shift+P",
            Cmd::ToggleTable => "Ctrl+Shift+T",
            Cmd::ToggleJson => "Ctrl+Shift+J",
            Cmd::ToggleInsights => "Ctrl+Shift+E",
            _ => "",
        }
    }

    pub(super) fn category(&self) -> &'static str {
        match self {
            Cmd::NewFile
            | Cmd::Open
            | Cmd::OpenRecent(_)
            | Cmd::Save
            | Cmd::SaveAs
            | Cmd::CloseTab => "File",
            Cmd::LoadEntire | Cmd::Exit => "File",
            Cmd::Edit(_) => "Edit",
            Cmd::Find | Cmd::Replace | Cmd::FindNext | Cmd::FindPrev => "Search",
            Cmd::CountMatches | Cmd::ExtractMatches | Cmd::FilterLines | Cmd::GoToLine => "Search",
            Cmd::NextError | Cmd::PrevError | Cmd::ToggleHighlight | Cmd::ClearHighlights => {
                "Search"
            }
            Cmd::ToggleBookmark | Cmd::NextBookmark | Cmd::PrevBookmark => "Bookmarks",
            Cmd::ToggleModules | Cmd::PinModules | Cmd::ReloadModules | Cmd::CreateModule => {
                "Modules"
            }
            Cmd::OpenModulesFolder | Cmd::ToggleDetector(_) | Cmd::Module(_) => "Modules",
            Cmd::Syntax(_) => "Syntax",
            Cmd::ToggleJson | Cmd::JsonPretty | Cmd::JsonMinify | Cmd::RunJq => "JSON",
            _ => "View",
        }
    }
}

impl MammothApp {
    pub(super) fn cmd_label(&self, cmd: &Cmd) -> String {
        let module_name = |id: &str| {
            self.registry
                .by_id(id)
                .map(|i| self.registry.entries[i].module.name().to_string())
                .unwrap_or_else(|| id.to_string())
        };
        match cmd {
            Cmd::NewFile => "New file".into(),
            Cmd::Open => "Open file…".into(),
            Cmd::OpenRecent(p) => format!("Open recent: {}", p.display()),
            Cmd::Save => "Save".into(),
            Cmd::SaveAs => "Save as…".into(),
            Cmd::CloseTab => "Close tab".into(),
            Cmd::LoadEntire => "Load entire file".into(),
            Cmd::Exit => "Exit".into(),
            Cmd::Edit(c) => match c {
                Command::Undo => "Undo",
                Command::Redo => "Redo",
                Command::Cut => "Cut",
                Command::Copy => "Copy",
                Command::SelectAll => "Select all",
                Command::DuplicateLine => "Duplicate line",
                Command::DeleteLine => "Delete line",
                Command::MoveLineUp => "Move line up",
                Command::MoveLineDown => "Move line down",
                Command::Indent => "Indent",
                Command::Outdent => "Outdent",
            }
            .into(),
            Cmd::Find => "Find".into(),
            Cmd::Replace => "Replace".into(),
            Cmd::FindNext => "Find next".into(),
            Cmd::FindPrev => "Find previous".into(),
            Cmd::CountMatches => "Count matches in file".into(),
            Cmd::ExtractMatches => "Extract matches to new tab".into(),
            Cmd::FilterLines => "Filter: show only lines that match".into(),
            Cmd::GoToLine => "Go to line…".into(),
            Cmd::ToggleBookmark => "Toggle bookmark on this line".into(),
            Cmd::NextBookmark => "Next bookmark".into(),
            Cmd::PrevBookmark => "Previous bookmark".into(),
            Cmd::NextError => "Next error".into(),
            Cmd::PrevError => "Previous error".into(),
            Cmd::ToggleHighlight => "Highlight word/selection (toggle)".into(),
            Cmd::ClearHighlights => "Clear highlighted terms".into(),
            Cmd::ToggleModules => {
                if self.modules_open {
                    "Hide modules".into()
                } else {
                    "Show modules".into()
                }
            }
            Cmd::PinModules => {
                if self.settings.modules_pinned {
                    "Unpin modules panel (float over the editor)".into()
                } else {
                    "Pin modules panel to the side".into()
                }
            }
            Cmd::ReloadModules => "Reload modules from disk".into(),
            Cmd::CreateModule => "Create a new module…".into(),
            Cmd::OpenModulesFolder => "Open modules folder".into(),
            Cmd::ToggleDetector(id) => {
                let on = self
                    .registry
                    .by_id(id)
                    .is_some_and(|i| self.registry.entries[i].enabled);
                format!(
                    "{} highlighting: {}",
                    if on { "Turn off" } else { "Turn on" },
                    module_name(id)
                )
            }
            Cmd::Module(a) => {
                if let ModuleAction::Breakdown(id) = a {
                    let by = self
                        .registry
                        .by_id(id)
                        .and_then(|i| self.registry.entries[i].grouper())
                        .map(|g| g.category_label.unwrap_or(g.label).to_lowercase())
                        .unwrap_or_default();
                    return format!("{}: Breakdown by {by}", module_name(id));
                }
                let (verb, id) = match a {
                    ModuleAction::Breakdown(id) => ("Breakdown", id),
                    ModuleAction::Find(id) => ("Find next", id),
                    ModuleAction::Count(id) => ("Count", id),
                    ModuleAction::Extract(id) => ("Extract all", id),
                    ModuleAction::Mask(id) => ("Mask all", id),
                };
                format!("{}: {verb}", module_name(id))
            }
            Cmd::Syntax(None) => "Syntax: Plain text".into(),
            Cmd::Syntax(Some(id)) => format!("Syntax: {}", module_name(id)),
            Cmd::ZoomIn => "Zoom in".into(),
            Cmd::ZoomOut => "Zoom out".into(),
            Cmd::ZoomReset => "Reset zoom".into(),
            Cmd::ToggleLineHighlight => {
                if self.settings.highlight_line {
                    "Don't highlight the current line".into()
                } else {
                    "Highlight the current line".into()
                }
            }
            Cmd::Settings => "Settings…".into(),
            Cmd::ToggleJson => {
                if self.json.open {
                    "Hide JSON inspector".into()
                } else {
                    "JSON inspector (tree, pretty, jq)".into()
                }
            }
            Cmd::JsonPretty => "Pretty-print JSON document".into(),
            Cmd::ToggleInsights => {
                if self.insights.open {
                    "Hide insights".into()
                } else {
                    "Insights: email providers, domains, top addresses".into()
                }
            }
            Cmd::ToggleHeatmap => {
                if self.settings.heatmap {
                    "Hide the heatmap beside the scrollbar".into()
                } else {
                    "Show the heatmap beside the scrollbar".into()
                }
            }
            Cmd::Convert(None) => "Convert to another format…".into(),
            Cmd::Convert(Some(f)) => format!("Convert to {}…", f.label()),
            Cmd::JsonMinify => "Minify JSON document".into(),
            Cmd::RunJq => "Run a jq filter on the file…".into(),
            Cmd::ToggleTable => {
                if self.tabs.get(self.active).is_some_and(|t| t.table_mode) {
                    "Text view".into()
                } else {
                    "Table view (sort, filter, edit cells)".into()
                }
            }
            Cmd::ChooseFont(Role::Editor) => "Choose editor font…".into(),
            Cmd::ChooseFont(Role::Interface) => "Choose interface font…".into(),
            Cmd::Shortcuts => "Keyboard shortcuts".into(),
            Cmd::Palette => "Command palette".into(),
        }
    }

    pub(super) fn cmd_enabled(&self, cmd: &Cmd) -> bool {
        let tab = self.tabs.get(self.active);
        let has_tab = tab.is_some();
        match cmd {
            Cmd::Save | Cmd::SaveAs | Cmd::CloseTab | Cmd::Find | Cmd::Replace | Cmd::GoToLine => {
                has_tab
            }
            Cmd::FindNext
            | Cmd::FindPrev
            | Cmd::CountMatches
            | Cmd::ExtractMatches
            | Cmd::FilterLines => has_tab,
            Cmd::ToggleBookmark
            | Cmd::NextBookmark
            | Cmd::PrevBookmark
            | Cmd::NextError
            | Cmd::PrevError
            | Cmd::ToggleHighlight
            | Cmd::ClearHighlights => has_tab,
            Cmd::Module(_) | Cmd::Syntax(_) => has_tab,
            Cmd::LoadEntire => tab.is_some_and(|t| t.is_partial()),
            Cmd::ToggleTable => tab.is_some_and(|t| t.is_delimited()),
            Cmd::ToggleJson | Cmd::JsonPretty | Cmd::JsonMinify | Cmd::RunJq | Cmd::Convert(_) => {
                has_tab
            }
            Cmd::Edit(Command::Undo) => tab.is_some_and(|t| t.doc.can_undo()),
            Cmd::Edit(Command::Redo) => tab.is_some_and(|t| t.doc.can_redo()),
            Cmd::Edit(_) => has_tab,
            _ => true,
        }
    }

    /// Everything the command palette offers, in display order.
    pub(super) fn all_commands(&self) -> Vec<Cmd> {
        let mut v = vec![
            Cmd::Open,
            Cmd::NewFile,
            Cmd::Save,
            Cmd::SaveAs,
            Cmd::LoadEntire,
            Cmd::Find,
            Cmd::Replace,
            Cmd::GoToLine,
            Cmd::CountMatches,
            Cmd::ExtractMatches,
            Cmd::FilterLines,
            Cmd::ToggleBookmark,
            Cmd::NextBookmark,
            Cmd::PrevBookmark,
            Cmd::NextError,
            Cmd::PrevError,
            Cmd::ToggleHighlight,
            Cmd::ClearHighlights,
            Cmd::ToggleTable,
            Cmd::ToggleInsights,
            Cmd::ToggleJson,
            Cmd::Convert(Some(crate::convert::Format::JsonLines)),
            Cmd::Convert(Some(crate::convert::Format::Delimited(b','))),
            Cmd::Convert(None),
            Cmd::ToggleHeatmap,
            Cmd::RunJq,
            Cmd::JsonPretty,
            Cmd::JsonMinify,
            Cmd::ToggleModules,
            Cmd::PinModules,
        ];
        for (_, e) in self.registry.detectors() {
            if e.error.is_some() {
                continue;
            }
            let id = e.module.id().to_string();
            if e.grouper().is_some() {
                v.push(Cmd::Module(ModuleAction::Breakdown(id.clone())));
            }
            v.push(Cmd::ToggleDetector(id.clone()));
            v.push(Cmd::Module(ModuleAction::Find(id.clone())));
            v.push(Cmd::Module(ModuleAction::Count(id.clone())));
            v.push(Cmd::Module(ModuleAction::Extract(id.clone())));
            v.push(Cmd::Module(ModuleAction::Mask(id)));
        }
        v.push(Cmd::Syntax(None));
        for (_, e) in self.registry.syntaxes() {
            v.push(Cmd::Syntax(Some(e.module.id().to_string())));
        }
        v.extend([
            Cmd::ReloadModules,
            Cmd::CreateModule,
            Cmd::OpenModulesFolder,
            Cmd::ChooseFont(Role::Editor),
            Cmd::ChooseFont(Role::Interface),
            Cmd::Settings,
            Cmd::ZoomIn,
            Cmd::ZoomOut,
            Cmd::ZoomReset,
            Cmd::ToggleLineHighlight,
            Cmd::Edit(Command::Undo),
            Cmd::Edit(Command::Redo),
            Cmd::Edit(Command::SelectAll),
            Cmd::Edit(Command::DuplicateLine),
            Cmd::Edit(Command::DeleteLine),
            Cmd::Edit(Command::MoveLineUp),
            Cmd::Edit(Command::MoveLineDown),
            Cmd::CloseTab,
            Cmd::Shortcuts,
        ]);
        v.extend(self.settings.recent.iter().cloned().map(Cmd::OpenRecent));
        v.retain(|c| self.cmd_enabled(c));
        v
    }

    pub(super) fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        if !self.cmd_enabled(&cmd) {
            return;
        }
        match cmd {
            Cmd::NewFile => self.new_tab(),
            Cmd::Open => self.open_dialog(ctx),
            Cmd::OpenRecent(p) => self.open_path(p, ctx),
            Cmd::Save => self.save_tab(self.active, false, ctx),
            Cmd::SaveAs => self.save_tab(self.active, true, ctx),
            Cmd::CloseTab => self.request_close(self.active),
            Cmd::LoadEntire => self.load_entire(ctx),
            Cmd::Exit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Cmd::Edit(c) => self.run_editor_command(c, ctx),
            Cmd::Find => self.open_find(false),
            Cmd::Replace => self.open_find(true),
            Cmd::FindNext => self.start_find(false, ctx),
            Cmd::FindPrev => self.start_find(true, ctx),
            Cmd::CountMatches => self.start_count(ctx),
            Cmd::ExtractMatches => self.start_extract(ctx),
            Cmd::FilterLines => self.start_filter(ctx),
            Cmd::ToggleBookmark => self.toggle_bookmark(),
            Cmd::NextBookmark => self.jump_bookmark(false),
            Cmd::PrevBookmark => self.jump_bookmark(true),
            Cmd::NextError => self.jump_to_level(&["ERROR", "FATAL"], "Errors", false, ctx),
            Cmd::PrevError => self.jump_to_level(&["ERROR", "FATAL"], "Errors", true, ctx),
            Cmd::ToggleHighlight => self.toggle_highlight_at_cursor(),
            Cmd::ClearHighlights => self.clear_highlights(),
            Cmd::GoToLine => {
                self.dialog = Dialog::GoTo {
                    text: String::new(),
                    focus: true,
                }
            }
            Cmd::ToggleModules => self.modules_open = !self.modules_open,
            Cmd::PinModules => {
                self.settings.modules_pinned = !self.settings.modules_pinned;
                self.modules_open = true;
            }
            Cmd::ReloadModules => self.reload_modules(),
            Cmd::CreateModule => self.create_module(ctx),
            Cmd::OpenModulesFolder => {
                if let Some(dir) = super::user_modules_dir() {
                    let _ = std::fs::create_dir_all(&dir);
                    super::open_in_file_manager(&dir);
                }
            }
            Cmd::ToggleDetector(id) => {
                if let Some(i) = self.registry.by_id(&id) {
                    let e = &mut self.registry.entries[i];
                    e.enabled = !e.enabled && e.error.is_none();
                }
            }
            Cmd::Module(a) => self.run_module_action(a, ctx),
            Cmd::Syntax(id) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.syntax = id;
                }
            }
            Cmd::ZoomIn => self.zoom(1.1),
            Cmd::ZoomOut => self.zoom(1.0 / 1.1),
            Cmd::ZoomReset => self.settings.font_size = 14.0,
            Cmd::ToggleLineHighlight => {
                self.settings.highlight_line = !self.settings.highlight_line
            }
            Cmd::Settings => self.show_settings = true,
            Cmd::ToggleTable => self.toggle_table(),
            Cmd::ToggleJson => self.toggle_json(),
            Cmd::ToggleInsights => self.toggle_insights(),
            Cmd::ToggleHeatmap => self.settings.heatmap = !self.settings.heatmap,
            Cmd::Convert(to) => self.open_convert(to),
            Cmd::JsonPretty => self.reformat_json(true),
            Cmd::JsonMinify => self.reformat_json(false),
            Cmd::RunJq => self.focus_jq(),
            Cmd::ChooseFont(role) => self.open_font_picker(role),
            Cmd::Shortcuts => self.show_help = true,
            Cmd::Palette => self.open_palette(),
        }
    }

    /// A menu entry for `cmd`; the chosen command is stored in `out`.
    pub(super) fn menu_item(&self, ui: &mut egui::Ui, cmd: Cmd, out: &mut Option<Cmd>) {
        let b = egui::Button::new(self.cmd_label(&cmd)).shortcut_text(cmd.shortcut());
        if ui.add_enabled(self.cmd_enabled(&cmd), b).clicked() {
            *out = Some(cmd);
            ui.close();
        }
    }
}

/// Fuzzy subsequence match; higher is better. `None` if `query` doesn't match.
pub(super) fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            let word_start = i == 0 || !t[i - 1].is_alphanumeric();
            score += if word_start { 12 } else { 2 };
            if last.is_some_and(|l| l + 1 == i) {
                score += 8;
            }
            if let Some(l) = last {
                score -= (i - l - 1).min(10) as i32;
            }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score - t.len() as i32 / 8)
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_prefers_word_starts() {
        assert!(
            fuzzy_score("ext", "Extract matches").unwrap()
                > fuzzy_score("ext", "Next tab").unwrap()
        );
        assert!(fuzzy_score("em ex", "Email addresses: Extract all").is_some());
        assert!(fuzzy_score("zzz", "Save").is_none());
    }
}
