//! Hot exit: open tabs, unsaved changes and the open folder are kept on disk
//! so quitting (or a crash, or Force Quit) never loses work.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub folder: Option<PathBuf>,
    /// Index into `tabs` of the tab that was in front.
    pub active: usize,
    pub tabs: Vec<Tab>,
    /// The split view layout (see `layout::Layout::to_json`); `None` for one group.
    #[serde(default)]
    pub layout: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tab {
    /// The file on disk; `None` for an untitled tab.
    pub path: Option<PathBuf>,
    /// Name of the file in the session folder holding unsaved text, if any.
    pub unsaved: Option<String>,
    pub caret: usize,
    pub syntax: String,
    pub crlf: bool,
    /// The split view group the tab is in.
    #[serde(default)]
    pub group: usize,
    /// Another view of the text of this earlier tab (File > New View into File).
    #[serde(default)]
    pub clone_of: Option<usize>,
}

/// The session folder: `session.json` plus one text file per unsaved tab.
pub struct Store {
    dir: PathBuf,
}

const SESSION_FILE: &str = "session.json";

impl Store {
    /// `~/Library/Application Support/TextEditor/session`. Tests get none, so
    /// they never restore each other's tabs; they use [`Store::at`].
    pub fn default_location() -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        crate::workspace::settings::config_dir().map(|d| Self::at(d.join("session")))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn load(&self) -> Option<Session> {
        let text = std::fs::read_to_string(self.dir.join(SESSION_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn read_text(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name)).ok()
    }

    pub fn write_text(&self, name: &str, text: &str) -> std::io::Result<()> {
        write_atomic(&self.dir.join(name), text)
    }

    /// Saves the session, then deletes text files no tab refers to any more.
    pub fn write_session(&self, session: &Session) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(session).map_err(std::io::Error::other)?;
        write_atomic(&self.dir.join(SESSION_FILE), &json)?;
        let keep: Vec<&str> = session.tabs.iter().filter_map(|t| t.unsaved.as_deref()).collect();
        for entry in std::fs::read_dir(&self.dir)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".txt") && !keep.contains(&name.as_ref()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

/// Writes a temporary file and renames it over the old one, so a crash
/// mid-write never leaves a half-written session behind.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_cleans_up_unused_text_files() {
        let dir = std::env::temp_dir().join(format!("text_editor_session_store_{}", std::process::id()));
        let store = Store::at(dir.clone());
        store.write_text("doc-1.txt", "draft").unwrap();
        store.write_text("doc-2.txt", "old").unwrap();
        let session = Session {
            folder: None,
            active: 0,
            tabs: vec![Tab { path: None, unsaved: Some("doc-1.txt".into()), caret: 3, syntax: "Plain Text".into(), crlf: false, group: 0, clone_of: None }],
            layout: None,
        };
        store.write_session(&session).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].caret, 3);
        assert_eq!(store.read_text("doc-1.txt").as_deref(), Some("draft"));
        assert_eq!(store.read_text("doc-2.txt"), None, "unreferenced text is deleted");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
