//! Packages > Install Package…, Upgrade Packages and Install Missing
//! Libraries: the online package index (see `crate::packages::package_index`), with
//! downloads in the background.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui;
use serde_json::json;

use super::{EditorApp, Target};
use crate::packages::package_index::{self, Index, IndexPackage};
use crate::packages::{self, Dirs, Package};
use crate::ui::palette::{Candidate, Mode};

#[derive(Default)]
pub(super) struct IndexState {
    job: Option<Receiver<Done>>,
    /// What the job is doing, for the status bar.
    pub(super) busy: Option<String>,
    /// The packages listed while the palette offers them.
    pub(super) offer: Option<Vec<IndexPackage>>,
}

enum Done {
    Listed(Result<Index, String>),
    /// What was installed, as "name version", or what went wrong.
    Installed(Vec<Result<String, String>>),
}

/// What a job on another thread needs to install packages and libraries.
#[derive(Clone)]
pub(super) struct Installer {
    pub(super) dirs: Dirs,
    channels: Vec<String>,
    repositories: Vec<String>,
    /// The plugin host's Python version, like `3.12`, to pick libraries for.
    python: Option<String>,
}

impl Installer {
    /// Where libraries go: the folder the plugin host imports from.
    pub(super) fn lib_dir(dirs: &Dirs) -> PathBuf {
        dirs.packages.parent().unwrap_or(&dirs.packages).join("Lib").join("python3")
    }

    fn index(&self, refresh: bool) -> Result<Index, String> {
        package_index::load(&self.channels, &self.repositories, &self.dirs.cache, refresh)
    }

    /// Downloads a package's release and installs it, with its version
    /// noted inside, then the libraries it needs that aren't installed.
    pub(super) fn install(&self, package: &IndexPackage, index: &Index) -> Vec<Result<String, String>> {
        let file = self.dirs.cache.join("downloads").join(format!("{}.te-package", package.name));
        if let Err(e) = package_index::fetch(&package.url, &file) {
            return vec![Err(e)];
        }
        let metadata = json!({ "version": package.version, "url": package.homepage, "description": package.description });
        // Only for showing the version; the package works without it.
        let _ = packages::add_metadata(&file, &metadata);
        let installed = self.dirs.install(&file);
        let _ = std::fs::remove_file(&file);
        let name = match installed {
            Ok(name) => name,
            Err(e) => return vec![Err(format!("{}: {e}", package.name))],
        };
        let needs = self.dirs.list().into_iter().find(|p| p.name == name).map(|p| needed_libraries(&p)).unwrap_or_default();
        let mut results = vec![Ok(format!("{name} {}", package.version))];
        results.extend(self.install_libraries(&needs, index));
        results
    }

    /// Installs those of `names` that aren't installed yet.
    fn install_libraries(&self, names: &[String], index: &Index) -> Vec<Result<String, String>> {
        let lib_dir = Self::lib_dir(&self.dirs);
        let python = self.python.as_deref().unwrap_or("3.8");
        package_index::missing_libraries(names, &lib_dir)
            .into_iter()
            .map(|name| {
                let library = index.libraries.iter().find(|l| package_index::same_library(&l.name, &name));
                let release = library.and_then(|l| l.release_for(python));
                let release = release.ok_or_else(|| format!("no library {name} for Python {python} in the index"))?;
                package_index::install_library(&name, release, &lib_dir, &self.dirs.cache)?;
                Ok(format!("library {name} {}", release.version))
            })
            .collect()
    }
}

/// The libraries a package's `dependencies.json` asks for here.
pub(super) fn needed_libraries(package: &Package) -> Vec<String> {
    package
        .read("dependencies.json")
        .and_then(|text| packages::parse_json(&text).ok())
        .map(|json| package_index::required_libraries(&json))
        .unwrap_or_default()
}

impl EditorApp {
    fn installer(&self) -> Option<Installer> {
        let python = crate::plugins::find_python(&self.settings.python_path).and_then(|p| crate::plugins::python_version(&p));
        Some(Installer {
            dirs: self.packages.clone()?,
            channels: self.settings.package_channels.clone(),
            repositories: self.settings.package_repositories.clone(),
            python,
        })
    }

    /// Lists the index's packages in the palette, once it's loaded.
    pub(super) fn install_from_index(&mut self, ctx: &egui::Context) {
        self.start_index_job(ctx, "Loading the package index…", |installer| Done::Listed(installer.index(false)));
    }

    /// Installs a newer release of every installed package that has one.
    pub(super) fn upgrade_packages(&mut self, ctx: &egui::Context) {
        let installed: HashMap<String, String> = self
            .package_list
            .iter()
            .filter(|p| p.archive.is_some())
            .filter_map(|p| Some((p.name.clone(), p.metadata.version.clone()?)))
            .collect();
        self.start_index_job(ctx, "Looking for package upgrades…", move |installer| {
            let index = match installer.index(true) {
                Ok(index) => index,
                Err(e) => return Done::Installed(vec![Err(e)]),
            };
            let newer = index.packages.iter().filter(|p| {
                installed.get(&p.name).is_some_and(|v| package_index::compare_versions(&p.version, v).is_gt())
            });
            Done::Installed(newer.flat_map(|p| installer.install(p, &index)).collect())
        });
    }

    /// The libraries enabled packages need that aren't installed yet.
    pub(super) fn missing_libraries(&self) -> Vec<String> {
        let Some(dirs) = &self.packages else {
            return Vec::new();
        };
        let mut needed: Vec<String> = Vec::new();
        for package in self.package_list.iter().filter(|p| !self.settings.ignored_packages.contains(&p.name)) {
            for name in needed_libraries(package) {
                if !needed.iter().any(|n| package_index::same_library(n, &name)) {
                    needed.push(name);
                }
            }
        }
        package_index::missing_libraries(&needed, &Installer::lib_dir(dirs))
    }

    /// Packages > Install Missing Libraries.
    pub(super) fn install_missing_libraries(&mut self, ctx: &egui::Context) {
        let missing = self.missing_libraries();
        if missing.is_empty() {
            self.message = Some("Every library the packages need is installed".into());
            return;
        }
        self.start_index_job(ctx, "Installing libraries…", move |installer| match installer.index(false) {
            Ok(index) => Done::Installed(installer.install_libraries(&missing, &index)),
            Err(e) => Done::Installed(vec![Err(e)]),
        });
    }

    /// Runs `job` on a thread with what it needs to install things.
    fn start_index_job(&mut self, ctx: &egui::Context, busy: &str, job: impl FnOnce(Installer) -> Done + Send + 'static) {
        if self.index.job.is_some() {
            return;
        }
        let Some(installer) = self.installer() else {
            return;
        };
        let (send, receive) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = send.send(job(installer));
            wake.request_repaint();
        });
        self.index.job = Some(receive);
        self.index.busy = Some(busy.to_owned());
    }

    /// Picks up what a job in the background has done.
    pub(super) fn poll_index(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.index.job else {
            return;
        };
        let done = match job.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.index = IndexState::default();
                return;
            }
        };
        self.index.job = None;
        self.index.busy = None;
        match done {
            Done::Listed(Err(e)) => self.message = Some(format!("Could not load the package index: {e}")),
            Done::Listed(Ok(index)) => self.offer_packages(ctx, index.packages),
            Done::Installed(results) => self.finish_install(ctx, results),
        }
    }

    /// The palette lists the index's packages; picking one installs it.
    fn offer_packages(&mut self, ctx: &egui::Context, listed: Vec<IndexPackage>) {
        self.message = Some(format!("{} packages available", listed.len()));
        // A plugin's quick panel that was open is dismissed.
        self.answer_plugin_prompt(None);
        self.index.offer = Some(listed);
        self.palette.show_prompt(Mode::Quick, "Install a package", "", 0);
        (self.palette_candidates, self.palette_targets) = self.palette_rows(ctx, Mode::Quick).into_iter().unzip();
        self.focus_editor(ctx);
    }

    /// The palette's rows for the packages on offer.
    pub(super) fn index_rows(&self) -> Vec<(Candidate, Target)> {
        let Some(listed) = &self.index.offer else {
            return Vec::new();
        };
        let installed: HashMap<&str, Option<&str>> =
            self.package_list.iter().map(|p| (p.name.as_str(), p.metadata.version.as_deref())).collect();
        listed
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let state = match installed.get(p.name.as_str()) {
                    Some(Some(v)) if v == &p.version => " (installed)".to_owned(),
                    Some(Some(v)) => format!(" (installed: {v})"),
                    Some(None) => " (installed)".to_owned(),
                    None => String::new(),
                };
                // The version first: a long description is what gets cut short.
                let hint = format!("{}{state}   {}", p.version, p.description);
                (Candidate { text: p.name.clone(), title_start: 0, hint }, Target::IndexPackage(i))
            })
            .collect()
    }

    /// A package from the palette's list was picked: install it, with the
    /// libraries it needs.
    pub(super) fn pick_index_package(&mut self, ctx: &egui::Context, index: usize) {
        let Some(package) = self.index.offer.take().and_then(|mut offer| (index < offer.len()).then(|| offer.swap_remove(index))) else {
            return;
        };
        let busy = format!("Installing {}…", package.name);
        self.start_index_job(ctx, &busy, move |installer| match installer.index(false) {
            Ok(index) => Done::Installed(installer.install(&package, &index)),
            Err(e) => Done::Installed(vec![Err(e)]),
        });
    }

    fn finish_install(&mut self, ctx: &egui::Context, results: Vec<Result<String, String>>) {
        let mut installed = Vec::new();
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(what) => {
                    // Installing means wanting it on, even if an older copy was off.
                    let name = what.rsplit_once(' ').map_or(what.as_str(), |(n, _)| n);
                    self.settings.ignored_packages.retain(|n| n != name);
                    installed.push(what);
                }
                Err(e) => errors.push(e),
            }
        }
        self.message = None;
        if !installed.is_empty() {
            self.save_settings();
            // Also restarts the plugin host, so it imports new libraries.
            self.reload_packages(ctx);
        }
        // A problem loading what was installed is said first.
        self.message.get_or_insert(match (installed.is_empty(), errors.first()) {
            (_, Some(e)) => format!("Could not install everything: {e}"),
            (true, None) => "All packages are up to date".to_owned(),
            (false, None) => format!("Installed {}", installed.join(", ")),
        });
    }
}
