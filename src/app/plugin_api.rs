//! The editor's side of Python plugins: starting the plugin host with the
//! packages' plugins, sending it commands and events, and answering its
//! calls, which are the plugin API (`src/plugin_host/texteditor.py`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers, Order, vec2};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde_json::{Map, Value, json};
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{Regex, Region, Scope};

use super::{EditorApp, PackageCommand, translate};
use crate::buffer::{Buffer, Edit, Selection};
use crate::completion::Completion;
use crate::document::Document;
use crate::editor_view::{self, Highlight, Phantom, PhantomLayout, draw};
use crate::keymap::{Condition, NamedCommand};
use crate::layout::Layout;
use crate::minihtml;
use crate::packages::Package;
use crate::palette::Mode;
use crate::plugins::{self, PluginHost};
use crate::settings::Settings;

/// How long the editor waits for a plugin command before letting it finish
/// in the background. Most commands are done well within this, so keys
/// pressed after them still apply after them.
const COMMAND_WAIT: Duration = Duration::from_millis(500);
/// `on_pre_save` listeners (formatters, say) may change the text first.
const PRE_SAVE_WAIT: Duration = Duration::from_secs(2);
/// A key binding's own condition is asked about while its key is pressed.
const QUERY_WAIT: Duration = Duration::from_millis(300);
const TOOK_TOO_LONG: &str = "it took too long";
/// Plugins' completions are asked for while typing, so not for long.
const COMPLETIONS_WAIT: Duration = Duration::from_millis(250);
/// Settings plugins can set on a view that change how the editor shows or
/// edits it. The name plugins use for each, then the editor's.
const VIEW_SETTINGS: [(&str, &str); 12] = [
    ("tab_size", "tab_size"),
    ("translate_tabs_to_spaces", "translate_tabs_to_spaces"),
    ("word_wrap", "word_wrap"),
    ("line_numbers", "line_numbers"),
    ("highlight_line", "highlight_line"),
    ("draw_indent_guides", "indent_guides"),
    ("indent_guides", "indent_guides"),
    ("auto_match_enabled", "auto_match_enabled"),
    ("auto_indent", "auto_indent"),
    ("auto_complete", "auto_complete"),
    ("caret_blink", "caret_blink"),
    ("font_size", "font_size"),
];
const MAX_LOG_LINES: usize = 2000;

#[derive(Default)]
pub(super) struct PluginState {
    pub(super) host: Option<PluginHost>,
    /// The plugin console: what plugins printed, and their errors.
    pub(super) log: Vec<String>,
    /// Commands still running in the background, by request id.
    pub(super) running: BTreeMap<u64, String>,
    /// The completions request whose late answers (a `CompletionList`
    /// filled in later) are still wanted, and its view.
    completion_request: Option<(u64, u64)>,
    /// Settings plugins set on views, beyond the editor's own.
    view_settings: HashMap<u64, Map<String, Value>>,
    /// Status bar text set by plugins, per view, by key.
    pub(super) status: HashMap<u64, BTreeMap<String, String>>,
    /// How plugins' regions are drawn, per view, by key. The regions
    /// themselves are marks in the view's buffer, so they follow edits.
    regions: HashMap<u64, BTreeMap<String, RegionStyle>>,
    /// Theme colors of scopes regions asked for, by theme and scope.
    scope_colors: RefCell<HashMap<(String, String), Option<Color32>>>,
    /// Phantoms per view; their places are marks in the buffer too.
    phantoms: HashMap<u64, Vec<PluginPhantom>>,
    next_phantom: u64,
    /// The popup a plugin showed, if it's still up.
    pub(super) popup: Option<PluginPopup>,
    /// Output panels by name; one at a time is shown below the editor.
    pub(super) panels: Vec<(String, Document)>,
    pub(super) shown_panel: Option<String>,
    /// The quick or input panel a plugin opened.
    pub(super) prompt: Option<Prompt>,
    /// What listeners last heard about each view: its revision and selections.
    seen: HashMap<u64, (u64, Vec<(usize, usize)>)>,
    active: Option<u64>,
    view_names: HashMap<u64, String>,
}

struct RegionStyle {
    scope: String,
    flags: u32,
}

struct PluginPhantom {
    id: u64,
    key: String,
    content: minihtml::Lines,
    layout: PhantomLayout,
    /// Called with a link's address when one is clicked.
    navigate: Option<u64>,
}

pub(super) struct PluginPopup {
    view: u64,
    content: minihtml::Lines,
    /// The char it's shown under.
    location: usize,
    max_width: f32,
    max_height: f32,
    navigate: Option<u64>,
    hide: Option<u64>,
    /// The view's text when it was shown: editing hides it.
    revision: u64,
}

/// Where a region of `key` is kept among the buffer's marks.
fn region_marks(key: &str) -> String {
    format!("region:{key}")
}

fn phantom_marks(id: u64) -> String {
    format!("phantom:{id}")
}

pub(super) struct Prompt {
    /// Quick panel rows: text and detail.
    pub(super) items: Vec<(String, String)>,
    /// Called with the chosen index or the typed text.
    done: Option<u64>,
    /// Input panels: called on Esc. A quick panel gets `done(-1)` instead.
    cancel: Option<u64>,
    input: bool,
}

impl EditorApp {
    /// (Re)starts the plugin host for the enabled packages, if any of them
    /// has plugins.
    pub(super) fn start_plugins(&mut self, ctx: &egui::Context) {
        self.plugins.host = None;
        self.plugins.seen.clear();
        let Some(dirs) = &self.packages else {
            return;
        };
        let enabled: Vec<&Package> =
            self.package_list.iter().filter(|p| !self.packages_ignored.contains(&p.name)).collect();
        let with_plugins: Vec<&str> = enabled.iter().filter(|p| p.has_plugins()).map(|p| p.name.as_str()).collect();
        if with_plugins.is_empty() {
            return;
        }
        let Some(python) = plugins::find_python(&self.settings.python_path) else {
            self.message = Some(format!(
                "{} has Python plugins: install Python 3, or set \"python_path\" in settings, to run them",
                with_plugins.join(", ")
            ));
            return;
        };
        // Libraries the plugins import that aren't installed: they'd fail to load.
        let missing = self.missing_libraries();
        if !missing.is_empty() {
            self.message = Some(format!(
                "Plugins need libraries that aren't installed ({}): Packages > Install Missing Libraries",
                missing.join(", ")
            ));
        }
        let packages: Vec<Value> = enabled
            .iter()
            .map(|p| json!({ "name": p.name, "folder": p.folder, "archive": p.archive, "root": p.archive_root() }))
            .collect();
        let config = dirs.packages.parent().map(Path::to_path_buf).unwrap_or_default();
        let platform = if cfg!(target_os = "macos") {
            "osx"
        } else if cfg!(windows) {
            "windows"
        } else {
            "linux"
        };
        let load = json!({
            "t": "load",
            "packages": packages,
            "info": {
                "platform": platform,
                "arch": if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" },
                "executable": std::env::current_exe().ok(),
                "packages_path": dirs.packages,
                "installed_packages_path": dirs.installed,
                "cache_path": dirs.cache,
            },
            // Where Package Control puts libraries that plugins import.
            "lib_paths": [config.join("Lib").join("python3"), config.join("Lib").join("python38")],
        });
        match PluginHost::start(&python, &dirs.cache.join("plugin_host"), load, ctx) {
            Ok(host) => self.plugins.host = Some(host),
            Err(e) => self.message = Some(format!("Plugins: {e}")),
        }
    }

    /// Handles whatever the plugin host said since the last frame.
    pub(super) fn poll_plugins(&mut self, ctx: &egui::Context) {
        while let Some(message) = self.plugins.host.as_ref().and_then(PluginHost::try_recv) {
            self.handle_plugin_message(ctx, message);
        }
    }

    /// Answers the host's calls until it finishes request `id`; its result,
    /// or what went wrong.
    fn wait_for_plugin(&mut self, ctx: &egui::Context, id: u64, wait: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + wait;
        loop {
            let Some(host) = &self.plugins.host else {
                return Err("the plugin host stopped".into());
            };
            match host.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(message) if message["t"] == "done" && message["id"] == id => {
                    return match message.get("e").and_then(Value::as_str) {
                        Some(error) => Err(error.to_owned()),
                        None => Ok(message.get("r").cloned().unwrap_or(Value::Null)),
                    };
                }
                Ok(message) => self.handle_plugin_message(ctx, message),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err(TOOK_TOO_LONG.into()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.plugins.host = None;
                    return Err("the plugin host stopped".into());
                }
            }
        }
    }

    fn handle_plugin_message(&mut self, ctx: &egui::Context, message: Value) {
        match message["t"].as_str() {
            Some("call") => {
                let method = message["m"].as_str().unwrap_or_default();
                let params = message.get("p").cloned().unwrap_or(Value::Null);
                let reply = match self.plugin_call(ctx, method, &params) {
                    Ok(result) => json!({ "t": "reply", "id": message["id"], "r": result }),
                    Err(error) => json!({ "t": "reply", "id": message["id"], "e": error }),
                };
                if let Some(host) = &mut self.plugins.host {
                    let _ = host.send(&reply);
                }
            }
            Some("loaded") => {
                if let Some(host) = &mut self.plugins.host {
                    host.commands = serde_json::from_value(message["commands"].clone()).unwrap_or_default();
                    host.events = serde_json::from_value(message["events"].clone()).unwrap_or_default();
                }
                let errors: Vec<String> = serde_json::from_value(message["errors"].clone()).unwrap_or_default();
                if let Some(first) = errors.first() {
                    self.message = Some(format!("Plugin failed to load: {first} (see Show Plugin Console)"));
                }
                // Listeners hear about changes from now on, not about what's already open.
                self.plugins.seen = self.views_now();
                self.plugins.active = Some(self.docs[self.active].id);
                self.translate_package_commands();
            }
            Some("log") => {
                let text = message["text"].as_str().unwrap_or_default().to_owned();
                self.plugins.log.push(text);
                if self.plugins.log.len() > MAX_LOG_LINES {
                    self.plugins.log.drain(..self.plugins.log.len() - MAX_LOG_LINES);
                }
            }
            Some("exit") if self.plugins.host.take().is_some() => {
                self.plugins.running.clear();
                self.message = Some("The plugin host stopped (see Show Plugin Console)".into());
            }
            // A command that went on in the background has finished.
            Some("done") => {
                let id = message["id"].as_u64().unwrap_or(0);
                // Completions from a plugin that was slow to answer.
                if let Some((request, view)) = self.plugins.completion_request
                    && request == id
                {
                    let items = message["r"]["items"].as_array().map(|a| a.iter().filter_map(Completion::from_plugin).collect()).unwrap_or_default();
                    self.add_late_completions(view, items);
                }
                if let Some(name) = self.plugins.running.remove(&id)
                    && let Some(error) = message.get("e").and_then(Value::as_str)
                {
                    self.message = Some(format!("Plugin command {name}: {error}"));
                }
            }
            // Completions a plugin gave after it was asked.
            Some("completions") => {
                let request = message["request"].as_u64();
                if let Some((id, view)) = self.plugins.completion_request
                    && Some(id) == request
                {
                    let items: Vec<Completion> = message["items"].as_array().map(|a| a.iter().filter_map(Completion::from_plugin).collect()).unwrap_or_default();
                    self.add_late_completions(view, items);
                }
            }
            _ => {}
        }
    }

    /// Runs a plugin command on the active view (or window, or app).
    pub(super) fn run_plugin_command(&mut self, ctx: &egui::Context, name: &str, args: &Value) {
        let view = self.docs[self.active].id;
        let Some(host) = &mut self.plugins.host else {
            self.message = Some(format!("The plugin for \"{name}\" isn't running"));
            return;
        };
        let id = host.next_id();
        let sent = host.send(&json!({ "t": "run", "id": id, "name": name, "args": args, "view": view }));
        match sent.and_then(|_| self.wait_for_plugin(ctx, id, COMMAND_WAIT).map(|_| ())) {
            Ok(()) => {}
            // Still going: it finishes in the background, however long it takes.
            Err(e) if e == TOOK_TOO_LONG => {
                self.plugins.running.insert(id, name.to_owned());
            }
            Err(e) => self.message = Some(format!("Plugin command {name}: {e}")),
        }
    }

    /// Tells listeners about an event on a view; with `wait`, waits for them
    /// and returns the first answer.
    pub(super) fn plugin_event(
        &mut self,
        ctx: &egui::Context,
        event: &str,
        view: u64,
        args: Value,
        wait: Option<Duration>,
    ) -> Option<Value> {
        let host = self.plugins.host.as_mut()?;
        if !host.events.contains(event) {
            return None;
        }
        let id = host.next_id();
        host.send(&json!({ "t": "event", "id": id, "event": event, "view": view, "args": args })).ok()?;
        self.wait_for_plugin(ctx, id, wait?).ok()
    }

    /// Before a file is written: plugins may change its text.
    pub(super) fn plugin_pre_save(&mut self, ctx: &egui::Context, view: u64) {
        self.plugin_event(ctx, "on_pre_save", view, json!([]), Some(PRE_SAVE_WAIT));
    }

    pub(super) fn plugin_post_save(&mut self, ctx: &egui::Context, view: u64) {
        self.plugin_event(ctx, "on_post_save", view, json!([]), None);
    }

    /// A key binding condition the editor doesn't know: plugins may answer
    /// it with `on_query_context`.
    pub(super) fn plugin_query_context(&mut self, ctx: &egui::Context, c: &Condition) -> bool {
        let operator =
            ["equal", "not_equal", "regex_match", "not_regex_match", "regex_contains", "not_regex_contains"]
                .iter()
                .position(|o| *o == c.operator)
                .unwrap_or(0);
        let view = self.docs[self.active].id;
        let args = json!([c.key, operator, c.operand, c.match_all]);
        self.plugin_event(ctx, "on_query_context", view, args, Some(QUERY_WAIT)).and_then(|v| v.as_bool()) == Some(true)
    }

    fn views_now(&self) -> HashMap<u64, (u64, Vec<(usize, usize)>)> {
        self.docs
            .iter()
            .map(|d| (d.id, (d.buffer.revision, d.buffer.selections().iter().map(|s| (s.anchor, s.head)).collect())))
            .collect()
    }

    /// Tells listeners what changed since the last frame: views opened or
    /// closed, text edited, selections moved, another tab in front.
    pub(super) fn send_plugin_events(&mut self, ctx: &egui::Context) {
        if self.plugins.host.as_ref().is_none_or(|h| h.events.is_empty()) {
            return;
        }
        let now = self.views_now();
        let mut events: Vec<(&str, u64)> = Vec::new();
        for doc in &self.docs {
            match (self.plugins.seen.get(&doc.id), &now[&doc.id]) {
                (None, _) => events.push((if doc.path.is_some() { "on_load" } else { "on_new" }, doc.id)),
                (Some((revision, sels)), (now_revision, now_sels)) => {
                    if revision != now_revision {
                        events.push(("on_modified", doc.id));
                    }
                    if sels != now_sels {
                        events.push(("on_selection_modified", doc.id));
                    }
                }
            }
        }
        for id in self.plugins.seen.keys().filter(|id| !now.contains_key(id)) {
            events.push(("on_close", *id));
        }
        let active = self.docs[self.active].id;
        if self.plugins.active != Some(active) {
            if let Some(old) = self.plugins.active.filter(|id| now.contains_key(id)) {
                events.push(("on_deactivated", old));
            }
            events.push(("on_activated", active));
        }
        self.plugins.seen = now;
        self.plugins.active = Some(active);
        for (event, view) in events {
            self.plugin_event(ctx, event, view, json!([]), None);
        }
    }

    /// The quick or input panel closed: tell the plugin what was picked
    /// (`None` if nothing was).
    pub(super) fn answer_plugin_prompt(&mut self, value: Option<Value>) {
        let Some(prompt) = self.plugins.prompt.take() else {
            return;
        };
        let (callback, args) = match value {
            Some(value) => (prompt.done, json!([value])),
            None if prompt.input => (prompt.cancel, json!([])),
            None => (prompt.done, json!([-1])),
        };
        if let (Some(callback), Some(host)) = (callback, &mut self.plugins.host) {
            let _ = host.send(&json!({ "t": "callback", "callback": callback, "args": args }));
        }
    }

    /// The editor's settings with any a plugin set on this view, for the
    /// ones that change how it's shown or edited (`tab_size`, `word_wrap`…).
    pub(super) fn settings_for(&self, view: u64) -> Settings {
        let Some(overrides) = self.plugins.view_settings.get(&view).filter(|o| !o.is_empty()) else {
            return self.settings.clone();
        };
        let Ok(Value::Object(mut merged)) = serde_json::to_value(&self.settings) else {
            return self.settings.clone();
        };
        for (theirs, ours) in VIEW_SETTINGS {
            let (Some(value), Some(old)) = (overrides.get(theirs), merged.get(ours)) else {
                continue;
            };
            let same_kind = (value.is_boolean() && old.is_boolean()) || (value.is_number() && old.is_number());
            if same_kind {
                merged.insert(ours.to_owned(), value.clone());
            }
        }
        serde_json::from_value::<Settings>(Value::Object(merged)).map(Settings::clamped).unwrap_or_else(|_| self.settings.clone())
    }

    /// The settings the active tab is edited with.
    pub(super) fn active_settings(&self) -> Settings {
        self.settings_for(self.docs[self.active].id)
    }

    /// What to mark in a tab: plugins' regions, and matches in Find Results.
    pub(super) fn highlights_for(&self, index: usize) -> Vec<Highlight> {
        let doc = &self.docs[index];
        let mut out = Vec::new();
        for (key, style) in self.plugins.regions.get(&doc.id).into_iter().flatten() {
            let color = self.scope_color(&style.scope);
            out.extend(doc.buffer.marks(&region_marks(key)).iter().map(|&(start, end)| Highlight { start, end, color, flags: style.flags }));
        }
        let find = self.highlighter.colors().find_highlight;
        out.extend(
            doc.buffer
                .marks(super::find_in_files::RESULTS_MARKS)
                .iter()
                .map(|&(start, end)| Highlight { start, end, color: find, flags: draw::NO_OUTLINE }),
        );
        out
    }

    /// The theme's color for a region's scope; with none, the caret's.
    fn scope_color(&self, scope: &str) -> Color32 {
        let key = (self.highlighter.theme_name().to_owned(), scope.to_owned());
        let found = *self.plugins.scope_colors.borrow_mut().entry(key).or_insert_with(|| self.region_color(scope));
        found.unwrap_or(self.highlighter.colors().caret)
    }

    /// The theme's color for a region's scope. Themes without colors for
    /// diffs get those of `markup.inserted` and the like, and then the usual
    /// green, red and orange; the `region.*` scopes have their colors
    /// too. Changed characters (`*.char`) are marked more strongly than
    /// changed lines.
    fn region_color(&self, scope: &str) -> Option<Color32> {
        let has = |word: &str| scope.split(['.', ' ']).any(|part| part == word);
        let (markup, fallback) = if has("inserted") {
            ("markup.inserted", Color32::from_rgb(80, 180, 90))
        } else if has("deleted") {
            ("markup.deleted", Color32::from_rgb(220, 80, 80))
        } else if has("changed") || has("modified") {
            ("markup.changed", Color32::from_rgb(230, 160, 60))
        } else {
            let named = [
                ("redish", (220, 80, 80)),
                ("orangish", (230, 140, 60)),
                ("yellowish", (220, 200, 80)),
                ("greenish", (100, 190, 100)),
                ("cyanish", (80, 190, 200)),
                ("bluish", (90, 140, 230)),
                ("purplish", (170, 110, 220)),
                ("pinkish", (230, 120, 180)),
            ];
            let color = named.iter().find(|(name, _)| has(name)).map(|(_, (r, g, b))| Color32::from_rgb(*r, *g, *b));
            return self.highlighter.scope_color(scope).or(color);
        };
        let color = self.highlighter.scope_color(scope).or_else(|| self.highlighter.scope_color(markup)).unwrap_or(fallback);
        Some(color.gamma_multiply(if has("char") { 0.55 } else { 0.22 }))
    }

    /// A setting a plugin set on a view, if it set one.
    pub(super) fn plugin_view_setting(&self, view: u64, key: &str) -> Option<Value> {
        self.plugins.view_settings.get(&view)?.get(key).cloned()
    }

    /// A tab's phantoms, where its edits have moved them.
    pub(super) fn phantoms_for(&self, index: usize) -> Vec<Phantom> {
        let doc = &self.docs[index];
        let Some(phantoms) = self.plugins.phantoms.get(&doc.id) else {
            return Vec::new();
        };
        phantoms
            .iter()
            .filter_map(|p| {
                let at = doc.buffer.marks(&phantom_marks(p.id)).first()?.0;
                Some(Phantom { id: p.id, at, content: p.content.clone(), layout: p.layout })
            })
            .collect()
    }

    /// A link in a phantom was clicked: tell its plugin where it leads.
    pub(super) fn phantom_navigate(&mut self, phantom: u64, href: String) {
        let callback = self.plugins.phantoms.values().flatten().find(|p| p.id == phantom).and_then(|p| p.navigate);
        if let (Some(callback), Some(host)) = (callback, &mut self.plugins.host) {
            let _ = host.send(&json!({ "t": "callback", "callback": callback, "args": [href], "keep": true }));
        }
    }

    /// What plugins' `on_query_completions` offer for `prefix`, and their
    /// flags (to leave out words or snippets).
    /// A plugin that answers with a `CompletionList` to fill in later sends
    /// those when it's ready (see `add_late_completions`).
    pub(super) fn plugin_completions(&mut self, ctx: &egui::Context, view: u64, prefix: &str, locations: &[usize]) -> (Vec<Completion>, u64) {
        self.plugins.completion_request = None;
        let Some(host) = self.plugins.host.as_mut().filter(|h| h.events.contains("on_query_completions")) else {
            return (Vec::new(), 0);
        };
        let id = host.next_id();
        let event = json!({ "t": "event", "id": id, "event": "on_query_completions", "view": view, "args": [prefix, locations] });
        if host.send(&event).is_err() {
            return (Vec::new(), 0);
        }
        // A slow plugin's completions can still arrive while the word is typed.
        self.plugins.completion_request = Some((id, view));
        let Ok(answer) = self.wait_for_plugin(ctx, id, COMPLETIONS_WAIT) else {
            return (Vec::new(), 0);
        };
        if answer.get("pending").and_then(Value::as_bool) != Some(true) {
            self.plugins.completion_request = None;
        }
        let items = answer.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(Completion::from_plugin).collect()).unwrap_or_default();
        (items, answer.get("flags").and_then(Value::as_u64).unwrap_or(0))
    }

    /// Esc closes a plugin's popup.
    pub(super) fn plugin_popup_keys(&mut self, ctx: &egui::Context) {
        if self.plugins.popup.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.hide_plugin_popup();
        }
    }

    /// Closes a plugin's popup and tells it so.
    fn hide_plugin_popup(&mut self) {
        let Some(popup) = self.plugins.popup.take() else {
            return;
        };
        if let (Some(callback), Some(host)) = (popup.hide, &mut self.plugins.host) {
            let _ = host.send(&json!({ "t": "callback", "callback": callback, "args": [] }));
        }
    }

    /// Draws a plugin's popup under its place in the text. Editing the
    /// view or clicking elsewhere closes it.
    pub(super) fn plugin_popup_ui(&mut self, ctx: &egui::Context) {
        let Some(popup) = &self.plugins.popup else {
            return;
        };
        let doc = self.docs.iter().find(|d| d.id == popup.view);
        let showing = doc.is_some_and(|d| d.buffer.revision == popup.revision && Some(d.id) == Some(self.docs[self.active].id));
        let Some(at) = doc.filter(|_| showing).and_then(|d| editor_view::char_rect(ctx, d, &self.settings_for(d.id), popup.location)) else {
            self.hide_plugin_popup();
            return;
        };
        let mut clicked = None;
        let area = egui::Area::new(egui::Id::unique("plugin-popup"))
            .order(Order::Foreground)
            .fixed_pos(at.left_bottom())
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(popup.max_width);
                    egui::ScrollArea::vertical().max_height(popup.max_height).show(ui, |ui| {
                        for line in &popup.content {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for run in line {
                                    let mut text = egui::RichText::new(&run.text);
                                    if run.code {
                                        text = text.monospace();
                                    }
                                    if run.bold {
                                        text = text.strong();
                                    }
                                    if run.italic {
                                        text = text.italics();
                                    }
                                    if let Some(color) = run.color {
                                        text = text.color(color);
                                    }
                                    match &run.link {
                                        Some(href) => {
                                            if ui.link(text).clicked() {
                                                clicked = Some(href.clone());
                                            }
                                        }
                                        None => {
                                            ui.label(text);
                                        }
                                    }
                                }
                            });
                        }
                    });
                });
            });
        if let Some(href) = clicked {
            if let (Some(callback), Some(host)) = (popup.navigate, &mut self.plugins.host) {
                let _ = host.send(&json!({ "t": "callback", "callback": callback, "args": [href], "keep": true }));
            }
            return;
        }
        let outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p))
        });
        if outside {
            self.hide_plugin_popup();
        }
    }

    /// Opens the plugin console as a new tab.
    pub(super) fn show_plugin_console(&mut self) {
        self.new_doc();
        let text = if self.plugins.log.is_empty() { "No plugin output yet.\n".to_owned() } else { self.plugins.log.join("\n") + "\n" };
        self.doc().buffer.insert(&text, crate::buffer::EditKind::Other);
    }

    /// The tab or output panel with view id `id`, ready to be edited.
    fn plugin_doc(&mut self, id: u64) -> Result<&mut Document, String> {
        if let Some(i) = self.docs.iter().position(|d| d.id == id) {
            self.hold_text(i);
            return Ok(&mut self.docs[i]);
        }
        self.plugins.panels.iter_mut().map(|(_, d)| d).find(|d| d.id == id).ok_or_else(|| format!("view {id} is closed"))
    }

    /// The plugin API: one call from a plugin.
    fn plugin_call(&mut self, ctx: &egui::Context, method: &str, p: &Value) -> Result<Value, String> {
        let num = |key: &str| p.get(key).and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
        let text = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        let view = p.get("view").and_then(Value::as_u64).unwrap_or(0);
        let ok = Ok(Value::Null);
        match method {
            "status_message" => {
                self.message = Some(text("text"));
                ok
            }
            "dialog" => Ok(plugin_dialog(&text("kind"), &text("text"), &text("ok"))),
            "get_clipboard" => Ok(arboard::Clipboard::new().and_then(|mut c| c.get_text()).map_or(Value::Null, Value::from)),
            "set_clipboard" => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(text("text"));
                }
                ok
            }
            "run_command" => {
                if let Some(i) = self.docs.iter().position(|d| d.id == view) {
                    self.active = i;
                }
                let cmd = NamedCommand { name: text("name"), args: p.get("args").cloned().unwrap_or(Value::Null) };
                // Not plugin commands: the host runs its own before asking.
                match translate(&cmd, &self.snippets, &HashMap::new()) {
                    Some(command) => self.run_command_now(ctx, command),
                    None => self.message = Some(format!("Unknown command \"{}\"", cmd.name)),
                }
                ok
            }
            "score_selector" => {
                let scopes: Vec<Scope> = text("scope").split_whitespace().filter_map(|s| Scope::new(s).ok()).collect();
                let score = ScopeSelectors::from_str(&text("selector"))
                    .ok()
                    .and_then(|s| s.does_match(&scopes))
                    .map_or(0, |power| power.0.max(1.0) as u64);
                Ok(json!(score))
            }
            "file_dialog" => {
                let mut dialog = FileDialog::new();
                if let Some(dir) = p.get("directory").and_then(Value::as_str) {
                    dialog = dialog.set_directory(dir);
                }
                let multi = p.get("multi").and_then(Value::as_bool).unwrap_or(false);
                Ok(match text("kind").as_str() {
                    "save" => json!(dialog.set_file_name(text("name")).save_file()),
                    "folder" if multi => json!(dialog.pick_folders().unwrap_or_default()),
                    "folder" => json!(dialog.pick_folder().into_iter().collect::<Vec<_>>()),
                    _ if multi => json!(dialog.pick_files().unwrap_or_default()),
                    _ => json!(dialog.pick_file().into_iter().collect::<Vec<_>>()),
                })
            }
            "active_view" => Ok(json!(self.docs[self.active].id)),
            "views" => Ok(json!(self.groups.iter().flat_map(|g| g.views.iter()).collect::<Vec<_>>())),
            "focus_view" => {
                if let Some(i) = self.docs.iter().position(|d| d.id == view) {
                    self.activate(i);
                }
                ok
            }
            "num_groups" => Ok(json!(self.groups.len())),
            "active_group" => Ok(json!(self.focus_group)),
            "focus_group" => {
                self.focus_group_now(ctx, num("group"));
                ok
            }
            "views_in_group" => Ok(json!(self.groups.get(num("group")).map(|g| g.views.clone()).unwrap_or_default())),
            "active_view_in_group" => Ok(json!(self.groups.get(num("group")).and_then(|g| g.active))),
            "get_view_index" => Ok(match self.group_of(view) {
                Some(g) => json!([g, self.groups[g].views.iter().position(|v| *v == view)]),
                None => json!([-1, -1]),
            }),
            "set_view_index" => {
                let g = num("group");
                if g < self.groups.len() && self.docs.iter().any(|d| d.id == view) {
                    for group in &mut self.groups {
                        group.remove(view);
                    }
                    let views = &mut self.groups[g].views;
                    views.insert(num("index").min(views.len()), view);
                    self.groups[g].active = Some(view);
                    self.sync_groups();
                }
                ok
            }
            "get_layout" => Ok(self.layout.to_json()),
            "set_layout" => match p.get("layout").and_then(Layout::from_json) {
                Some(layout) => {
                    self.set_layout(layout);
                    ok
                }
                None => Err("that layout doesn't fit together".into()),
            },
            "auto_complete_visible" => Ok(json!(self.completion.is_some())),
            "popup_visible" => Ok(json!(self.plugins.popup.as_ref().is_some_and(|p| p.view == view))),
            "show_popup" => {
                let doc = self.plugin_doc(view)?;
                let location = match p.get("location").and_then(Value::as_i64) {
                    Some(at) if at >= 0 => (at as usize).min(doc.buffer.len()),
                    _ => doc.buffer.primary().head,
                };
                let revision = doc.buffer.revision;
                let size = |key: &str, default: f32| p.get(key).and_then(Value::as_f64).map_or(default, |n| n as f32).max(40.0);
                self.hide_plugin_popup();
                self.plugins.popup = Some(PluginPopup {
                    view,
                    content: minihtml::parse(&text("content")),
                    location,
                    max_width: size("max_width", 320.0),
                    max_height: size("max_height", 240.0),
                    navigate: p.get("navigate").and_then(Value::as_u64),
                    hide: p.get("hide").and_then(Value::as_u64),
                    revision,
                });
                ok
            }
            "update_popup" => {
                if let Some(popup) = self.plugins.popup.as_mut().filter(|p| p.view == view) {
                    popup.content = minihtml::parse(&text("content"));
                }
                ok
            }
            "hide_popup" => {
                if self.plugins.popup.as_ref().is_some_and(|p| p.view == view) {
                    self.hide_plugin_popup();
                }
                ok
            }
            "new_file" => {
                self.new_doc();
                self.sync_groups();
                let syntax = text("syntax");
                if let Some(name) = syntax_name(&syntax).filter(|n| self.syntax_names.contains(n)) {
                    self.doc().set_syntax(name);
                }
                Ok(json!(self.docs[self.active].id))
            }
            "open_file" => {
                let mut path = text("path");
                let mut line = None;
                if p.get("encoded").and_then(Value::as_bool) == Some(true) {
                    // "path:line" or "path:line:column".
                    let mut parts: Vec<&str> = path.rsplitn(3, ':').collect();
                    parts.reverse();
                    let numbers: Vec<usize> = parts[1..].iter().map_while(|n| n.parse().ok()).collect();
                    if !numbers.is_empty() {
                        line = Some(numbers[0]);
                        path = parts[..parts.len() - numbers.len()].join(":");
                    }
                }
                self.open_path(Path::new(&path));
                self.sync_groups();
                if let Some(line) = line {
                    self.goto_line(line);
                }
                Ok(json!(self.docs[self.active].id))
            }
            "find_open_file" => {
                let path = PathBuf::from(text("path"));
                Ok(json!(self.docs.iter().find(|d| d.path.as_deref() == Some(path.as_path())).map(|d| d.id)))
            }
            "quick_panel" => {
                let items: Vec<(String, String)> = serde_json::from_value(p["items"].clone()).unwrap_or_default();
                let selected = p.get("selected").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
                let done = p.get("callback").and_then(Value::as_u64);
                self.index.offer = None;
                self.plugins.prompt = Some(Prompt { items, done, cancel: None, input: false });
                self.palette.show_prompt(Mode::Quick, &text("placeholder"), "", selected);
                (self.palette_candidates, self.palette_targets) = self.palette_rows(ctx, Mode::Quick).into_iter().unzip();
                ok
            }
            "input_panel" => {
                let done = p.get("done").and_then(Value::as_u64);
                let cancel = p.get("cancel").and_then(Value::as_u64);
                self.plugins.prompt = Some(Prompt { items: Vec::new(), done, cancel, input: true });
                self.palette.show_prompt(Mode::Input, &text("caption"), &text("text"), 0);
                (self.palette_candidates, self.palette_targets) = (Vec::new(), Vec::new());
                ok
            }
            "create_output_panel" => {
                let name = text("name");
                let id = match self.plugins.panels.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, doc)) => {
                        doc.buffer = Buffer::new("");
                        doc.id
                    }
                    None => {
                        let id = self.take_id();
                        self.plugins.panels.push((name, Document::new_untitled(id)));
                        id
                    }
                };
                Ok(json!(id))
            }
            "find_output_panel" => {
                let name = text("name");
                Ok(json!(self.plugins.panels.iter().find(|(n, _)| *n == name).map(|(_, d)| d.id)))
            }
            "destroy_output_panel" => {
                let name = text("name");
                self.plugins.panels.retain(|(n, _)| *n != name);
                if self.plugins.shown_panel.as_ref() == Some(&name) {
                    self.plugins.shown_panel = None;
                }
                ok
            }
            "active_panel" => Ok(match (&self.plugins.shown_panel, self.find.open) {
                (Some(name), _) => json!(format!("output.{name}")),
                (None, true) => json!("find"),
                (None, false) => Value::Null,
            }),
            "panels" => Ok(json!(self.plugins.panels.iter().map(|(n, _)| format!("output.{n}")).collect::<Vec<_>>())),
            "folders" => Ok(json!(self.workspace.as_ref().map(|ws| vec![ws.root.clone()]).unwrap_or_default())),
            "app_settings" => serde_json::to_value(&self.settings).map_err(|e| e.to_string()),
            "set_sidebar" => {
                let visible = p.get("visible").and_then(Value::as_bool).unwrap_or(true);
                if self.windows.is_empty() {
                    self.settings.show_sidebar = visible;
                    self.save_settings();
                } else {
                    // A plugin's window: for that window only.
                    self.sidebar_override = Some(visible);
                }
                ok
            }
            "sidebar_visible" => Ok(json!(self.sidebar_override.unwrap_or(self.settings.show_sidebar))),
            "list_syntaxes" => {
                let names = self.syntax_names.clone();
                Ok(json!(names.iter().map(|n| self.syntax_json(n)).collect::<Vec<_>>()))
            }
            "syntax_for_file" => {
                let name = self.highlighter.detect_syntax(Some(Path::new(&text("path"))), &text("first_line"));
                Ok(self.syntax_json(&name))
            }
            "begin_edit" => {
                self.plugin_doc(view)?.buffer.begin_undo_group();
                ok
            }
            "end_edit" => {
                self.plugin_doc(view)?.buffer.end_undo_group();
                ok
            }
            _ => self.plugin_view_call(ctx, method, view, p, &num, &text),
        }
    }

    /// The API calls about one view.
    fn plugin_view_call(
        &mut self,
        ctx: &egui::Context,
        method: &str,
        view: u64,
        p: &Value,
        num: &dyn Fn(&str) -> usize,
        text: &dyn Fn(&str) -> String,
    ) -> Result<Value, String> {
        let ok = Ok(Value::Null);
        match method {
            "view_valid" => return Ok(json!(self.plugin_doc(view).is_ok())),
            "view_window" => return Ok(json!(if self.docs.iter().any(|d| d.id == view) { 1 } else { 0 })),
            "view_element" => {
                return Ok(json!(self.plugins.panels.iter().any(|(_, d)| d.id == view).then_some("output:output")));
            }
            "view_name" => return Ok(json!(self.plugins.view_names.get(&view))),
            // Tabs showing the same text share a buffer.
            "buffer_id" => return Ok(json!(self.docs.iter().find(|d| d.id == view).map_or(view, |d| d.family))),
            "set_view_name" => {
                self.plugins.view_names.insert(view, text("name"));
                // A tab without a file shows the name.
                if let Some(doc) = self.docs.iter_mut().find(|d| d.id == view) {
                    doc.name = Some(text("name")).filter(|n| !n.is_empty());
                }
                return ok;
            }
            "is_read_only" => return Ok(json!(self.plugin_doc(view)?.read_only)),
            "set_read_only" => {
                self.plugin_doc(view)?.read_only = p.get("value").and_then(Value::as_bool) == Some(true);
                return ok;
            }
            "is_scratch" => return Ok(json!(self.plugin_doc(view)?.scratch)),
            "set_scratch" => {
                self.plugin_doc(view)?.scratch = p.get("value").and_then(Value::as_bool) == Some(true);
                return ok;
            }
            "close_view" => {
                let Some(i) = self.docs.iter().position(|d| d.id == view) else {
                    return Ok(json!(false));
                };
                self.close(ctx, i);
                return Ok(json!(!self.docs.iter().any(|d| d.id == view)));
            }
            "view_setting" => {
                let key = text("key");
                if let Some(value) = self.plugins.view_settings.get(&view).and_then(|s| s.get(&key)) {
                    return Ok(value.clone());
                }
                if key == "syntax" {
                    let name = self.plugin_doc(view)?.syntax().to_owned();
                    return Ok(json!(syntax_path(&name)));
                }
                let settings = serde_json::to_value(&self.settings).map_err(|e| e.to_string())?;
                return Ok(settings.get(&key).cloned().unwrap_or(Value::Null));
            }
            "set_view_setting" => {
                // The syntax setting is the view's language itself.
                if text("key") == "syntax" {
                    let name = p.get("value").and_then(Value::as_str).and_then(syntax_name).filter(|n| self.syntax_names.contains(n));
                    if let Some(name) = name {
                        self.plugin_doc(view)?.set_syntax(name);
                    }
                    return ok;
                }
                let settings = self.plugins.view_settings.entry(view).or_default();
                match p.get("value").cloned().unwrap_or(Value::Null) {
                    Value::Null => settings.remove(&text("key")),
                    value => settings.insert(text("key"), value),
                };
                return ok;
            }
            "set_status" => {
                let statuses = self.plugins.status.entry(view).or_default();
                match p.get("value").and_then(Value::as_str) {
                    Some(value) => statuses.insert(text("key"), value.to_owned()),
                    None => statuses.remove(&text("key")),
                };
                return ok;
            }
            "get_status" => return Ok(json!(self.plugins.status.get(&view).and_then(|s| s.get(&text("key"))))),
            "add_regions" => {
                let regions: Vec<(usize, usize)> = serde_json::from_value(p["regions"].clone()).unwrap_or_default();
                let key = text("key");
                let empty = regions.is_empty();
                self.plugin_doc(view)?.buffer.set_marks(&region_marks(&key), regions);
                let styles = self.plugins.regions.entry(view).or_default();
                if empty {
                    styles.remove(&key);
                } else {
                    let flags = p.get("flags").and_then(Value::as_u64).unwrap_or(0) as u32;
                    styles.insert(key, RegionStyle { scope: text("scope"), flags });
                }
                return ok;
            }
            "get_regions" => return Ok(json!(self.plugin_doc(view)?.buffer.marks(&region_marks(&text("key"))))),
            "add_phantom" => {
                let id = self.plugins.next_phantom + 1;
                self.plugins.next_phantom = id;
                let (a, b) = (num("a"), num("b"));
                self.plugin_doc(view)?.buffer.set_marks(&phantom_marks(id), vec![(a, b)]);
                let phantom = PluginPhantom {
                    id,
                    key: text("key"),
                    content: minihtml::parse(&text("content")),
                    layout: PhantomLayout::from_number(p.get("layout").and_then(Value::as_u64).unwrap_or(0)),
                    navigate: p.get("navigate").and_then(Value::as_u64),
                };
                self.plugins.phantoms.entry(view).or_default().push(phantom);
                return Ok(json!(id));
            }
            "erase_phantoms" | "erase_phantom_by_id" => {
                let (key, id) = (text("key"), p.get("id").and_then(Value::as_u64));
                let phantoms = self.plugins.phantoms.entry(view).or_default();
                let (gone, kept): (Vec<PluginPhantom>, Vec<PluginPhantom>) =
                    std::mem::take(phantoms).into_iter().partition(|ph| if method == "erase_phantoms" { ph.key == key } else { Some(ph.id) == id });
                *phantoms = kept;
                let doc = self.plugin_doc(view)?;
                for ph in gone {
                    doc.buffer.set_marks(&phantom_marks(ph.id), Vec::new());
                }
                return ok;
            }
            "query_phantoms" => {
                let ids: Vec<u64> = serde_json::from_value(p["ids"].clone()).unwrap_or_default();
                let b = &self.plugin_doc(view)?.buffer;
                return Ok(json!(ids.iter().map(|id| b.marks(&phantom_marks(*id)).first().copied()).collect::<Vec<_>>()));
            }
            "set_syntax" => {
                let name = syntax_name(&text("path")).filter(|n| self.syntax_names.contains(n));
                if let Some(name) = name {
                    self.plugin_doc(view)?.set_syntax(name);
                }
                return ok;
            }
            "syntax" => {
                let name = self.plugin_doc(view)?.syntax().to_owned();
                return Ok(self.syntax_json(&name));
            }
            "scope_name" => {
                let doc = self.docs.iter().chain(self.plugins.panels.iter().map(|(_, d)| d)).find(|d| d.id == view);
                let doc = doc.ok_or_else(|| format!("view {view} is closed"))?;
                let scopes = editor_view::scopes_at(doc, &self.highlighter, num("pt").min(doc.buffer.len()));
                let names: Vec<String> = scopes.iter().map(|s| s.build_string()).collect();
                return Ok(json!(names.join(" ") + " "));
            }
            "show" => {
                let center = p.get("center").and_then(Value::as_bool) == Some(true);
                let doc = self.plugin_doc(view)?;
                let at = num("a").min(doc.buffer.len());
                doc.view.reveal_pos(at, center);
                return ok;
            }
            "viewport_position" => {
                let (offset, _) = self.plugin_doc(view)?.view.viewport();
                return Ok(json!([offset.x, offset.y]));
            }
            "set_viewport_position" => {
                let xy = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                self.plugin_doc(view)?.view.scroll_to(vec2(xy("x"), xy("y")));
                return ok;
            }
            "viewport_extent" => {
                let (_, size) = self.plugin_doc(view)?.view.viewport();
                return Ok(json!([size.x, size.y]));
            }
            "line_height" | "em_width" | "layout_extent" => {
                let settings = self.settings_for(view);
                let doc = self.plugin_doc(view)?;
                let m = editor_view::Metrics::new(ctx, &settings);
                return Ok(match method {
                    "line_height" => json!(m.row),
                    "em_width" => json!(m.space),
                    _ => {
                        let (_, size) = doc.view.viewport();
                        let (_, bottom) = editor_view::layout_of_char(ctx, doc, &settings, doc.buffer.len());
                        json!([size.x, bottom + m.row])
                    }
                });
            }
            "text_to_layout" | "text_to_window" => {
                let settings = self.settings_for(view);
                let doc = self.plugin_doc(view)?;
                let pt = num("pt");
                if method == "text_to_window" {
                    let rect = editor_view::char_rect(ctx, doc, &settings, pt).unwrap_or(egui::Rect::NOTHING);
                    return Ok(json!([rect.min.x, rect.min.y]));
                }
                let (x, y) = editor_view::layout_of_char(ctx, doc, &settings, pt);
                return Ok(json!([x, y]));
            }
            "layout_to_text" | "window_to_text" => {
                let settings = self.settings_for(view);
                let doc = self.plugin_doc(view)?;
                let xy = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let (mut x, mut y) = (xy("x"), xy("y"));
                if method == "window_to_text"
                    && let Some(g) = doc.view.geometry
                {
                    x -= g.origin.x + g.text_left;
                    y -= g.origin.y;
                }
                return Ok(json!(editor_view::char_at_layout(ctx, doc, &settings, x, y)));
            }
            "visible_region" => return Ok(json!(editor_view::visible_range(self.plugin_doc(view)?))),
            "file_name" => return Ok(json!(self.plugin_doc(view)?.path)),
            "is_dirty" => return Ok(json!(self.plugin_doc(view)?.dirty())),
            "line_endings" => return Ok(json!(if self.plugin_doc(view)?.crlf() { "Windows" } else { "Unix" })),
            _ => {}
        }

        let b = &mut self.plugin_doc(view)?.buffer;
        let len = b.len();
        let clamp = |n: usize| n.min(len);
        let (a, z) = (clamp(num("a")), clamp(num("b")));
        let (a, z) = (a.min(z), a.max(z));
        let line_range = |b: &Buffer, line: usize| (b.line_start(line), b.line_end(line));
        Ok(match method {
            "size" => json!(len),
            "substr" => json!(b.slice(a, z)),
            "change_count" => json!(b.revision),
            "insert" => {
                let pt = clamp(num("pt"));
                b.apply_edits(vec![Edit { start: pt, end: pt, text: text("text") }]);
                Value::Null
            }
            "replace" => {
                b.apply_edits(vec![Edit { start: a, end: z, text: text("text") }]);
                Value::Null
            }
            "sel" => json!(b.selections().iter().map(|s| [s.anchor, s.head]).collect::<Vec<_>>()),
            "set_sel" => {
                let regions: Vec<(usize, usize)> = serde_json::from_value(p["regions"].clone()).unwrap_or_default();
                let sels: Vec<Selection> = regions.iter().map(|&(a, h)| Selection::new(clamp(a), clamp(h))).collect();
                // Plugins may clear every selection; here there is always a caret.
                if !sels.is_empty() {
                    let primary = sels.len() - 1;
                    b.set_selections(sels, primary);
                }
                Value::Null
            }
            "line" => json!([b.line_start(b.line_of(a)), b.line_end(b.line_of(z))]),
            "full_line" => {
                let last = b.line_of(z);
                let end = if last + 1 < b.line_count() { b.line_start(last + 1) } else { b.len() };
                json!([b.line_start(b.line_of(a)), end])
            }
            "lines" => json!((b.line_of(a)..=b.line_of(z)).map(|l| line_range(b, l)).collect::<Vec<_>>()),
            "word" => json!([b.word_at(a).0, b.word_at(z).1]),
            "extract_scope" => {
                let (start, end) = b.word_at(clamp(num("pt")));
                json!([start, end])
            }
            "classify" => json!(0),
            "rowcol" => {
                let pt = clamp(num("pt"));
                let line = b.line_of(pt);
                json!([line, pt - b.line_start(line)])
            }
            // A column may be negative (`text_point(row + 1,
            // -1)` is the end of `row`), and a row past the last is the end.
            "text_point" => {
                let row = num("row");
                let col = p.get("col").and_then(Value::as_i64).unwrap_or(0);
                if row >= b.line_count() {
                    json!(len)
                } else if p.get("clamp").and_then(Value::as_bool) == Some(true) {
                    json!((b.line_start(row) as i64 + col).clamp(b.line_start(row) as i64, b.line_end(row) as i64))
                } else {
                    json!((b.line_start(row) as i64 + col).clamp(0, len as i64))
                }
            }
            "find" | "find_all" => {
                let flags = p.get("flags").and_then(Value::as_u64).unwrap_or(0);
                let start = if method == "find" { clamp(num("start")) } else { 0 };
                let found = find_in(b, &text("pattern"), flags, start, method == "find_all")?;
                if method == "find" { json!(found.first()) } else { json!(found) }
            }
            _ => return Err(format!("{method} is not supported")),
        })
    }

    fn syntax_json(&self, name: &str) -> Value {
        let scope = self.highlighter.base_scope(name).map(|s| s.build_string()).unwrap_or_default();
        json!({ "path": syntax_path(name), "name": name, "scope": scope })
    }

    /// Runs a translated command right away, as a plugin asked.
    fn run_command_now(&mut self, ctx: &egui::Context, command: PackageCommand) {
        let index = self.add_package_command(command);
        self.apply(ctx, super::Action::PackageCommand(index));
    }
}

/// Where a language's definition would be, as plugins name it. Bundled
/// languages have no file, so this is only a name to give back later.
fn syntax_path(name: &str) -> String {
    format!("Packages/{name}/{name}.sublime-syntax")
}

/// The language a `Packages/…/Name.sublime-syntax` path (or plain name) means.
fn syntax_name(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let name = file.strip_suffix(".sublime-syntax").or_else(|| file.strip_suffix(".tmLanguage")).unwrap_or(file);
    (!name.is_empty()).then(|| name.to_owned())
}

/// `message_dialog`, `error_message`, `ok_cancel_dialog` and `yes_no_cancel_dialog`.
fn plugin_dialog(kind: &str, text: &str, ok_title: &str) -> Value {
    let dialog = MessageDialog::new().set_title("TextEditor").set_description(text);
    match kind {
        "error" => {
            dialog.set_level(MessageLevel::Error).set_buttons(MessageButtons::Ok).show();
            Value::Null
        }
        "ok_cancel" => {
            let ok = if ok_title.is_empty() { "OK" } else { ok_title };
            let result = dialog.set_buttons(MessageButtons::OkCancelCustom(ok.into(), "Cancel".into())).show();
            json!(result == MessageDialogResult::Custom(ok.into()) || result == MessageDialogResult::Ok)
        }
        "yes_no_cancel" => json!(match dialog.set_buttons(MessageButtons::YesNoCancel).show() {
            MessageDialogResult::Yes => 1,
            MessageDialogResult::No => 2,
            _ => 0,
        }),
        _ => {
            dialog.set_buttons(MessageButtons::Ok).show();
            Value::Null
        }
    }
}

/// `View.find` and `find_all`: a regex (or with flag 1, literal text; flag
/// 2 ignores case) from char `start`, as char ranges.
fn find_in(b: &Buffer, pattern: &str, flags: u64, start: usize, all: bool) -> Result<Vec<(usize, usize)>, String> {
    let mut pattern = if flags & 1 != 0 { crate::search::escape_regex(pattern) } else { pattern.to_owned() };
    if flags & 2 != 0 {
        pattern = format!("(?i){pattern}");
    }
    if let Some(error) = Regex::try_compile(&pattern) {
        return Err(format!("bad pattern: {error}"));
    }
    let regex = Regex::new(pattern);
    let text = b.text();
    let rope = b.rope();
    let mut pos = rope.char_to_byte(start);
    let mut found = Vec::new();
    let mut region = Region::new();
    while pos <= text.len() && regex.search(&text, pos, text.len(), Some(&mut region)) {
        let Some((s, e)) = region.pos(0) else {
            break;
        };
        found.push((rope.byte_to_char(s), rope.byte_to_char(e)));
        if !all {
            break;
        }
        pos = if e > s { e } else { e + text[e..].chars().next().map_or(1, char::len_utf8) };
    }
    Ok(found)
}

