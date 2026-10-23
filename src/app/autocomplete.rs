//! The autocomplete list (see `crate::completion`): it opens while a word
//! is typed, or with Edit > Show Completions, and sits under the caret.

use eframe::egui::{self, AtomExt, Button, Color32, Key, Modifiers, Order, TextFormat, text::LayoutJob};

use super::{Action, EditorApp};
use crate::buffer::{EditKind, Selection};
use crate::completion::{self, Completion, Insert, Popup};
use crate::editor_view;
use crate::keymap::NamedCommand;
use crate::macros::Step;

/// Typing opens the list once a word is this long.
const MIN_TYPED_CHARS: usize = 2;
/// The flags plugins' completions come with.
const INHIBIT_WORD_COMPLETIONS: u64 = 8;
const INHIBIT_EXPLICIT_COMPLETIONS: u64 = 16;
/// Words from other tabs: only this many from each, so many big files
/// don't slow typing down.
const OTHER_TAB_WORDS: usize = 2_000;
const ROWS_SHOWN: f32 = 10.0;
const MATCH_COLOR: Color32 = Color32::from_rgb(250, 200, 100);

impl EditorApp {
    /// Up, Down, Enter, Tab and Esc work the open list, before key bindings
    /// or the editor see them.
    pub(super) fn completion_keys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let active = self.docs[self.active].id;
        if self.completion.as_ref().is_none_or(|c| c.doc != active) || !self.editor_focused(ctx) {
            return;
        }
        let (up, down, accept, close) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter) | i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if let Some(popup) = &mut self.completion {
            if up {
                popup.step(-1);
            }
            if down {
                popup.step(1);
            }
        }
        if accept {
            actions.push(Action::CommitCompletion);
        }
        if close {
            self.completion = None;
        }
    }

    /// After the editor took this frame's input: narrows the list to what's
    /// typed now, closes it when the caret leaves the word, and opens it
    /// when a word is being typed.
    pub(super) fn update_completion(&mut self, ctx: &egui::Context, typed: bool) {
        let doc = &self.docs[self.active];
        let b = &doc.buffer;
        let head = b.primary().head;
        let (start, prefix) = completion::prefix_at(b, head);
        let now = (b.revision, head);
        if let Some(popup) = &mut self.completion {
            // Moving the caret without typing closes the list.
            let moved = popup.seen.0 == now.0 && popup.seen.1 != now.1;
            // Deleting the whole word closes the list, unless it was opened
            // before anything was typed.
            let same_word = popup.doc == doc.id && popup.start == start && (!prefix.is_empty() || popup.prefix.is_empty());
            let keep = same_word && !moved && b.selections().iter().all(Selection::is_empty);
            if keep && (popup.prefix == prefix || popup.filter(&prefix)) {
                popup.seen = now;
            } else {
                self.completion = None;
            }
        }
        let auto = self.settings_for(doc.id).auto_complete && !doc.read_only;
        let typing_word = typed && prefix.chars().count() >= MIN_TYPED_CHARS;
        if self.completion.is_none() && auto && typing_word {
            self.show_completions(ctx, false);
        }
    }

    /// Opens the list for the word before the caret. `explicit` is for the
    /// command: then it opens even before anything is typed.
    pub(super) fn show_completions(&mut self, ctx: &egui::Context, explicit: bool) {
        let doc = &self.docs[self.active];
        let (doc_id, head) = (doc.id, doc.buffer.primary().head);
        let (start, prefix) = completion::prefix_at(&doc.buffer, head);
        if !explicit && prefix.chars().count() < MIN_TYPED_CHARS {
            self.completion = None;
            return;
        }
        let locations: Vec<usize> = doc.buffer.selections().iter().map(|s| s.head).collect();
        // Plugins' completions come first.
        let (mut items, flags) = self.plugin_completions(ctx, doc_id, &prefix, &locations);
        let Some(index) = self.docs.iter().position(|d| d.id == doc_id) else {
            return;
        };
        let doc = &self.docs[index];
        if flags & INHIBIT_EXPLICIT_COMPLETIONS == 0 {
            let scopes = editor_view::scopes_at(doc, &self.highlighter, start);
            for s in self.snippets.iter().filter(|s| !s.trigger.is_empty() && s.applies_to(&scopes)) {
                let title = s.title();
                let annotation = if title == s.trigger { "snippet".to_owned() } else { title };
                items.push(Completion { trigger: s.trigger.clone(), annotation, insert: Insert::Snippet(s.content.clone()) });
            }
        }
        if flags & INHIBIT_WORD_COMPLETIONS == 0 {
            items.extend(completion::buffer_words(&doc.buffer, head, (start, head)).iter().map(|w| Completion::word(w)));
            for other in self.docs.iter().filter(|d| d.id != doc_id) {
                let words = completion::buffer_words(&other.buffer, 0, (usize::MAX, usize::MAX));
                items.extend(words.iter().take(OTHER_TAB_WORDS).map(|w| Completion::word(w)));
            }
        }
        // The first of each trigger wins: a plugin's over a plain word.
        let mut seen = std::collections::HashSet::new();
        items.retain(|c| seen.insert(c.trigger.clone()));
        self.completion = Popup::new(items, doc_id, start, &prefix).map(|mut p| {
            p.seen = (doc.buffer.revision, head);
            p
        });
        if explicit && self.completion.is_none() {
            self.message = Some("No completions".into());
        }
    }

    /// Completions a plugin gave late: they join the open list, or open it
    /// if the word is still being typed.
    pub(super) fn add_late_completions(&mut self, view: u64, items: Vec<Completion>) {
        let doc = &self.docs[self.active];
        if doc.id != view || items.is_empty() {
            return;
        }
        let head = doc.buffer.primary().head;
        let (start, prefix) = completion::prefix_at(&doc.buffer, head);
        let seen = (doc.buffer.revision, head);
        match &mut self.completion {
            Some(popup) if popup.doc == view && popup.start == start => {
                // Plugins' first, and each trigger once.
                let mut all = items;
                all.append(&mut popup.items);
                let mut triggers = std::collections::HashSet::new();
                all.retain(|c| triggers.insert(c.trigger.clone()));
                popup.items = all;
                popup.filter(&prefix);
            }
            None if !prefix.is_empty() => {
                self.completion = Popup::new(items, view, start, &prefix).map(|mut p| {
                    p.seen = seen;
                    p
                });
            }
            _ => {}
        }
    }

    /// Puts the chosen completion in place of the word typed so far, at
    /// every caret that has the same word before it.
    pub(super) fn accept_completion(&mut self, ctx: &egui::Context) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(choice) = popup.selected().cloned() else {
            return;
        };
        let Some(index) = self.docs.iter().position(|d| d.id == popup.doc) else {
            return;
        };
        self.hold_text(index);
        let settings = self.settings_for(popup.doc);
        let n = popup.prefix.chars().count();
        let doc = &mut self.docs[index];
        let b = &doc.buffer;
        let sels: Vec<Selection> = b
            .selections()
            .iter()
            .map(|s| {
                let start = s.head.saturating_sub(n);
                if s.is_empty() && b.slice(start, s.head) == popup.prefix { Selection::new(start, s.head) } else { *s }
            })
            .collect();
        let primary = b.primary_index();
        doc.buffer.set_selections(sels, primary);
        if let Some(steps) = &mut self.recording {
            steps.extend(completion_steps(n, &choice.insert));
        }
        match choice.insert {
            Insert::Text(text) => doc.buffer.insert(&text, EditKind::Other),
            Insert::Snippet(template) => editor_view::insert_snippet(doc, &template, &settings, 0),
            Insert::Command(name, args) => {
                doc.buffer.insert("", EditKind::Other);
                if !self.run_named_command(ctx, &NamedCommand { name: name.clone(), args }) {
                    self.message = Some(format!("Unknown command \"{name}\""));
                }
            }
        }
        if let Some(doc) = self.docs.get_mut(index) {
            doc.view.reveal(false);
        }
    }

    /// Draws the open list under the start of the word, and takes clicks on it.
    pub(super) fn completion_ui(&mut self, ctx: &egui::Context) {
        let Some(doc_id) = self.completion.as_ref().map(|p| p.doc) else {
            return;
        };
        let settings = self.settings_for(doc_id);
        let Some(popup) = &mut self.completion else {
            return;
        };
        let Some(doc) = self.docs.iter().find(|d| d.id == doc_id) else {
            return;
        };
        let Some(at) = editor_view::char_rect(ctx, doc, &settings, popup.start) else {
            return;
        };
        let font = egui::FontId::monospace(settings.font_size);
        let row_height = ctx.fonts_mut(|f| f.row_height(&font)) + 4.0;
        let mut clicked = None;
        egui::Area::new(egui::Id::unique("autocomplete"))
            .order(Order::Foreground)
            .fixed_pos(at.left_bottom())
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(240.0);
                    egui::ScrollArea::vertical().max_height(row_height * ROWS_SHOWN).show(ui, |ui| {
                        for (row, (i, positions)) in popup.shown.iter().enumerate() {
                            let c = &popup.items[*i];
                            let mut job = LayoutJob::default();
                            for (k, ch) in c.trigger.chars().enumerate() {
                                let color = if positions.contains(&k) { MATCH_COLOR } else { Color32::PLACEHOLDER };
                                job.append(ch.encode_utf8(&mut [0; 4]), 0.0, TextFormat::simple(font.clone(), color));
                            }
                            let button = Button::selectable(row == popup.selected, job)
                                .right_text(egui::RichText::new(&c.annotation).weak().atom_shrink(true))
                                .truncate()
                                .min_size(egui::vec2(ui.available_width(), 0.0));
                            let response = ui.add(button);
                            if response.clicked() {
                                clicked = Some(row);
                            }
                            if row == popup.selected && popup.reveal {
                                response.scroll_to_me(None);
                            }
                        }
                    });
                });
            });
        popup.reveal = false;
        if let Some(row) = clicked {
            popup.selected = row;
            self.accept_completion(ctx);
            self.focus_editor(ctx);
        }
    }
}

/// What accepting a completion does, as macro steps: delete the `typed`
/// chars of the word, then put the completion in.
fn completion_steps(typed: usize, insert: &Insert) -> Vec<Step> {
    let command = |name: &str, args: serde_json::Value| Step::Command(NamedCommand { name: name.to_owned(), args });
    let mut steps = vec![command("left_delete", serde_json::Value::Null); typed];
    steps.push(match insert {
        Insert::Text(text) => command("insert", serde_json::json!({ "characters": text })),
        Insert::Snippet(template) => command("insert_snippet", serde_json::json!({ "contents": template })),
        Insert::Command(name, args) => command(name, args.clone()),
    });
    steps
}
