#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

//! Mammoth — a fast, modular text editor for huge files.

mod app;
mod convert;
mod csv;
mod document;
mod editor;
mod fonts;
mod icons;
mod json_tools;
mod logo;
mod modules;
mod search;
mod table;
mod theme;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mammoth")
            .with_inner_size([1320.0, 820.0])
            .with_min_inner_size([520.0, 320.0])
            .with_drag_and_drop(true)
            // Frameless: Mammoth draws its own title bar (see app/titlebar.rs).
            .with_decorations(false)
            .with_icon(icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Mammoth",
        options,
        Box::new(|cc| Ok(Box::new(app::MammothApp::new(cc, files)))),
    )
}

/// The window/taskbar icon, rendered from the vector logo.
fn icon() -> egui::IconData {
    let n = 256;
    egui::IconData {
        rgba: logo::render(n),
        width: n as u32,
        height: n as u32,
    }
}
