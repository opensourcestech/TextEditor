//! Recording, playing back and saving macros (see `crate::macros`).

use eframe::egui::{self, Event, ImeEvent};
use rfd::FileDialog;
use serde_json::{Value, json};

use super::{Action, EditorApp, PackageCommand, Standard};
use crate::editor_view::{self, ViewContext};
use crate::keymap::NamedCommand;
use crate::macros::{self, Step};
use crate::packages;

impl EditorApp {
    /// Tools > Record Macro: starts recording, or stops and keeps the macro
    /// for Playback Macro.
    pub(super) fn toggle_recording(&mut self) {
        match self.recording.take() {
            Some(steps) if steps.is_empty() => self.message = Some("Stopped recording: nothing was recorded".into()),
            Some(steps) => {
                self.message = Some(format!("Recorded a macro of {} step{}", steps.len(), if steps.len() == 1 { "" } else { "s" }));
                self.last_macro = steps;
            }
            None => {
                self.recording = Some(Vec::new());
                self.message = Some("Recording a macro; Tools > Stop Recording Macro to finish".into());
            }
        }
    }

    /// While recording: what the editor gets this frame, typing and keys.
    /// Called before the editor handles them, after key bindings took theirs.
    pub(super) fn record_input(&mut self, ctx: &egui::Context) {
        if !self.editor_focused(ctx) {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        let Some(steps) = &mut self.recording else {
            return;
        };
        for event in events {
            let step = match event {
                Event::Text(text) | Event::Ime(ImeEvent::Commit(text)) if !text.is_empty() => Step::Type(text),
                Event::Paste(text) => Step::Paste(text),
                Event::Copy => Step::Command(NamedCommand { name: "copy".into(), args: Value::Null }),
                Event::Cut => Step::Command(NamedCommand { name: "cut".into(), args: Value::Null }),
                Event::Key { key, pressed: true, modifiers, .. } if macros::is_editor_key(key, modifiers) => Step::Key(key, modifiers),
                _ => continue,
            };
            // Typing goes in as one step per run of text.
            match (steps.last_mut(), step) {
                (Some(Step::Type(before)), Step::Type(more)) => before.push_str(&more),
                (_, step) => steps.push(step),
            }
        }
    }

    /// While recording: a menu command, key binding or palette entry that
    /// changes the text or selections.
    pub(super) fn record_action(&mut self, action: Action) {
        let command = match action {
            Action::Edit(cmd) => Some(macros::editor_command(cmd)),
            Action::InsertSnippet(i) => {
                self.snippets.get(i).map(|s| NamedCommand { name: "insert_snippet".into(), args: json!({ "contents": s.content }) })
            }
            Action::PackageCommand(i) => self.package_commands.get(i).and_then(|c| self.package_step(c)),
            Action::RunMacro(i) => self.macro_files.get(i).map(|m| NamedCommand {
                name: "run_macro_file".into(),
                args: json!({ "file": format!("res://Packages/{}", m.path) }),
            }),
            _ => None,
        };
        if let (Some(command), Some(steps)) = (command, &mut self.recording) {
            steps.push(Step::Command(command));
        }
    }

    /// A package command as a macro step, for the ones that edit.
    fn package_step(&self, command: &PackageCommand) -> Option<NamedCommand> {
        let named = |name: &str, args: Value| Some(NamedCommand { name: name.to_owned(), args });
        match command {
            PackageCommand::Run(Action::Edit(cmd)) => Some(macros::editor_command(*cmd)),
            PackageCommand::Insert(text) => named("insert", json!({ "characters": text })),
            PackageCommand::Snippet(template) => named("insert_snippet", json!({ "contents": template })),
            PackageCommand::Move(motion, extend) => Some(macros::motion_command(*motion, *extend)),
            PackageCommand::Delete(motion) => macros::delete_command(*motion),
            PackageCommand::Macro(path) => named("run_macro_file", json!({ "file": format!("res://Packages/{path}") })),
            // A plugin's text command edits; window and app commands don't.
            PackageCommand::Plugin(name, args) => {
                let is_text = self.plugins.host.as_ref().is_some_and(|h| h.commands.get(name).is_some_and(|k| k == "text"));
                is_text.then(|| NamedCommand { name: name.clone(), args: args.clone() })
            }
            _ => None,
        }
    }

    /// Does a macro's steps in the active tab, as one undo step.
    pub(super) fn play_macro(&mut self, ctx: &egui::Context, steps: &[Step]) {
        if steps.is_empty() {
            self.message = Some("No macro to play back: record one with Tools > Record Macro".into());
            return;
        }
        // Commands the macro runs aren't recorded again.
        let recording = self.recording.take();
        self.sync_clones();
        let id = self.docs[self.active].id;
        self.doc().buffer.begin_undo_group();
        for step in steps {
            let settings = self.active_settings();
            match step {
                // A char at a time, as typed, so `(` gets its `)` again.
                Step::Type(text) => {
                    for c in text.chars() {
                        editor_view::type_text(&mut self.doc().buffer, c.encode_utf8(&mut [0; 4]), &settings);
                    }
                }
                Step::Paste(text) => editor_view::paste(self.doc(), text),
                Step::Key(key, mods) => {
                    let cx = ViewContext {
                        settings: &settings,
                        highlighter: &self.highlighter,
                        find: None,
                        snippets: &self.snippets,
                        highlights: &[],
                        phantoms: &[],
                    };
                    editor_view::press_key(ctx, &mut self.docs[self.active], *key, *mods, &cx);
                }
                Step::Command(c) => {
                    let standard = match c.name.as_str() {
                        "undo" | "soft_undo" => Some(Standard::Undo),
                        "redo" | "redo_or_repeat" | "soft_redo" => Some(Standard::Redo),
                        "copy" => Some(Standard::Copy),
                        "cut" => Some(Standard::Cut),
                        "paste" => Some(Standard::Paste),
                        "select_all" => Some(Standard::SelectAll),
                        _ => None,
                    };
                    match standard {
                        Some(op) => self.standard_now(ctx, op),
                        None if self.run_named_command(ctx, c) => {}
                        // Commands it doesn't know are skipped.
                        None => self.message = Some(format!("The macro's \"{}\" command isn't available", c.name)),
                    }
                }
            }
        }
        if let Some(doc) = self.docs.iter_mut().find(|d| d.id == id) {
            doc.buffer.end_undo_group();
            doc.view.reveal(false);
        }
        self.recording = recording;
    }

    /// Undo, Cut, Paste… done now on the active tab, rather than sent as
    /// input for next frame, so a macro's steps happen in order.
    fn standard_now(&mut self, ctx: &egui::Context, op: Standard) {
        let doc = self.doc();
        match op {
            Standard::Undo => {
                doc.buffer.undo();
            }
            Standard::Redo => {
                doc.buffer.redo();
            }
            Standard::SelectAll => doc.buffer.select_all(),
            Standard::Copy => editor_view::copy(ctx, doc, false),
            Standard::Cut => editor_view::copy(ctx, doc, true),
            Standard::Paste => {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    editor_view::paste(doc, &text);
                }
            }
        }
    }

    /// Tools > Save Macro: writes the last macro to `Packages/User`, where
    /// it shows up in Tools > Macros.
    pub(super) fn save_macro(&mut self, ctx: &egui::Context) {
        let Some(dirs) = self.packages.clone() else {
            return;
        };
        let user = dirs.packages.join(packages::USER);
        if let Err(e) = dirs.create() {
            self.message = Some(format!("Could not create {}: {e}", user.display()));
            return;
        }
        let dialog = FileDialog::new()
            .set_directory(&user)
            .set_file_name("Untitled.te-macro")
            .add_filter("Macro", &["te-macro", "sublime-macro"]);
        let Some(path) = dialog.save_file() else {
            return;
        };
        let path = if crate::packages::is_kind(&path.to_string_lossy(), "macro") { path } else { path.with_extension("te-macro") };
        match std::fs::write(&path, macros::to_json(&self.last_macro)) {
            Ok(()) => {
                self.reload_packages(ctx);
                self.message = Some(format!("Saved the macro to {}", path.display()));
            }
            Err(e) => self.message = Some(format!("Could not save {}: {e}", path.display())),
        }
    }
}
