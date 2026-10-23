//! An online package index: channels
//! (like Package Control's own, which lists thousands of packages) and
//! repositories say which packages there are and where each release can be
//! downloaded. Files are fetched with `curl`, which macOS, Windows 10+ and
//! most Linux systems have, so the editor needs no web client of its own.
//!
//! Plugins can need libraries (`requests`, `certifi`…): a package lists them
//! in its `dependencies.json`, and they're installed from the index into the
//! `Lib/python3` folder the plugin host imports from.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// Package Control's channel: every package it lists, with releases.
pub const DEFAULT_CHANNEL: &str = "https://packagecontrol.io/channel_v3.json";
/// Package Control 4's channel, which lists the libraries plugins use.
pub const LIBRARY_CHANNEL: &str = "https://packagecontrol.github.io/channel/channel_v4.json";
/// For compatibility with Sublime Text's packages, the Sublime Text build
/// this editor counts as when a package needs a certain one. It's what
/// `version()` tells plugins.
pub const API_BUILD: u32 = 4200;
/// The key a release in Package Control's channels and repositories gives
/// the Sublime Text builds it's for (`">=4000"`, `"3000 - 4200"`, `"*"`). The
/// name is Package Control's, so it has to stay as it is.
const BUILD_KEY: &str = "sublime_text";
/// A channel fetched less than this long ago is read from the cache.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const DOWNLOAD_SECONDS: &str = "300";

/// A package the index offers, with the release to install.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexPackage {
    pub name: String,
    pub description: String,
    pub homepage: String,
    pub version: String,
    pub url: String,
}

/// A library plugins can import, with its releases for this system: Python
/// wheels, or older "dependencies" (zips with modules in folders like `all/`).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexLibrary {
    pub name: String,
    pub releases: Vec<LibraryRelease>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryRelease {
    pub version: String,
    pub url: String,
    /// The Python versions it's for, like `3.8` or `3.13`; none means any.
    pub python_versions: Vec<String>,
}

impl IndexLibrary {
    /// The release for this Python version (`3.12`, say): the newest made
    /// for it, else for the nearest Python 3 version below it, else one for
    /// any version.
    pub fn release_for(&self, python: &str) -> Option<&LibraryRelease> {
        let version = |v: &str| -> (u32, u32) {
            let mut parts = v.split('.').map(|n| n.parse().unwrap_or(0));
            (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
        };
        let want = version(python);
        let fits = |r: &&LibraryRelease| -> Option<(u32, u32)> {
            if r.python_versions.is_empty() {
                return Some((0, 0));
            }
            r.python_versions.iter().map(|v| version(v)).filter(|v| v.0 == want.0 && *v <= want).max()
        };
        self.releases
            .iter()
            .filter_map(|r| Some((fits(&r)?, r)))
            .max_by(|(a, r), (b, q)| a.cmp(b).then_with(|| compare_versions(&r.version, &q.version)))
            .map(|(_, r)| r)
    }
}

/// Everything the channels and repositories offer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    pub packages: Vec<IndexPackage>,
    pub libraries: Vec<IndexLibrary>,
}

/// The libraries listed in a channel or repository file: Package Control
/// 4's `libraries`, and the older `dependencies`.
fn parse_libraries(json: &Value) -> Vec<IndexLibrary> {
    let mut entries: Vec<&Value> = Vec::new();
    for key in ["libraries_cache", "dependencies_cache"] {
        if let Some(cache) = json.get(key).and_then(Value::as_object) {
            entries.extend(cache.values().filter_map(Value::as_array).flatten());
        }
    }
    for key in ["libraries", "dependencies"] {
        entries.extend(json.get(key).and_then(Value::as_array).into_iter().flatten());
    }
    let mut found: Vec<IndexLibrary> = Vec::new();
    for entry in entries {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let releases: Vec<LibraryRelease> = entry
            .get("releases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|r| usable(r))
            .filter_map(|r| {
                Some(LibraryRelease {
                    version: r.get("version")?.as_str()?.to_owned(),
                    url: r.get("url")?.as_str()?.to_owned(),
                    python_versions: r
                        .get("python_versions")
                        .and_then(Value::as_array)
                        .map(|v| v.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default(),
                })
            })
            .collect();
        // Package Control 4's libraries win over the older dependencies.
        if !releases.is_empty() && !found.iter().any(|l| same_library(&l.name, name)) {
            found.push(IndexLibrary { name: name.to_owned(), releases });
        }
    }
    found
}

/// Library names compare without case, with `-` and `_` the same.
pub fn same_library(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace('-', "_");
    norm(a) == norm(b)
}

/// The libraries a package's `dependencies.json` asks for on this system:
/// `{ "<platform>": { "<build spec>": ["name", …] } }`, the most specific
/// platform (`osx-arm64`, then `osx`, then `*`) and first matching build.
pub fn required_libraries(json: &Value) -> Vec<String> {
    let Some(platforms) = json.as_object() else {
        return Vec::new();
    };
    let os = platform_name();
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let by_build = [format!("{os}-{arch}"), os.to_owned(), "*".to_owned()].into_iter().find_map(|k| platforms.get(&k));
    let Some(by_build) = by_build.and_then(Value::as_object) else {
        return Vec::new();
    };
    by_build
        .iter()
        .find(|(spec, _)| build_matches(spec, API_BUILD))
        .and_then(|(_, names)| names.as_array())
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

const INSTALLED_LIBRARIES: &str = ".libraries.json";

/// The libraries installed in `lib_dir`, with their versions.
pub fn installed_libraries(lib_dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(lib_dir.join(INSTALLED_LIBRARIES))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Of `names`, those not installed in `lib_dir` yet.
pub fn missing_libraries(names: &[String], lib_dir: &Path) -> Vec<String> {
    let installed = installed_libraries(lib_dir);
    names.iter().filter(|n| !installed.keys().any(|i| same_library(i, n))).cloned().collect()
}

/// Downloads a library's release and unpacks it into `lib_dir`: a wheel as
/// it is, an older dependency zip from its folder for this system.
pub fn install_library(name: &str, release: &LibraryRelease, lib_dir: &Path, cache: &Path) -> Result<(), String> {
    let file = cache.join("downloads").join(format!("{name}.zip"));
    fetch(&release.url, &file)?;
    let result = unpack_library(&file, release.url.ends_with(".whl"), lib_dir);
    let _ = std::fs::remove_file(&file);
    result?;
    let mut installed = installed_libraries(lib_dir);
    installed.insert(name.to_owned(), release.version.clone());
    let json = serde_json::to_string_pretty(&installed).unwrap_or_default();
    std::fs::write(lib_dir.join(INSTALLED_LIBRARIES), json).map_err(|e| format!("{}: {e}", lib_dir.display()))
}

fn unpack_library(file: &Path, wheel: bool, lib_dir: &Path) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", file.display());
    let mut zip = zip::ZipArchive::new(std::fs::File::open(file).map_err(|e| fail(&e))?).map_err(|e| fail(&e))?;
    let names: Vec<PathBuf> = (0..zip.len()).filter_map(|i| zip.by_index(i).ok()?.enclosed_name()).collect();
    // Where the modules are inside the zip, and which parts to leave out.
    let from: PathBuf = if wheel {
        PathBuf::new()
    } else {
        // GitHub's zips have one top folder; the modules are in a folder
        // for this system inside it, or in `all/`.
        let top = names.first().and_then(|n| n.components().next()).map(|c| PathBuf::from(c.as_os_str()));
        let top = top.filter(|t| names.iter().all(|n| n.starts_with(t))).unwrap_or_default();
        let os = platform_name();
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
        let folders = ["st4_py38", "st4", "st3"]
            .iter()
            .flat_map(|v| [format!("{v}_{os}_{arch}"), format!("{v}_{os}"), (*v).to_owned()])
            .chain(["all".to_owned()]);
        let found = folders.map(|f| top.join(f)).find(|f| names.iter().any(|n| n.starts_with(f) && n != f));
        found.ok_or_else(|| format!("{} has no folder for this system", file.display()))?
    };
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| fail(&e))?;
        // Never outside the folder, whatever the zip says.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(&from) else {
            continue;
        };
        let mut parts = rel.components();
        // A wheel's `name.data/purelib/…` goes in as if at the top; its
        // other `.data` folders (scripts, headers) aren't for importing.
        let rel = match parts.next().map(|c| c.as_os_str().to_string_lossy().into_owned()) {
            Some(first) if wheel && first.ends_with(".data") => match parts.next().map(|c| c.as_os_str().to_owned()) {
                Some(kind) if kind == "purelib" || kind == "platlib" => parts.as_path().to_path_buf(),
                _ => continue,
            },
            _ => rel.to_path_buf(),
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = lib_dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| fail(&e))?;
            continue;
        }
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| fail(&e))?;
        std::fs::write(&dest, data).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Reads a channel or repository file (Package Control's schema 2 to 4):
/// the packages with a release for this editor and system, and the
/// repositories it names whose packages it doesn't include.
pub fn parse(json: &Value) -> (Vec<IndexPackage>, Vec<String>) {
    let mut lists: Vec<&Value> = Vec::new();
    let mut cached: Vec<&str> = Vec::new();
    if let Some(cache) = json.get("packages_cache").and_then(Value::as_object) {
        for (repository, packages) in cache {
            cached.push(repository);
            lists.extend(packages.as_array().into_iter().flatten());
        }
    }
    lists.extend(json.get("packages").and_then(Value::as_array).into_iter().flatten());
    let mut found: Vec<IndexPackage> = Vec::new();
    for entry in lists {
        if let Some(package) = package(entry)
            && !found.iter().any(|p| p.name == package.name)
        {
            found.push(package);
        }
    }
    let more = json
        .get("repositories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|url| !cached.contains(url))
        .map(str::to_owned)
        .collect();
    (found, more)
}

fn package(entry: &Value) -> Option<IndexPackage> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let name = text("name");
    let best = entry
        .get("releases")?
        .as_array()?
        .iter()
        .filter(|r| usable(r))
        .filter_map(|r| Some((r.get("version")?.as_str()?, r.get("url")?.as_str()?)))
        .max_by(|a, b| compare_versions(a.0, b.0))?;
    if name.is_empty() {
        return None;
    }
    let homepage = if text("homepage").is_empty() { text("details") } else { text("homepage") };
    Some(IndexPackage { name, description: text("description"), homepage, version: best.0.to_owned(), url: best.1.to_owned() })
}

/// Whether a release is for this system and this editor (`API_BUILD`).
fn usable(release: &Value) -> bool {
    let platforms_ok = match release.get("platforms") {
        None => true,
        Some(Value::String(p)) => platform_matches(p),
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).any(platform_matches),
        Some(_) => false,
    };
    let build_ok = release.get(BUILD_KEY).and_then(Value::as_str).is_none_or(|spec| build_matches(spec, API_BUILD));
    platforms_ok && build_ok
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "osx"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn platform_matches(platform: &str) -> bool {
    let os = platform_name();
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    platform == "*" || platform == os || platform == format!("{os}-{arch}")
}

/// Build specs, as package indexes write them: `*`, `>3000`, `>=4000`, `<3000`, `<=4100` and
/// `3000 - 3999`.
fn build_matches(spec: &str, build: u32) -> bool {
    let spec = spec.trim();
    if spec == "*" {
        return true;
    }
    let number = |s: &str| s.trim().parse::<u32>().ok();
    if let Some((low, high)) = spec.split_once('-') {
        return matches!((number(low), number(high)), (Some(l), Some(h)) if l <= build && build <= h);
    }
    let (op, rest) = ["<=", ">=", "<", ">"].iter().find_map(|op| spec.strip_prefix(op).map(|r| (*op, r))).unwrap_or(("=", spec));
    let Some(n) = number(rest) else {
        return false;
    };
    match op {
        "<=" => build <= n,
        ">=" => build >= n,
        "<" => build < n,
        ">" => build > n,
        _ => build == n,
    }
}

/// Compares versions like `1.10.2`, `v2.0` or `2024.01.05.10.00.00` part by
/// part, as numbers.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<u64> { v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a).cmp(&parts(b))
}

/// Downloads `url` to `dest` with curl, through a temporary file so a
/// failed download never leaves half a file behind.
pub fn fetch(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = dest.with_extension("download");
    let output = std::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--max-time", DOWNLOAD_SECONDS, "--output"])
        .arg(&tmp)
        .arg(url)
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!("could not download {url}: {why}"));
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// Every package and library the channels and repositories offer. Each
/// file is read from `cache` when it was fetched recently, unless `refresh`.
pub fn load(channels: &[String], repositories: &[String], cache: &Path, refresh: bool) -> Result<Index, String> {
    let mut index = Index::default();
    let mut errors = Vec::new();
    let add = |json: &Value, index: &mut Index| -> Vec<String> {
        let (found, repos) = parse(json);
        for p in found {
            if !index.packages.iter().any(|q| q.name == p.name) {
                index.packages.push(p);
            }
        }
        for l in parse_libraries(json) {
            if !index.libraries.iter().any(|q| same_library(&q.name, &l.name)) {
                index.libraries.push(l);
            }
        }
        repos
    };
    let mut more: Vec<String> = repositories.to_vec();
    for url in channels {
        match read(url, cache, refresh) {
            Ok(json) => more.extend(add(&json, &mut index)),
            Err(e) => errors.push(e),
        }
    }
    for url in more {
        match read(&url, cache, refresh) {
            Ok(json) => {
                add(&json, &mut index);
            }
            Err(e) => errors.push(e),
        }
    }
    if index.packages.is_empty()
        && let Some(first) = errors.into_iter().next()
    {
        return Err(first);
    }
    index.packages.sort_by_key(|p| p.name.to_lowercase());
    Ok(index)
}

/// A channel or repository's JSON, from the cache if it's fresh.
fn read(url: &str, cache: &Path, refresh: bool) -> Result<Value, String> {
    let file = cache_file(url, cache);
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < MAX_AGE);
    if refresh || !fresh {
        fetch(url, &file)?;
    }
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    crate::packages::parse_json(&text).map_err(|e| format!("{url}: {e}"))
}

fn cache_file(url: &str, cache: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    cache.join("index").join(format!("{:016x}.json", h.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_the_newest_release_this_editor_can_use() {
        let channel = json!({
            "schema_version": "3.0.0",
            "repositories": ["https://a.example/repo.json", "https://b.example/repo.json"],
            "packages_cache": { "https://a.example/repo.json": [
                { "name": "Linter", "description": "Lints", "homepage": "https://x", "releases": [
                    { "sublime_text": ">=4000", "platforms": ["*"], "version": "1.9.0", "url": "u190" },
                    { "sublime_text": ">=4000", "platforms": ["*"], "version": "1.10.0", "url": "u1100" },
                    { "sublime_text": ">=9000", "platforms": ["*"], "version": "2.0.0", "url": "u200" },
                ] },
                { "name": "Old", "releases": [{ "sublime_text": "<3000", "platforms": ["*"], "version": "1", "url": "u" }] },
                { "name": "Elsewhere", "releases": [{ "sublime_text": "*", "platforms": ["nowhere"], "version": "1", "url": "u" }] },
            ] },
        });
        let (packages, more) = parse(&channel);
        assert_eq!(packages, vec![IndexPackage {
            name: "Linter".into(),
            description: "Lints".into(),
            homepage: "https://x".into(),
            version: "1.10.0".into(),
            url: "u1100".into(),
        }]);
        assert_eq!(more, vec!["https://b.example/repo.json"], "repositories the channel didn't include");
    }

    #[test]
    fn libraries_for_this_python_and_what_packages_need() {
        let channel = json!({
            "libraries_cache": { "r": [{ "name": "requests", "releases": [
                { "version": "2.0", "url": "old.whl", "python_versions": ["3.8"], "platforms": ["*"], "sublime_text": "*" },
                { "version": "2.1", "url": "new.whl", "python_versions": ["3.13"], "platforms": ["*"], "sublime_text": "*" },
                { "version": "2.2", "url": "elsewhere.whl", "python_versions": ["3.13"], "platforms": ["amiga"], "sublime_text": "*" },
            ] }] },
            "dependencies_cache": { "r": [
                { "name": "Requests", "releases": [{ "version": "1.0", "url": "dep.zip", "platforms": ["*"], "sublime_text": "*" }] },
                { "name": "pathlib", "releases": [{ "version": "1.0", "url": "path.zip", "platforms": ["*"], "sublime_text": "*" }] },
            ] },
        });
        let libraries = parse_libraries(&channel);
        assert_eq!(libraries.len(), 2, "the newer `requests` wins over the old dependency of that name");
        let requests = &libraries[0];
        assert_eq!(requests.release_for("3.13").unwrap().url, "new.whl");
        assert_eq!(requests.release_for("3.12").unwrap().url, "old.whl", "the nearest older Python");
        assert_eq!(libraries[1].release_for("3.12").unwrap().url, "path.zip", "for any Python");

        let os = platform_name();
        let mut deps = json!({ "*": { "<3000": ["old"], "*": ["requests", "pathlib"] } });
        deps[os] = json!({ ">=4000": ["mine"] });
        assert_eq!(required_libraries(&deps), ["mine"], "this system's own list");
        assert_eq!(required_libraries(&json!({ "*": { "*": ["a"] } })), ["a"]);
        assert!(required_libraries(&json!([])).is_empty());
    }

    #[test]
    fn unpacks_wheels_and_older_dependencies_safely() {
        use crate::packages::tests::write_zip;
        let dir = std::env::temp_dir().join(format!("text_editor_libs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("Lib");
        let wheel = dir.join("w.whl");
        write_zip(&wheel, &[
            ("fast/__init__.py", "x = 1"),
            ("fast-1.0.dist-info/METADATA", "m"),
            ("fast-1.0.data/purelib/extra.py", "y = 2"),
            ("fast-1.0.data/scripts/tool", "#!"),
            ("../escape.py", "no"),
        ]);
        unpack_library(&wheel, true, &lib).unwrap();
        assert!(lib.join("fast/__init__.py").is_file() && lib.join("extra.py").is_file());
        assert!(!lib.join("tool").exists() && !dir.join("escape.py").exists());

        let dep = dir.join("d.zip");
        write_zip(&dep, &[("thing-1.0/all/thing.py", "z"), ("thing-1.0/.sublime-dependency", "50")]);
        unpack_library(&dep, false, &lib).unwrap();
        assert!(lib.join("thing.py").is_file());

        let release = LibraryRelease { version: "1.0".into(), url: format!("file://{}", wheel.display()), python_versions: Vec::new() };
        install_library("fast", &release, &lib, &dir.join("cache")).unwrap();
        assert_eq!(missing_libraries(&["Fast".into(), "other".into()], &lib), ["other"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn builds_platforms_and_versions() {
        assert!(build_matches("*", 4200) && build_matches(">=3000", 4200) && build_matches("3000 - 4200", 4200));
        assert!(!build_matches("<3000", 4200) && !build_matches(">4200", 4200) && !build_matches("nonsense", 4200));
        assert!(platform_matches("*") && !platform_matches("amiga"));
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Greater);
        assert_eq!(compare_versions("v2.0", "2.0"), Equal);
        assert_eq!(compare_versions("2024.01.05", "2025.1.1"), Less);
    }

    #[test]
    fn loads_channels_through_the_cache() {
        let dir = std::env::temp_dir().join(format!("text_editor_index_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = dir.join("repo.json");
        std::fs::write(&repo, r#"{ "packages": [{ "name": "Zed", "releases": [{ "version": "1.0", "url": "file:///zed.zip" }] }] }"#).unwrap();
        let channel = dir.join("channel.json");
        let repo_url = format!("file://{}", repo.display());
        std::fs::write(&channel, json!({ "repositories": [repo_url] }).to_string()).unwrap();
        let channels = [format!("file://{}", channel.display())];
        let cache = dir.join("cache");
        let index = load(&channels, &[], &cache, false).unwrap();
        assert_eq!(index.packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Zed"]);
        // From the cache, even with the files gone.
        std::fs::remove_file(&repo).unwrap();
        assert_eq!(load(&channels, &[], &cache, false).unwrap().packages.len(), 1);
        assert!(load(&channels, &[], &cache, true).is_err(), "a refresh fetches them again");
        assert!(fetch("file:///no/such/file", &dir.join("x")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
