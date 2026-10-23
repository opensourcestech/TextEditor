use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Never shown in the side bar or searched.
const HIDDEN: &[&str] = &[".git", ".svn", ".hg", "CVS", ".DS_Store"];
/// Shown in the side bar but left out of Goto Anything: usually huge and generated.
const NOT_INDEXED: &[&str] = &["node_modules", "target", "build", "dist", "__pycache__"];
/// Stop indexing past this many files so a huge folder can't stall the app.
const MAX_FILES: usize = 50_000;

/// A folder opened with "Open Folder…": the side bar tree and the list of
/// files that Goto Anything searches.
pub struct Workspace {
    pub root: PathBuf,
    /// Paths relative to `root`, with `/` separators, sorted.
    pub files: Vec<String>,
    pub truncated: bool,
    scan: Option<Receiver<(Vec<String>, bool)>>,
    /// Directory listings for the side bar, read once when a folder is expanded.
    children: HashMap<PathBuf, Vec<(PathBuf, bool)>>,
}

impl Workspace {
    pub fn open(root: PathBuf, ctx: &eframe::egui::Context) -> Self {
        let mut ws = Self {
            root,
            files: Vec::new(),
            truncated: false,
            scan: None,
            children: HashMap::new(),
        };
        ws.refresh(ctx);
        ws
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Re-reads the side bar and re-indexes files in the background.
    pub fn refresh(&mut self, ctx: &eframe::egui::Context) {
        self.children.clear();
        let (tx, rx) = std::sync::mpsc::channel();
        let root = self.root.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(index_files(&root));
            ctx.request_repaint();
        });
        self.scan = Some(rx);
    }

    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Picks up a finished background scan. Returns true when `files` changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.scan else {
            return false;
        };
        match rx.try_recv() {
            Ok((files, truncated)) => {
                self.files = files;
                self.truncated = truncated;
                self.scan = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.scan = None;
                false
            }
        }
    }

    /// Entries of `dir` as (path, is_dir): folders first, then by name.
    pub fn children(&mut self, dir: &Path) -> Vec<(PathBuf, bool)> {
        self.children
            .entry(dir.to_path_buf())
            .or_insert_with(|| list_dir(dir))
            .clone()
    }

    /// `path` relative to the folder, for display; `None` if it is outside.
    pub fn relative(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }
}

/// Whether files and folders of this name are left out of searches.
pub fn skipped(name: &str) -> bool {
    HIDDEN.contains(&name) || NOT_INDEXED.contains(&name)
}

fn list_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(PathBuf, bool)> = read
        .flatten()
        .filter(|e| !HIDDEN.contains(&e.file_name().to_string_lossy().as_ref()))
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.path(), is_dir)
        })
        .collect();
    entries.sort_by_cached_key(|(path, is_dir)| {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        (!is_dir, name)
    });
    entries
}

/// All files under `root` (relative, `/`-separated, sorted) and whether the
/// list was cut short at `MAX_FILES`.
fn index_files(root: &Path) -> (Vec<String>, bool) {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name();
            if skipped(&name.to_string_lossy()) {
                continue;
            }
            // Symlinked folders are skipped so a link loop can't recurse forever.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(rel) = entry.path().strip_prefix(root) {
                files.push(rel.to_string_lossy().replace('\\', "/"));
                if files.len() >= MAX_FILES {
                    files.sort();
                    return (files, true);
                }
            }
        }
    }
    files.sort();
    (files, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_files_but_skips_vcs_and_build_folders() {
        let root = std::env::temp_dir().join(format!("text_editor_ws_{}", std::process::id()));
        for dir in ["src/nested", ".git", "target/debug"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in ["Cargo.toml", "src/main.rs", "src/nested/x.rs", ".git/HEAD", "target/debug/bin"] {
            std::fs::write(root.join(file), "").unwrap();
        }

        let (files, truncated) = index_files(&root);
        assert_eq!(files, vec!["Cargo.toml", "src/main.rs", "src/nested/x.rs"]);
        assert!(!truncated);

        // The side bar hides .git but still shows target, folders first.
        let names: Vec<String> = list_dir(&root)
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["src", "target", "Cargo.toml"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
