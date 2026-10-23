use std::path::{Path, PathBuf};

/// How many recent files and folders File → Open Recent remembers.
const MAX_RECENT: usize = 15;

/// Recently opened files and folders, newest first, kept between launches.
pub struct Recent {
    paths: Vec<PathBuf>,
    /// Where the list is saved; `None` keeps it in memory only (tests).
    file: Option<PathBuf>,
}

impl Recent {
    /// Loads `recent.json` from the settings folder.
    pub fn load() -> Self {
        let file = crate::settings::config_dir().map(|d| d.join("recent.json"));
        let paths = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|text| serde_json::from_str::<Vec<PathBuf>>(&text).ok())
            .unwrap_or_default();
        Self { paths, file }
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self { paths: Vec::new(), file: None }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Moves `path` to the top of the list.
    pub fn add(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.paths.retain(|p| *p != path);
        self.paths.insert(0, path);
        self.paths.truncate(MAX_RECENT);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.save();
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.save();
    }

    fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Losing the recent list isn't worth bothering the user about.
        if let Ok(text) = serde_json::to_string_pretty(&self.paths) {
            let _ = std::fs::write(file, text);
        }
    }
}

/// Menu label for a recent entry: `main.rs — ~/git/project/src`.
pub fn label(path: &Path) -> String {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let Some(parent) = path.parent() else {
        return name;
    };
    let mut dir = parent.display().to_string();
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = parent.strip_prefix(&home)
    {
        dir = Path::new("~").join(rest).display().to_string();
    }
    let slash = if path.is_dir() { "/" } else { "" };
    format!("{name}{slash} — {dir}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates() {
        let mut r = Recent::in_memory();
        r.add(Path::new("/nonexistent/a.rs"));
        r.add(Path::new("/nonexistent/b.rs"));
        r.add(Path::new("/nonexistent/a.rs"));
        assert_eq!(r.paths(), &[PathBuf::from("/nonexistent/a.rs"), PathBuf::from("/nonexistent/b.rs")]);
        r.remove(Path::new("/nonexistent/a.rs"));
        assert_eq!(r.paths(), &[PathBuf::from("/nonexistent/b.rs")]);
    }

    #[test]
    fn keeps_only_the_newest_entries() {
        let mut r = Recent::in_memory();
        for i in 0..MAX_RECENT + 5 {
            r.add(&PathBuf::from(format!("/nonexistent/{i}.txt")));
        }
        assert_eq!(r.paths().len(), MAX_RECENT);
        assert_eq!(r.paths()[0], PathBuf::from(format!("/nonexistent/{}.txt", MAX_RECENT + 4)));
    }

    #[test]
    fn labels_show_name_then_folder() {
        assert_eq!(label(Path::new("/nonexistent/src/main.rs")), "main.rs — /nonexistent/src");
    }
}
