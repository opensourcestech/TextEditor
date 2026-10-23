//! Menus from packages (see `crate::packages::menu_file`): their items join the
//! menu bar and the right-click menus of the editor, the side bar and tabs,
//! running the commands they name through the same translation as key
//! bindings. Also the side bar's and tabs' own right-click items.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui;
use serde_json::json;

use super::{Action, EditorApp, PackageCommand, Standard, translate};
use crate::packages::keymap::NamedCommand;
use crate::ui::menu::{Item, Menu};
use crate::packages::menu_file::{self, MenuItem};

/// A top-level menu from packages, ready to add to the menu bar.
#[derive(Clone)]
pub(super) struct Extra {
    /// Which standard menu it joins, like `"tools"`; `None` for one of its own.
    id: Option<String>,
    caption: String,
    items: Vec<Item<Action>>,
}

impl EditorApp {
    /// Turns the packages' menus into menu items, translating their
    /// commands. Items for commands this editor can't run are left out.
    pub(super) fn translate_package_menus(&mut self, plugins: &HashMap<String, String>) {
        let main = std::mem::take(&mut self.package_main_menu);
        self.main_menu_extras = main
            .iter()
            .filter_map(|top| match top {
                MenuItem::Entry { caption, id, children, .. } => {
                    let items = self.menu_items(children, plugins);
                    (!items.is_empty()).then(|| Extra { id: id.clone(), caption: caption.clone(), items })
                }
                MenuItem::Separator => None,
            })
            .collect();
        self.package_main_menu = main;
        let context = std::mem::take(&mut self.package_context_menu);
        self.context_menu_extras = self.menu_items(&context, plugins);
        self.package_context_menu = context;
    }

    fn menu_items(&mut self, items: &[MenuItem], plugins: &HashMap<String, String>) -> Vec<Item<Action>> {
        let mut out: Vec<Item<Action>> = Vec::new();
        for item in items {
            match item {
                MenuItem::Separator => {
                    if !out.is_empty() && !matches!(out.last(), Some(Item::Separator)) {
                        out.push(Item::Separator);
                    }
                }
                MenuItem::Entry { caption, children, .. } if !children.is_empty() => {
                    let sub = self.menu_items(children, plugins);
                    if !sub.is_empty() {
                        out.push(Item::Submenu { label: caption.clone(), items: sub });
                    }
                }
                MenuItem::Entry { caption, command: Some(command), .. } => {
                    if let Some(cmd) = translate(command, &self.snippets, plugins) {
                        let i = self.add_package_command(cmd);
                        out.push(Item::cmd(caption.clone(), Action::PackageCommand(i), None));
                    }
                }
                MenuItem::Entry { .. } => {}
            }
        }
        while matches!(out.last(), Some(Item::Separator)) {
            out.pop();
        }
        out
    }

    /// Adds packages' menus to the menu bar: into the standard menu of the
    /// same id, or as menus of their own before Window. Items for commands
    /// the menus already have are left out.
    pub(super) fn add_package_menus(&self, menus: &mut Vec<Menu<Action>>) {
        if self.main_menu_extras.is_empty() {
            return;
        }
        let mut existing = Vec::new();
        for menu in menus.iter() {
            actions_in(&menu.items, &mut existing);
        }
        let mac = cfg!(target_os = "macos");
        for extra in &self.main_menu_extras {
            let items = self.fresh(&extra.items, &existing);
            if items.is_empty() {
                continue;
            }
            let standard = match extra.id.as_deref() {
                Some("file") => Some("File"),
                Some("edit") => Some("Edit"),
                Some("selection") => Some("Selection"),
                Some("find") => Some("Find"),
                Some("goto") => Some("Goto"),
                Some("tools") => Some("Tools"),
                Some("view") => Some("View"),
                Some("window") if mac => Some("Window"),
                Some("preferences") => None,
                _ => Some(extra.caption.as_str()),
            };
            match standard {
                Some(title) => match menus.iter_mut().find(|m| m.title == title) {
                    Some(menu) => {
                        menu.items.push(Item::Separator);
                        menu.items.extend(items);
                    }
                    None => {
                        let at = menus.iter().position(|m| m.title == "Window").unwrap_or(menus.len());
                        menus.insert(at, Menu { title: title.to_owned(), items });
                    }
                },
                // Preferences: under the app menu on
                // macOS, in File elsewhere, next to Packages.
                None => {
                    let host = if mac { "TextEditor" } else { "File" };
                    let Some(menu) = menus.iter_mut().find(|m| m.title == host) else {
                        continue;
                    };
                    let at = menu.items.iter().position(|i| matches!(i, Item::Submenu { label, .. } if label == "Packages"));
                    let preferences = Item::Submenu { label: "Preferences".into(), items };
                    match at {
                        Some(at) => menu.items.insert(at + 1, preferences),
                        None => menu.items.push(preferences),
                    }
                }
            }
        }
    }

    /// `items` without those whose command is already in `existing`.
    fn fresh(&self, items: &[Item<Action>], existing: &[Action]) -> Vec<Item<Action>> {
        let mut out = Vec::new();
        for item in items {
            match item {
                Item::Command { action: Action::PackageCommand(i), .. } => {
                    let duplicate = matches!(self.package_commands.get(*i), Some(PackageCommand::Run(a)) if existing.contains(a));
                    if !duplicate {
                        out.push(item.clone());
                    }
                }
                Item::Submenu { label, items } => {
                    let sub = self.fresh(items, existing);
                    if !sub.is_empty() {
                        out.push(Item::Submenu { label: label.clone(), items: sub });
                    }
                }
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// The editor's right-click menu: the clipboard, then packages' items.
    pub(super) fn context_menu_items(&self) -> Vec<Item<Action>> {
        let mut items = vec![
            Item::cmd("Cut", Action::Standard(Standard::Cut), None),
            Item::cmd("Copy", Action::Standard(Standard::Copy), None),
            Item::cmd("Paste", Action::Standard(Standard::Paste), None),
            Item::Separator,
            Item::cmd("Select All", Action::Standard(Standard::SelectAll), None),
        ];
        let base: Vec<Action> = items.iter().filter_map(|i| if let Item::Command { action, .. } = i { Some(*action) } else { None }).collect();
        let extras = self.fresh(&self.context_menu_extras, &base);
        if !extras.is_empty() {
            items.push(Item::Separator);
            items.extend(extras);
        }
        items
    }
}

fn actions_in(items: &[Item<Action>], out: &mut Vec<Action>) {
    for item in items {
        match item {
            Item::Command { action, .. } => out.push(*action),
            Item::Submenu { items, .. } => actions_in(items, out),
            _ => {}
        }
    }
}

/// What was picked in a side bar or tab menu, done after drawing.
pub(super) enum MenuPick {
    Open(PathBuf),
    Reveal(PathBuf),
    CopyPath(PathBuf),
    FindInFolder(PathBuf),
    RefreshFolder,
    /// Close every tab of the group but this one, or those to its right.
    CloseOthers(u64),
    CloseRight(u64),
    CloneView(u64),
    /// A package's item, its arguments filled in.
    Command(NamedCommand),
}

/// "Reveal in Finder", as each system calls it.
const REVEAL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(windows) {
    "Show in Explorer"
} else {
    "Open Containing Folder"
};

/// The side bar's right-click menu for a file or folder: the built-in
/// items, then packages' (`extras`), which get the path in their arguments.
pub(super) fn side_bar_menu(ui: &mut egui::Ui, path: &Path, is_dir: bool, extras: &[MenuItem], pick: &mut Option<MenuPick>) {
    let mut set = |p: MenuPick| *pick = Some(p);
    if !is_dir && ui.button("Open").clicked() {
        set(MenuPick::Open(path.to_path_buf()));
    }
    if is_dir && ui.button("Find in Folder…").clicked() {
        set(MenuPick::FindInFolder(path.to_path_buf()));
    }
    if ui.button(REVEAL).clicked() {
        set(MenuPick::Reveal(path.to_path_buf()));
    }
    if ui.button("Copy Path").clicked() {
        set(MenuPick::CopyPath(path.to_path_buf()));
    }
    if is_dir && ui.button("Refresh Folder").clicked() {
        set(MenuPick::RefreshFolder);
    }
    if !extras.is_empty() {
        ui.separator();
        let mut clicked = None;
        menu_file::show(ui, extras, &mut clicked);
        if let Some(command) = clicked {
            let text = path.display().to_string();
            let values = [
                ("paths", json!([text])),
                ("files", if is_dir { json!([]) } else { json!([text]) }),
                ("dirs", if is_dir { json!([text]) } else { json!([]) }),
            ];
            set(MenuPick::Command(menu_file::with_args(&command, &values)));
        }
    }
}

/// A tab's right-click menu, after its Close and Move items: more of the
/// built-in items, then packages' (`extras`), given the tab's group and index.
pub(super) fn tab_menu(ui: &mut egui::Ui, id: u64, path: Option<&Path>, place: (usize, usize), extras: &[MenuItem], pick: &mut Option<MenuPick>) {
    let mut set = |p: MenuPick| *pick = Some(p);
    if ui.button("Close Other Tabs").clicked() {
        set(MenuPick::CloseOthers(id));
    }
    if ui.button("Close Tabs to the Right").clicked() {
        set(MenuPick::CloseRight(id));
    }
    ui.separator();
    if ui.button("New View into File").clicked() {
        set(MenuPick::CloneView(id));
    }
    if let Some(path) = path {
        if ui.button("Copy File Path").clicked() {
            set(MenuPick::CopyPath(path.to_path_buf()));
        }
        if ui.button(REVEAL).clicked() {
            set(MenuPick::Reveal(path.to_path_buf()));
        }
    }
    if !extras.is_empty() {
        ui.separator();
        let mut clicked = None;
        menu_file::show(ui, extras, &mut clicked);
        if let Some(command) = clicked {
            let values = [("group", json!(place.0)), ("index", json!(place.1))];
            set(MenuPick::Command(menu_file::with_args(&command, &values)));
        }
    }
}

impl EditorApp {
    /// A package menu with only the items whose commands can run.
    pub(super) fn runnable_menu(&self, items: &[MenuItem]) -> Vec<MenuItem> {
        let plugins = self.plugins.host.as_ref().map(|h| h.commands.clone()).unwrap_or_default();
        menu_file::keep(items, &|c| translate(c, &self.snippets, &plugins).is_some())
    }

    /// Does what was picked in a side bar or tab menu.
    pub(super) fn menu_pick(&mut self, ctx: &egui::Context, pick: MenuPick) {
        let position = |app: &Self, id: u64| app.docs.iter().position(|d| d.id == id);
        match pick {
            MenuPick::Open(path) => {
                self.open_path(&path);
                self.focus_editor(ctx);
            }
            MenuPick::Reveal(path) => {
                if let Err(e) = reveal(&path) {
                    self.message = Some(format!("Could not show {}: {e}", path.display()));
                }
            }
            MenuPick::CopyPath(path) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(path.display().to_string());
                }
            }
            MenuPick::FindInFolder(path) => {
                self.open_find_in_files();
                self.find_files.scope = path.display().to_string();
            }
            MenuPick::RefreshFolder => {
                if let Some(ws) = &mut self.workspace {
                    ws.refresh(ctx);
                }
            }
            MenuPick::CloseOthers(id) | MenuPick::CloseRight(id) => {
                let right = matches!(pick, MenuPick::CloseRight(_));
                let Some(group) = self.group_of(id) else {
                    return;
                };
                let views = &self.groups[group].views;
                let at = views.iter().position(|v| *v == id).unwrap_or(0);
                let doomed: Vec<u64> =
                    views.iter().enumerate().filter(|(i, _)| if right { *i > at } else { *i != at }).map(|(_, v)| *v).collect();
                for doomed in doomed {
                    if let Some(i) = position(self, doomed) {
                        self.close(ctx, i);
                    }
                }
            }
            MenuPick::CloneView(id) => {
                if let Some(i) = position(self, id) {
                    self.activate(i);
                    self.clone_active_view(ctx);
                }
            }
            MenuPick::Command(command) => {
                if !self.run_named_command(ctx, &command) {
                    self.message = Some(format!("Unknown command \"{}\"", command.name));
                }
            }
        }
    }
}

/// Shows a file or folder in the system's file manager.
fn reveal(path: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    command.spawn().map(|_| ())
}
