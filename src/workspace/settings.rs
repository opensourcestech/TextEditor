use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// User preferences, stored as JSON so they can also be edited by hand,
/// like Sublime's `Preferences.sublime-settings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of a color theme: a bundled one, or a `.tmTheme` file from a package.
    pub theme: String,
    pub font_size: f32,
    pub tab_size: usize,
    /// Insert spaces instead of a tab character when Tab is pressed.
    pub translate_tabs_to_spaces: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    /// Type `(` and get `()`, with the caret in between.
    pub auto_match_enabled: bool,
    /// Keep the indentation of the line above when pressing Enter.
    pub auto_indent: bool,
    /// Offer ways to finish a word while typing it.
    pub auto_complete: bool,
    pub caret_blink: bool,
    /// Faint lines at each indentation level.
    pub indent_guides: bool,
    /// Show the side bar (open files and the open folder).
    pub show_sidebar: bool,
    /// Wrap long lines at the window edge instead of scrolling sideways.
    pub word_wrap: bool,
    /// Quit without asking about unsaved changes; they are kept and restored
    /// next time, as are all open tabs.
    pub hot_exit: bool,
    /// Packages that are installed but switched off.
    pub ignored_packages: Vec<String>,
    /// Python 3 for plugins; empty means the first one found.
    pub python_path: String,
    /// Where Packages > Install Package… finds packages: Package Control
    /// channels, and repositories of your own.
    pub package_channels: Vec<String>,
    pub package_repositories: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "base16-ocean.dark".to_owned(),
            font_size: 14.0,
            tab_size: 4,
            translate_tabs_to_spaces: true,
            line_numbers: true,
            highlight_line: true,
            auto_match_enabled: true,
            auto_indent: true,
            auto_complete: true,
            caret_blink: true,
            indent_guides: true,
            show_sidebar: true,
            word_wrap: false,
            hot_exit: true,
            ignored_packages: Vec::new(),
            python_path: String::new(),
            package_channels: vec![
                crate::packages::package_index::DEFAULT_CHANNEL.to_owned(),
                crate::packages::package_index::LIBRARY_CHANNEL.to_owned(),
            ],
            package_repositories: Vec::new(),
        }
    }
}

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 40.0;

impl Settings {
    /// Loads the settings file. A missing file gives the defaults; a broken
    /// one gives the defaults plus an error to show the user.
    pub fn load() -> (Self, Option<String>) {
        // Each test starts from the defaults, whatever other tests saved.
        if cfg!(test) {
            return (Self::default(), None);
        }
        let Some(path) = settings_path() else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(_) => (Self::default(), None),
        }
    }

    pub fn parse(text: &str) -> (Self, Option<String>) {
        match serde_json::from_str::<Self>(text) {
            Ok(settings) => (settings.clamped(), None),
            Err(e) => (Self::default(), Some(format!("Settings file has an error: {e}"))),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path().ok_or("No settings folder on this system")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Keeps hand-edited values in a usable range.
    pub fn clamped(mut self) -> Self {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.tab_size = self.tab_size.clamp(1, 16);
        self
    }

    /// One indentation step: a tab or `tab_size` spaces.
    pub fn indent_unit(&self) -> String {
        if self.translate_tabs_to_spaces {
            " ".repeat(self.tab_size)
        } else {
            "\t".to_owned()
        }
    }
}

/// `~/Library/Application Support/TextEditor` on macOS.
pub fn config_dir() -> Option<PathBuf> {
    // Tests must never touch the user's real settings or recent files.
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!("text_editor_test_config_{}", std::process::id())));
    }
    dirs::config_dir().map(|d| d.join("TextEditor"))
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_use_defaults_and_values_are_clamped() {
        let (s, err) = Settings::parse(r#"{ "font_size": 200, "tab_size": 2 }"#);
        assert!(err.is_none());
        assert_eq!(s.font_size, MAX_FONT_SIZE);
        assert_eq!(s.tab_size, 2);
        assert_eq!(s.theme, Settings::default().theme);
        assert_eq!(s.indent_unit(), "  ");
    }

    #[test]
    fn broken_json_reports_an_error() {
        let (s, err) = Settings::parse("{ nope");
        assert!(err.is_some());
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn round_trips_through_json() {
        let s = Settings { theme: "Solarized (light)".into(), ..Settings::default() };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(Settings::parse(&text).0, s);
    }
}
