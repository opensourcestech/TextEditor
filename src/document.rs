use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::editor_view::ViewState;
use crate::highlight::LineCache;

/// One open file (or an unsaved "untitled" buffer) shown as a tab.
pub struct Document {
    /// Stable id so each tab keeps its own scroll position.
    pub id: u64,
    pub path: Option<PathBuf>,
    pub buffer: Buffer,
    pub highlight: LineCache,
    pub view: ViewState,
    /// The file used Windows line endings; text is kept as `\n` in memory
    /// and converted back to `\r\n` on save.
    crlf: bool,
    /// The tab's name when it has no file, like "Find Results".
    pub name: Option<String>,
    /// Never asks to be saved, like Find Results or a plugin's scratch view.
    pub scratch: bool,
    /// Can't be typed into, only read, selected and copied; plugins can
    /// still change it.
    pub read_only: bool,
    /// Tabs showing the same text (File > New View into File) share this:
    /// the id of the first of them.
    pub family: u64,
    /// Of the tabs showing the same text, the one whose buffer is the real
    /// one, with the undo history. The others show copies that replay its
    /// changes (see `app::groups`).
    pub holds_text: bool,
}

impl Document {
    pub fn new_untitled(id: u64) -> Self {
        Self::from_text(id, None, "", false)
    }

    pub fn open(id: u64, path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let raw = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
        let crlf = raw.contains("\r\n");
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Ok(Self::from_text(id, Some(path.to_path_buf()), &text, crlf))
    }

    /// A tab with unsaved changes kept from the last session.
    pub fn restored(id: u64, path: Option<PathBuf>, text: &str, crlf: bool) -> Self {
        let mut doc = Self::from_text(id, path, text, crlf);
        doc.buffer.mark_unsaved();
        doc
    }

    pub fn crlf(&self) -> bool {
        self.crlf
    }

    fn from_text(id: u64, path: Option<PathBuf>, text: &str, crlf: bool) -> Self {
        let buffer = Buffer::new(text);
        Self {
            id,
            path,
            view: ViewState::new(&buffer),
            buffer,
            highlight: LineCache::new("Plain Text".to_owned()),
            crlf,
            name: None,
            scratch: false,
            read_only: false,
            family: id,
            holds_text: true,
        }
    }

    /// Another tab showing this one's text, with its own selections and
    /// scrolling.
    pub fn clone_view(&mut self, id: u64) -> Self {
        // From now on this tab's changes are kept for the new one to replay.
        self.buffer.set_logging(true);
        let buffer = self.buffer.snapshot();
        Self {
            id,
            path: self.path.clone(),
            view: ViewState::new(&buffer),
            buffer,
            highlight: LineCache::new(self.syntax().to_owned()),
            crlf: self.crlf,
            name: self.name.clone(),
            scratch: self.scratch,
            read_only: self.read_only,
            family: self.family,
            holds_text: false,
        }
    }

    pub fn save_to(&mut self, path: &Path) -> Result<(), String> {
        let text = self.buffer.text();
        let out = if self.crlf { text.replace('\n', "\r\n") } else { text };
        std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.buffer.mark_saved();
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        !self.scratch && self.buffer.is_dirty()
    }

    pub fn syntax(&self) -> &str {
        &self.highlight.syntax
    }

    pub fn set_syntax(&mut self, name: String) {
        self.highlight.syntax = name;
    }

    /// Name shown on the tab and in the window title.
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| "untitled".to_owned())
    }

    pub fn line_ending_label(&self) -> &'static str {
        if self.crlf { "CRLF" } else { "LF" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{EditKind, Motion};

    #[test]
    fn crlf_files_round_trip() {
        let dir = std::env::temp_dir().join(format!("text_editor_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("win.txt");
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();

        let mut doc = Document::open(1, &path).unwrap();
        assert_eq!(doc.buffer.text(), "one\ntwo\n");
        doc.buffer.move_selections(Motion::DocEnd, false, &|_, _| 1.0);
        doc.buffer.insert("three\n", EditKind::Other);
        assert!(doc.dirty());
        doc.save_to(&path).unwrap();
        assert!(!doc.dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\r\ntwo\r\nthree\r\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
