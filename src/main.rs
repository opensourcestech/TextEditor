// No console window behind the editor on Windows (debug builds keep it, for
// their output).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod language;
mod packages;
mod plugins;
mod text;
mod ui;
mod workspace;

use std::path::PathBuf;

fn main() -> eframe::Result {
    #[cfg(target_os = "macos")]
    ui::mac_open::install();

    // Files passed on the command line open as tabs, and a folder opens in the
    // side bar: `text_editor . src/main.rs`
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    // An empty icon stops eframe from swapping in its egui logo, so the
    // Dock keeps showing the app bundle's icon while running. Windows takes
    // the window and taskbar icon from the window, so it gets the app's.
    #[cfg(not(windows))]
    let icon = eframe::egui::IconData::default();
    #[cfg(windows)]
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../packaging/windows/AppIcon.png")).unwrap_or_default();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([400.0, 300.0])
            .with_title("TextEditor")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "TextEditor",
        options,
        Box::new(|cc| Ok(Box::new(app::EditorApp::new(cc, paths)))),
    )
}
