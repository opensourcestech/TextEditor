//! Packages. A package is a folder in `Packages/` or a zip renamed to
//! `.te-package` in `Installed Packages/`, holding color schemes
//! (`.te-color-scheme`, `.tmTheme`), languages (`.te-syntax`), snippets
//! (`.te-snippet`), command palette entries (`.te-commands`), key bindings
//! (`Default.te-keymap`), macros (`.te-macro`), menus (`Main.te-menu`,
//! `Context.te-menu`…) and Python plugins.
//!
//! Sublime Text's packages work as they are: each `.te-` name has a
//! `.sublime-` twin (`.sublime-package`, `.sublime-syntax`…) that's read
//! the same way.
//!
//! A file in `Packages/<Name>/` replaces the same file inside the package's
//! archive, so one file of an installed package can be changed without
//! unpacking it. `User` loads last, so its files win.

pub mod keymap;
pub mod macros;
pub mod menu_file;
pub mod package_index;

use std::collections::BTreeMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zip::ZipArchive;

/// The user's own package: a folder, loaded last.
pub const USER: &str = "User";
/// TextEditor's own prefix for package files, then Sublime Text's.
const PREFIXES: [&str; 2] = ["te-", "sublime-"];
/// What installed archives are called; new ones get the first.
const ARCHIVE_EXTS: [&str; 2] = ["te-package", "sublime-package"];
const METADATA_FILE: &str = "package-metadata.json";
/// The menu files TextEditor reads, by name.
const MENUS: [&str; 4] =
    [crate::packages::menu_file::MAIN, crate::packages::menu_file::CONTEXT, crate::packages::menu_file::SIDE_BAR, crate::packages::menu_file::TAB_CONTEXT];
/// Key bindings for every platform, then for this one: `Default.te-keymap`
/// and `Default (OSX).te-keymap`, say.
const KEYMAPS: [&str; 2] = [
    "Default",
    if cfg!(target_os = "macos") {
        "Default (OSX)"
    } else if cfg!(windows) {
        "Default (Windows)"
    } else {
        "Default (Linux)"
    },
];

/// Whether a file is of a kind of package file: `x.te-<kind>`, or Sublime
/// Text's `x.sublime-<kind>`.
pub fn is_kind(path: &str, kind: &str) -> bool {
    PREFIXES.iter().any(|p| has_extension(path, &format!("{p}{kind}")))
}

/// A file's name without its folders or extension: `Main` for `menus/Main.te-menu`.
pub fn base_name(path: &str) -> &str {
    let name = file_name(path);
    name.rsplit_once('.').map_or(name, |(base, _)| base)
}

/// Which key bindings file this is: 0 for every platform's, 1 for this
/// one's. Only at the top of a package.
fn keymap_rank(path: &str) -> Option<usize> {
    (is_kind(path, "keymap") && !path.contains('/')).then(|| KEYMAPS.iter().position(|k| *k == base_name(path))).flatten()
}

fn is_menu(path: &str) -> bool {
    is_kind(path, "menu") && MENUS.contains(&base_name(path))
}
/// Bigger files are skipped. No theme or syntax comes close, and it stops a
/// malformed zip from filling memory.
const MAX_FILE_BYTES: u64 = 16 << 20;
/// How deep to look inside package folders, which also stops symlink loops.
const MAX_DEPTH: usize = 8;

/// `package-metadata.json` in a package's root, as the package index (and
/// Sublime Text's Package Control)
/// writes it. Every field is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub version: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug)]
pub struct Package {
    pub name: String,
    /// `Installed Packages/<name>.te-package` (or `.sublime-package`), if there is one.
    pub archive: Option<PathBuf>,
    /// `Packages/<name>/`, if there is one; its files override the archive's.
    pub folder: Option<PathBuf>,
    pub metadata: Metadata,
}

/// A file read from a package.
#[derive(Clone)]
pub struct Resource {
    pub package: String,
    /// Path inside the package, with `/` separators.
    pub path: String,
    pub data: Vec<u8>,
}

impl Resource {
    /// The file name without its extension, e.g. `Dracula` for
    /// `themes/Dracula.tmTheme`. Themes are listed under this name.
    pub fn stem(&self) -> &str {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        name.rsplit_once('.').map_or(name, |(stem, _)| stem)
    }

    pub fn text(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.data).map_err(|_| format!("{} is not UTF-8 text", self.label()))
    }

    /// Where the file came from, for error messages.
    pub fn label(&self) -> String {
        format!("{}/{}", self.package, self.path)
    }
}

/// Everything read from the enabled packages, in load order: a later file
/// with the same name replaces an earlier one.
#[derive(Default)]
pub struct Loaded {
    /// `.tmTheme` and `.sublime-color-scheme` files.
    pub themes: Vec<Resource>,
    pub syntaxes: Vec<Resource>,
    pub snippets: Vec<Resource>,
    pub commands: Vec<Resource>,
    pub keymaps: Vec<Resource>,
    pub macros: Vec<Resource>,
    pub menus: Vec<Resource>,
    /// Packages that could not be read.
    pub errors: Vec<String>,
}

/// The two package folders inside the settings folder.
#[derive(Clone, Debug)]
pub struct Dirs {
    /// Unpacked packages and overrides, including `User`.
    pub packages: PathBuf,
    /// `.te-package` (and `.sublime-package`) archives.
    pub installed: PathBuf,
    /// Prepared package languages, which are slow to build (see `highlight::Syntaxes`).
    pub cache: PathBuf,
}

impl Dirs {
    /// Inside `~/Library/Application Support/TextEditor`. Tests get none, so
    /// they never see each other's packages; they use [`Dirs::at`].
    pub fn default_location() -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        crate::workspace::settings::config_dir().map(|d| Self::at(&d))
    }

    pub fn at(root: &Path) -> Self {
        Self { packages: root.join("Packages"), installed: root.join("Installed Packages"), cache: root.join("Cache") }
    }

    /// Creates both folders and `Packages/User`, so there is somewhere to put files.
    pub fn create(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.packages.join(USER))?;
        std::fs::create_dir_all(&self.installed)
    }

    /// Every package, alphabetical with `User` last: the order they load in.
    pub fn list(&self) -> Vec<Package> {
        let mut found: BTreeMap<String, Package> = BTreeMap::new();
        fn entry(found: &mut BTreeMap<String, Package>, name: String) -> &mut Package {
            found.entry(name.clone()).or_insert_with(|| Package {
                name,
                archive: None,
                folder: None,
                metadata: Metadata::default(),
            })
        }
        for path in children(&self.installed) {
            if path.is_file()
                && path.extension().is_some_and(|e| ARCHIVE_EXTS.iter().any(|x| e == *x))
                && let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
            {
                entry(&mut found, name).archive = Some(path);
            }
        }
        for path in children(&self.packages) {
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned)
            {
                entry(&mut found, name).folder = Some(path);
            }
        }
        let mut list: Vec<Package> = found.into_values().collect();
        for package in &mut list {
            package.metadata = package.read_metadata();
        }
        list.sort_by_key(|p| (p.name == USER, p.name.to_lowercase()));
        list
    }

    /// Copies a `.te-package`, `.sublime-package` or `.zip` file into
    /// `Installed Packages` as `<name>.te-package`, replacing any older copy,
    /// and returns the package's name.
    pub fn install(&self, file: &Path) -> Result<String, String> {
        let name = install_name(file).ok_or_else(|| format!("{} has no usable package name", file.display()))?;
        // Only replace an installed copy with something that can be read.
        open_archive(file)?;
        std::fs::create_dir_all(&self.installed).map_err(|e| format!("{}: {e}", self.installed.display()))?;
        let dest = self.installed.join(format!("{name}.{}", ARCHIVE_EXTS[0]));
        let tmp = dest.with_extension("tmp");
        std::fs::copy(file, &tmp)
            .and_then(|_| std::fs::rename(&tmp, &dest))
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        // An older copy under Sublime's name would load as well.
        for ext in &ARCHIVE_EXTS[1..] {
            let _ = std::fs::remove_file(self.installed.join(format!("{name}.{ext}")));
        }
        Ok(name)
    }

    /// Deletes an installed archive. A folder of the same name in `Packages/`
    /// is kept, since it may hold the user's own changes.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let paths: Vec<PathBuf> = ARCHIVE_EXTS.iter().map(|ext| self.installed.join(format!("{name}.{ext}"))).filter(|p| p.is_file()).collect();
        if paths.is_empty() {
            return Err(format!("{name} isn't installed"));
        }
        for path in paths {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }

    /// Changes whenever a file in either folder is added, removed or saved,
    /// so packages are only read again when something changed.
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for dir in [&self.installed, &self.packages] {
            walk(dir, 0, &mut |path, meta| {
                path.hash(&mut h);
                meta.len().hash(&mut h);
                meta.modified().ok().hash(&mut h);
            });
        }
        h.finish()
    }
}

/// Reads the themes and syntaxes of every package not in `ignored`.
pub fn load(packages: &[Package], ignored: &[String]) -> Loaded {
    let mut loaded = Loaded::default();
    for package in packages.iter().filter(|p| !ignored.contains(&p.name)) {
        let files = match package.files() {
            Ok(files) => files,
            Err(e) => {
                loaded.errors.push(format!("Package {}: {e}", package.name));
                continue;
            }
        };
        let mut keymaps = Vec::new();
        for (path, data) in files {
            let resource = Resource { package: package.name.clone(), path, data };
            let path = resource.path.as_str();
            let list = if is_kind(path, "color-scheme") || has_extension(path, "tmTheme") {
                &mut loaded.themes
            } else if is_kind(path, "syntax") {
                &mut loaded.syntaxes
            } else if is_kind(path, "snippet") {
                &mut loaded.snippets
            } else if is_kind(path, "commands") {
                &mut loaded.commands
            } else if is_kind(path, "macro") {
                &mut loaded.macros
            } else if is_menu(path) {
                &mut loaded.menus
            } else {
                &mut keymaps
            };
            list.push(resource);
        }
        // The platform's own key bindings come after the shared ones, so they win.
        keymaps.sort_by_key(|k: &Resource| keymap_rank(&k.path));
        loaded.keymaps.extend(keymaps);
    }
    loaded
}

impl Package {
    /// Whether the package has Python plugins: `.py` files at its top.
    pub fn has_plugins(&self) -> bool {
        let in_folder = self.folder.as_ref().is_some_and(|folder| {
            children(folder).iter().any(|p| p.is_file() && p.extension().is_some_and(|e| e == "py"))
        });
        in_folder
            || self.archive.as_ref().and_then(|a| open_archive(a).ok()).is_some_and(|(zip, root)| {
                zip.file_names()
                    .filter_map(Result::ok)
                    .any(|n| n.strip_prefix(root.as_str()).is_some_and(|rest| rest.ends_with(".py") && !rest.contains('/')))
            })
    }

    /// Where the package's files start inside its archive: "" or a GitHub
    /// zip's top folder, like `Name-main/`.
    pub fn archive_root(&self) -> Option<String> {
        open_archive(self.archive.as_ref()?).ok().map(|(_, root)| root)
    }

    /// The package's files that TextEditor uses, by path: the archive's, then
    /// the folder's, which replace any with the same path.
    fn files(&self) -> Result<BTreeMap<String, Vec<u8>>, String> {
        let mut files = BTreeMap::new();
        if let Some(archive) = &self.archive {
            let (mut zip, root) = open_archive(archive)?;
            let names: Vec<String> = zip.file_names().filter_map(Result::ok).map(String::from).collect();
            for name in names {
                let Some(path) = name.strip_prefix(&root).filter(|p| is_resource(p)) else {
                    continue;
                };
                let entry = zip.by_name(&name).map_err(|e| format!("{name}: {e}"))?;
                if entry.size() <= MAX_FILE_BYTES {
                    let mut data = Vec::new();
                    entry.take(MAX_FILE_BYTES).read_to_end(&mut data).map_err(|e| format!("{name}: {e}"))?;
                    files.insert(path.to_owned(), data);
                }
            }
        }
        if let Some(folder) = &self.folder {
            walk(folder, 0, &mut |path, meta| {
                let Some(rel) = relative(folder, path).filter(|p| is_resource(p)) else {
                    return;
                };
                if meta.len() <= MAX_FILE_BYTES
                    && let Ok(data) = std::fs::read(path)
                {
                    files.insert(rel, data);
                }
            });
        }
        Ok(files)
    }

    /// One file of the package as text: from its folder, else its archive.
    pub fn read(&self, path: &str) -> Option<String> {
        if let Some(folder) = &self.folder
            && let Ok(text) = std::fs::read_to_string(folder.join(path))
        {
            return Some(text);
        }
        let (mut zip, root) = open_archive(self.archive.as_ref()?).ok()?;
        let mut text = String::new();
        zip.by_name(&format!("{root}{path}")).ok()?.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
        Some(text)
    }

    /// `package-metadata.json` from the folder, else from the archive.
    fn read_metadata(&self) -> Metadata {
        let from_folder = || std::fs::read(self.folder.as_ref()?.join(METADATA_FILE)).ok();
        let from_archive = || {
            let (mut zip, root) = open_archive(self.archive.as_ref()?).ok()?;
            let mut data = Vec::new();
            zip.by_name(&format!("{root}{METADATA_FILE}")).ok()?.take(MAX_FILE_BYTES).read_to_end(&mut data).ok()?;
            Some(data)
        };
        from_folder()
            .or_else(from_archive)
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }
}

type Archive = ZipArchive<BufReader<File>>;

/// Opens a zip and finds its root: GitHub's downloads put everything in one
/// top-level folder (`Name-main/`), which is skipped, as Package Control does.
fn open_archive(path: &Path) -> Result<(Archive, String), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let zip = ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("{} is not a zip file: {e}", path.display()))?;
    let root = {
        let mut tops = zip.file_names().filter_map(Result::ok).map(|n| match n.split_once('/') {
            Some((top, _)) => format!("{top}/"),
            // A file at the top level: the root is the zip itself.
            None => String::new(),
        });
        let first = tops.next().unwrap_or_default();
        if !first.is_empty() && tops.all(|t| t == first) { first } else { String::new() }
    };
    Ok((zip, root))
}

/// Writes `package-metadata.json` into a downloaded archive, inside its
/// root, as Package Control does, so the version shows in the menus and
/// upgrades can tell what's installed. Archives that have one keep theirs.
pub fn add_metadata(archive: &Path, metadata: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let (zip, root) = open_archive(archive)?;
    let name = format!("{root}{METADATA_FILE}");
    if zip.file_names().any(|n| n.is_ok_and(|n| n == name.as_str())) {
        return Ok(());
    }
    drop(zip);
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", archive.display());
    let file = std::fs::OpenOptions::new().read(true).write(true).open(archive).map_err(|e| fail(&e))?;
    let mut writer = zip::ZipWriter::new_append(file).map_err(|e| fail(&e))?;
    writer.start_file(name, zip::write::SimpleFileOptions::default()).map_err(|e| fail(&e))?;
    writer.write_all(serde_json::to_string_pretty(metadata).unwrap_or_default().as_bytes()).map_err(|e| fail(&e))?;
    writer.finish().map_err(|e| fail(&e))?;
    Ok(())
}

/// The name an archive is installed under: its file name, minus the `-main`
/// or `-master` that GitHub adds to downloaded zips.
fn install_name(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    let name = stem.strip_suffix("-main").or_else(|| stem.strip_suffix("-master")).unwrap_or(stem);
    (!name.is_empty() && name != USER).then(|| name.to_owned())
}

fn is_resource(path: &str) -> bool {
    ["color-scheme", "syntax", "snippet", "commands", "macro"].iter().any(|kind| is_kind(path, kind))
        || has_extension(path, "tmTheme")
        || keymap_rank(path).is_some()
        || is_menu(path)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Reads JSON the way package files are written: with `//` and `/* */` comments and
/// trailing commas allowed.
pub fn parse_json(text: &str) -> Result<serde_json::Value, String> {
    // First drop comments, then commas that only have `]` or `}` after them.
    let mut plain = String::with_capacity(text.len());
    each_outside_strings(text, &mut plain, |rest| {
        if let Some(after) = rest.strip_prefix("//") {
            Some(after.find('\n').map_or("", |i| &after[i..]))
        } else if let Some(after) = rest.strip_prefix("/*") {
            Some(after.find("*/").map_or("", |i| &after[i + 2..]))
        } else {
            None
        }
    });
    let mut json = String::with_capacity(plain.len());
    each_outside_strings(&plain, &mut json, |rest| {
        let after = rest.strip_prefix(',')?;
        after.trim_start().starts_with([']', '}']).then_some(after)
    });
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

/// Copies `text` to `out`, letting `skip` drop text outside string literals:
/// given the rest of the input, it returns what to continue from instead.
fn each_outside_strings(text: &str, out: &mut String, skip: impl Fn(&str) -> Option<&str>) {
    let mut rest = text;
    let mut in_string = false;
    while let Some(c) = rest.chars().next() {
        if !in_string && let Some(next) = skip(rest) {
            rest = next;
            continue;
        }
        let len = if in_string && c == '\\' { rest.chars().take(2).map(char::len_utf8).sum() } else { c.len_utf8() };
        out.push_str(&rest[..len]);
        if c == '"' {
            in_string = !in_string;
        }
        rest = &rest[len..];
    }
}

fn has_extension(path: &str, ext: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

/// Visible entries of a folder; none if it doesn't exist.
fn children(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}

/// Calls `f` for every visible file under `dir`, in a stable order.
fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path, &std::fs::Metadata)) {
    if depth > MAX_DEPTH {
        return;
    }
    for path in children(dir) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, depth + 1, f);
        } else {
            f(&path, &meta);
        }
    }
}

/// `path` relative to `base`, with `/` separators.
fn relative(base: &Path, path: &Path) -> Option<String> {
    let parts: Vec<&str> = path.strip_prefix(base).ok()?.iter().map(|p| p.to_str()).collect::<Option<_>>()?;
    Some(parts.join("/"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;

    pub const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>name</key><string>Fixture</string>
  <key>settings</key><array><dict><key>settings</key><dict>
    <key>background</key><string>#102030</string>
    <key>foreground</key><string>#E0E0E0</string>
  </dict></dict></array>
</dict></plist>
"#;

    pub const SYNTAX: &str = r#"%YAML 1.2
---
name: Fancy
file_extensions: [fancy]
scope: source.fancy
contexts:
  main:
    - match: '\bfancy\b'
      scope: keyword.fancy
"#;

    /// A fresh, empty folder to act as the settings folder.
    pub fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("text_editor_packages_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a zip holding `files` (path, contents).
    pub fn write_zip(path: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, text) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn summary(loaded: &Loaded) -> Vec<(String, String)> {
        loaded
            .themes
            .iter()
            .chain(&loaded.syntaxes)
            .map(|r| (r.label(), String::from_utf8_lossy(&r.data).into_owned()))
            .collect()
    }

    #[test]
    fn folders_override_archives_and_user_loads_last() {
        let root = temp_root("override");
        let dirs = Dirs::at(&root);
        // A GitHub-style zip with everything inside one top-level folder.
        write_zip(
            &dirs.installed.join("Night.sublime-package"),
            &[
                ("night-main/package-metadata.json", r#"{ "version": "1.2.0" }"#),
                ("night-main/Night.tmTheme", "zipped theme"),
                ("night-main/syntaxes/Fancy.sublime-syntax", "zipped syntax"),
                ("night-main/README.md", "not a resource"),
            ],
        );
        write(&dirs.packages.join("Night/syntaxes/Fancy.sublime-syntax"), "override");
        write(&dirs.packages.join("User/Mine.tmTheme"), "user theme");
        write(&dirs.packages.join("Alpha/Alpha.tmTheme"), "alpha theme");
        write(&dirs.packages.join(".git/ignored.tmTheme"), "hidden");

        let list = dirs.list();
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Night", "User"]);
        let night = &list[1];
        assert!(night.archive.is_some() && night.folder.is_some());
        assert_eq!(night.metadata.version.as_deref(), Some("1.2.0"));

        let loaded = load(&list, &[]);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let s = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            summary(&loaded),
            [
                s("Alpha/Alpha.tmTheme", "alpha theme"),
                s("Night/Night.tmTheme", "zipped theme"),
                s("User/Mine.tmTheme", "user theme"),
                s("Night/syntaxes/Fancy.sublime-syntax", "override"),
            ]
        );
        assert_eq!(loaded.themes[1].stem(), "Night");

        // Ignored packages are skipped entirely.
        let loaded = load(&list, &["Night".to_owned()]);
        assert_eq!(loaded.themes.len(), 2);
        assert!(loaded.syntaxes.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn install_and_remove_archives() {
        let root = temp_root("install");
        let dirs = Dirs::at(&root);
        let download = root.join("Downloads/Night-main.zip");
        write_zip(&download, &[("Night-main/Night.tmTheme", THEME)]);

        let before = dirs.fingerprint();
        assert_eq!(dirs.install(&download).unwrap(), "Night");
        assert!(dirs.installed.join("Night.te-package").is_file());
        assert_ne!(dirs.fingerprint(), before, "installing changes the fingerprint");
        // Installed again from a file under Sublime's name: one copy, under ours.
        let sublime = root.join("Downloads/Night.sublime-package");
        std::fs::copy(&download, &sublime).unwrap();
        std::fs::copy(&download, dirs.installed.join("Night.sublime-package")).unwrap();
        dirs.install(&sublime).unwrap();
        assert!(!dirs.installed.join("Night.sublime-package").exists());
        assert_eq!(load(&dirs.list(), &[]).themes[0].label(), "Night/Night.tmTheme");

        // Something that isn't a zip is refused and installs nothing.
        let bad = root.join("Downloads/Broken.sublime-package");
        write(&bad, "not a zip");
        assert!(dirs.install(&bad).is_err());
        assert_eq!(dirs.list().len(), 1);

        // Removing keeps the user's own folder of overrides.
        write(&dirs.packages.join("Night/Night.tmTheme"), "mine");
        dirs.remove("Night").unwrap();
        let list = dirs.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].archive.is_none() && list[0].folder.is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn collects_every_kind_of_package_file() {
        let root = temp_root("kinds");
        let dirs = Dirs::at(&root);
        let tools = dirs.packages.join("Tools");
        let platform = format!("{}.te-keymap", KEYMAPS[1]);
        for name in [
            "Sea.sublime-color-scheme",
            "Ink.te-color-scheme",
            "snippets/fn.sublime-snippet",
            "snippets/let.te-snippet",
            "Tools.sublime-commands",
            "Mine.te-commands",
            "Stamp.te-macro",
            "Main.te-menu",
            "Context.sublime-menu",
            // Not a menu TextEditor has.
            "Other.te-menu",
            &platform,
            "Default.sublime-keymap",
            // Key bindings only count at the top of a package.
            "nested/Default.te-keymap",
            "plugin.py",
        ] {
            write(&tools.join(name), "x");
        }
        let loaded = load(&dirs.list(), &[]);
        let paths = |list: &[Resource]| list.iter().map(|r| r.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&loaded.themes), ["Ink.te-color-scheme", "Sea.sublime-color-scheme"]);
        assert_eq!(paths(&loaded.snippets), ["snippets/fn.sublime-snippet", "snippets/let.te-snippet"]);
        assert_eq!(paths(&loaded.commands), ["Mine.te-commands", "Tools.sublime-commands"]);
        assert_eq!(paths(&loaded.macros), ["Stamp.te-macro"]);
        assert_eq!(paths(&loaded.menus), ["Context.sublime-menu", "Main.te-menu"]);
        assert_eq!(paths(&loaded.keymaps), ["Default.sublime-keymap".to_owned(), platform], "the platform's own bindings load last");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_json_with_comments_and_trailing_commas() {
        let value = parse_json(
            r#"// keys
            [
                { "keys": ["super+k"], /* inline */ "text": "a // not a comment, \"q\"", },
                { "url": "http://x/*y*/", }, // trailing
            ]"#,
        )
        .unwrap();
        assert_eq!(value[0]["text"], "a // not a comment, \"q\"");
        assert_eq!(value[1]["url"], "http://x/*y*/");
        assert!(parse_json("[1, 2").is_err());
    }

    #[test]
    fn broken_archives_are_reported_not_fatal() {
        let root = temp_root("broken");
        let dirs = Dirs::at(&root);
        write(&dirs.installed.join("Bad.sublime-package"), "not a zip");
        write(&dirs.packages.join("User/Mine.tmTheme"), "user theme");
        let loaded = load(&dirs.list(), &[]);
        assert_eq!(loaded.errors.len(), 1);
        assert!(loaded.errors[0].starts_with("Package Bad:"), "{}", loaded.errors[0]);
        assert_eq!(loaded.themes.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
