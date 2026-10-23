use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Key, KeyboardShortcut, Modifiers, ViewportCommand};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde_json::{Value, json};

use crate::buffer::{Motion, Selection};
use crate::completion;
use crate::document::Document;
use crate::editor_view::{self, Command};
use crate::highlight::{Highlighter, Syntaxes, blend};
use crate::keymap::{self, Condition, NamedCommand};
use crate::layout::{Group, Layout, Preset};
use crate::macros::{MacroFile, Step};
use crate::menu::{Item, Menu, NativeItem};
use crate::packages::{self, Dirs, Loaded, Package, Resource};
use crate::palette::{Candidate, Mode, Palette, Pick};
use crate::recent::{self, Recent};
use crate::session::{Session, Store, Tab};
use crate::settings::{self, MAX_FONT_SIZE, MIN_FONT_SIZE, Settings};
use crate::snippets::Snippet;
use crate::menu_file::MenuItem;
use crate::workspace::Workspace;

mod autocomplete;
mod find_in_files;
mod groups;
mod macro_ui;
mod package_index_ui;
mod package_menus;
mod plugin_api;

const fn key(modifiers: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(modifiers, key))
}
const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);
const CMD_CTRL: Modifiers = Modifiers::COMMAND.plus(Modifiers::CTRL);
const CMD_ALT_SHIFT: Modifiers = CMD_ALT.plus(Modifiers::SHIFT);
const CTRL: Modifiers = Modifiers::CTRL;
const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);
/// While working, the session is saved at most this often, so a crash or
/// Force Quit loses at most this much.
const SESSION_SAVE_EVERY: Duration = Duration::from_secs(1);
/// Plugins' `on_pre_close` runs while the tab is still open.
const PRE_CLOSE_WAIT: Duration = Duration::from_millis(300);

/// Everything the user can trigger from a menu, shortcut, palette or button.
/// Collected during the frame and applied afterwards, which keeps borrows simple.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    New,
    Open,
    OpenFolder,
    /// Index into the recent list.
    OpenRecent(usize),
    ClearRecent,
    CloseFolder,
    RefreshFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseActive,
    Close(usize),
    /// Another tab showing the active tab's text.
    CloneView,
    /// A plugin's window: the tabs set aside meanwhile (see `groups::Stash`).
    NewWindow,
    CloseWindow,
    Quit,
    Find,
    FindNext,
    FindPrev,
    FindAll,
    FindInFiles,
    /// Search with what's in the Find in Files panel.
    SearchFiles,
    ReplaceInFiles,
    /// Go to the next (or previous) line of Find Results.
    NextResult,
    PrevResult,
    GotoAnything,
    CommandPalette,
    GotoLine,
    ToggleSidebar,
    NextTab,
    PrevTab,
    Edit(Command),
    /// Undo, Cut, Paste… go to whichever widget has focus (editor or a text field).
    Standard(Standard),
    /// Index into `EditorApp::syntax_names`.
    SetSyntax(usize),
    /// Index into `EditorApp::theme_names`.
    SetTheme(usize),
    FontBigger,
    FontSmaller,
    FontReset,
    Toggle(Flag),
    OpenSettings,
    OpenSettingsFile,
    BrowsePackages,
    /// Install from a file.
    InstallPackage,
    /// Install from the online package index.
    InstallFromIndex,
    UpgradePackages,
    InstallLibraries,
    ReloadPackages,
    /// Index into `EditorApp::package_list`.
    DisablePackage(usize),
    EnablePackage(usize),
    RemovePackage(usize),
    /// Index into `EditorApp::package_commands`.
    PackageCommand(usize),
    /// Index into `EditorApp::snippets`.
    InsertSnippet(usize),
    ShowPluginConsole,
    SetLayout(Preset),
    FocusGroup(usize),
    /// Move the active tab to this group.
    MoveToGroup(usize),
    /// Show the autocomplete list now, even before typing.
    AutoComplete,
    CommitCompletion,
    HideCompletion,
    /// Start recording a macro, or stop.
    RecordMacro,
    PlaybackMacro,
    SaveMacro,
    /// Index into `EditorApp::macro_files`.
    RunMacro(usize),
}

/// What a package's key binding or palette entry does, translated from the
/// command it names (see [`translate`]).
#[derive(Clone, Debug, PartialEq)]
enum PackageCommand {
    Run(Action),
    /// Type this text at every caret.
    Insert(String),
    /// Insert this snippet template at every caret.
    Snippet(String),
    /// Move every caret, extending the selections if true.
    Move(Motion, bool),
    /// Delete from every caret to where this moves it.
    Delete(Motion),
    /// Show or hide a plugin's output panel.
    ShowPanel(String),
    HidePanel,
    /// A command of a Python plugin, with its arguments.
    Plugin(String, Value),
    /// Play a package's macro file, by its path like `Package/Name.sublime-macro`.
    Macro(String),
    /// A layout as `set_layout` gives it.
    Layout(Value),
    /// Open a package's settings beside the user's own copy, as
    /// `edit_settings` does: the `base_file` and the `default` text for a
    /// new user file.
    EditSettings(String, String),
    /// Open a file; `${packages}` and the like are filled in first.
    OpenFile(String),
    OpenUrl(String),
}

/// A package key binding that this editor can carry out.
struct KeyBinding {
    keys: Vec<KeyboardShortcut>,
    context: Vec<Condition>,
    /// Index into `EditorApp::package_commands`.
    command: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Standard {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// Settings with an on/off switch in the View menu.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Flag {
    LineNumbers,
    HighlightLine,
    IndentGuides,
    TabsToSpaces,
    AutoMatch,
    CaretBlink,
    WordWrap,
    AutoComplete,
}

impl Flag {
    fn get(self, s: &mut Settings) -> &mut bool {
        match self {
            Flag::LineNumbers => &mut s.line_numbers,
            Flag::HighlightLine => &mut s.highlight_line,
            Flag::IndentGuides => &mut s.indent_guides,
            Flag::TabsToSpaces => &mut s.translate_tabs_to_spaces,
            Flag::AutoMatch => &mut s.auto_match_enabled,
            Flag::CaretBlink => &mut s.caret_blink,
            Flag::WordWrap => &mut s.word_wrap,
            Flag::AutoComplete => &mut s.auto_complete,
        }
    }
}

/// What a palette row does when picked.
enum Target {
    Tab(usize),
    File(PathBuf),
    Run(Action),
    /// A row of a plugin's quick panel.
    Plugin(usize),
    /// A package the online index offers.
    IndexPackage(usize),
}

#[derive(Default)]
struct FindBar {
    open: bool,
    query: String,
    match_case: bool,
    request_focus: bool,
    status: String,
}

pub struct EditorApp {
    docs: Vec<Document>,
    active: usize,
    next_id: u64,
    highlighter: Highlighter,
    settings: Settings,
    settings_open: bool,
    find: FindBar,
    message: Option<String>,
    window_title: String,
    allow_close: bool,
    workspace: Option<Workspace>,
    palette: Palette,
    /// Rows of the open palette and what each one does; built when it opens.
    palette_candidates: Vec<Candidate>,
    palette_targets: Vec<Target>,
    syntax_names: Vec<String>,
    theme_names: Vec<String>,
    /// Where packages live; `None` in most tests.
    packages: Option<Dirs>,
    /// Packages found at the last load, enabled or not.
    package_list: Vec<Package>,
    /// `Dirs::fingerprint` and `ignored_packages` as of the last load.
    packages_fingerprint: u64,
    packages_ignored: Vec<String>,
    /// `Syntaxes::key` of the languages in use.
    syntaxes_key: u64,
    /// Package languages being prepared in the background, and their key.
    syntax_job: Option<(u64, mpsc::Receiver<Syntaxes>)>,
    snippets: Vec<Snippet>,
    package_commands: Vec<PackageCommand>,
    /// In load order: later ones win.
    key_bindings: Vec<KeyBinding>,
    /// Command palette entries from packages: caption and command index.
    palette_entries: Vec<(String, usize)>,
    /// The packages' key bindings and palette entries as read, translated
    /// again when plugins (and so their commands) load.
    package_keymaps: Vec<keymap::Binding>,
    package_entries: Vec<keymap::PaletteEntry>,
    plugins: plugin_api::PluginState,
    /// The first key of a two-key binding (like ⌘K ⌘B), waiting for the second.
    pending_key: Option<KeyboardShortcut>,
    /// The theme in settings that isn't installed, once it has been reported.
    missing_theme: Option<String>,
    recent: Recent,
    /// Input events to feed egui next frame (menu-bar Cut/Copy/Paste…).
    pending_events: Vec<egui::Event>,
    /// Where hot exit keeps open tabs and unsaved text; `None` in most tests.
    session: Option<Store>,
    /// Revision of each tab's text as last written to the session folder.
    session_written: HashMap<u64, u64>,
    /// Fingerprint of the session as last saved, to skip saves when nothing changed.
    session_saved: u64,
    last_session_save: Instant,
    #[cfg(target_os = "macos")]
    /// The macOS menu bar; without it the menus are drawn in the window.
    native_menu: Option<crate::menu::NativeMenuBar<Action>>,
    /// Split views: how the editor area is divided, and each group's tabs.
    layout: Layout,
    groups: Vec<Group>,
    /// The group new tabs go into: the active tab's, or an empty one clicked.
    focus_group: usize,
    /// The autocomplete list, while it's open.
    completion: Option<completion::Popup>,
    /// The macro being recorded, the last one recorded, and macro files
    /// from packages.
    recording: Option<Vec<Step>>,
    last_macro: Vec<Step>,
    macro_files: Vec<MacroFile>,
    /// Packages' `Main.sublime-menu` and `Context.sublime-menu`, merged, and
    /// as menu items once their commands are translated.
    package_main_menu: Vec<MenuItem>,
    package_context_menu: Vec<MenuItem>,
    package_side_bar_menu: Vec<MenuItem>,
    package_tab_menu: Vec<MenuItem>,
    main_menu_extras: Vec<package_menus::Extra>,
    context_menu_extras: Vec<Item<Action>>,
    find_files: find_in_files::FindFiles,
    /// The layout from before a package's settings opened beside the user's
    /// own copy, and those two tabs: closing both puts the layout back.
    settings_layout: Option<(Layout, Vec<u64>)>,
    /// The online package index's jobs and offer.
    index: package_index_ui::IndexState,
    /// Windows set aside while a plugin's window is showing, oldest first,
    /// and whether that window shows the side bar (a plugin may hide it).
    windows: Vec<groups::Stash>,
    sidebar_override: Option<bool>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        Self::with_session(cc, paths, Store::default_location())
    }

    fn with_session(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>, session: Option<Store>) -> Self {
        let ctx = &cc.egui_ctx;
        // ⌘+ / ⌘- change the editor font instead of zooming the whole window.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let (settings, settings_error) = Settings::load();
        let packages = Dirs::default_location();
        let package_list = packages.as_ref().map(Dirs::list).unwrap_or_default();
        let loaded = packages::load(&package_list, &settings.ignored_packages);
        let highlighter = Highlighter::new(&settings.theme, &Loaded::default());
        let mut app = Self {
            docs: Vec::new(),
            active: 0,
            next_id: 0,
            syntax_names: highlighter.syntax_names(),
            theme_names: highlighter.theme_names(),
            packages_fingerprint: packages.as_ref().map_or(0, Dirs::fingerprint),
            packages_ignored: settings.ignored_packages.clone(),
            packages,
            package_list,
            syntaxes_key: Syntaxes::key(&[]),
            syntax_job: None,
            snippets: Vec::new(),
            package_commands: Vec::new(),
            key_bindings: Vec::new(),
            palette_entries: Vec::new(),
            package_keymaps: Vec::new(),
            package_entries: Vec::new(),
            plugins: plugin_api::PluginState::default(),
            pending_key: None,
            missing_theme: None,
            recent: Recent::load(),
            highlighter,
            settings,
            settings_open: false,
            find: FindBar::default(),
            message: settings_error,
            window_title: String::new(),
            allow_close: false,
            workspace: None,
            palette: Palette::default(),
            palette_candidates: Vec::new(),
            palette_targets: Vec::new(),
            pending_events: Vec::new(),
            session,
            session_written: HashMap::new(),
            session_saved: 0,
            last_session_save: Instant::now() - SESSION_SAVE_EVERY,
            #[cfg(target_os = "macos")]
            native_menu: crate::menu::NativeMenuBar::new(ctx),
            layout: Layout::default(),
            groups: vec![Group::default()],
            focus_group: 0,
            completion: None,
            recording: None,
            last_macro: Vec::new(),
            macro_files: Vec::new(),
            package_main_menu: Vec::new(),
            package_context_menu: Vec::new(),
            package_side_bar_menu: Vec::new(),
            package_tab_menu: Vec::new(),
            main_menu_extras: Vec::new(),
            context_menu_extras: Vec::new(),
            find_files: find_in_files::FindFiles::default(),
            settings_layout: None,
            index: package_index_ui::IndexState::default(),
            windows: Vec::new(),
            sidebar_override: None,
        };
        // Before restoring tabs, so they can use package languages.
        app.use_packages(ctx, loaded);
        #[cfg(target_os = "macos")]
        crate::mac_open::set_waker(ctx);
        app.restore_session(ctx);
        // Files given on the command line or from Finder open on top.
        for path in paths {
            app.open_any(ctx, path);
        }
        if app.docs.is_empty() {
            app.new_doc();
        }
        app.sync_groups();
        app.focus_editor(ctx);
        app
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn doc(&mut self) -> &mut Document {
        &mut self.docs[self.active]
    }

    fn new_doc(&mut self) {
        let id = self.take_id();
        self.docs.push(Document::new_untitled(id));
        self.active = self.docs.len() - 1;
    }

    fn open_path(&mut self, path: &Path) {
        // Switch to the tab if the file is already open.
        if let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(path)) {
            self.active = i;
            self.recent.add(path);
            return;
        }
        let id = self.take_id();
        match Document::open(id, path) {
            Ok(mut doc) => {
                let first_line = doc.buffer.line_text(0);
                doc.set_syntax(self.highlighter.detect_syntax(Some(path), &first_line));
                // Replace a lone empty untitled tab instead of keeping it around.
                let replace = self.docs.len() == 1
                    && self.docs[0].path.is_none()
                    && !self.docs[0].dirty()
                    && self.docs[0].buffer.len() == 0;
                if replace {
                    self.docs[0] = doc;
                    self.active = 0;
                } else {
                    self.docs.push(doc);
                    self.active = self.docs.len() - 1;
                }
                self.recent.add(path);
            }
            Err(e) => self.message = Some(format!("Could not open {e}")),
        }
    }

    /// A file opens as a tab, a folder in the side bar.
    fn open_any(&mut self, ctx: &egui::Context, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(ctx, path);
        } else {
            self.open_path(&path);
        }
    }

    fn open_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let path = path.canonicalize().unwrap_or(path);
        self.recent.add(&path);
        self.workspace = Some(Workspace::open(path, ctx));
        if !self.settings.show_sidebar {
            self.settings.show_sidebar = true;
            self.save_settings();
        }
    }

    fn focus_editor(&self, ctx: &egui::Context) {
        let id = editor_view::editor_id(self.docs[self.active].id);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    /// Whether the keyboard is in the active tab's editor (not a panel or field).
    fn editor_focused(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.focused()) == Some(editor_view::editor_id(self.docs[self.active].id))
    }

    /// Runs a command by name, plugins' included. False if
    /// this editor has no such command.
    fn run_named_command(&mut self, ctx: &egui::Context, cmd: &NamedCommand) -> bool {
        let plugin_commands = self.plugins.host.as_ref().map(|h| h.commands.clone()).unwrap_or_default();
        match translate(cmd, &self.snippets, &plugin_commands) {
            Some(command) => {
                let index = self.add_package_command(command);
                self.run_package_command(ctx, index);
                true
            }
            None => false,
        }
    }

    /// Returns false if the user cancelled (e.g. closed the Save As dialog).
    fn save(&mut self, ctx: &egui::Context, index: usize, ask_path: bool) -> bool {
        let doc = &self.docs[index];
        let path = match (&doc.path, ask_path) {
            (Some(p), false) => p.clone(),
            _ => {
                let mut dialog = FileDialog::new().set_file_name(doc.title());
                if let Some(dir) = doc.path.as_ref().and_then(|p| p.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                match dialog.save_file() {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        // Plugins (a formatter, say) may change the text before it's written.
        let id = self.docs[index].id;
        self.plugin_pre_save(ctx, id);
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return false;
        };
        let doc = &mut self.docs[index];
        if let Err(e) = doc.save_to(&path) {
            self.message = Some(format!("Could not save {e}"));
            return false;
        }
        let first_line = doc.buffer.line_text(0);
        doc.set_syntax(self.highlighter.detect_syntax(Some(&path), &first_line));
        self.message = Some(format!("Saved {}", path.display()));
        self.plugin_post_save(ctx, id);
        if ask_path {
            self.recent.add(&path);
        }
        // Saving the settings file applies it right away.
        if settings::settings_path().is_some_and(|p| p == path) {
            let (loaded, error) = Settings::parse(&self.docs[index].buffer.text());
            if let Some(error) = error {
                self.message = Some(error);
            } else {
                self.settings = loaded;
                self.apply_theme(ctx);
            }
        }
        true
    }

    /// Saves every tab with unsaved changes that has a file. Untitled tabs
    /// are left alone rather than asking where to save each.
    fn save_all(&mut self, ctx: &egui::Context) {
        self.sync_clones();
        // One save per file, however many tabs show it.
        let ids: Vec<u64> =
            self.docs.iter().filter(|d| d.dirty() && d.path.is_some() && d.holds_text).map(|d| d.id).collect();
        let count = ids.len();
        for id in ids {
            // A plugin saving one tab may close another.
            if let Some(i) = self.docs.iter().position(|d| d.id == id) {
                self.save(ctx, i, false);
            }
        }
        if count > 1 {
            self.message = Some(format!("Saved {count} files"));
        }
    }

    fn close(&mut self, ctx: &egui::Context, index: usize) {
        self.sync_clones();
        let family = self.docs[index].family;
        let sibling = (0..self.docs.len()).find(|&i| i != index && self.docs[i].family == family);
        // Another tab still shows this text: it takes over, with no need to ask.
        if let Some(sibling) = sibling
            && self.docs[index].holds_text
        {
            self.hold_text(sibling);
        }
        if sibling.is_none() && self.docs[index].dirty() {
            let answer = MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description(format!("Save changes to \"{}\" before closing?", self.docs[index].title()))
                .set_buttons(MessageButtons::YesNoCancel)
                .show();
            match answer {
                MessageDialogResult::Yes => {
                    if !self.save(ctx, index, false) {
                        return;
                    }
                }
                MessageDialogResult::No => {}
                _ => return,
            }
        }
        // Plugins hear about it while the tab is still there.
        let id = self.docs[index].id;
        self.plugin_event(ctx, "on_pre_close", id, json!([]), Some(PRE_CLOSE_WAIT));
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        let was_active = index == self.active;
        let active_id = self.docs[self.active].id;
        let group = self.group_of(id).unwrap_or(self.focus_group);
        self.docs.remove(index);
        for g in &mut self.groups {
            g.remove(id);
        }
        if self.completion.as_ref().is_some_and(|c| c.doc == id) {
            self.completion = None;
        }
        if self.docs.is_empty() {
            self.focus_group = group.min(self.groups.len() - 1);
            self.new_doc();
        } else if was_active {
            // The tab beside it in its group comes to the front; with none
            // left, that group stays (empty) and another tab is active.
            let group = group.min(self.groups.len() - 1);
            let next = self.groups[group].active.or_else(|| self.groups.iter().find_map(|g| g.active));
            self.active = next.and_then(|id| self.docs.iter().position(|d| d.id == id)).unwrap_or(0);
            self.focus_group = group;
        } else {
            self.active = self.docs.iter().position(|d| d.id == active_id).unwrap_or(0);
        }
        self.sync_groups();
        // The last tab of a window a plugin opened: the window set aside comes back.
        if !self.windows.is_empty() && self.groups.iter().all(|g| g.views.is_empty()) {
            self.pop_window();
        }
        if self.settings_layout.as_ref().is_some_and(|(_, ids)| !ids.iter().any(|id| self.docs.iter().any(|d| d.id == *id))) {
            // Both sides of a package's settings are closed: back to the layout from before.
            let (layout, _) = self.settings_layout.take().unwrap();
            if self.layout.preset() == Some(Preset::Columns2) {
                self.set_layout(layout);
            }
        }
    }

    /// Asks about unsaved files. Returns true if it is fine to quit.
    fn confirm_quit(&mut self, ctx: &egui::Context) -> bool {
        self.sync_clones();
        let dirty: Vec<usize> = (0..self.docs.len()).filter(|&i| self.docs[i].dirty() && self.docs[i].holds_text).collect();
        if dirty.is_empty() {
            return true;
        }
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(format!("{} file(s) have unsaved changes. Save them before quitting?", dirty.len()))
            .set_buttons(MessageButtons::YesNoCancel)
            .show();
        match answer {
            MessageDialogResult::Yes => dirty.into_iter().all(|i| self.save(ctx, i, false)),
            MessageDialogResult::No => true,
            _ => false,
        }
    }

    // ----- Session (hot exit) --------------------------------------------------

    /// Reopens the tabs and folder from last time, unsaved changes included.
    fn restore_session(&mut self, ctx: &egui::Context) {
        let Some(session) = self.session.as_ref().and_then(Store::load) else {
            return;
        };
        if let Some(folder) = session.folder.filter(|f| f.is_dir()) {
            self.open_folder(ctx, folder);
        }
        if let Some(layout) = session.layout.as_ref().and_then(Layout::from_json) {
            self.set_layout(layout);
        }
        let mut active = None;
        // The tab made for each session tab, for later views of the same text.
        let mut made: Vec<Option<usize>> = Vec::new();
        for (i, tab) in session.tabs.into_iter().enumerate() {
            // Each tab goes back into its group as it's opened.
            self.focus_group = tab.group.min(self.groups.len() - 1);
            let unsaved = tab.unsaved.as_deref().and_then(|name| self.session.as_ref()?.read_text(name));
            let original = tab.clone_of.and_then(|t| made.get(t).copied().flatten());
            made.push(None);
            if let Some(original) = original {
                let id = self.take_id();
                let doc = self.docs[original].clone_view(id);
                self.docs.push(doc);
                self.active = self.docs.len() - 1;
            } else {
                match (unsaved, &tab.path) {
                    (Some(text), _) => {
                        let id = self.take_id();
                        let mut doc = Document::restored(id, tab.path.clone(), &text, tab.crlf);
                        doc.set_syntax(tab.syntax.clone());
                        self.docs.push(doc);
                        self.active = self.docs.len() - 1;
                    }
                    (None, Some(path)) if path.is_file() => self.open_path(path),
                    // A saved file that has since been deleted or moved.
                    _ => continue,
                }
            }
            made[i] = Some(self.active);
            let doc = &mut self.docs[self.active];
            let caret = tab.caret.min(doc.buffer.len());
            doc.buffer.set_single(Selection::caret(caret));
            doc.view.reveal(true);
            if i == session.active {
                active = Some(self.active);
            }
            self.sync_groups();
        }
        if let Some(i) = active {
            self.active = i;
        }
        self.sync_groups();
        self.session_saved = self.session_fingerprint();
    }

    /// Writes open tabs to the session folder. Unsaved text is kept only with
    /// hot exit on, and rewritten only for tabs that changed since last time.
    fn save_session(&mut self) {
        // Tabs opened since the last frame join their group first.
        self.sync_groups();
        self.last_session_save = Instant::now();
        self.session_saved = self.session_fingerprint();
        let Some(store) = &self.session else {
            return;
        };
        let (layout, groups) = self.session_groups();
        let mut session = Session {
            folder: self.workspace.as_ref().map(|ws| ws.root.clone()),
            layout: (layout != Layout::default()).then(|| layout.to_json()),
            ..Default::default()
        };
        let group_of = |id: u64| groups.iter().position(|g| g.views.contains(&id)).unwrap_or(0);
        let mut written = HashMap::new();
        // Tabs in group order, so each group's tabs come back in order.
        let order: Vec<usize> = groups
            .iter()
            .flat_map(|g| g.views.iter())
            .filter_map(|id| self.docs.iter().position(|d| d.id == *id))
            .collect();
        // Where each family's first tab went, for the others to be views of it.
        let mut family_tab: HashMap<u64, usize> = HashMap::new();
        for i in order {
            let doc = &self.docs[i];
            if let Some(&first) = family_tab.get(&doc.family) {
                if i == self.active {
                    session.active = session.tabs.len();
                }
                session.tabs.push(Tab {
                    path: doc.path.clone(),
                    unsaved: None,
                    caret: doc.buffer.primary().head,
                    syntax: doc.syntax().to_owned(),
                    crlf: doc.crlf(),
                    group: group_of(doc.id),
                    clone_of: Some(first),
                });
                continue;
            }
            let keep_text = self.settings.hot_exit && doc.dirty();
            if doc.path.is_none() && !keep_text {
                continue; // an empty or discarded untitled tab
            }
            let unsaved = if keep_text {
                let name = format!("doc-{}.txt", doc.id);
                let revision = doc.buffer.revision;
                if self.session_written.get(&doc.id) != Some(&revision)
                    && let Err(e) = store.write_text(&name, &doc.buffer.text())
                {
                    self.message = Some(format!("Could not save unsaved changes for next time: {e}"));
                    continue;
                }
                written.insert(doc.id, revision);
                Some(name)
            } else {
                None
            };
            if i == self.active {
                session.active = session.tabs.len();
            }
            family_tab.insert(doc.family, session.tabs.len());
            session.tabs.push(Tab {
                path: doc.path.clone(),
                unsaved,
                caret: doc.buffer.primary().head,
                syntax: doc.syntax().to_owned(),
                crlf: doc.crlf(),
                group: group_of(doc.id),
                clone_of: None,
            });
        }
        if let Err(e) = store.write_session(&session) {
            self.message = Some(format!("Could not save the session: {e}"));
        }
        self.session_written = written;
    }

    /// Changes whenever something the session stores changes.
    fn session_fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.active.hash(&mut h);
        self.settings.hot_exit.hash(&mut h);
        self.workspace.as_ref().map(|ws| &ws.root).hash(&mut h);
        for doc in &self.docs {
            (doc.id, &doc.path, doc.buffer.revision, doc.dirty(), doc.buffer.primary().head, doc.syntax()).hash(&mut h);
        }
        self.layout.to_json().to_string().hash(&mut h);
        for g in &self.groups {
            g.views.hash(&mut h);
        }
        h.finish()
    }

    // ----- Settings and themes -----------------------------------------------

    /// Applies the theme in `settings` to the editor and the window chrome.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.highlighter.set_theme(&self.settings.theme) {
            self.missing_theme = None;
        } else if self.missing_theme.as_ref() != Some(&self.settings.theme) {
            // The setting is kept, so the theme comes back if its package
            // does; until then another one is shown. Said once, not on
            // every settings change.
            self.message = Some(format!(
                "Theme \"{}\" is not installed; showing {} instead",
                self.settings.theme,
                self.highlighter.theme_name()
            ));
            self.missing_theme = Some(self.settings.theme.clone());
        }
        let c = *self.highlighter.colors();
        let (theme, mut visuals, system) = if c.dark {
            (egui::Theme::Dark, egui::Visuals::dark(), egui::SystemTheme::Dark)
        } else {
            (egui::Theme::Light, egui::Visuals::light(), egui::SystemTheme::Light)
        };
        // Panels a shade off the editor background.
        let shade = if c.dark { Color32::BLACK } else { Color32::WHITE };
        visuals.panel_fill = blend(c.background, shade, 0.25);
        visuals.window_fill = visuals.panel_fill;
        visuals.extreme_bg_color = c.background;
        visuals.selection.bg_fill = c.selection;
        ctx.set_visuals_of(theme, visuals);
        ctx.set_theme(theme);
        // Match the title bar too.
        ctx.send_viewport_cmd(ViewportCommand::SetTheme(system));
    }

    fn settings_changed(&mut self, ctx: &egui::Context) {
        self.settings = self.settings.clone().clamped();
        self.apply_theme(ctx);
        self.save_settings();
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.message = Some(format!("Could not save settings: {e}"));
        }
    }

    // ----- Packages ----------------------------------------------------------

    /// Reads every package again, e.g. after one was installed or switched
    /// off, and puts what they hold to use.
    fn reload_packages(&mut self, ctx: &egui::Context) {
        self.packages_ignored = self.settings.ignored_packages.clone();
        let Some(dirs) = &self.packages else {
            return;
        };
        self.packages_fingerprint = dirs.fingerprint();
        self.package_list = dirs.list();
        let loaded = packages::load(&self.package_list, &self.packages_ignored);
        self.use_packages(ctx, loaded);
    }

    /// Themes, snippets, key bindings and palette entries take effect now;
    /// languages from the cache, or once built in the background.
    fn use_packages(&mut self, ctx: &egui::Context, loaded: Loaded) {
        let mut errors = loaded.errors;
        self.highlighter.reload_themes(&Loaded { themes: loaded.themes, ..Loaded::default() });
        self.theme_names = self.highlighter.theme_names();
        errors.extend_from_slice(self.highlighter.theme_errors());

        self.snippets = loaded
            .snippets
            .iter()
            .filter_map(|file| {
                let path = format!("{}/{}", file.package, file.path);
                file.text()
                    .and_then(|text| Snippet::parse(text, path))
                    .map_err(|e| errors.push(format!("{}: {e}", file.label())))
                    .ok()
            })
            .collect();

        self.package_keymaps.clear();
        self.package_entries.clear();
        for file in &loaded.keymaps {
            match file.text().and_then(keymap::parse_keymap) {
                Ok(bindings) => self.package_keymaps.extend(bindings.into_iter().filter(|b| b.keys.len() <= 2)),
                Err(e) => errors.push(format!("{}: {e}", file.label())),
            }
        }
        for file in &loaded.commands {
            match file.text().and_then(keymap::parse_commands) {
                Ok(entries) => self.package_entries.extend(entries),
                Err(e) => errors.push(format!("{}: {e}", file.label())),
            }
        }
        self.macro_files.clear();
        for file in &loaded.macros {
            match file.text().and_then(crate::macros::parse) {
                Ok(steps) => self.macro_files.push(MacroFile { path: format!("{}/{}", file.package, file.path), steps }),
                Err(e) => errors.push(format!("{}: {e}", file.label())),
            }
        }
        self.package_main_menu.clear();
        self.package_context_menu.clear();
        self.package_side_bar_menu.clear();
        self.package_tab_menu.clear();
        for file in &loaded.menus {
            // Sublime's Default package describes Sublime's own menus, which
            // this editor has already.
            if file.package == "Default" {
                continue;
            }
            use crate::menu_file as m;
            let into = match packages::base_name(&file.path) {
                m::MAIN => &mut self.package_main_menu,
                m::SIDE_BAR => &mut self.package_side_bar_menu,
                m::TAB_CONTEXT => &mut self.package_tab_menu,
                _ => &mut self.package_context_menu,
            };
            match file.text().and_then(m::parse) {
                Ok(items) => m::merge(into, items),
                Err(e) => errors.push(format!("{}: {e}", file.label())),
            }
        }
        // Plugin commands join in once the plugins have loaded.
        self.start_plugins(ctx);
        self.translate_package_commands();

        if self.prepare_syntaxes(ctx, loaded.syntaxes) {
            errors.extend_from_slice(self.highlighter.syntax_errors());
        }
        if let Some(problem) = problems(&errors) {
            self.message = Some(problem);
        }
        self.apply_theme(ctx);
    }

    /// Turns the packages' key bindings and palette entries into commands
    /// this editor can run. Ones for commands it doesn't have, such as those
    /// of plugins that aren't running, are left out.
    fn translate_package_commands(&mut self) {
        self.package_commands.clear();
        self.key_bindings.clear();
        self.palette_entries.clear();
        let plugin_commands = self.plugins.host.as_ref().map(|h| h.commands.clone()).unwrap_or_default();
        for b in std::mem::take(&mut self.package_keymaps) {
            if let Some(cmd) = translate(&b.command, &self.snippets, &plugin_commands)
                && !just_types_its_key(&b.keys, &cmd)
            {
                let command = self.add_package_command(cmd);
                self.key_bindings.push(KeyBinding { keys: b.keys.clone(), context: b.context.clone(), command });
            }
            self.package_keymaps.push(b);
        }
        for entry in std::mem::take(&mut self.package_entries) {
            if let Some(cmd) = translate(&entry.command, &self.snippets, &plugin_commands) {
                let command = self.add_package_command(cmd);
                self.palette_entries.push((entry.caption.clone(), command));
            }
            self.package_entries.push(entry);
        }
        self.translate_package_menus(&plugin_commands);
    }

    /// The index of `cmd` in `package_commands`, adding it if needed.
    fn add_package_command(&mut self, cmd: PackageCommand) -> usize {
        self.package_commands.iter().position(|c| *c == cmd).unwrap_or_else(|| {
            self.package_commands.push(cmd);
            self.package_commands.len() - 1
        })
    }

    /// Switches to the languages in `files`: right away if they're cached
    /// (returns true), otherwise once they're built on a background thread.
    fn prepare_syntaxes(&mut self, ctx: &egui::Context, files: Vec<Resource>) -> bool {
        let key = Syntaxes::key(&files);
        if key == self.syntaxes_key {
            self.syntax_job = None;
            return false;
        }
        let cache = self.packages.as_ref().map(|d| d.cache.clone());
        let ready = if files.is_empty() {
            Some(Syntaxes::build(&files))
        } else {
            cache.as_deref().and_then(|dir| Syntaxes::cached(&files, dir))
        };
        if let Some(syntaxes) = ready {
            self.syntax_job = None;
            self.set_syntaxes(key, syntaxes);
            return true;
        }
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let syntaxes = match &cache {
                Some(dir) => Syntaxes::build_and_cache(&files, dir),
                None => Syntaxes::build(&files),
            };
            // The app may have moved on to other packages; then nobody listens.
            if send.send(syntaxes).is_ok() {
                ctx.request_repaint();
            }
        });
        self.syntax_job = Some((key, receive));
        false
    }

    /// Takes the languages built in the background, once they're ready.
    fn poll_syntaxes(&mut self) {
        let Some((key, receive)) = &self.syntax_job else {
            return;
        };
        let key = *key;
        match receive.try_recv() {
            Ok(syntaxes) => {
                self.syntax_job = None;
                if let Some(problem) = problems(&syntaxes.errors) {
                    self.message = Some(problem);
                }
                self.set_syntaxes(key, syntaxes);
            }
            Err(mpsc::TryRecvError::Disconnected) => self.syntax_job = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn set_syntaxes(&mut self, key: u64, syntaxes: Syntaxes) {
        self.highlighter.set_syntaxes(syntaxes);
        self.syntaxes_key = key;
        self.syntax_names = self.highlighter.syntax_names();
        // Tabs whose language went away, or that had none, are detected again,
        // so installing a language colors files that are already open.
        for doc in &mut self.docs {
            if doc.syntax() == "Plain Text" || !self.syntax_names.iter().any(|n| n == doc.syntax()) {
                let first_line = doc.buffer.line_text(0);
                doc.set_syntax(self.highlighter.detect_syntax(doc.path.as_deref(), &first_line));
            }
        }
    }

    /// Whether a key binding's `context` conditions all hold right now.
    /// A condition this editor can't check means the binding doesn't apply.
    fn context_holds(&mut self, ctx: &egui::Context, conditions: &[Condition]) -> bool {
        conditions.iter().all(|c| self.condition_holds(ctx, c))
    }

    fn condition_holds(&mut self, ctx: &egui::Context, c: &Condition) -> bool {
        let doc = &self.docs[self.active];
        let b = &doc.buffer;
        // Tests about the text at each selection pass if one selection
        // passes, or with `match_all`, if every one does.
        let each = |test: &dyn Fn(&Selection) -> bool| {
            if c.match_all { b.selections().iter().all(test) } else { b.selections().iter().any(test) }
        };
        let line_of = |pos: usize| b.line_of(pos);
        let editor_focused = ctx.memory(|m| m.focused()) == Some(editor_view::editor_id(doc.id));
        match c.key.as_str() {
            "selector" => each(&|s| c.compare_selector(&editor_view::scopes_at(doc, &self.highlighter, s.start()))),
            "eol_selector" => each(&|s| {
                let eol = b.line_end(line_of(s.end()));
                c.compare_selector(&editor_view::scopes_at(doc, &self.highlighter, eol))
            }),
            "preceding_text" => each(&|s| c.compare_text(&b.slice(b.line_start(line_of(s.start())), s.start()))),
            "following_text" => each(&|s| c.compare_text(&b.slice(s.end(), b.line_end(line_of(s.end()))))),
            "text" => each(&|s| c.compare_text(&b.slice(s.start(), s.end()))),
            "selection_empty" => each(&|s| c.compare(Value::Bool(s.is_empty()))),
            "num_selections" => c.compare(Value::from(b.selections().len())),
            "has_next_field" | "has_prev_field" => c.compare(Value::Bool(b.has_fields())),
            "panel_visible" => c.compare(Value::Bool(self.find.open || self.find_files.open)),
            "panel" => c.compare(Value::from(if self.find.open {
                "find"
            } else if self.find_files.open {
                "find_in_files"
            } else {
                ""
            })),
            "panel_has_focus" => c.compare(Value::Bool((self.find.open || self.find_files.open) && !editor_focused)),
            "overlay_visible" => c.compare(Value::Bool(self.palette.open)),
            "overlay" => {
                let name = match (self.palette.open, self.palette.mode) {
                    (false, _) => "",
                    (true, Mode::Commands) => "command_palette",
                    (true, Mode::Files) => "goto",
                    (true, Mode::Quick | Mode::Input) => "",
                };
                c.compare(Value::from(name))
            }
            "auto_complete_visible" => c.compare(Value::Bool(self.completion.is_some())),
            "popup_visible" => c.compare(Value::Bool(self.plugins.popup.is_some())),
            "is_recording_macro" => c.compare(Value::Bool(self.recording.is_some())),
            "group_has_multiselect" => c.compare(Value::Bool(false)),
            key if key.starts_with("setting.") => {
                let name = &key["setting.".len()..];
                // A plugin's setting on the view first, then the editor's own.
                let setting = self.plugin_view_setting(doc.id, name).or_else(|| {
                    serde_json::to_value(self.settings_for(doc.id)).ok().and_then(|s| s.get(name).cloned())
                });
                setting.is_some_and(|value| c.compare(value))
            }
            // Plugins may know: they answer with on_query_context.
            _ => self.plugin_query_context(ctx, c),
        }
    }

    /// Runs a package key binding's or palette entry's command.
    fn run_package_command(&mut self, ctx: &egui::Context, index: usize) {
        let edits = matches!(
            self.package_commands.get(index),
            Some(PackageCommand::Insert(_) | PackageCommand::Snippet(_) | PackageCommand::Delete(_))
        );
        if edits && self.doc().read_only {
            self.message = Some("This view is read-only".into());
            return;
        }
        match self.package_commands.get(index).cloned() {
            Some(PackageCommand::Run(action)) => self.apply(ctx, action),
            Some(PackageCommand::Insert(text)) => {
                let settings = self.active_settings();
                editor_view::insert_typed(&mut self.doc().buffer, &text, &settings);
                self.doc().view.reveal(false);
            }
            Some(PackageCommand::Snippet(template)) => {
                let settings = self.active_settings();
                editor_view::insert_snippet(self.doc(), &template, &settings, 0);
            }
            Some(PackageCommand::Move(motion, extend)) => {
                let settings = self.active_settings();
                editor_view::motion(ctx, self.doc(), motion, extend, false, &settings);
            }
            // Backspace as the key does it: a whole soft tab, an empty pair.
            Some(PackageCommand::Delete(Motion::Left)) => {
                let settings = self.active_settings();
                editor_view::backspace(&mut self.doc().buffer, None, &settings);
                self.doc().view.reveal(false);
            }
            Some(PackageCommand::Delete(motion)) => {
                let settings = self.active_settings();
                editor_view::motion(ctx, self.doc(), motion, false, true, &settings);
            }
            Some(PackageCommand::Macro(path)) => match self.macro_files.iter().find(|m| m.path == path) {
                Some(m) => {
                    let steps = m.steps.clone();
                    self.play_macro(ctx, &steps);
                }
                None => self.message = Some(format!("No macro {path}")),
            },
            Some(PackageCommand::Layout(layout)) => match Layout::from_json(&layout) {
                Some(layout) => self.set_layout(layout),
                None => self.message = Some("That layout doesn't fit together".into()),
            },
            Some(PackageCommand::EditSettings(base, default)) => self.edit_package_settings(ctx, &base, &default),
            Some(PackageCommand::OpenFile(path)) => {
                let path = self.expand_variables(&path);
                if Path::new(&path).is_file() {
                    self.open_path(Path::new(&path));
                    self.focus_editor(ctx);
                } else {
                    self.message = Some(format!("{path} doesn't exist"));
                }
            }
            Some(PackageCommand::OpenUrl(url)) => {
                if let Err(e) = open_with_system(&url) {
                    self.message = Some(format!("Could not open {url}: {e}"));
                }
            }
            Some(PackageCommand::ShowPanel(name)) => {
                if self.plugins.panels.iter().any(|(n, _)| *n == name) {
                    self.plugins.shown_panel = Some(name);
                }
            }
            Some(PackageCommand::HidePanel) => self.plugins.shown_panel = None,
            Some(PackageCommand::Plugin(name, args)) => self.run_plugin_command(ctx, &name, &args),
            None => {}
        }
    }

    /// Reloads when a package was switched on or off, or (checked when the
    /// window comes back to the front) when files in the package folders changed.
    fn reload_packages_if_changed(&mut self, ctx: &egui::Context) {
        let refocused = ctx.input(|i| i.raw.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true))));
        let files_changed =
            || self.packages.as_ref().is_some_and(|d| d.fingerprint() != self.packages_fingerprint);
        if self.settings.ignored_packages != self.packages_ignored || (refocused && files_changed()) {
            self.reload_packages(ctx);
        }
    }

    fn set_package_enabled(&mut self, ctx: &egui::Context, index: usize, enabled: bool) {
        let Some(name) = self.package_list.get(index).map(|p| p.name.clone()) else {
            return;
        };
        self.settings.ignored_packages.retain(|n| *n != name);
        if !enabled {
            self.settings.ignored_packages.push(name.clone());
        }
        self.save_settings();
        self.reload_packages(ctx);
        let state = if enabled { "Enabled" } else { "Disabled" };
        self.message.get_or_insert(format!("{state} package {name}"));
    }

    fn install_package(&mut self, ctx: &egui::Context) {
        let Some(dirs) = &self.packages else {
            return;
        };
        let Some(file) = FileDialog::new().add_filter("Package", &["te-package", "sublime-package", "zip"]).pick_file() else {
            return;
        };
        match dirs.install(&file) {
            Ok(name) => {
                // Installing means wanting it on, even if an older copy was off.
                self.settings.ignored_packages.retain(|n| *n != name);
                self.save_settings();
                self.message = None;
                self.reload_packages(ctx);
                self.message.get_or_insert(format!("Installed package {name}"));
            }
            Err(e) => self.message = Some(format!("Could not install the package: {e}")),
        }
    }

    fn remove_package(&mut self, ctx: &egui::Context, index: usize) {
        let (Some(dirs), Some(package)) = (&self.packages, self.package_list.get(index)) else {
            return;
        };
        let name = package.name.clone();
        let confirmed = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Remove Package")
            .set_description(format!("Remove the package {name}?"))
            .set_buttons(MessageButtons::OkCancelCustom("Remove".into(), "Cancel".into()))
            .show();
        if confirmed != MessageDialogResult::Custom("Remove".into()) {
            return;
        }
        if let Err(e) = dirs.remove(&name) {
            self.message = Some(format!("Could not remove the package: {e}"));
            return;
        }
        if self.settings.ignored_packages.contains(&name) {
            self.settings.ignored_packages.retain(|n| *n != name);
            self.save_settings();
        }
        self.message = None;
        self.reload_packages(ctx);
        self.message.get_or_insert(format!("Removed package {name}"));
    }

    /// Shows the Packages folder in Finder (or the system's file manager).
    fn browse_packages(&mut self) {
        let Some(dirs) = &self.packages else {
            return;
        };
        let result = dirs.create().and_then(|_| open_with_system(&dirs.packages.to_string_lossy()));
        if let Err(e) = result {
            self.message = Some(format!("Could not open {}: {e}", dirs.packages.display()));
        }
    }

    /// Fills in `${packages}`, `${platform}`, `${file}` and the like, as
    /// in menu and command arguments.
    fn expand_variables(&self, text: &str) -> String {
        let doc = &self.docs[self.active];
        let mut vars: Vec<(&str, String)> = vec![("platform", if cfg!(target_os = "macos") {
            "OSX"
        } else if cfg!(windows) {
            "Windows"
        } else {
            "Linux"
        }
        .to_owned())];
        if let Some(dirs) = &self.packages {
            vars.push(("packages", dirs.packages.display().to_string()));
        }
        if let Some(path) = &doc.path {
            vars.push(("file", path.display().to_string()));
            vars.push(("file_path", path.parent().map(|p| p.display().to_string()).unwrap_or_default()));
            vars.push(("file_name", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
        if let Some(ws) = &self.workspace {
            vars.push(("folder", ws.root.display().to_string()));
        }
        let mut out = text.to_owned();
        for (name, value) in vars {
            out = out.replace(&format!("${{{name}}}"), &value).replace(&format!("${name}"), &value);
        }
        out
    }

    /// A package's settings file beside the user's own copy of it, as in
    /// the package's on the left (read from its zip if need be),
    /// the user's in `Packages/User` on the right, made from `default`.
    fn edit_package_settings(&mut self, ctx: &egui::Context, base: &str, default: &str) {
        let Some(dirs) = self.packages.clone() else {
            return;
        };
        let base = self.expand_variables(base);
        let name = base.rsplit(['/', '\\']).next().unwrap_or(&base).to_owned();
        let user = dirs.packages.join(packages::USER).join(&name);
        if !user.exists() {
            // `$0` marks where the caret goes.
            let text = if default.is_empty() { "{\n\t\n}\n".to_owned() } else { default.replace("$0", "") };
            if let Err(e) = dirs.create().and_then(|_| std::fs::write(&user, text)) {
                self.message = Some(format!("Could not create {}: {e}", user.display()));
                return;
            }
        }
        let before = self.layout.clone();
        self.set_layout(Preset::Columns2.layout());
        self.focus_group = 0;
        let base_path = PathBuf::from(&base);
        if base_path.is_file() {
            self.open_path(&base_path);
        } else if let Some(text) = self.read_package_resource(&base, &dirs) {
            // Inside a zip: shown, not saved back.
            self.new_doc();
            let doc = self.doc();
            doc.buffer = crate::buffer::Buffer::new(&text);
            doc.name = Some(name.clone());
            doc.scratch = true;
        }
        self.sync_groups();
        let base_id = self.docs[self.active].id;
        self.focus_group = 1;
        self.open_path(&user);
        self.sync_groups();
        if before != self.layout {
            self.settings_layout = Some((before, vec![base_id, self.docs[self.active].id]));
        }
        self.focus_editor(ctx);
    }

    /// The text of `.../Packages/Name/path` from that package's folder or zip.
    fn read_package_resource(&self, path: &str, dirs: &Dirs) -> Option<String> {
        let rel = path.strip_prefix(&dirs.packages.display().to_string())?.trim_start_matches(['/', '\\']);
        let (package, file) = rel.split_once(['/', '\\'])?;
        let package = self.package_list.iter().find(|p| p.name == package)?;
        package.read(file)
    }

    /// The Packages submenu: folder shortcuts, then each package to switch
    /// off, on, or remove. In the palette these read e.g.
    /// "Packages: Disable Package: Dracula".
    fn package_menu(&self) -> Vec<Item<Action>> {
        use Action::*;
        use Item as I;
        let available = self.packages.is_some();
        let ignored = &self.settings.ignored_packages;
        let submenu = |label: &str, empty: &str, items: Vec<Item<Action>>| I::Submenu {
            label: label.into(),
            items: if items.is_empty() { vec![I::cmd(empty, ReloadPackages, None).enabled(false)] } else { items },
        };
        let each = |keep: &dyn Fn(&Package) -> bool, action: fn(usize) -> Action| {
            self.package_list
                .iter()
                .enumerate()
                .filter(|(_, p)| keep(p))
                .map(|(i, p)| I::cmd(package_label(p), action(i), None))
                .collect::<Vec<_>>()
        };
        vec![
            I::cmd("Browse Packages…", BrowsePackages, None).enabled(available),
            I::cmd("Install Package…", InstallFromIndex, None).enabled(available && self.index.busy.is_none()),
            I::cmd("Install Package from File…", InstallPackage, None).enabled(available),
            I::cmd("Upgrade Packages", UpgradePackages, None).enabled(available && self.index.busy.is_none()),
            I::cmd("Install Missing Libraries", InstallLibraries, None).enabled(available && self.index.busy.is_none()),
            I::cmd("Reload Packages", ReloadPackages, None).enabled(available),
            I::Separator,
            submenu("Disable Package", "No Enabled Packages", each(&|p| !ignored.contains(&p.name), DisablePackage)),
            submenu("Enable Package", "No Disabled Packages", each(&|p| ignored.contains(&p.name), EnablePackage)),
            submenu("Remove Package", "No Installed Packages", each(&|p| p.archive.is_some(), RemovePackage)),
            I::Separator,
            I::cmd("Show Plugin Console", ShowPluginConsole, None).enabled(available),
        ]
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let before = self.settings.clone();
        let mut open = self.settings_open;
        let mut actions = Vec::new();
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    let s = &mut self.settings;
                    ui.label("Color theme");
                    egui::ComboBox::from_id_salt("theme").selected_text(&s.theme).width(220.0).show_ui(ui, |ui| {
                        for name in &self.theme_names {
                            ui.selectable_value(&mut s.theme, name.clone(), name);
                        }
                    });
                    ui.end_row();
                    ui.label("Font size");
                    ui.add(egui::Slider::new(&mut s.font_size, MIN_FONT_SIZE..=MAX_FONT_SIZE).step_by(1.0));
                    ui.end_row();
                    ui.label("Tab width");
                    ui.add(egui::DragValue::new(&mut s.tab_size).range(1..=16));
                    ui.end_row();
                    ui.label("Indentation");
                    ui.checkbox(&mut s.translate_tabs_to_spaces, "Indent using spaces");
                    ui.end_row();
                    ui.label("Editing");
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.auto_match_enabled, "Close brackets and quotes");
                        ui.checkbox(&mut s.auto_indent, "Keep indentation on Enter");
                        ui.checkbox(&mut s.auto_complete, "Show completions while typing");
                        ui.checkbox(&mut s.hot_exit, "Keep unsaved changes when quitting")
                            .on_hover_text("Quit without asking; unsaved tabs come back next time");
                    });
                    ui.end_row();
                    ui.label("Display");
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.word_wrap, "Word wrap");
                        ui.checkbox(&mut s.line_numbers, "Line numbers");
                        ui.checkbox(&mut s.highlight_line, "Highlight current line");
                        ui.checkbox(&mut s.indent_guides, "Indent guides");
                        ui.checkbox(&mut s.caret_blink, "Blinking caret");
                    });
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Edit settings.json").clicked() {
                        actions.push(Action::OpenSettingsFile);
                    }
                    if ui.button("Reset to defaults").clicked() {
                        self.settings = Settings::default();
                    }
                    if ui.button("Browse Packages…").clicked() {
                        actions.push(Action::BrowsePackages);
                    }
                });
                ui.weak("More themes and languages come from packages.");
            });
        self.settings_open = open;
        if self.settings != before {
            self.settings_changed(ctx);
        }
        for action in actions {
            self.apply(ctx, action);
        }
    }

    // ----- Menus, shortcuts and the palette ----------------------------------

    /// The menu bar as shown, with the keys of package key bindings.
    fn menus(&self) -> Vec<Menu<Action>> {
        let mut menus = self.base_menus();
        let keys: Vec<(KeyboardShortcut, Option<Action>, bool)> = self
            .key_bindings
            .iter()
            .filter(|b| b.keys.len() == 1)
            .map(|b| {
                let action = match &self.package_commands[b.command] {
                    PackageCommand::Run(action) => Some(*action),
                    _ => None,
                };
                (b.keys[0], action, b.context.is_empty())
            })
            .collect();
        if !keys.is_empty() {
            for menu in &mut menus {
                rebind_menu_keys(&mut menu.items, &keys);
            }
        }
        menus
    }

    /// The whole menu bar with its own keys. It is also the source of the
    /// command palette and the keyboard shortcuts, so all three always agree.
    fn base_menus(&self) -> Vec<Menu<Action>> {
        let mac = cfg!(target_os = "macos");
        let doc = &self.docs[self.active];
        let s = &self.settings;
        use Action::*;
        use Command as C;
        use Item as I;

        let mut recent: Vec<Item<Action>> = self
            .recent
            .paths()
            .iter()
            .enumerate()
            .map(|(i, path)| I::cmd(recent::label(path), OpenRecent(i), None))
            .collect();
        if recent.is_empty() {
            recent.push(I::cmd("No Recent Files", ClearRecent, None).enabled(false));
        } else {
            recent.extend([I::Separator, I::cmd("Clear Recent", ClearRecent, None)]);
        }
        let mut file = vec![
            I::cmd("New File", New, key(CMD, Key::N)),
            I::cmd("Open…", Open, key(CMD, Key::O)),
            I::cmd("Open Folder…", OpenFolder, key(CMD_SHIFT, Key::O)),
            I::Submenu { label: "Open Recent".into(), items: recent },
            I::cmd("Refresh Folder", RefreshFolder, None).enabled(self.workspace.is_some()),
            I::cmd("Close Folder", CloseFolder, None).enabled(self.workspace.is_some()),
            I::Separator,
            I::cmd("Save", Save, key(CMD, Key::S)),
            I::cmd("Save As…", SaveAs, key(CMD_SHIFT, Key::S)),
            I::cmd("Save All", SaveAll, key(CMD_ALT, Key::S)),
            I::Separator,
            I::cmd("New View into File", CloneView, None),
            I::cmd("Close Tab", CloseActive, key(CMD, Key::W)),
            I::cmd("Close Window", CloseWindow, key(CMD_SHIFT, Key::W)).enabled(!self.windows.is_empty()),
        ];
        if !mac {
            file.extend([
                I::Separator,
                I::cmd("Settings…", OpenSettings, key(CMD, Key::Comma)),
                I::Submenu { label: "Packages".into(), items: self.package_menu() },
                I::cmd("Quit", Quit, key(CMD, Key::Q)),
            ]);
        }
        let edit = vec![
            I::cmd("Undo", Standard(self::Standard::Undo), key(CMD, Key::Z)),
            I::cmd("Redo", Standard(self::Standard::Redo), key(CMD_SHIFT, Key::Z)),
            I::Separator,
            I::cmd("Cut", Standard(self::Standard::Cut), key(CMD, Key::X)),
            I::cmd("Copy", Standard(self::Standard::Copy), key(CMD, Key::C)),
            I::cmd("Paste", Standard(self::Standard::Paste), key(CMD, Key::V)),
            I::Separator,
            I::cmd("Duplicate Line", Edit(C::DuplicateLine), key(CMD_SHIFT, Key::D)),
            I::cmd("Delete Line", Edit(C::DeleteLine), key(CTRL_SHIFT, Key::K)),
            I::cmd("Insert Line After", Edit(C::InsertLineAfter), key(CMD, Key::Enter)),
            I::cmd("Insert Line Before", Edit(C::InsertLineBefore), key(CMD_SHIFT, Key::Enter)),
            I::Separator,
            I::cmd("Indent", Edit(C::Indent), key(CMD, Key::CloseBracket)),
            I::cmd("Unindent", Edit(C::Outdent), key(CMD, Key::OpenBracket)),
            I::cmd("Toggle Comment", Edit(C::ToggleComment), key(CMD, Key::Slash))
                .enabled(editor_view::line_comment(doc.syntax()).is_some()),
            I::Separator,
            I::cmd("Show Completions", AutoComplete, key(CTRL, Key::Space)),
        ];
        let selection = vec![
            I::cmd("Select All", Standard(self::Standard::SelectAll), key(CMD, Key::A)),
            I::cmd("Expand Selection to Line", Edit(C::ExpandToLine), key(CMD, Key::L)),
            I::cmd("Split into Lines", Edit(C::SplitIntoLines), key(CMD_SHIFT, Key::L)),
            I::Separator,
            I::cmd("Add Next Occurrence", Edit(C::AddNextOccurrence), key(CMD, Key::D)),
            I::cmd("Select All Occurrences", Edit(C::SelectAllOccurrences), key(CMD_CTRL, Key::G)),
            I::cmd("Add Caret Above", Edit(C::AddCaretAbove), key(CTRL_SHIFT, Key::ArrowUp)),
            I::cmd("Add Caret Below", Edit(C::AddCaretBelow), key(CTRL_SHIFT, Key::ArrowDown)),
            I::cmd("Single Selection", Edit(C::SingleSelection), None),
        ];
        let find = vec![
            I::cmd("Find…", Find, key(CMD, Key::F)),
            I::cmd("Find Next", FindNext, key(CMD, Key::G)),
            I::cmd("Find Previous", FindPrev, key(CMD_SHIFT, Key::G)),
            I::cmd("Find All", FindAll, None),
            I::Separator,
            I::cmd("Find in Files…", FindInFiles, key(CMD_SHIFT, Key::F)),
            I::cmd("Next Result", NextResult, key(Modifiers::NONE, Key::F4)).enabled(self.find_files.has_results()),
            I::cmd("Previous Result", PrevResult, key(Modifiers::SHIFT, Key::F4)).enabled(self.find_files.has_results()),
        ];
        let goto = vec![
            I::cmd("Goto Anything…", GotoAnything, key(CMD, Key::P)),
            // Control, not Command, on macOS too.
            I::cmd("Go to Line…", GotoLine, key(CTRL, Key::G)),
            I::Separator,
            I::cmd("Next Tab", NextTab, key(CMD_ALT, Key::ArrowRight)),
            I::cmd("Previous Tab", PrevTab, key(CMD_ALT, Key::ArrowLeft)),
        ];
        let themes = self
            .theme_names
            .iter()
            .enumerate()
            .map(|(i, name)| I::check(name.clone(), SetTheme(i), *name == s.theme))
            .collect();
        let syntaxes = self
            .syntax_names
            .iter()
            .enumerate()
            .map(|(i, name)| I::check(name.clone(), SetSyntax(i), name == doc.syntax()))
            .collect();
        let digit = |n: usize| [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5][n.min(4)];
        let layouts = Preset::ALL
            .into_iter()
            .map(|p| {
                let keys = match p {
                    Preset::Single => key(CMD_ALT, Key::Num1),
                    Preset::Columns2 => key(CMD_ALT, Key::Num2),
                    Preset::Columns3 => key(CMD_ALT, Key::Num3),
                    Preset::Columns4 => key(CMD_ALT, Key::Num4),
                    Preset::Rows2 => key(CMD_ALT_SHIFT, Key::Num2),
                    Preset::Rows3 => key(CMD_ALT_SHIFT, Key::Num3),
                    Preset::Grid4 => key(CMD_ALT, Key::Num5),
                };
                let item = I::check(p.label(), SetLayout(p), self.layout.preset() == Some(p));
                match keys {
                    Some(k) => item.shortcut(k),
                    None => item,
                }
            })
            .collect();
        let groups = self.groups.len();
        let focus_groups = (0..groups.max(4))
            .map(|g| I::cmd(format!("Group {}", g + 1), FocusGroup(g), key(CTRL, digit(g))).enabled(g < groups))
            .collect();
        let move_groups = (0..groups.max(4))
            .map(|g| I::cmd(format!("Group {}", g + 1), MoveToGroup(g), key(CTRL_SHIFT, digit(g))).enabled(g < groups))
            .collect();
        let view = vec![
            I::cmd("Command Palette…", CommandPalette, key(CMD_SHIFT, Key::P)),
            I::check("Side Bar", ToggleSidebar, s.show_sidebar).shortcut(KeyboardShortcut::new(CMD, Key::Backslash)),
            I::check("Word Wrap", Toggle(Flag::WordWrap), s.word_wrap),
            I::Separator,
            I::Submenu { label: "Layout".into(), items: layouts },
            I::Submenu { label: "Focus Group".into(), items: focus_groups },
            I::Submenu { label: "Move File to Group".into(), items: move_groups },
            I::Separator,
            I::Submenu { label: "Theme".into(), items: themes },
            I::Submenu { label: "Syntax".into(), items: syntaxes },
            I::Separator,
            I::cmd("Larger Font", FontBigger, key(CMD, Key::Equals)),
            I::cmd("Smaller Font", FontSmaller, key(CMD, Key::Minus)),
            I::cmd("Reset Font Size", FontReset, key(CMD, Key::Num0)),
            I::Separator,
            I::check("Line Numbers", Toggle(Flag::LineNumbers), s.line_numbers),
            I::check("Highlight Current Line", Toggle(Flag::HighlightLine), s.highlight_line),
            I::check("Indent Guides", Toggle(Flag::IndentGuides), s.indent_guides),
            I::check("Indent Using Spaces", Toggle(Flag::TabsToSpaces), s.translate_tabs_to_spaces),
            I::check("Close Brackets Automatically", Toggle(Flag::AutoMatch), s.auto_match_enabled),
            I::check("Show Completions While Typing", Toggle(Flag::AutoComplete), s.auto_complete),
            I::check("Blinking Caret", Toggle(Flag::CaretBlink), s.caret_blink),
        ];
        let recording = self.recording.is_some();
        let mut macros: Vec<Item<Action>> =
            self.macro_files.iter().enumerate().map(|(i, m)| I::cmd(m.name(), RunMacro(i), None)).collect();
        if macros.is_empty() {
            macros.push(I::cmd("No Macros", PlaybackMacro, None).enabled(false));
        }
        let tools = vec![
            I::cmd(if recording { "Stop Recording Macro" } else { "Record Macro" }, RecordMacro, if mac { key(CTRL, Key::Q) } else { None }),
            I::cmd("Playback Macro", PlaybackMacro, if mac { key(CTRL_SHIFT, Key::Q) } else { None })
                .enabled(!recording && !self.last_macro.is_empty()),
            I::cmd("Save Macro…", SaveMacro, None).enabled(!recording && !self.last_macro.is_empty() && self.packages.is_some()),
            I::Submenu { label: "Macros".into(), items: macros },
        ];

        let mut menus = Vec::new();
        if mac {
            menus.push(Menu {
                title: "TextEditor".into(),
                items: vec![
                    I::Native(NativeItem::About),
                    I::Separator,
                    I::cmd("Settings…", OpenSettings, key(CMD, Key::Comma)),
                    I::cmd("Edit settings.json", OpenSettingsFile, None),
                    I::Submenu { label: "Packages".into(), items: self.package_menu() },
                    I::Separator,
                    I::Native(NativeItem::Services),
                    I::Separator,
                    I::Native(NativeItem::Hide),
                    I::Native(NativeItem::HideOthers),
                    I::Native(NativeItem::ShowAll),
                    I::Separator,
                    I::cmd("Quit TextEditor", Quit, key(CMD, Key::Q)),
                ],
            });
        }
        for (title, items) in [
            ("File", file),
            ("Edit", edit),
            ("Selection", selection),
            ("Find", find),
            ("Goto", goto),
            ("Tools", tools),
            ("View", view),
        ] {
            menus.push(Menu { title: title.into(), items });
        }
        if mac {
            menus.push(Menu {
                title: "Window".into(),
                items: vec![
                    I::Native(NativeItem::Minimize),
                    I::Native(NativeItem::Zoom),
                    I::Native(NativeItem::Fullscreen),
                    I::Separator,
                    I::Native(NativeItem::BringAllToFront),
                ],
            });
        }
        self.add_package_menus(&mut menus);
        menus
    }

    /// Every menu command as (palette name, action, shortcut).
    fn commands(&self) -> Vec<(String, Action, Option<KeyboardShortcut>)> {
        Self::commands_in(self.menus())
    }

    fn commands_in(menus: Vec<Menu<Action>>) -> Vec<(String, Action, Option<KeyboardShortcut>)> {
        fn walk(
            prefix: &str,
            items: Vec<Item<Action>>,
            out: &mut Vec<(String, Action, Option<KeyboardShortcut>)>,
        ) {
            for item in items {
                match item {
                    Item::Command { label, action, shortcut, enabled: true, .. } => {
                        out.push((format!("{prefix}: {}", label.trim_end_matches('…')), action, shortcut));
                    }
                    Item::Submenu { label, items } => walk(&format!("{prefix}: {label}"), items, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        for menu in menus {
            let prefix = if menu.title == "TextEditor" { "App".to_owned() } else { menu.title };
            walk(&prefix, menu.items, &mut out);
        }
        out
    }

    /// The action for a key pressed this frame, if any: package key bindings
    /// first (later packages winning), then the menus' own keys.
    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Action> {
        // Package bindings need exactly their modifiers: a binding for `'`
        // must not catch `"` (Shift-'). The menus' keys are matched the
        // looser egui way, with the most modifiers first.
        // Conditions are only checked for bindings of keys pressed just now.
        let pressed: Vec<Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                })
                .collect()
        });
        if pressed.is_empty() {
            return Vec::new();
        }
        let wanted = |b: &KeyBinding| {
            let key = match self.pending_key {
                Some(first) if b.keys.len() == 2 && b.keys[0] == first => b.keys[1],
                _ => b.keys[0],
            };
            pressed.contains(&key.logical_key)
        };
        let candidates: Vec<(Vec<KeyboardShortcut>, Vec<Condition>, usize)> = self
            .key_bindings
            .iter()
            .rev()
            .filter(|b| wanted(b))
            .map(|b| (b.keys.clone(), b.context.clone(), b.command))
            .collect();
        let mut table: Vec<(Vec<KeyboardShortcut>, Action, bool)> = Vec::new();
        for (keys, context, command) in candidates {
            if self.context_holds(ctx, &context) {
                table.push((keys, Action::PackageCommand(command), true));
            }
        }
        table.extend(
            Self::commands_in(self.base_menus())
                .into_iter()
                // Clipboard and undo keys reach widgets directly as egui events.
                .filter(|(_, action, _)| !matches!(action, Action::Standard(_)))
                .filter_map(|(_, action, key)| Some((vec![key?], action, false))),
        );
        // Most modifiers first: a plain ⌘S check would also match ⇧⌘S.
        table.sort_by_key(|(keys, ..)| {
            let m = keys[0].modifiers;
            std::cmp::Reverse(m.shift as u8 + m.alt as u8 + m.ctrl as u8 + m.command as u8)
        });
        let pending = self.pending_key;
        let (action, pending) = ctx.input_mut(|i| {
            if let Some(first) = pending {
                // The next key completes a two-key binding, or cancels it.
                let done = table.iter().filter(|(k, ..)| k.len() == 2 && k[0] == first).find(|(k, ..)| consume_exact(i, &k[1]));
                if done.is_some() {
                    i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                }
                return (done.map(|(_, a, _)| *a), None);
            }
            for (keys, action, exact) in &table {
                let pressed = if *exact { consume_exact(i, &keys[0]) } else { i.consume_shortcut(&keys[0]) };
                if pressed {
                    // A bound key that types something (like `'`) must not also type it.
                    i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    return if keys.len() == 1 { (Some(*action), None) } else { (None, Some(keys[0])) };
                }
            }
            (None, None)
        });
        self.pending_key = pending;
        action.into_iter().collect()
    }

    fn open_palette(&mut self, ctx: &egui::Context, mode: Mode, query: &str) {
        self.palette.show(mode, query);
        (self.palette_candidates, self.palette_targets) = self.palette_rows(ctx, mode).into_iter().unzip();
    }

    fn palette_rows(&self, ctx: &egui::Context, mode: Mode) -> Vec<(Candidate, Target)> {
        if mode == Mode::Quick && self.index.offer.is_some() {
            return self.index_rows();
        }
        if matches!(mode, Mode::Quick | Mode::Input) {
            let items = self.plugins.prompt.as_ref().map(|p| p.items.as_slice()).unwrap_or_default();
            return items
                .iter()
                .enumerate()
                .map(|(i, (text, detail))| {
                    (Candidate { text: text.clone(), title_start: 0, hint: detail.clone() }, Target::Plugin(i))
                })
                .collect();
        }
        if mode == Mode::Commands {
            let row = |text: String, hint: String, action| (Candidate { text, title_start: 0, hint }, Target::Run(action));
            let mut rows: Vec<(Candidate, Target)> = self
                .commands()
                .into_iter()
                .map(|(name, action, key)| row(name, key.map(|k| ctx.format_shortcut(&k)).unwrap_or_default(), action))
                .collect();
            // Package entries, and the snippets that fit the current file.
            for (caption, command) in &self.palette_entries {
                rows.push(row(caption.clone(), String::new(), Action::PackageCommand(*command)));
            }
            let doc = &self.docs[self.active];
            let scopes = editor_view::scopes_at(doc, &self.highlighter, doc.buffer.primary().head);
            for (i, snippet) in self.snippets.iter().enumerate().filter(|(_, s)| s.applies_to(&scopes)) {
                rows.push(row(format!("Snippet: {}", snippet.title()), snippet.trigger.clone(), Action::InsertSnippet(i)));
            }
            return rows;
        }

        let ws = self.workspace.as_ref();
        let path_row = |text: String, hint: Option<&str>| {
            // The title is the file name; the folders above it go on the right.
            let split = text.rfind('/').map_or(0, |i| i + 1);
            let title_start = text[..split].chars().count();
            let hint = hint.map_or_else(|| text[..split].trim_end_matches('/').to_owned(), str::to_owned);
            Candidate { text, title_start, hint }
        };
        // Open tabs first, then every file in the folder that isn't already open.
        let mut rows: Vec<(Candidate, Target)> = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, doc)| !self.stashed(doc.id))
            .map(|(i, doc)| {
                let text = doc
                    .path
                    .as_deref()
                    .and_then(|p| ws.and_then(|w| w.relative(p)))
                    .unwrap_or_else(|| doc.title());
                (path_row(text, Some("open tab")), Target::Tab(i))
            })
            .collect();
        if let Some(ws) = ws {
            let open: Vec<&Path> = self.docs.iter().filter_map(|d| d.path.as_deref()).collect();
            for rel in &ws.files {
                let path = ws.root.join(rel);
                if !open.contains(&path.as_path()) {
                    rows.push((path_row(rel.clone(), None), Target::File(path)));
                }
            }
        }
        rows
    }

    fn pick(&mut self, ctx: &egui::Context, pick: Pick, actions: &mut Vec<Action>) {
        let line = match pick {
            Pick::Line(line) => Some(line),
            Pick::Text(text) => {
                self.answer_plugin_prompt(Some(Value::from(text)));
                None
            }
            Pick::Item { index, line } => {
                match &self.palette_targets[index] {
                    Target::IndexPackage(i) => {
                        let i = *i;
                        self.pick_index_package(ctx, i);
                        self.focus_editor(ctx);
                        return;
                    }
                    Target::Plugin(i) => {
                        let i = *i;
                        self.answer_plugin_prompt(Some(Value::from(i)));
                        self.focus_editor(ctx);
                        return;
                    }
                    Target::Tab(i) => {
                        let i = (*i).min(self.docs.len() - 1);
                        self.activate(i);
                    }
                    Target::File(path) => self.open_path(&path.clone()),
                    Target::Run(action) => {
                        actions.push(*action);
                        self.focus_editor(ctx);
                        return;
                    }
                }
                line
            }
        };
        if let Some(line) = line {
            self.goto_line(line);
        }
        self.focus_editor(ctx);
    }

    // ----- Actions -----------------------------------------------------------

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        // Whatever the action edits, the active tab holds its text.
        self.sync_clones();
        if self.recording.is_some() {
            self.record_action(action);
        }
        match action {
            Action::New => self.new_doc(),
            Action::Open => {
                if let Some(paths) = FileDialog::new().pick_files() {
                    for p in paths {
                        self.open_path(&p);
                    }
                }
            }
            Action::OpenFolder => {
                if let Some(path) = FileDialog::new().pick_folder() {
                    self.open_folder(ctx, path);
                }
            }
            Action::OpenRecent(i) => {
                let Some(path) = self.recent.paths().get(i).cloned() else {
                    return;
                };
                if path.exists() {
                    self.open_any(ctx, path);
                    self.focus_editor(ctx);
                } else {
                    self.message = Some(format!("{} no longer exists", path.display()));
                    self.recent.remove(&path);
                }
            }
            Action::ClearRecent => self.recent.clear(),
            Action::CloseFolder => self.workspace = None,
            Action::RefreshFolder => {
                if let Some(ws) = &mut self.workspace {
                    ws.refresh(ctx);
                }
            }
            Action::Save => {
                self.save(ctx, self.active, false);
            }
            Action::SaveAs => {
                self.save(ctx, self.active, true);
            }
            Action::SaveAll => self.save_all(ctx),
            Action::CloseActive => self.close(ctx, self.active),
            Action::CloneView => self.clone_active_view(ctx),
            Action::NewWindow => self.new_window(),
            Action::CloseWindow => self.close_window(ctx),
            Action::Close(i) => self.close(ctx, i),
            Action::Find => {
                self.find.open = true;
                self.find_files.open = false;
                self.find.request_focus = true;
                // Start from the selected text.
                let p = self.doc().buffer.primary();
                if !p.is_empty() && self.doc().buffer.line_of(p.start()) == self.doc().buffer.line_of(p.end()) {
                    self.find.query = self.doc().buffer.slice(p.start(), p.end());
                }
            }
            Action::FindNext => self.find_step(true),
            Action::FindPrev => self.find_step(false),
            Action::FindAll => self.find_all(ctx),
            Action::FindInFiles => self.open_find_in_files(),
            Action::SearchFiles => self.search_files(ctx),
            Action::ReplaceInFiles => self.confirm_replace_in_files(ctx),
            Action::NextResult => self.step_result(ctx, true),
            Action::PrevResult => self.step_result(ctx, false),
            Action::GotoAnything => self.open_palette(ctx, Mode::Files, ""),
            Action::CommandPalette => self.open_palette(ctx, Mode::Commands, ""),
            Action::GotoLine => self.open_palette(ctx, Mode::Files, ":"),
            Action::ToggleSidebar => match &mut self.sidebar_override {
                // In a plugin's window, for that window only.
                Some(shown) => *shown = !*shown,
                None => {
                    self.settings.show_sidebar = !self.settings.show_sidebar;
                    self.save_settings();
                }
            },
            Action::NextTab => self.step_tab(ctx, 1),
            Action::PrevTab => self.step_tab(ctx, -1),
            Action::Edit(cmd) if self.doc().read_only && !cmd.only_selects() => self.message = Some("This view is read-only".into()),
            Action::Edit(cmd) => {
                let settings = self.active_settings();
                editor_view::run(ctx, self.doc(), cmd, &settings);
            }
            Action::Standard(op) => self.standard(ctx, op),
            Action::SetSyntax(i) => {
                if let Some(name) = self.syntax_names.get(i).cloned() {
                    self.doc().set_syntax(name);
                }
            }
            Action::SetTheme(i) => {
                if let Some(name) = self.theme_names.get(i) {
                    self.settings.theme = name.clone();
                    self.settings_changed(ctx);
                }
            }
            Action::FontBigger | Action::FontSmaller | Action::FontReset => {
                self.settings.font_size = match action {
                    Action::FontBigger => self.settings.font_size + 1.0,
                    Action::FontSmaller => self.settings.font_size - 1.0,
                    _ => Settings::default().font_size,
                };
                self.settings_changed(ctx);
            }
            Action::Toggle(flag) => {
                let value = flag.get(&mut self.settings);
                *value = !*value;
                self.settings_changed(ctx);
            }
            Action::OpenSettings => self.settings_open = true,
            Action::OpenSettingsFile => {
                let Some(path) = settings::settings_path() else {
                    return;
                };
                if !path.exists()
                    && let Err(e) = self.settings.save()
                {
                    self.message = Some(format!("Could not create the settings file: {e}"));
                    return;
                }
                self.open_path(&path);
                self.settings_open = false;
                self.focus_editor(ctx);
            }
            Action::BrowsePackages => self.browse_packages(),
            Action::InstallPackage => self.install_package(ctx),
            Action::InstallFromIndex => self.install_from_index(ctx),
            Action::UpgradePackages => self.upgrade_packages(ctx),
            Action::InstallLibraries => self.install_missing_libraries(ctx),
            Action::ReloadPackages => {
                self.message = None;
                self.reload_packages(ctx);
                let count = self.package_list.len();
                self.message.get_or_insert(format!("Reloaded {count} package{}", if count == 1 { "" } else { "s" }));
            }
            Action::DisablePackage(i) => self.set_package_enabled(ctx, i, false),
            Action::EnablePackage(i) => self.set_package_enabled(ctx, i, true),
            Action::RemovePackage(i) => self.remove_package(ctx, i),
            Action::PackageCommand(i) => self.run_package_command(ctx, i),
            Action::ShowPluginConsole => self.show_plugin_console(),
            Action::InsertSnippet(i) => {
                if let Some(template) = self.snippets.get(i).map(|s| s.content.clone()) {
                    let settings = self.active_settings();
                    editor_view::insert_snippet(self.doc(), &template, &settings, 0);
                }
            }
            Action::SetLayout(preset) => self.set_layout(preset.layout()),
            Action::FocusGroup(g) => self.focus_group_now(ctx, g),
            Action::MoveToGroup(g) => self.move_to_group(ctx, g),
            Action::AutoComplete => self.show_completions(ctx, true),
            Action::CommitCompletion => self.accept_completion(ctx),
            Action::HideCompletion => self.completion = None,
            Action::RecordMacro => self.toggle_recording(),
            Action::PlaybackMacro => {
                let steps = self.last_macro.clone();
                self.play_macro(ctx, &steps);
            }
            Action::SaveMacro => self.save_macro(ctx),
            Action::RunMacro(i) => {
                if let Some(m) = self.macro_files.get(i) {
                    let steps = m.steps.clone();
                    self.play_macro(ctx, &steps);
                }
            }
            Action::Quit => {
                // With hot exit, unsaved work is kept for next time instead of asking.
                let quit = self.settings.hot_exit || self.confirm_quit(ctx);
                if quit {
                    self.save_session();
                    self.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    /// Menu-bar Undo/Cut/Paste… become the matching egui input next frame, so
    /// they act on whatever has focus: the editor, find bar or palette.
    fn standard(&mut self, ctx: &egui::Context, op: Standard) {
        let key = |key: Key, modifiers: Modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let event = match op {
            Standard::Undo => key(Key::Z, CMD),
            Standard::Redo => key(Key::Z, CMD_SHIFT),
            Standard::SelectAll => key(Key::A, CMD),
            Standard::Cut => egui::Event::Cut,
            Standard::Copy => egui::Event::Copy,
            Standard::Paste => {
                let text = arboard::Clipboard::new().and_then(|mut c| c.get_text());
                match text {
                    Ok(text) => egui::Event::Paste(text),
                    Err(_) => return,
                }
            }
        };
        self.pending_events.push(event);
        ctx.request_repaint();
    }

    /// Selects the next (or previous) match after the selection, wrapping around.
    fn find_step(&mut self, forward: bool) {
        if self.find.query.is_empty() {
            self.find.open = true;
            self.find.request_focus = true;
            return;
        }
        let (query, case) = (self.find.query.clone(), self.find.match_case);
        let b = &mut self.docs[self.active].buffer;
        let matches = b.find_all(&query, case);
        if matches.is_empty() {
            self.find.status = "No matches".to_owned();
            return;
        }
        let p = b.primary();
        let pick = if forward {
            matches.iter().position(|&(s, _)| s >= p.end()).unwrap_or(0)
        } else {
            matches.iter().rposition(|&(_, e)| e <= p.start()).unwrap_or(matches.len() - 1)
        };
        let (start, end) = matches[pick];
        b.set_single(Selection::new(start, end));
        self.doc().view.reveal(true);
        self.find.status = format!("{} of {}", pick + 1, matches.len());
    }

    /// Selects every match at once, ready for multi-cursor editing.
    fn find_all(&mut self, ctx: &egui::Context) {
        let (query, case) = (self.find.query.clone(), self.find.match_case);
        let b = &mut self.docs[self.active].buffer;
        let matches = b.find_all(&query, case);
        if matches.is_empty() {
            self.find.status = "No matches".to_owned();
            return;
        }
        let count = matches.len();
        b.set_selections(matches.into_iter().map(|(s, e)| Selection::new(s, e)).collect(), 0);
        self.doc().view.reveal(true);
        self.find.status = format!("{count} selected");
        self.focus_editor(ctx);
    }

    /// Puts the cursor at the start of a 1-based line (clamped to the last line).
    fn goto_line(&mut self, line: usize) {
        let b = &mut self.docs[self.active].buffer;
        let line = line.saturating_sub(1).min(b.line_count() - 1);
        let at = b.line_start(line);
        b.set_single(Selection::caret(at));
        self.doc().view.reveal(true);
    }

    // ----- Panels ------------------------------------------------------------

    fn find_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label("Find:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.find.query).desired_width(260.0).hint_text("text to find"),
            );
            if self.find.request_focus {
                field.request_focus();
                self.find.request_focus = false;
            }
            if field.changed() {
                self.find.status.clear();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let mods = ui.input(|i| i.modifiers);
                actions.push(if mods.alt {
                    Action::FindAll
                } else if mods.shift {
                    Action::FindPrev
                } else {
                    Action::FindNext
                });
                self.find.request_focus = !mods.alt;
            }
            ui.checkbox(&mut self.find.match_case, "Match case");
            if ui.button("Previous").clicked() {
                actions.push(Action::FindPrev);
            }
            if ui.button("Next").clicked() {
                actions.push(Action::FindNext);
            }
            if ui.button("Find All").on_hover_text("Select every match (Option+Enter)").clicked() {
                actions.push(Action::FindAll);
            }
            ui.label(&self.find.status);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let escape = field.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
                if ui.small_button("×").clicked() || escape {
                    self.find.open = false;
                    actions.push(Action::Edit(Command::SingleSelection));
                }
            });
        });
    }

    /// Open files, then the open folder's tree.
    fn sidebar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let active_path = self.docs[self.active].path.clone();
        let mut open = None;
        let mut switch_to = None;
        // Right-clicking a file or folder: its menu, with packages' items.
        let extras = self.runnable_menu(&self.package_side_bar_menu);
        let mut pick = None;
        let mut menu = |response: &egui::Response, path: &Path, is_dir: bool| {
            response.context_menu(|ui| package_menus::side_bar_menu(ui, path, is_dir, &extras, &mut pick));
        };
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.weak("OPEN FILES");
            // With split views, each group's tabs under its own heading.
            let split = self.groups.len() > 1;
            for (g, group) in self.groups.iter().enumerate() {
                if split {
                    ui.weak(format!("  GROUP {}", g + 1));
                }
                for i in group.views.iter().filter_map(|id| self.docs.iter().position(|d| d.id == *id)) {
                    let doc = &self.docs[i];
                    ui.horizontal(|ui| {
                        let label = if doc.dirty() { format!("{} •", doc.title()) } else { doc.title() };
                        let row = ui.selectable_label(i == self.active, label);
                        if row.clicked() {
                            switch_to = Some(i);
                        }
                        if let Some(path) = &doc.path {
                            row.on_hover_text(path.display().to_string());
                        }
                        if ui.small_button("×").on_hover_text("Close").clicked() {
                            actions.push(Action::Close(i));
                        }
                    });
                }
            }
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.weak("FOLDERS");
                if self.workspace.as_ref().is_some_and(|ws| ws.scanning()) {
                    ui.spinner().on_hover_text("Indexing files for Goto Anything");
                }
            });
            let Some(ws) = &mut self.workspace else {
                if ui.button("Open Folder…").clicked() {
                    actions.push(Action::OpenFolder);
                }
                return;
            };
            if ws.truncated {
                ui.weak(format!("Only the first {} files are searchable", ws.files.len()));
            }
            let root = ws.root.clone();
            let header = egui::CollapsingHeader::new(ws.name())
                .id_salt(("folder-root", &root))
                .default_open(true)
                .show(ui, |ui| folder_tree(ui, ws, &root, active_path.as_deref(), &mut open, &mut menu))
                .header_response
                .on_hover_text(root.display().to_string());
            menu(&header, &root, true);
        });
        if let Some(pick) = pick {
            self.menu_pick(ui.ctx(), pick);
        }
        if let Some(i) = switch_to {
            self.activate(i);
            self.focus_editor(ui.ctx());
        }
        if let Some(path) = open {
            self.open_path(&path);
            self.focus_editor(ui.ctx());
        }
    }

    fn has_native_menu(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.native_menu.is_some();
        #[cfg(not(target_os = "macos"))]
        return false;
    }

    /// A plugin's output panel: its text, read-only, with a close button.
    fn output_panel(&mut self, ui: &mut egui::Ui, name: &str) {
        let Some((_, doc)) = self.plugins.panels.iter().find(|(n, _)| n == name) else {
            self.plugins.shown_panel = None;
            return;
        };
        let text = doc.buffer.text();
        let mut close = false;
        ui.horizontal(|ui| {
            ui.weak(name);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| close = ui.small_button("Close").clicked());
        });
        egui::ScrollArea::both().stick_to_bottom(true).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).monospace()).wrap_mode(egui::TextWrapMode::Extend));
        });
        if close {
            self.plugins.shown_panel = None;
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let doc = &self.docs[self.active];
        let b = &doc.buffer;
        let settings = self.settings_for(doc.id);
        // The left side gives way (cut short with …) to the right side.
        let left = |ui: &mut egui::Ui| {
            let head = b.primary().head;
            let line = b.line_of(head);
            ui.label(format!("Line {}, Column {}", line + 1, head - b.line_start(line) + 1));
            let count = b.selections().len();
            if count > 1 {
                ui.separator();
                ui.label(format!("{count} selection regions"));
            }
            if self.recording.is_some() {
                ui.separator();
                ui.colored_label(ui.visuals().warn_fg_color, "Recording Macro");
            }
            if !self.windows.is_empty() {
                ui.separator();
                ui.label("A plugin's window: close its tabs (or File > Close Window) to get yours back");
            }
            if let Some(busy) = &self.index.busy {
                ui.separator();
                ui.spinner();
                ui.label(busy);
            }
            if let Some(name) = self.plugins.running.values().next() {
                ui.separator();
                ui.spinner();
                ui.label(format!("Running {name}…"));
            }
            if let Some(progress) = self.find_files.progress() {
                ui.separator();
                ui.spinner();
                ui.label(progress);
            }
            if let Some(msg) = &self.message {
                ui.separator();
                ui.label(msg);
            }
            // Plugins' status text for this view, in key order.
            for text in self.plugins.status.get(&doc.id).into_iter().flat_map(|s| s.values()) {
                ui.separator();
                ui.label(text);
            }
        };
        let right = |ui: &mut egui::Ui| {
            ui.label(doc.syntax());
            ui.separator();
            let indent = if settings.translate_tabs_to_spaces { "Spaces" } else { "Tab Size" };
            ui.label(format!("{indent}: {}", settings.tab_size));
            ui.separator();
            ui.label(doc.line_ending_label());
            ui.separator();
            ui.label("UTF-8");
        };
        egui::Sides::new().shrink_left().truncate().show(ui, left, right);
    }
}

impl eframe::App for EditorApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.pending_events);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.reload_packages_if_changed(&ctx);
        self.poll_syntaxes();
        self.poll_plugins(&ctx);
        self.poll_find_in_files(&ctx);
        self.poll_index(&ctx);
        self.sync_groups();
        self.sync_clones();
        let menus = self.menus();
        #[cfg(target_os = "macos")]
        let mut actions = match &mut self.native_menu {
            Some(native) => {
                native.update(&menus);
                native.poll()
            }
            None => Vec::new(),
        };
        #[cfg(not(target_os = "macos"))]
        let mut actions = Vec::new();
        // The autocomplete list and plugin popups take their keys (arrows,
        // Enter, Esc) before key bindings and the editor see them.
        self.completion_keys(&ctx, &mut actions);
        self.plugin_popup_keys(&ctx);
        actions.extend(self.shortcuts(&ctx));

        // Window close button / Cmd+Q from the OS: ask about unsaved files first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            // On a Mac, closing the window leaves the app running in the
            // Dock; ⌘Q (which allows the close) quits. Everything is kept
            // in the session meanwhile, as when quitting.
            #[cfg(target_os = "macos")]
            let hidden = {
                self.save_session();
                crate::mac_open::hide_app()
            };
            #[cfg(not(target_os = "macos"))]
            let hidden = false;
            if !hidden {
                actions.push(Action::Quit);
            }
        }

        // Files dragged onto the window open as tabs; a folder opens in the side bar.
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in dropped.into_iter().filter(|p| !p.as_os_str().is_empty()) {
            self.open_any(&ctx, path);
        }
        // Files opened from Finder or the Dock.
        #[cfg(target_os = "macos")]
        for path in crate::mac_open::take() {
            self.open_any(&ctx, path);
            self.focus_editor(&ctx);
        }

        // The palette goes first so its keys (arrows, Enter, Esc) never reach the editor.
        if self.workspace.as_mut().is_some_and(|ws| ws.poll()) && self.palette.open {
            (self.palette_candidates, self.palette_targets) =
                self.palette_rows(&ctx, self.palette.mode).into_iter().unzip();
            self.palette.invalidate();
        }
        if self.palette.open {
            match self.palette.ui(&ctx, &self.palette_candidates) {
                Some(pick) => self.pick(&ctx, pick, &mut actions),
                None if !self.palette.open => {
                    // A plugin's quick or input panel was dismissed, or the package list.
                    self.index.offer = None;
                    self.answer_plugin_prompt(None);
                    self.focus_editor(&ctx);
                }
                None => {}
            }
        }
        if self.settings_open {
            self.settings_window(&ctx);
        }

        if !self.has_native_menu() {
            egui::Panel::top("menu").show(ui, |ui| crate::menu::egui_menu_bar(ui, &menus, &mut actions));
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.sidebar_override.unwrap_or(self.settings.show_sidebar) {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(220.0)
                .size_range(120.0..=500.0)
                .show(ui, |ui| self.sidebar(ui, &mut actions));
        }
        if self.find.open {
            egui::Panel::bottom("find").show(ui, |ui| self.find_bar(ui, &mut actions));
        }
        if self.find_files.open {
            egui::Panel::bottom("find-in-files").show(ui, |ui| self.find_in_files_panel(ui, &mut actions));
        }
        if let Some(name) = self.plugins.shown_panel.clone() {
            egui::Panel::bottom("output").resizable(true).default_size(160.0).show(ui, |ui| self.output_panel(ui, &name));
        }

        // What reaches the editor this frame: typing opens the autocomplete
        // list, and a macro being recorded takes note of it.
        let typed = self.editor_focused(&ctx) && ctx.input(|i| i.events.iter().any(is_typing));
        if self.recording.is_some() {
            self.record_input(&ctx);
        }
        let editor_frame = egui::Frame::NONE.fill(self.highlighter.colors().background);
        egui::CentralPanel::default().frame(editor_frame).show(ui, |ui| self.editor_area(ui, &mut actions));
        self.update_completion(&ctx, typed);
        self.completion_ui(&ctx);
        self.plugin_popup_ui(&ctx);

        for action in actions {
            self.apply(&ctx, action);
        }
        self.sync_groups();
        self.sync_clones();
        self.send_plugin_events(&ctx);

        // Keep the session current so even a crash or Force Quit loses at most a second.
        if self.session_fingerprint() != self.session_saved {
            if self.last_session_save.elapsed() >= SESSION_SAVE_EVERY {
                self.save_session();
            } else {
                ctx.request_repaint_after(SESSION_SAVE_EVERY);
            }
        }

        let doc = &self.docs[self.active];
        let folder = self.workspace.as_ref().map(|ws| format!(" — {}", ws.name())).unwrap_or_default();
        let dirty = if doc.dirty() { " ●" } else { "" };
        let title = format!("{}{dirty}{folder} — TextEditor", doc.title());
        if title != self.window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

/// Whether an input event types into the editor.
fn is_typing(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Text(_) | egui::Event::Ime(egui::ImeEvent::Commit(_)))
}

/// Opens a folder or a web address with the system's own app for it.
fn open_with_system(target: &str) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(target).spawn().map(|_| ())
}

/// One message for package files that could not be read, or `None`.
fn problems(errors: &[String]) -> Option<String> {
    let first = errors.first()?;
    Some(match errors.len() {
        1 => format!("Could not load {first}"),
        n => format!("Could not load {first} (and {} more)", n - 1),
    })
}

/// The editor's version of a command packages name (TextEditor's own names,
/// which are also Sublime Text's), for the commands it has.
fn translate(
    cmd: &NamedCommand,
    snippets: &[Snippet],
    plugins: &HashMap<String, String>,
) -> Option<self::PackageCommand> {
    use Action::*;
    use Command as C;
    // `self::` because `Action::*` brings in a variant of the same name.
    use self::PackageCommand::Run;
    let arg = |key: &str| cmd.args.get(key).and_then(Value::as_str);
    let flag = |key: &str| cmd.args.get(key).and_then(Value::as_bool);
    // A plugin's command wins over a built-in one of the same name.
    if plugins.contains_key(&cmd.name) {
        return Some(self::PackageCommand::Plugin(cmd.name.clone(), cmd.args.clone()));
    }
    let extend = flag("extend") == Some(true);
    let action = match cmd.name.as_str() {
        "move_to" => {
            let motion = match arg("to")? {
                "bol" | "hardbol" => Motion::LineStart,
                "eol" | "hardeol" => Motion::LineEnd,
                "bof" => Motion::DocStart,
                "eof" => Motion::DocEnd,
                _ => return None,
            };
            return Some(self::PackageCommand::Move(motion, extend));
        }
        "move" => {
            let forward = flag("forward") == Some(true);
            let motion = match (arg("by")?, forward) {
                ("characters", true) => Motion::Right,
                ("characters", false) => Motion::Left,
                ("words" | "word_ends" | "subwords" | "subword_ends", true) => Motion::WordRight,
                ("words" | "word_ends" | "subwords" | "subword_ends", false) => Motion::WordLeft,
                ("lines", true) => Motion::Down,
                ("lines", false) => Motion::Up,
                ("pages", true) => Motion::PageDown(0),
                ("pages", false) => Motion::PageUp(0),
                _ => return None,
            };
            return Some(self::PackageCommand::Move(motion, extend));
        }
        "left_delete" => return Some(self::PackageCommand::Delete(Motion::Left)),
        "right_delete" => return Some(self::PackageCommand::Delete(Motion::Right)),
        "delete_word" => {
            let forward = flag("forward") != Some(false);
            return Some(self::PackageCommand::Delete(if forward { Motion::WordRight } else { Motion::WordLeft }));
        }
        "show_panel" if arg("panel").is_some_and(|p| p.starts_with("output.")) => {
            return Some(self::PackageCommand::ShowPanel(arg("panel")?["output.".len()..].to_owned()));
        }
        "hide_panel" => return Some(self::PackageCommand::HidePanel),
        "set_layout" => return Some(self::PackageCommand::Layout(cmd.args.clone())),
        "edit_settings" if arg("base_file").is_some() => {
            let default = arg("default").unwrap_or_default().to_owned();
            return Some(self::PackageCommand::EditSettings(arg("base_file")?.to_owned(), default));
        }
        "open_file" => return Some(self::PackageCommand::OpenFile(arg("file")?.to_owned())),
        "open_url" => return Some(self::PackageCommand::OpenUrl(arg("url")?.to_owned())),
        "new_file" => New,
        "prompt_open" | "prompt_open_file" => Open,
        "prompt_open_folder" => OpenFolder,
        "save" => Save,
        "prompt_save_as" | "save_as" => SaveAs,
        "save_all" => SaveAll,
        "close" | "close_file" => CloseActive,
        "clone_file" => CloneView,
        "new_window" => NewWindow,
        "close_window" => CloseWindow,
        "exit" => Quit,
        "show_panel" if arg("panel") == Some("find_in_files") => FindInFiles,
        "show_panel" if arg("panel").is_some_and(|p| p.ends_with("find")) => Find,
        "find_next" => FindNext,
        "find_prev" => FindPrev,
        "find_all" => FindAll,
        "next_result" => NextResult,
        "prev_result" => PrevResult,
        "focus_group" => FocusGroup(cmd.args.get("group")?.as_u64()? as usize),
        "move_to_group" => MoveToGroup(cmd.args.get("group")?.as_u64()? as usize),
        "auto_complete" => AutoComplete,
        "commit_completion" => CommitCompletion,
        "hide_auto_complete" => HideCompletion,
        "toggle_record_macro" => RecordMacro,
        "run_macro" => PlaybackMacro,
        "save_macro" => SaveMacro,
        "find_under_expand" => Edit(C::AddNextOccurrence),
        "find_all_under" => Edit(C::SelectAllOccurrences),
        "show_overlay" => match (arg("overlay")?, arg("text").unwrap_or("")) {
            ("command_palette", _) => CommandPalette,
            ("goto", ":") => GotoLine,
            ("goto", "") => GotoAnything,
            // `@` and `#` search symbols and words, which this editor can't.
            _ => return None,
        },
        "toggle_side_bar" => ToggleSidebar,
        "next_view" | "next_view_in_stack" => NextTab,
        "prev_view" | "prev_view_in_stack" => PrevTab,
        "undo" | "soft_undo" => Standard(self::Standard::Undo),
        "redo" | "redo_or_repeat" | "soft_redo" => Standard(self::Standard::Redo),
        "cut" => Standard(self::Standard::Cut),
        "copy" => Standard(self::Standard::Copy),
        "paste" => Standard(self::Standard::Paste),
        "select_all" => Standard(self::Standard::SelectAll),
        "duplicate_line" => Edit(C::DuplicateLine),
        "delete_line" => Edit(C::DeleteLine),
        "insert_line_after" => Edit(C::InsertLineAfter),
        "insert_line_before" => Edit(C::InsertLineBefore),
        "delete_to_line_start" => return Some(self::PackageCommand::Delete(Motion::LineStart)),
        "delete_to_line_end" => return Some(self::PackageCommand::Delete(Motion::LineEnd)),
        // Sublime's Default package does these with macros, which this
        // editor has built in. Other macro files come from packages.
        "run_macro_file" => match arg("file")?.rsplit('/').next()? {
            "Delete Line.sublime-macro" => Edit(C::DeleteLine),
            "Add Line.sublime-macro" => Edit(C::InsertLineAfter),
            "Add Line Before.sublime-macro" => Edit(C::InsertLineBefore),
            "Delete to Hard BOL.sublime-macro" | "Delete to BOL.sublime-macro" => {
                return Some(self::PackageCommand::Delete(Motion::LineStart));
            }
            "Delete to Hard EOL.sublime-macro" | "Delete to EOL.sublime-macro" => {
                return Some(self::PackageCommand::Delete(Motion::LineEnd));
            }
            _ => {
                let file = arg("file")?;
                let path = file.trim_start_matches("res://").trim_start_matches("Packages/");
                return Some(self::PackageCommand::Macro(path.to_owned()));
            }
        },
        "indent" => Edit(C::Indent),
        "unindent" => Edit(C::Outdent),
        "toggle_comment" if flag("block") != Some(true) => Edit(C::ToggleComment),
        "expand_selection" if arg("to") == Some("line") => Edit(C::ExpandToLine),
        "split_selection_into_lines" => Edit(C::SplitIntoLines),
        "select_lines" => Edit(if flag("forward") == Some(false) { C::AddCaretAbove } else { C::AddCaretBelow }),
        "single_selection" => Edit(C::SingleSelection),
        "increase_font_size" => FontBigger,
        "decrease_font_size" => FontSmaller,
        "reset_font_size" => FontReset,
        "toggle_setting" => Toggle(match arg("setting")? {
            "word_wrap" => Flag::WordWrap,
            "line_numbers" => Flag::LineNumbers,
            "highlight_line" => Flag::HighlightLine,
            "draw_indent_guides" | "indent_guides" => Flag::IndentGuides,
            "translate_tabs_to_spaces" => Flag::TabsToSpaces,
            "auto_match_enabled" => Flag::AutoMatch,
            "caret_blink" => Flag::CaretBlink,
            "auto_complete" => Flag::AutoComplete,
            _ => return None,
        }),
        "edit_settings" => OpenSettings,
        // Package Control's commands.
        "install_package" => InstallFromIndex,
        "upgrade_all_packages" | "upgrade_package" => UpgradePackages,
        "satisfy_libraries" | "satisfy_dependencies" => InstallLibraries,
        "insert" => return arg("characters").map(|c| self::PackageCommand::Insert(c.to_owned())),
        "insert_snippet" => {
            if let Some(contents) = arg("contents") {
                return Some(self::PackageCommand::Snippet(contents.to_owned()));
            }
            // `"name": "Packages/Rust/fn.sublime-snippet"`
            let name = arg("name")?.strip_prefix("Packages/")?;
            let snippet = snippets.iter().find(|s| s.path == name)?;
            return Some(self::PackageCommand::Snippet(snippet.content.clone()));
        }
        _ => return None,
    };
    Some(Run(action))
}

/// Takes a press of exactly this key and these modifiers from the input.
fn consume_exact(i: &mut egui::InputState, shortcut: &KeyboardShortcut) -> bool {
    let found = i.events.iter().position(|e| {
        matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. }
            if *key == shortcut.logical_key && modifiers.matches_exact(shortcut.modifiers))
    });
    found.map(|at| i.events.remove(at)).is_some()
}

/// Key bindings can bind Tab and Enter to inserting a tab or newline; the editor's
/// own handling of those keys does that and more (snippets, indentation).
fn just_types_its_key(keys: &[KeyboardShortcut], cmd: &PackageCommand) -> bool {
    let PackageCommand::Insert(text) = cmd else {
        return false;
    };
    let plain = |key: Key| keys.len() == 1 && keys[0].logical_key == key && !keys[0].modifiers.command;
    (text == "\t" && plain(Key::Tab)) || (text == "\n" && plain(Key::Enter))
}

/// Package key bindings change the menus' keys: a key bound to something
/// else is taken off its menu item (so the macOS menu doesn't catch it
/// first), and an item whose command is bound shows that key instead.
fn rebind_menu_keys(items: &mut [Item<Action>], keys: &[(KeyboardShortcut, Option<Action>, bool)]) {
    for item in items {
        match item {
            Item::Command { action, shortcut, .. } => {
                if let Some(key) = *shortcut
                    && keys.iter().any(|(k, a, _)| *k == key && *a != Some(*action))
                {
                    *shortcut = None;
                }
                // Only bindings without conditions always do what the menu shows.
                // The item's own key is kept when it is one of them.
                let bound = |(k, a, always): &&(KeyboardShortcut, Option<Action>, bool)| {
                    *always && *a == Some(*action) && Some(*k) != *shortcut
                };
                let own_kept = shortcut.is_some_and(|key| keys.iter().any(|(k, a, _)| *k == key && *a == Some(*action)));
                if !own_kept && let Some((key, ..)) = keys.iter().rev().find(bound) {
                    *shortcut = Some(*key);
                }
            }
            Item::Submenu { items, .. } => rebind_menu_keys(items, keys),
            _ => {}
        }
    }
}

/// A package's name, with its version when it has one.
fn package_label(package: &Package) -> String {
    match &package.metadata.version {
        Some(version) => format!("{} {version}", package.name),
        None => package.name.clone(),
    }
}

/// Draws one folder level of the side bar; folders expand on click, and
/// `menu` gives each row its right-click menu.
fn folder_tree(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    dir: &Path,
    active: Option<&Path>,
    open: &mut Option<PathBuf>,
    menu: &mut dyn FnMut(&egui::Response, &Path, bool),
) {
    for (path, is_dir) in ws.children(dir) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if is_dir {
            let header = egui::CollapsingHeader::new(name)
                .id_salt(&path)
                .show(ui, |ui| folder_tree(ui, ws, &path, active, open, menu))
                .header_response;
            menu(&header, &path, true);
        } else {
            let row = ui.selectable_label(active == Some(path.as_path()), name);
            if row.clicked() {
                *open = Some(path.clone());
            }
            menu(&row, &path, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The whole app in a test window. Off the main thread there's no native
    /// menu bar, so the menus are drawn in the window where tests can click them.
    fn app() -> Harness<'static, EditorApp> {
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_eframe(|cc| EditorApp::new(cc, Vec::new()));
        h.run_steps(2);
        h
    }

    fn temp_folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("text_editor_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), "hello\n").unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn side_bar_shows_without_a_folder_and_toggles_from_the_view_menu() {
        let mut h = app();
        assert!(h.query_by_label("OPEN FILES").is_some());
        assert!(h.query_by_label("Open Folder…").is_some());

        h.get_by_label("View").click();
        h.run_steps(2);
        h.get_by_label_contains("Side Bar").click();
        h.run_steps(2);
        assert!(h.query_by_label("OPEN FILES").is_none());
        assert!(!h.state().settings.show_sidebar);

        h.get_by_label("View").click();
        h.run_steps(2);
        h.get_by_label_contains("Side Bar").click();
        h.run_steps(2);
        assert!(h.query_by_label("OPEN FILES").is_some());
    }

    /// An app that keeps its session in `store`, like the real one does in
    /// the settings folder.
    fn app_with_session(store: &Path) -> Harness<'static, EditorApp> {
        let store = store.to_path_buf();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_eframe(move |cc| EditorApp::with_session(cc, Vec::new(), Some(Store::at(store))));
        h.run_steps(2);
        h
    }

    fn type_text(h: &mut Harness<'_, EditorApp>, text: &str) {
        let ctx = h.ctx.clone();
        h.state().focus_editor(&ctx);
        h.run_steps(1);
        h.event(egui::Event::Text(text.into()));
        h.run_steps(2);
    }

    fn fresh_store(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("text_editor_store_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn unsaved_tabs_come_back_after_quitting() {
        let store = fresh_store("quit");
        let dir = temp_folder("hot_exit", &["notes.txt"]);
        let notes = dir.join("notes.txt");
        {
            let mut h = app_with_session(&store);
            type_text(&mut h, "draft");
            h.state_mut().open_path(&notes);
            type_text(&mut h, "edited ");
            let ctx = h.ctx.clone();
            // Quitting must not ask anything: there's no dialog to answer in a test.
            h.state_mut().apply(&ctx, Action::Quit);
            assert!(h.state().allow_close);
        }
        let h = app_with_session(&store);
        let app = h.state();
        assert_eq!(app.docs.len(), 2);
        assert_eq!(app.docs[0].buffer.text(), "draft");
        assert!(app.docs[0].path.is_none() && app.docs[0].dirty());
        assert_eq!(app.docs[1].buffer.text(), "edited hello\n");
        assert_eq!(app.docs[1].path.as_deref(), Some(notes.as_path()));
        assert!(app.docs[1].dirty(), "restored changes are still unsaved");
        assert_eq!(app.active, 1);
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "hello\n", "the file itself is untouched");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn unsaved_work_survives_a_force_quit() {
        let store = fresh_store("force_quit");
        {
            let mut h = app_with_session(&store);
            type_text(&mut h, "crash-safe");
            // A second passes without quitting; then the app is killed.
            h.state_mut().last_session_save = Instant::now() - SESSION_SAVE_EVERY;
            h.run_steps(1);
        }
        let h = app_with_session(&store);
        assert_eq!(h.state().docs[0].buffer.text(), "crash-safe");
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn saved_files_reopen_and_deleted_ones_are_skipped() {
        let store = fresh_store("reopen");
        let dir = temp_folder("reopen", &["keep.rs", "gone.rs"]);
        {
            let mut h = app_with_session(&store);
            h.state_mut().open_path(&dir.join("keep.rs"));
            h.state_mut().open_path(&dir.join("gone.rs"));
            let ctx = h.ctx.clone();
            h.state_mut().apply(&ctx, Action::Quit);
        }
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        let h = app_with_session(&store);
        let app = h.state();
        let paths: Vec<_> = app.docs.iter().filter_map(|d| d.path.clone()).collect();
        assert_eq!(paths, vec![dir.join("keep.rs")]);
        assert!(!app.docs[0].dirty());
        assert_eq!(app.docs[0].syntax(), "Rust");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn word_wrap_toggles_from_the_view_menu() {
        let mut h = app();
        assert!(!h.state().settings.word_wrap);
        h.get_by_label("View").click();
        h.run_steps(2);
        h.get_by_label("Word Wrap").click();
        h.run_steps(2);
        assert!(h.state().settings.word_wrap);
    }

    #[test]
    fn typing_works_right_after_launch() {
        let mut h = app();
        h.event(egui::Event::Text("hi".into()));
        h.run_steps(2);
        assert_eq!(h.state().docs[0].buffer.text(), "hi");
    }

    #[test]
    fn folder_files_appear_in_the_side_bar_and_open_on_click() {
        let dir = temp_folder("sidebar", &["alpha.txt", "beta.rs"]);
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_folder(&ctx, dir.clone());
        h.run_steps(2);
        h.get_by_label("beta.rs").click();
        h.run_steps(2);
        let app = h.state();
        assert_eq!(app.docs[app.active].path.as_deref(), Some(dir.join("beta.rs").as_path()));
        assert_eq!(app.docs[app.active].syntax(), "Rust");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_recent_reopens_a_closed_file() {
        let dir = temp_folder("recent", &["gamma.md"]);
        let file = dir.join("gamma.md");
        let mut h = app();
        h.state_mut().open_path(&file);
        let ctx = h.ctx.clone();
        h.state_mut().close(&ctx, 0);
        h.run_steps(2);
        assert!(h.state().docs.iter().all(|d| d.path.is_none()));

        h.get_by_label("File").click();
        h.run_steps(2);
        h.get_by_label("Open Recent").hover();
        h.run_steps(2);
        h.get_by_label_contains("gamma.md —").click();
        h.run_steps(2);
        let app = h.state();
        assert_eq!(app.docs[app.active].path.as_deref(), Some(file.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn packages_add_snippets_key_bindings_commands_and_color_schemes() {
        use crate::packages::tests::temp_root;
        let root = temp_root("extras");
        let dirs = Dirs::at(&root);
        let tools = dirs.packages.join("Tools");
        std::fs::create_dir_all(&tools).unwrap();
        let write = |name: &str, text: &str| std::fs::write(tools.join(name), text).unwrap();
        write("Sea.sublime-color-scheme", r##"{ "globals": { "background": "#204060", "foreground": "white" } }"##);
        write(
            "fn.sublime-snippet",
            "<snippet><content><![CDATA[fn ${1:name}() {\n\t$0\n}]]></content>\
             <tabTrigger>fn</tabTrigger><description>Function</description></snippet>",
        );
        write(
            "Default.sublime-keymap",
            r#"[
                { "keys": ["f6"], "command": "toggle_setting", "args": { "setting": "word_wrap" } },
                { "keys": ["primary+k", "primary+b"], "command": "toggle_side_bar" },
                { "keys": ["primary+d"], "command": "duplicate_line" },
                { "keys": ["f7"], "command": "insert", "args": { "characters": "!" },
                  "context": [{ "key": "selection_empty" }] },
                { "keys": ["f8"], "command": "some_python_plugin" },
                { "keys": ["tab"], "command": "insert", "args": { "characters": "\t" } },
                { "keys": ["`"], "command": "insert", "args": { "characters": "tick" } },
            ]"#,
        );
        write(
            "Tools.sublime-commands",
            r#"[{ "caption": "Tools: Say Hi", "command": "insert_snippet", "args": { "contents": "hi $1!" } }]"#,
        );

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        let app = h.state();
        assert!(!app.message.as_deref().unwrap_or("").starts_with("Could not"), "{:?}", app.message);
        assert!(app.theme_names.iter().any(|n| n == "Sea"));
        assert_eq!(app.key_bindings.len(), 5, "the plugin command and plain Tab are left out");

        // The package's ⌘D moves from Add Next Occurrence to Duplicate Line.
        let commands = app.commands();
        let shortcut = |name: &str| commands.iter().find(|c| c.0 == name).and_then(|c| c.2);
        assert_eq!(shortcut("Edit: Duplicate Line"), Some(KeyboardShortcut::new(CMD, Key::D)));
        assert_eq!(shortcut("Selection: Add Next Occurrence"), None);

        // A snippet: type its trigger, press Tab, fill in the fields.
        type_text(&mut h, "fn");
        h.key_press(Key::Tab);
        h.run_steps(2);
        type_text(&mut h, "main");
        h.key_press(Key::Tab);
        h.run_steps(2);
        type_text(&mut h, "body");
        let text = |h: &Harness<'_, EditorApp>| h.state().docs[h.state().active].buffer.text();
        assert_eq!(text(&h), "fn main() {\n    body\n}");

        // Key bindings: one key, one limited by a condition, and a two-key sequence.
        h.key_press(Key::F6);
        h.run_steps(2);
        assert!(h.state().settings.word_wrap);
        h.key_press(Key::F7);
        h.run_steps(2);
        assert_eq!(text(&h), "fn main() {\n    body!\n}");
        let sidebar = h.state().settings.show_sidebar;
        h.key_press_modifiers(CMD, Key::K);
        h.run_steps(2);
        h.key_press_modifiers(CMD, Key::B);
        h.run_steps(2);
        assert_ne!(h.state().settings.show_sidebar, sidebar);
        h.key_press_modifiers(CMD, Key::D);
        h.run_steps(2);
        assert_eq!(text(&h), "fn main() {\n    body!\n    body!\n}");

        // A bound key that types a character doesn't also type it. A real key
        // press brings its Key and Text events in the same frame.
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.input_mut().events.extend([key(Key::Backtick, Modifiers::NONE), egui::Event::Text("`".into())]);
        h.run_steps(2);
        assert_eq!(text(&h), "fn main() {\n    body!\n    body!tick\n}");
        // With Shift it's a different key (`~`), which the binding leaves alone.
        h.input_mut().events.extend([key(Key::Backtick, Modifiers::SHIFT), egui::Event::Text("~".into())]);
        h.run_steps(2);
        assert_eq!(text(&h), "fn main() {\n    body!\n    body!tick~\n}");

        // The package's palette entry, and the snippet itself.
        let rows: Vec<String> = h.state().palette_rows(&ctx, Mode::Commands).into_iter().map(|(c, _)| c.text).collect();
        assert!(rows.iter().any(|r| r == "Tools: Say Hi"), "{rows:?}");
        assert!(rows.iter().any(|r| r == "Snippet: Function"), "{rows:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn key_binding_conditions_look_at_the_text_and_scope() {
        let dir = temp_folder("conditions", &[]);
        let file = dir.join("main.rs");
        std::fs::write(&file, "let s = \"hi\";\n").unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_path(&file);
        // The caret between `h` and `i`, inside the string.
        h.state_mut().doc().buffer.set_single(Selection::caret(10));
        let app = h.state_mut();
        let mut holds = |key: &str, op: &str, operand: Value| app.condition_holds(&ctx, &Condition::new(key, op, operand, false));
        assert!(holds("preceding_text", "regex_contains", "\"h$".into()));
        assert!(holds("following_text", "regex_match", "i\";".into()));
        assert!(!holds("following_text", "regex_match", "i".into()));
        assert!(holds("selector", "equal", "source.rust string".into()));
        assert!(!holds("selector", "equal", "comment".into()));
        assert!(holds("selection_empty", "equal", true.into()));
        assert!(holds("panel_has_focus", "equal", false.into()));
        assert!(!holds("no_such_key", "equal", true.into()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Runs frames until `done` holds, for work that finishes in the background.
    fn wait_until(h: &mut Harness<'_, EditorApp>, what: &str, done: impl Fn(&EditorApp) -> bool) {
        for _ in 0..1000 {
            h.run_steps(1);
            if done(h.state()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}; plugin console:\n{}", h.state().plugins.log.join("\n"));
    }

    #[test]
    fn snippet_transforms_follow_typing_in_the_editor() {
        let mut h = app();
        let settings = h.state().settings.clone();
        editor_view::insert_snippet(h.state_mut().doc(), "struct ${1:Name}; // ${1/(.*)/\\L$1/}", &settings, 0);
        h.run_steps(1);
        type_text(&mut h, "MyType");
        assert_eq!(h.state().docs[h.state().active].buffer.text(), "struct MyType; // mytype");
    }

    /// Written for Sublime Text: its plugins run as they are.
    #[test]
    fn python_plugins_run_commands_answer_conditions_and_listen() {
        use crate::packages::tests::{temp_root, write_zip};
        if crate::plugins::find_python("").is_none() {
            eprintln!("No Python 3 here; skipping the plugin test");
            return;
        }
        let root = temp_root("plugins");
        let dirs = Dirs::at(&root);
        let hello = dirs.packages.join("Hello");
        std::fs::create_dir_all(&hello).unwrap();
        let write = |name: &str, text: &str| std::fs::write(hello.join(name), text).unwrap();
        write("helpers.py", "def shout(text):\n    return text.upper()\n");
        write(
            "hello.py",
            r#"import sublime
import sublime_plugin
from .helpers import shout


class HelloCommand(sublime_plugin.TextCommand):
    def run(self, edit, name="world"):
        self.view.insert(edit, 0, shout("hello ") )
        self.view.insert(edit, self.view.size(), name)
        print("said hello to", name)


class TidyCommand(sublime_plugin.TextCommand):
    def run(self, edit):
        whole = sublime.Region(0, self.view.size())
        lines = [line.rstrip() for line in self.view.substr(whole).split("\n")]
        self.view.replace(edit, whole, "\n".join(lines))


class Listener(sublime_plugin.EventListener):
    def on_pre_save(self, view):
        view.run_command("tidy")

    def on_query_context(self, view, key, operator, operand, match_all):
        if key == "hello_ready":
            return True
        return None


class PickCommand(sublime_plugin.WindowCommand):
    def run(self):
        self.window.show_quick_panel(["one", ["two", "the second"]], self.picked)

    def picked(self, index):
        self.window.active_view().run_command("insert", {"characters": "picked %d" % index})
"#,
        );
        write(
            "Default.sublime-keymap",
            r#"[{ "keys": ["f5"], "command": "hello", "args": { "name": "keys" }, "context": [{ "key": "hello_ready" }] }]"#,
        );
        write("Hello.sublime-commands", r#"[{ "caption": "Hello: Pick One", "command": "pick" }]"#);
        // A zipped package, with GitHub's top-level folder.
        write_zip(
            &dirs.installed.join("Zipped.sublime-package"),
            &[(
                "Zipped-main/zed.py",
                "import sublime_plugin\n\nclass ZedCommand(sublime_plugin.TextCommand):\n    def run(self, edit):\n        self.view.insert(edit, self.view.size(), '!')\n",
            )],
        );
        let file = root.join("notes.txt");
        std::fs::write(&file, "a  \nb\t\n").unwrap();

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_path(&file);
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        wait_until(&mut h, "plugins to load", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 4));
        let text = |h: &Harness<'_, EditorApp>| h.state().docs[h.state().active].buffer.text();

        // A key binding whose condition only the plugin knows, running its command.
        h.state().focus_editor(&ctx);
        h.key_press(Key::F5);
        h.run_steps(2);
        assert_eq!(text(&h), "HELLO a  \nb\t\nkeys");
        // The whole command is one undo step.
        h.state_mut().doc().buffer.undo();
        assert_eq!(text(&h), "a  \nb\t\n");

        // A plugin from a zip.
        h.state_mut().run_plugin_command(&ctx, "zed", &serde_json::json!({}));
        assert_eq!(text(&h), "a  \nb\t\n!");

        // on_pre_save tidies the text before it's written.
        let ctx2 = ctx.clone();
        let index = h.state().active;
        assert!(h.state_mut().save(&ctx2, index, false));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nb\n!");

        // A palette entry opens the plugin's quick panel; picking calls it back.
        let rows: Vec<String> = h.state().palette_rows(&ctx, Mode::Commands).into_iter().map(|(c, _)| c.text).collect();
        assert!(rows.iter().any(|r| r == "Hello: Pick One"), "{rows:?}");
        h.state_mut().run_plugin_command(&ctx, "pick", &serde_json::json!({}));
        h.run_steps(1);
        assert!(h.state().palette.open && h.state().palette.mode == Mode::Quick);
        assert_eq!(h.state().palette_candidates.len(), 2);
        h.key_press(Key::ArrowDown);
        h.key_press(Key::Enter);
        // `insert` types at the caret, which is at the start.
        wait_until(&mut h, "the quick panel callback", |app| app.docs[app.active].buffer.text().starts_with("picked 1"));

        // print() goes to the plugin console.
        assert!(h.state().plugins.log.iter().any(|l| l == "said hello to keys"), "{:?}", h.state().plugins.log);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Package languages are built on a background thread; wait for them.
    fn wait_for_languages(h: &mut Harness<'_, EditorApp>) {
        for _ in 0..500 {
            h.run_steps(1);
            if h.state().syntax_job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("languages never finished loading");
    }

    #[test]
    fn packages_add_themes_and_languages_and_can_be_switched_off() {
        use crate::packages::tests::{SYNTAX, THEME, temp_root, write_zip};
        let root = temp_root("app");
        let dirs = Dirs::at(&root);
        let download = root.join("Night-main.zip");
        write_zip(&download, &[("Night-main/Night.tmTheme", THEME), ("Night-main/Fancy.sublime-syntax", SYNTAX)]);
        let file = root.join("demo.fancy");
        std::fs::write(&file, "fancy\n").unwrap();

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_path(&file);
        h.state_mut().packages = Some(dirs.clone());
        assert_eq!(h.state().docs[h.state().active].syntax(), "Plain Text");

        // Installing colors the open file (once its language is ready) and adds the theme.
        dirs.install(&download).unwrap();
        h.state_mut().reload_packages(&ctx);
        wait_for_languages(&mut h);
        let app = h.state();
        assert!(app.theme_names.iter().any(|n| n == "Night"));
        assert_eq!(app.docs[app.active].syntax(), "Fancy");
        let commands: Vec<String> = app.commands().into_iter().map(|c| c.0).collect();
        let prefix = if cfg!(target_os = "macos") { "App" } else { "File" };
        assert!(commands.contains(&format!("{prefix}: Packages: Disable Package: Night")), "{commands:?}");
        let night = app.theme_names.iter().position(|n| n == "Night").unwrap();
        h.state_mut().apply(&ctx, Action::SetTheme(night));
        assert_eq!(h.state().highlighter.theme_name(), "Night");

        // Switch it off from the menu.
        let menu = if cfg!(target_os = "macos") { "TextEditor" } else { "File" };
        h.get_by_label(menu).click();
        h.run_steps(2);
        h.get_by_label("Packages").hover();
        h.run_steps(2);
        h.get_by_label("Disable Package").hover();
        h.run_steps(2);
        h.get_by_label("Night").click();
        h.run_steps(2);
        let app = h.state();
        assert_eq!(app.settings.ignored_packages, ["Night"]);
        assert!(!app.theme_names.iter().any(|n| n == "Night"));
        assert_eq!(app.highlighter.theme_name(), crate::highlight::DEFAULT_THEME);
        assert_eq!(app.docs[app.active].syntax(), "Plain Text");

        // The theme choice is kept while its package is off, so it comes back.
        assert_eq!(app.settings.theme, "Night");

        // And back on: this time the language comes from the cache, at once.
        h.state_mut().apply(&ctx, Action::EnablePackage(0));
        let app = h.state();
        assert!(app.syntax_job.is_none());
        assert!(app.settings.ignored_packages.is_empty());
        assert_eq!(app.docs[app.active].syntax(), "Fancy");
        assert_eq!(app.highlighter.theme_name(), "Night");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn text(h: &Harness<'_, EditorApp>) -> String {
        h.state().docs[h.state().active].buffer.text()
    }

    /// The id of the tab with this file open.
    fn tab(app: &EditorApp, path: &Path) -> u64 {
        app.docs.iter().find(|d| d.path.as_deref() == Some(path)).unwrap().id
    }

    #[test]
    fn split_views_keep_their_own_tabs_and_come_back_after_quitting() {
        let store = fresh_store("split");
        let dir = temp_folder("split", &["left.rs", "right.rs"]);
        let (left, right) = (dir.join("left.rs"), dir.join("right.rs"));
        {
            let mut h = app_with_session(&store);
            let ctx = h.ctx.clone();
            h.state_mut().open_path(&left);
            h.state_mut().apply(&ctx, Action::SetLayout(Preset::Columns2));
            // The new group is empty; focusing it sends the next file there.
            h.state_mut().apply(&ctx, Action::FocusGroup(1));
            h.state_mut().open_path(&right);
            h.run_steps(2);
            let app = h.state();
            assert_eq!(app.groups.len(), 2);
            assert_eq!(app.groups[0].views, [tab(app, &left)]);
            assert_eq!(app.groups[1].views, [tab(app, &right)]);
            assert_eq!(app.docs[app.active].path.as_deref(), Some(right.as_path()));

            // Moving the active tab over, and back.
            h.state_mut().apply(&ctx, Action::MoveToGroup(0));
            h.run_steps(1);
            assert_eq!(h.state().groups[0].views.len(), 2);
            assert!(h.state().groups[1].views.is_empty(), "the group stays, empty");
            h.state_mut().apply(&ctx, Action::MoveToGroup(1));
            h.run_steps(1);
            // Next Tab stays within the group.
            h.state_mut().apply(&ctx, Action::NextTab);
            assert_eq!(h.state().docs[h.state().active].path.as_deref(), Some(right.as_path()));
            h.state_mut().apply(&ctx, Action::FocusGroup(0));
            assert_eq!(h.state().docs[h.state().active].path.as_deref(), Some(left.as_path()));
            h.state_mut().apply(&ctx, Action::Quit);
        }
        let mut h = app_with_session(&store);
        h.run_steps(1);
        let app = h.state();
        assert_eq!(app.layout.preset(), Some(Preset::Columns2));
        assert_eq!(app.groups[0].views, [tab(app, &left)]);
        assert_eq!(app.groups[1].views, [tab(app, &right)]);
        assert_eq!(app.docs[app.active].path.as_deref(), Some(left.as_path()));

        // Back to one group: every tab joins it.
        let ctx = h.ctx.clone();
        h.state_mut().apply(&ctx, Action::SetLayout(Preset::Single));
        assert_eq!(h.state().groups.len(), 1);
        assert_eq!(h.state().groups[0].views.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn closing_a_groups_last_tab_leaves_it_empty_and_focused() {
        let dir = temp_folder("close_split", &["a.txt", "b.txt"]);
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_path(&dir.join("a.txt"));
        h.state_mut().apply(&ctx, Action::SetLayout(Preset::Columns2));
        h.state_mut().apply(&ctx, Action::FocusGroup(1));
        h.state_mut().open_path(&dir.join("b.txt"));
        h.run_steps(1);
        h.state_mut().apply(&ctx, Action::CloseActive);
        h.run_steps(1);
        let app = h.state();
        assert!(app.groups[1].views.is_empty());
        assert_eq!(app.docs[app.active].path.as_deref(), Some(dir.join("a.txt").as_path()));
        assert_eq!(app.focus_group, 1, "a new file goes where the closed one was");
        h.state_mut().apply(&ctx, Action::New);
        h.run_steps(1);
        assert_eq!(h.state().groups[1].views.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn typing_a_word_offers_completions_from_the_text() {
        let mut h = app();
        type_text(&mut h, "hello help\nh");
        assert!(h.state().completion.is_none(), "one letter isn't enough");
        type_text(&mut h, "e");
        let shown = |h: &Harness<'_, EditorApp>| -> Vec<String> {
            let popup = h.state().completion.as_ref().expect("the list is open");
            popup.shown.iter().map(|(i, _)| popup.items[*i].trigger.clone()).collect()
        };
        assert_eq!(shown(&h), ["help", "hello"], "the nearest word first");
        h.key_press(Key::ArrowDown);
        h.run_steps(1);
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(text(&h), "hello help\nhello");
        assert!(h.state().completion.is_none());

        // Esc closes it, and moving the caret does too.
        type_text(&mut h, " he");
        assert!(h.state().completion.is_some());
        h.key_press(Key::Escape);
        h.run_steps(1);
        assert!(h.state().completion.is_none());
        assert_eq!(text(&h), "hello help\nhello he", "Esc only closed the list");
        type_text(&mut h, "l");
        assert!(h.state().completion.is_some());
        h.key_press(Key::ArrowLeft);
        h.run_steps(1);
        assert!(h.state().completion.is_none());

        // With the setting off it only opens when asked.
        h.state_mut().settings.auto_complete = false;
        type_text(&mut h, "x\nhe");
        assert!(h.state().completion.is_none());
        let ctx = h.ctx.clone();
        h.state_mut().apply(&ctx, Action::AutoComplete);
        assert!(h.state().completion.is_some());
    }

    #[test]
    fn macros_record_typing_and_keys_and_play_them_back() {
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().apply(&ctx, Action::RecordMacro);
        type_text(&mut h, "(");
        type_text(&mut h, "x");
        h.key_press(Key::ArrowRight);
        h.run_steps(1);
        h.key_press(Key::Enter);
        h.run_steps(1);
        h.state_mut().apply(&ctx, Action::Edit(Command::DuplicateLine));
        h.state_mut().apply(&ctx, Action::RecordMacro);
        assert_eq!(text(&h), "(x)\n\n");
        assert!(h.state().recording.is_none());
        assert_eq!(h.state().last_macro.len(), 4, "{:?}", h.state().last_macro);

        h.state_mut().apply(&ctx, Action::PlaybackMacro);
        assert_eq!(text(&h), "(x)\n\n(x)\n\n", "typed as before, brackets paired");
        h.state_mut().doc().buffer.undo();
        assert_eq!(text(&h), "(x)\n\n", "the whole playback is one undo step");

        // Saved, it reads as commands.
        let saved: Value = serde_json::from_str(&crate::macros::to_json(&h.state().last_macro)).unwrap();
        let names: Vec<&str> = saved.as_array().unwrap().iter().map(|c| c["command"].as_str().unwrap()).collect();
        assert_eq!(names, ["insert", "move", "insert", "duplicate_line"]);
    }

    #[test]
    fn packages_add_macros_and_menus() {
        use crate::packages::tests::temp_root;
        let root = temp_root("menus");
        let dirs = Dirs::at(&root);
        let tools = dirs.packages.join("Tools");
        let default = dirs.packages.join("Default");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&default).unwrap();
        let write = |dir: &Path, name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(&tools, "Semicolon.sublime-macro", r#"[{ "command": "move_to", "args": { "to": "eol" } }, { "command": "insert", "args": { "characters": ";" } }]"#);
        write(&tools, "Tools.sublime-settings", "{ \"x\": 1 }\n");
        write(
            &tools,
            "Main.sublime-menu",
            r#"[
                { "id": "tools", "children": [
                    { "caption": "End With Semicolon", "command": "run_macro_file", "args": { "file": "res://Packages/Tools/Semicolon.sublime-macro" } },
                ] },
                { "id": "preferences", "children": [{ "id": "package-settings", "caption": "Package Settings", "children": [
                    { "caption": "Tools", "children": [{ "caption": "Settings", "command": "edit_settings",
                        "args": { "base_file": "${packages}/Tools/Tools.sublime-settings", "default": "{\n\t$0\n}\n" } }] },
                ] }] },
                { "caption": "Extras", "children": [
                    { "caption": "Insert Star", "command": "insert", "args": { "characters": "*" } },
                    { "caption": "Wrap Lines", "command": "toggle_setting", "args": { "setting": "word_wrap" } },
                    { "caption": "Save Again", "command": "save" },
                    { "caption": "Not Here", "command": "no_such_command" },
                ] },
            ]"#,
        );
        write(&tools, "Context.sublime-menu", r#"[{ "caption": "Shout", "command": "insert", "args": { "characters": "!" } }]"#);
        write(&default, "Main.sublime-menu", r#"[{ "caption": "Sublime's Own", "children": [{ "caption": "Quit", "command": "exit" }] }]"#);

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs.clone());
        h.state_mut().reload_packages(&ctx);
        let app = h.state();
        assert!(!app.message.as_deref().unwrap_or("").starts_with("Could not"), "{:?}", app.message);
        let commands: Vec<String> = app.commands().into_iter().map(|c| c.0).collect();
        let prefs = if cfg!(target_os = "macos") { "App" } else { "File" };
        for wanted in [
            "Tools: End With Semicolon".to_owned(),
            "Tools: Macros: Semicolon".to_owned(),
            "Extras: Insert Star".to_owned(),
            format!("{prefs}: Preferences: Package Settings: Tools: Settings"),
        ] {
            assert!(commands.contains(&wanted), "{wanted} missing from {commands:?}");
        }
        assert!(!commands.iter().any(|c| c == "Extras: Save Again" || c == "Extras: Wrap Lines"), "the menus have these already");
        assert!(!commands.iter().any(|c| c.contains("Not Here") || c.contains("Sublime's Own")));
        let context: Vec<String> = app
            .context_menu_items()
            .into_iter()
            .filter_map(|i| if let Item::Command { label, .. } = i { Some(label) } else { None })
            .collect();
        assert!(context.contains(&"Shout".to_owned()), "{context:?}");

        // The menu item plays the package's macro.
        type_text(&mut h, "let x = 1");
        h.state_mut().doc().buffer.set_single(Selection::caret(0));
        let run = h.state().commands().into_iter().find(|c| c.0 == "Tools: End With Semicolon").unwrap().1;
        h.state_mut().apply(&ctx, run);
        assert_eq!(text(&h), "let x = 1;");

        // Package settings open beside the user's own copy.
        let settings = h.state().commands().into_iter().find(|c| c.0.ends_with("Tools: Settings")).unwrap().1;
        h.state_mut().apply(&ctx, settings);
        h.run_steps(1);
        let app = h.state();
        let user = dirs.packages.join("User").join("Tools.sublime-settings");
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "{\n\t\n}\n");
        assert_eq!(app.groups.len(), 2);
        assert!(app.groups[0].views.contains(&tab(app, &tools.join("Tools.sublime-settings"))));
        assert_eq!(app.groups[1].views, [tab(app, &user)]);
        // Closing both puts the layout back as it was.
        let (base, user) = (tab(app, &tools.join("Tools.sublime-settings")), tab(app, &user));
        for id in [base, user] {
            let i = h.state().docs.iter().position(|d| d.id == id).unwrap();
            h.state_mut().close(&ctx, i);
        }
        assert_eq!(h.state().layout.preset(), Some(Preset::Single));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn find_in_files_lists_results_opens_them_and_replaces() {
        let dir = temp_folder("find_in_files", &[]);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.rs"), "let needle = 1;\nneedle();\n").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing\n").unwrap();
        std::fs::write(dir.join("sub/c.md"), "# Needle\n").unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().open_folder(&ctx, dir.clone());
        h.state_mut().apply(&ctx, Action::FindInFiles);
        h.state_mut().find_files.query = "needle".into();
        h.state_mut().apply(&ctx, Action::SearchFiles);
        wait_until(&mut h, "the search", |app| app.find_files.has_results());
        let app = h.state();
        let results = &app.docs[app.active];
        assert_eq!(results.title(), "Find Results");
        assert!(!results.dirty(), "never asks to be saved");
        let report = results.buffer.text();
        assert!(report.contains(&format!("{}:\n   1: let needle = 1;\n   2: needle();\n", dir.join("a.rs").display())), "{report}");
        assert!(report.ends_with("3 matches across 2 files\n"), "{report}");
        assert_eq!(results.buffer.marks(find_in_files::RESULTS_MARKS).len(), 3);

        // F4 goes from one result to the next.
        h.state_mut().apply(&ctx, Action::NextResult);
        let at = |h: &Harness<'_, EditorApp>| {
            let doc = &h.state().docs[h.state().active];
            (doc.path.clone().unwrap(), doc.buffer.primary().head)
        };
        assert_eq!(at(&h), (dir.join("a.rs"), 4));
        h.state_mut().apply(&ctx, Action::NextResult);
        assert_eq!(at(&h), (dir.join("a.rs"), 16));

        // A double click on a result line opens it too.
        let results_id = h.state().find_files.results.as_ref().unwrap().doc;
        let index = h.state().docs.iter().position(|d| d.id == results_id).unwrap();
        let line = report.lines().position(|l| l.ends_with("# Needle")).unwrap();
        let pos = h.state().docs[index].buffer.line_start(line);
        h.state_mut().docs[index].buffer.set_single(Selection::caret(pos));
        h.state_mut().open_result_at_caret(&ctx, results_id);
        assert_eq!(at(&h), (dir.join("sub/c.md"), 2));

        // Replacing changes the tabs, not yet the files.
        h.state_mut().find_files.replace = "pin".into();
        h.state_mut().replace_in_files_now(&ctx);
        let app = h.state();
        let a = app.docs.iter().find(|d| d.path.as_deref() == Some(dir.join("a.rs").as_path())).unwrap();
        assert_eq!(a.buffer.text(), "let pin = 1;\npin();\n");
        assert!(a.dirty());
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "let needle = 1;\nneedle();\n");
        h.state_mut().apply(&ctx, Action::SaveAll);
        assert_eq!(std::fs::read_to_string(dir.join("a.rs")).unwrap(), "let pin = 1;\npin();\n");
        assert_eq!(std::fs::read_to_string(dir.join("sub/c.md")).unwrap(), "# pin\n");

        // Regex search with groups, in the open files only.
        let f = &mut h.state_mut().find_files;
        (f.query, f.regex, f.case_sensitive, f.scope, f.replace) = (r"let (\w+)".into(), true, true, "<open files>".into(), "const $1".into());
        h.state_mut().apply(&ctx, Action::SearchFiles);
        wait_until(&mut h, "the regex search", |app| app.find_files.results.as_ref().is_some_and(|r| r.query.regex));
        h.state_mut().replace_in_files_now(&ctx);
        let a = h.state().docs.iter().find(|d| d.path.as_deref() == Some(dir.join("a.rs").as_path())).unwrap();
        assert_eq!(a.buffer.text(), "const pin = 1;\npin();\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plugins_use_regions_settings_phantoms_popups_completions_and_layouts() {
        if crate::plugins::find_python("").is_none() {
            eprintln!("No Python 3 here; skipping the plugin test");
            return;
        }
        use crate::packages::tests::temp_root;
        let root = temp_root("plugin_ui");
        let dirs = Dirs::at(&root);
        let package = dirs.packages.join("Extras");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("extras.py"),
            r#"import texteditor


class MarkCommand(texteditor.TextCommand):
    def run(self, edit):
        self.view.add_regions("word", [texteditor.Region(4, 7)], "invalid", "", texteditor.DRAW_NO_FILL)
        self.view.settings().set("tab_size", 2)
        self.view.settings().set("translate_tabs_to_spaces", False)
        self.view.add_phantom("notes", texteditor.Region(0), "<b>Note:</b> <a href='more'>more</a>", texteditor.LAYOUT_INLINE, self.clicked)
        self.view.show_popup("<i>hello</i>", location=0)
        print("popup", self.view.is_popup_visible(), self.view.line_height() > 0)

    def clicked(self, href):
        self.view.run_command("insert", {"characters": href})


class SplitCommand(texteditor.WindowCommand):
    def run(self):
        self.window.set_layout({"cols": [0.0, 0.3, 1.0], "rows": [0.0, 1.0], "cells": [[0, 0, 1, 1], [1, 0, 2, 1]]})
        view = self.window.active_view()
        self.window.set_view_index(view, 1, 0)
        print("groups", self.window.num_groups(), self.window.get_view_index(view), self.window.get_layout()["cols"])


class Completions(texteditor.EventListener):
    def on_query_completions(self, view, prefix, locations):
        return [["zebra\tanimal", "zebra(${1:x})"], texteditor.CompletionItem("zeta", "letter")]
"#,
        )
        .unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        type_text(&mut h, "the cat");
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        wait_until(&mut h, "plugins to load", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 2));

        h.state_mut().run_plugin_command(&ctx, "mark", &serde_json::json!({}));
        let index = h.state().active;
        let id = h.state().docs[index].id;
        let highlights = h.state().highlights_for(index);
        assert_eq!((highlights[0].start, highlights[0].end, highlights[0].flags), (4, 7, editor_view::draw::NO_FILL));
        // Regions follow edits.
        h.state_mut().doc().buffer.set_single(Selection::caret(0));
        type_text(&mut h, ">>");
        let highlights = h.state().highlights_for(index);
        assert_eq!((highlights[0].start, highlights[0].end), (6, 9));
        // The view's own settings change how it's edited.
        let settings = h.state().settings_for(id);
        assert_eq!((settings.tab_size, settings.translate_tabs_to_spaces), (2, false));
        assert_eq!(h.state().settings.tab_size, 4, "the editor's settings stay");
        // The phantom sits where it was put; its link calls the plugin back.
        let phantoms = h.state().phantoms_for(index);
        assert_eq!(phantoms.len(), 1);
        assert_eq!(crate::minihtml::plain_text(&phantoms[0].content), "Note: more");
        // The popup is up until the text changes.
        wait_until(&mut h, "the plugin's print", |app| app.plugins.log.iter().any(|l| l == "popup True True"));
        h.state_mut().phantom_navigate(phantoms[0].id, "more".into());
        wait_until(&mut h, "the phantom's link", |app| app.docs[app.active].buffer.text().contains("more"));
        h.run_steps(1);
        assert!(h.state().plugins.popup.is_none(), "editing closed the popup");

        // A layout from a plugin, with the view moved into the new group.
        h.state_mut().run_plugin_command(&ctx, "split", &serde_json::json!({}));
        wait_until(&mut h, "the layout", |app| app.plugins.log.iter().any(|l| l.starts_with("groups")));
        assert!(h.state().plugins.log.iter().any(|l| l == "groups 2 (1, 0) [0.0, 0.3, 1.0]"), "{:?}", h.state().plugins.log);
        assert_eq!(h.state().groups[1].views, [id]);

        // Plugins' completions come first.
        let end = h.state().docs[index].buffer.len();
        h.state_mut().doc().buffer.set_single(Selection::caret(end));
        type_text(&mut h, " ze");
        let popup = h.state().completion.as_ref().expect("the list is open");
        let triggers: Vec<&str> = popup.shown.iter().map(|(i, _)| popup.items[*i].trigger.as_str()).collect();
        assert_eq!(triggers, ["zebra", "zeta"]);
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert!(text(&h).ends_with(" zebra(x)"), "{}", text(&h));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn views_into_one_file_share_its_text_and_undo() {
        let store = fresh_store("clones");
        let dir = temp_folder("clones", &["shared.txt"]);
        let file = dir.join("shared.txt");
        {
            let mut h = app_with_session(&store);
            let ctx = h.ctx.clone();
            h.state_mut().open_path(&file);
            h.state_mut().apply(&ctx, Action::SetLayout(Preset::Columns2));
            h.state_mut().apply(&ctx, Action::CloneView);
            h.state_mut().apply(&ctx, Action::MoveToGroup(1));
            h.run_steps(1);
            let (first, second) = (h.state().groups[0].views[0], h.state().groups[1].views[0]);
            let text_of = |h: &Harness<'_, EditorApp>, id: u64| h.state().docs.iter().find(|d| d.id == id).unwrap().buffer.text();

            // Typed in the second view, it shows in the first.
            h.state_mut().doc().buffer.set_single(Selection::caret(0));
            type_text(&mut h, "one ");
            h.run_steps(1);
            assert_eq!(text_of(&h, second), "one hello\n");
            assert_eq!(text_of(&h, first), "one hello\n");
            // Typed in the first, undone there: both follow.
            h.state_mut().apply(&ctx, Action::FocusGroup(0));
            h.state_mut().doc().buffer.set_single(Selection::caret(0));
            type_text(&mut h, ">");
            assert_eq!(text_of(&h, second), ">one hello\n");
            h.key_press_modifiers(CMD, Key::Z);
            h.run_steps(2);
            assert_eq!(text_of(&h, first), "one hello\n");
            assert_eq!(text_of(&h, second), "one hello\n");
            // One save for both, and both are clean after it.
            assert!(h.state().docs.iter().all(|d| d.dirty()));
            h.state_mut().apply(&ctx, Action::SaveAll);
            h.run_steps(1);
            assert!(h.state().docs.iter().all(|d| !d.dirty()));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "one hello\n");
            h.state_mut().apply(&ctx, Action::Quit);
        }
        let mut h = app_with_session(&store);
        h.run_steps(1);
        let ctx = h.ctx.clone();
        let app = h.state();
        assert_eq!(app.docs.len(), 2, "both views come back");
        assert_eq!(app.docs[0].family, app.docs[1].family);
        assert_eq!((app.groups[0].views.len(), app.groups[1].views.len()), (1, 1));
        // Closing one view never asks about unsaved changes: the other keeps them.
        type_text(&mut h, "x");
        let active = h.state().active;
        h.state_mut().close(&ctx, active);
        h.run_steps(1);
        let app = h.state();
        assert_eq!(app.docs.len(), 1);
        assert!(app.docs[0].buffer.text().contains('x') && app.docs[0].dirty());
        h.state_mut().doc().buffer.undo();
        assert_eq!(text(&h), "one hello\n", "the undo history stayed with the text");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }

    #[test]
    fn completions_accepted_while_recording_play_back() {
        let mut h = app();
        let ctx = h.ctx.clone();
        type_text(&mut h, "hello\n");
        h.state_mut().apply(&ctx, Action::RecordMacro);
        type_text(&mut h, "he");
        h.key_press(Key::Enter);
        h.run_steps(2);
        type_text(&mut h, "\n");
        h.state_mut().apply(&ctx, Action::RecordMacro);
        assert_eq!(text(&h), "hello\nhello\n");
        h.state_mut().apply(&ctx, Action::PlaybackMacro);
        assert_eq!(text(&h), "hello\nhello\nhello\n");
    }

    #[test]
    fn packages_install_and_upgrade_from_an_index() {
        use crate::packages::tests::{THEME, temp_root, write_zip};
        let root = temp_root("index");
        let dirs = Dirs::at(&root);
        let release = |version: &str| {
            let zip = root.join(format!("Night-{version}.zip"));
            write_zip(&zip, &[("night-main/Night.tmTheme", THEME)]);
            serde_json::json!({ "version": version, "url": format!("file://{}", zip.display()), "sublime_text": "*", "platforms": ["*"] })
        };
        let channel = root.join("channel.json");
        let write_channel = |releases: Vec<Value>| {
            let json = serde_json::json!({ "schema_version": "3.0.0", "packages_cache": { "local": [
                { "name": "Night", "description": "A dark theme", "releases": releases },
            ] } });
            std::fs::write(&channel, json.to_string()).unwrap();
        };
        write_channel(vec![release("1.0")]);

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs);
        h.state_mut().settings.package_channels = vec![format!("file://{}", channel.display())];
        h.state_mut().apply(&ctx, Action::InstallFromIndex);
        wait_until(&mut h, "the index", |app| app.palette.open && !app.palette_candidates.is_empty());
        assert_eq!(h.state().palette_candidates[0].text, "Night");
        h.state_mut().pick_index_package(&ctx, 0);
        wait_until(&mut h, "the install", |app| app.theme_names.iter().any(|n| n == "Night"));
        let version = |h: &Harness<'_, EditorApp>| h.state().package_list.iter().find(|p| p.name == "Night").and_then(|p| p.metadata.version.clone());
        assert_eq!(version(&h).as_deref(), Some("1.0"), "{:?}", h.state().message);

        // Nothing newer yet; then there is.
        h.state_mut().apply(&ctx, Action::UpgradePackages);
        wait_until(&mut h, "the check", |app| app.index.busy.is_none());
        assert_eq!(h.state().message.as_deref(), Some("All packages are up to date"));
        write_channel(vec![release("1.0"), release("1.1")]);
        h.state_mut().apply(&ctx, Action::UpgradePackages);
        wait_until(&mut h, "the upgrade", |app| app.index.busy.is_none());
        assert_eq!(version(&h).as_deref(), Some("1.1"), "{:?}", h.state().message);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slow_plugin_commands_and_late_completions_finish_in_the_background() {
        if crate::plugins::find_python("").is_none() {
            eprintln!("No Python 3 here; skipping the plugin test");
            return;
        }
        use crate::packages::tests::temp_root;
        let root = temp_root("slow_plugin");
        let dirs = Dirs::at(&root);
        let package = dirs.packages.join("Slow");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("slow.py"),
            r#"import time
import texteditor


class SlowCommand(texteditor.TextCommand):
    def run(self, edit):
        time.sleep(1.5)
        self.view.insert(edit, 0, "slow ")


class Late(texteditor.EventListener):
    def on_query_completions(self, view, prefix, locations):
        later = texteditor.CompletionList()
        texteditor.set_timeout(lambda: later.set_completions(["quokka"]), 50)
        return later
"#,
        )
        .unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        wait_until(&mut h, "plugins to load", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 1));

        let started = Instant::now();
        h.state_mut().run_plugin_command(&ctx, "slow", &serde_json::json!({}));
        assert!(started.elapsed() < Duration::from_millis(1200), "the editor didn't wait for it");
        assert_eq!(h.state().plugins.running.values().next().map(String::as_str), Some("slow"));
        wait_until(&mut h, "the slow command", |app| app.plugins.running.is_empty());
        assert_eq!(text(&h), "slow ", "{:?}", h.state().message);

        h.state_mut().doc().buffer.set_single(Selection::caret(5));
        type_text(&mut h, "qu");
        wait_until(&mut h, "the late completions", |app| app.completion.is_some());
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(text(&h), "slow quokka");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn packages_from_the_index_bring_their_libraries() {
        use crate::packages::tests::{temp_root, write_zip};
        let root = temp_root("libraries");
        let dirs = Dirs::at(&root);
        let package = root.join("Fetch.zip");
        write_zip(&package, &[
            ("Fetch-main/fetch.py", "import fast\nprint('fast says', fast.VALUE)\n"),
            ("Fetch-main/dependencies.json", r#"{ "*": { "*": ["fast"] } }"#),
        ]);
        let wheel = root.join("fast-1.0-py3-none-any.whl");
        write_zip(&wheel, &[("fast/__init__.py", "VALUE = 42\n"), ("fast-1.0.dist-info/METADATA", "Name: fast\n")]);
        let release = |url: &Path| serde_json::json!({ "version": "1.0", "url": format!("file://{}", url.display()), "sublime_text": "*", "platforms": ["*"] });
        let channel = root.join("channel.json");
        let json = serde_json::json!({
            "packages_cache": { "local": [{ "name": "Fetch", "releases": [release(&package)] }] },
            "libraries_cache": { "local": [{ "name": "fast", "releases": [release(&wheel)] }] },
        });
        std::fs::write(&channel, json.to_string()).unwrap();

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs.clone());
        h.state_mut().settings.package_channels = vec![format!("file://{}", channel.display())];
        h.state_mut().apply(&ctx, Action::InstallFromIndex);
        wait_until(&mut h, "the index", |app| app.palette.open && !app.palette_candidates.is_empty());
        h.state_mut().pick_index_package(&ctx, 0);
        wait_until(&mut h, "the install", |app| app.index.busy.is_none());
        let lib = root.join("Lib").join("python3");
        assert!(lib.join("fast/__init__.py").is_file(), "{:?}", h.state().message);
        assert_eq!(h.state().message.as_deref(), Some("Installed Fetch 1.0, library fast 1.0"));
        if crate::plugins::find_python("").is_some() {
            wait_until(&mut h, "the plugin to import its library", |app| app.plugins.log.iter().any(|l| l == "fast says 42"));
        }

        // Gone again: the editor says so, and puts it back when asked.
        std::fs::remove_dir_all(&lib).unwrap();
        h.state_mut().reload_packages(&ctx);
        assert!(h.state().message.as_deref().unwrap_or("").contains("Install Missing Libraries"), "{:?}", h.state().message);
        h.state_mut().apply(&ctx, Action::InstallLibraries);
        wait_until(&mut h, "the libraries", |app| app.index.busy.is_none());
        assert!(lib.join("fast/__init__.py").is_file(), "{:?}", h.state().message);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn side_bar_and_tab_menus_have_their_own_and_packages_items() {
        use crate::packages::tests::temp_root;
        let root = temp_root("side_menus");
        let dirs = Dirs::at(&root);
        let tools = dirs.packages.join("Tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            tools.join("Side Bar.sublime-menu"),
            r#"[{ "caption": "Stamp", "command": "insert", "args": { "characters": "S", "paths": [] } }, { "caption": "Nope", "command": "nope" }]"#,
        )
        .unwrap();
        std::fs::write(tools.join("Tab Context.sublime-menu"), r#"[{ "caption": "Tab Stamp", "command": "insert", "args": { "characters": "T" } }]"#).unwrap();
        let dir = temp_folder("side_menus", &["a.txt", "b.txt", "c.txt"]);
        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        let side = h.state().runnable_menu(&h.state().package_side_bar_menu);
        assert_eq!(side.len(), 1, "the item with no command to run is left out");
        assert_eq!(h.state().runnable_menu(&h.state().package_tab_menu).len(), 1);
        let crate::menu_file::MenuItem::Entry { command: Some(stamp), .. } = &side[0] else { panic!() };
        let filled = crate::menu_file::with_args(stamp, &[("paths", serde_json::json!(["/x"]))]);
        assert_eq!(filled.args["paths"], serde_json::json!(["/x"]));
        h.state_mut().menu_pick(&ctx, package_menus::MenuPick::Command(filled));
        assert_eq!(text(&h), "S");
        // Unchanged again, so closing it needn't ask.
        h.state_mut().doc().buffer.undo();

        // Find in Folder searches there; Close Other Tabs and Close Tabs to the Right.
        h.state_mut().menu_pick(&ctx, package_menus::MenuPick::FindInFolder(dir.clone()));
        assert!(h.state().find_files.open);
        assert_eq!(h.state().find_files.scope, dir.display().to_string());
        for f in ["a.txt", "b.txt", "c.txt"] {
            h.state_mut().open_path(&dir.join(f));
        }
        h.run_steps(1);
        let b = tab(h.state(), &dir.join("b.txt"));
        h.state_mut().menu_pick(&ctx, package_menus::MenuPick::CloseRight(b));
        assert!(!h.state().docs.iter().any(|d| d.path.as_deref() == Some(dir.join("c.txt").as_path())));
        h.state_mut().menu_pick(&ctx, package_menus::MenuPick::CloseOthers(b));
        let left: Vec<String> = h.state().docs.iter().map(|d| d.title()).collect();
        assert_eq!(left, ["b.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Pictures of the app, to look at: run with
    /// `cargo test screenshots -- --ignored`; they go to the folder in
    /// `TEXTEDITOR_SCREENSHOTS`, or `text_editor_screenshots` in the temp folder.
    #[test]
    #[ignore = "renders pictures to look at, rather than checking anything"]
    fn screenshots() {
        use crate::packages::tests::temp_root;
        let out = std::env::var_os("TEXTEDITOR_SCREENSHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("text_editor_screenshots"));
        std::fs::create_dir_all(&out).unwrap();
        let shot = |h: &mut Harness<'_, EditorApp>, name: &str| {
            h.run_steps(3);
            h.render().unwrap().save(out.join(format!("{name}.png"))).unwrap();
        };
        let dir = temp_folder("screenshots", &[]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let greeting = hello_world();\n    println!(\"{greeting}\");\n}\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "/// Says hello.\npub fn hello_world() -> String {\n    \"hello, world\".to_owned()\n}\n").unwrap();
        let new = || {
            let mut h = Harness::builder()
                .with_size(egui::vec2(1100.0, 650.0))
                .build_eframe(|cc| EditorApp::new(cc, Vec::new()));
            h.run_steps(2);
            h
        };

        // Two groups, a file in each, and the folder in the side bar.
        let mut h = new();
        let ctx = h.ctx.clone();
        h.state_mut().open_folder(&ctx, dir.clone());
        h.state_mut().open_path(&dir.join("src/main.rs"));
        h.state_mut().apply(&ctx, Action::SetLayout(Preset::Columns2));
        h.state_mut().apply(&ctx, Action::FocusGroup(1));
        h.state_mut().open_path(&dir.join("src/lib.rs"));
        h.state_mut().apply(&ctx, Action::CloneView);
        shot(&mut h, "1-split-views");

        // A tab's right-click menu.
        h.get_all_by_label("lib.rs").last().unwrap().click_secondary();
        shot(&mut h, "2-tab-menu");
        h.key_press(Key::Escape);
        h.run_steps(2);

        // Find in Files with its results.
        h.state_mut().apply(&ctx, Action::FindInFiles);
        h.state_mut().find_files.query = "hello_world".into();
        h.state_mut().apply(&ctx, Action::SearchFiles);
        wait_until(&mut h, "the search", |app| app.find_files.has_results());
        shot(&mut h, "3-find-in-files");

        // The package list, with descriptions far too long for a row.
        let long = "Abstract Markup Language syntax highlighting, code snippets, and build system for Sublime Text, and more besides";
        let offer = ["Abrase", "Abstract Markup Language", "A File Icon"]
            .iter()
            .map(|name| crate::package_index::IndexPackage {
                name: name.to_string(),
                description: long.into(),
                homepage: String::new(),
                version: "2014.11.13.21.09.24".into(),
                url: String::new(),
            })
            .collect();
        h.state_mut().index.offer = Some(offer);
        h.state_mut().palette.show_prompt(Mode::Quick, "Install a package", "", 0);
        (h.state_mut().palette_candidates, h.state_mut().palette_targets) = h.state().index_rows().into_iter().unzip();
        shot(&mut h, "3b-package-list");
        h.key_press(Key::Escape);
        h.run_steps(2);

        // The autocomplete list.
        let mut h = new();
        type_text(&mut h, "fn hello_world() {}\nfn help_me() {}\n\nhel");
        shot(&mut h, "4-autocomplete");

        // The View menu's Layout submenu (drawn in the window off macOS's main thread).
        h.get_by_label("View").click();
        h.run_steps(2);
        h.get_by_label("Layout").hover();
        shot(&mut h, "5-layout-menu");

        // A plugin's regions, phantoms and popup.
        if crate::plugins::find_python("").is_some() {
            let root = temp_root("screenshots");
            let dirs = Dirs::at(&root);
            std::fs::create_dir_all(dirs.packages.join("Lint")).unwrap();
            std::fs::write(
                dirs.packages.join("Lint/lint.py"),
                r#"import texteditor


class LintCommand(texteditor.TextCommand):
    def run(self, edit):
        v = self.view
        v.add_regions("err", [texteditor.Region(4, 9)], "invalid", "", texteditor.DRAW_NO_FILL | texteditor.DRAW_NO_OUTLINE | texteditor.DRAW_SQUIGGLY_UNDERLINE)
        v.add_regions("found", [texteditor.Region(14, 19)], "string", "", 0)
        v.add_phantom("hint", texteditor.Region(9), "<span style='color: #e06c75'>unused variable</span>", texteditor.LAYOUT_INLINE)
        v.add_phantom("note", texteditor.Region(24), "<b>note:</b> this line is explained<br>across <a href='more'>two lines</a>", texteditor.LAYOUT_BELOW)
        v.show_popup("<b>value</b>: <code>i32</code><br>Hover help from a plugin. <a href='docs'>Docs</a>", location=v.size() - 1)
"#,
            )
            .unwrap();
            let mut h = new();
            let ctx = h.ctx.clone();
            type_text(&mut h, "let value = 1;\nlet total = value + 2;\nprintln!(\"{total}\");\n");
            h.state_mut().packages = Some(dirs);
            h.state_mut().reload_packages(&ctx);
            wait_until(&mut h, "plugins", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 1));
            h.state_mut().run_plugin_command(&ctx, "lint", &serde_json::json!({}));
            shot(&mut h, "6-plugin-regions-phantoms-popup");
            std::fs::remove_dir_all(&root).unwrap();
        }
        eprintln!("Screenshots in {}", out.display());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plugins_open_windows_name_tabs_and_make_them_read_only() {
        if crate::plugins::find_python("").is_none() {
            eprintln!("No Python 3 here; skipping the plugin test");
            return;
        }
        use crate::packages::tests::temp_root;
        let root = temp_root("plugin_window");
        let dirs = Dirs::at(&root);
        std::fs::create_dir_all(dirs.packages.join("Diff")).unwrap();
        std::fs::write(
            dirs.packages.join("Diff/diff.py"),
            r#"import texteditor


class ShowDiffCommand(texteditor.WindowCommand):
    def run(self):
        self.window.run_command("new_window")
        win = texteditor.active_window()
        win.set_layout({"cols": [0.0, 0.5, 1.0], "rows": [0.0, 1.0], "cells": [[0, 0, 1, 1], [1, 0, 2, 1]]})
        win.set_sidebar_visible(False)
        for group, name in enumerate(["left", "right"]):
            view = win.new_file()
            view.set_name(name)
            view.set_scratch(True)
            view.settings().set("is_diff", True)
            view.run_command("append_text", {"text": "one\ntwo\n"})
            view.set_read_only(True)
            win.set_view_index(view, group, 0)
        print("ends", view.text_point(1, -1), view.text_point(9, 0), view.text_point(0, 99, clamp_column=True))


class AppendTextCommand(texteditor.TextCommand):
    def run(self, edit, text):
        self.view.insert(edit, self.view.size(), text)


class Listener(texteditor.EventListener):
    def on_pre_close(self, view):
        if view.settings().get("is_diff"):
            win = view.window()
            texteditor.set_timeout(lambda: win.run_command("close_window"), 10)
"#,
        )
        .unwrap();
        std::fs::write(
            dirs.packages.join("Diff/Default.sublime-keymap"),
            r#"[{ "keys": ["."], "command": "move_to", "args": { "to": "eof" }, "context": [{ "key": "setting.is_diff" }] }]"#,
        )
        .unwrap();
        let mut h = app();
        let ctx = h.ctx.clone();
        type_text(&mut h, "mine");
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        wait_until(&mut h, "plugins", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 2));
        let mine = h.state().docs[h.state().active].id;

        h.state_mut().run_plugin_command(&ctx, "show_diff", &serde_json::json!({}));
        h.run_steps(2);
        let app = h.state();
        assert_eq!(app.windows.len(), 1, "a window opened");
        assert!(app.stashed(mine) && !app.groups.iter().any(|g| g.views.contains(&mine)), "ours is set aside");
        let titles: Vec<String> = app.groups.iter().flat_map(|g| g.views.iter()).map(|id| app.docs.iter().find(|d| d.id == *id).unwrap().title()).collect();
        assert_eq!(titles, ["left", "right"]);
        assert!(!app.settings.show_sidebar || app.sidebar_override == Some(false), "the side bar is hidden for that window only");
        assert!(app.settings.show_sidebar);
        assert!(app.plugins.log.iter().any(|l| l == "ends 3 8 3"), "{:?}", app.plugins.log);

        // Read-only: typing does nothing, but a key bound with the view's setting works.
        let right = h.state().groups[1].views[0];
        let i = h.state().docs.iter().position(|d| d.id == right).unwrap();
        h.state_mut().activate(i);
        type_text(&mut h, "x");
        assert_eq!(text(&h), "one\ntwo\n");
        h.input_mut().events.extend([
            egui::Event::Key { key: Key::Period, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
            egui::Event::Text(".".into()),
        ]);
        h.run_steps(2);
        assert_eq!(h.state().docs[h.state().active].buffer.primary().head, 8, "the binding's condition saw the view's setting");

        // Closing one closes the window (the plugin's on_pre_close), and ours comes back.
        let active = h.state().active;
        h.state_mut().close(&ctx, active);
        wait_until(&mut h, "the window to close", |app| app.windows.is_empty());
        let app = h.state();
        assert_eq!(app.docs.iter().map(|d| d.title()).collect::<Vec<_>>(), ["untitled"]);
        assert_eq!(app.docs[app.active].id, mine);
        assert_eq!(app.groups.len(), 1);
        assert!(app.sidebar_override.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn packages_in_texteditors_own_format_with_texteditor_plugins() {
        use crate::packages::tests::{SYNTAX, temp_root, write_zip};
        let root = temp_root("own_format");
        let dirs = Dirs::at(&root);
        let mine = dirs.packages.join("Mine");
        std::fs::create_dir_all(&mine).unwrap();
        let write = |name: &str, text: &str| std::fs::write(mine.join(name), text).unwrap();
        write("Ink.te-color-scheme", r##"{ "globals": { "background": "#101820", "foreground": "white" } }"##);
        write("Fancy.te-syntax", SYNTAX);
        write("hi.te-snippet", "<snippet><content>hello $1</content><tabTrigger>hi</tabTrigger></snippet>");
        write("Mine.te-commands", r#"[{ "caption": "Mine: Shout", "command": "shout" }]"#);
        write("Default.te-keymap", r#"[{ "keys": ["f9"], "command": "shout" }]"#);
        write("Main.te-menu", r#"[{ "id": "tools", "children": [{ "caption": "Shout", "command": "shout" }] }]"#);
        write("Stamp.te-macro", r#"[{ "command": "insert", "args": { "characters": "!" } }]"#);
        write("Mine.te-settings", r#"{ "word": "HEY" }"#);
        write(
            "mine.py",
            r#"import texteditor


class ShoutCommand(texteditor.TextCommand):
    def run(self, edit):
        word = texteditor.load_settings("Mine.te-settings").get("word")
        self.view.insert(edit, self.view.size(), word)
        self.view.add_regions("shout", [texteditor.Region(0, 3)], "", "", texteditor.DRAW_NO_FILL)


class Listener(texteditor.EventListener):
    def on_query_context(self, view, key, operator, operand, match_all):
        return True if key == "mine" else None
"#,
        );
        // A zipped package under TextEditor's own name.
        let zip = root.join("Zed.te-package");
        write_zip(&zip, &[("zed.py", "import texteditor\n\nclass ZedCommand(texteditor.TextCommand):\n    def run(self, edit):\n        self.view.insert(edit, 0, 'z')\n")]);
        dirs.install(&zip).unwrap();

        let mut h = app();
        let ctx = h.ctx.clone();
        h.state_mut().packages = Some(dirs);
        h.state_mut().reload_packages(&ctx);
        wait_for_languages(&mut h);
        let app = h.state();
        assert!(!app.message.as_deref().unwrap_or("").starts_with("Could not"), "{:?}", app.message);
        assert!(app.theme_names.iter().any(|n| n == "Ink"));
        assert!(app.syntax_names.iter().any(|n| n == "Fancy"));
        assert!(app.macro_files.iter().any(|m| m.name() == "Stamp"));
        let commands: Vec<String> = app.commands().into_iter().map(|c| c.0).collect();
        assert!(commands.iter().any(|c| c == "Tools: Shout") && commands.iter().any(|c| c == "Tools: Macros: Stamp"), "{commands:?}");
        assert!(app.palette_entries.iter().any(|(caption, _)| caption == "Mine: Shout"));

        type_text(&mut h, "hi");
        h.key_press(Key::Tab);
        h.run_steps(2);
        assert_eq!(text(&h), "hello ");
        if crate::plugins::find_python("").is_none() {
            eprintln!("No Python 3 here; skipping the plugin part");
            return;
        }
        wait_until(&mut h, "plugins", |app| app.plugins.host.as_ref().is_some_and(|host| host.commands.len() == 2));
        h.key_press(Key::F9);
        h.run_steps(2);
        assert_eq!(text(&h), "hello HEY", "{:?}", h.state().plugins.log);
        assert!(h.state_mut().condition_holds(&ctx, &Condition::new("mine", "equal", true.into(), false)));
        h.state_mut().run_plugin_command(&ctx, "zed", &serde_json::json!({}));
        assert_eq!(text(&h), "zhello HEY");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
