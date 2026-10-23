//! Find in Files (⇧⌘F): the panel, searches running in the background,
//! the Find Results tab, and Replace (see `crate::search`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Align, Key};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

use super::{Action, EditorApp};
use crate::buffer::{Buffer, Edit, Selection};
use crate::editor_view::ViewState;
use crate::search::{self, FileMatches, Query, Scope, Update};

/// The marks that highlight matches in Find Results.
pub(super) const RESULTS_MARKS: &str = "find:results";

#[derive(Default)]
pub(super) struct FindFiles {
    pub(super) open: bool,
    pub(super) query: String,
    pub(super) scope: String,
    pub(super) replace: String,
    pub(super) regex: bool,
    pub(super) case_sensitive: bool,
    pub(super) whole_word: bool,
    request_focus: bool,
    status: String,
    job: Option<Job>,
    pub(super) results: Option<Results>,
    /// Replace as soon as the search under way finishes.
    replace_when_done: bool,
}

struct Job {
    receive: Receiver<Update>,
    cancel: Arc<AtomicBool>,
    query: Query,
    progress: (usize, usize),
}

/// The last search's results, shown in the Find Results tab.
pub(super) struct Results {
    pub(super) doc: u64,
    pub(super) query: Query,
    files: Vec<FileMatches>,
    /// Each result line of the tab (from 0), with its file, line and column.
    targets: Vec<(usize, PathBuf, usize, usize)>,
    /// The result F4 went to last.
    at: Option<usize>,
}

impl FindFiles {
    fn query(&self) -> Query {
        Query { pattern: self.query.clone(), regex: self.regex, case_sensitive: self.case_sensitive, whole_word: self.whole_word }
    }

    pub(super) fn has_results(&self) -> bool {
        self.results.as_ref().is_some_and(|r| !r.targets.is_empty())
    }

    /// Whether `doc` is the Find Results tab.
    pub(super) fn is_results(&self, doc: u64) -> bool {
        self.results.as_ref().is_some_and(|r| r.doc == doc)
    }

    /// "Searching 200 of 1,500 files" while a search runs.
    pub(super) fn progress(&self) -> Option<String> {
        let job = self.job.as_ref()?;
        Some(match job.progress {
            (_, 0) => "Searching…".to_owned(),
            (done, total) => format!("Searching {done} of {total} files"),
        })
    }
}

impl EditorApp {
    /// Opens the panel, starting from the selected text.
    pub(super) fn open_find_in_files(&mut self) {
        let f = &mut self.find_files;
        f.open = true;
        f.request_focus = true;
        self.find.open = false;
        let b = &self.docs[self.active].buffer;
        let p = b.primary();
        if !p.is_empty() && b.line_of(p.start()) == b.line_of(p.end()) {
            self.find_files.query = b.slice(p.start(), p.end());
        }
    }

    pub(super) fn find_in_files_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let f = &mut self.find_files;
        let mut enter = false;
        let mut close = false;
        egui::Grid::new("find-in-files").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Find:");
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut f.query).desired_width(320.0).hint_text("text to find"));
                if f.request_focus {
                    field.request_focus();
                    f.request_focus = false;
                }
                enter |= field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                close |= field.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
                ui.checkbox(&mut f.regex, "Regex");
                ui.checkbox(&mut f.case_sensitive, "Match case");
                ui.checkbox(&mut f.whole_word, "Whole word");
            });
            ui.end_row();
            ui.label("Where:");
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut f.scope)
                        .desired_width(320.0)
                        .hint_text("<open folders>, <open files>, *.rs, -target"),
                );
                enter |= field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                close |= field.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
                if ui.button("Folder…").on_hover_text("Search in another folder").clicked()
                    && let Some(folder) = FileDialog::new().pick_folder()
                {
                    let folder = folder.display().to_string();
                    f.scope = if f.scope.trim().is_empty() { folder } else { format!("{}, {folder}", f.scope.trim()) };
                }
            });
            ui.end_row();
            ui.label("Replace:");
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut f.replace).desired_width(320.0).hint_text("replacement"));
                close |= field.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
                if ui.button("Find").clicked() {
                    actions.push(Action::SearchFiles);
                }
                if ui.button("Replace").on_hover_text("Replace every match, leaving the files unsaved").clicked() {
                    actions.push(Action::ReplaceInFiles);
                }
                ui.label(&f.status);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    close |= ui.small_button("×").clicked();
                });
            });
            ui.end_row();
        });
        if enter {
            actions.push(Action::SearchFiles);
            f.request_focus = true;
        }
        if close {
            f.open = false;
            self.focus_editor(ui.ctx());
        }
    }

    /// Starts searching in the background, stopping any search under way.
    pub(super) fn search_files(&mut self, ctx: &egui::Context) {
        let query = self.find_files.query();
        if let Err(e) = query.compile() {
            self.find_files.status = e;
            return;
        }
        if let Some(job) = self.find_files.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let folder = self.workspace.as_ref().map(|ws| ws.root.clone());
        let scope = Scope::parse(&self.find_files.scope, folder.as_deref());
        let open_files: Vec<PathBuf> =
            if scope.open_files { self.docs.iter().filter_map(|d| d.path.clone()).collect() } else { Vec::new() };
        // Unsaved changes are searched, not what's on disk.
        let texts: HashMap<PathBuf, String> =
            self.docs.iter().filter(|d| d.dirty()).filter_map(|d| Some((d.path.clone()?, d.buffer.text()))).collect();
        let (send, receive) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, wake, q) = (cancel.clone(), ctx.clone(), query.clone());
        std::thread::spawn(move || {
            let mut files = scope.files();
            files.extend(open_files);
            files.sort();
            files.dedup();
            search::run(q, files, texts, stop, send, move || wake.request_repaint());
        });
        self.find_files.job = Some(Job { receive, cancel, query, progress: (0, 0) });
        self.find_files.status.clear();
    }

    /// Takes what a search running in the background found.
    pub(super) fn poll_find_in_files(&mut self, ctx: &egui::Context) {
        loop {
            let Some(job) = &mut self.find_files.job else {
                return;
            };
            match job.receive.try_recv() {
                Ok(Update::Progress(done, total)) => job.progress = (done, total),
                Ok(Update::Done(files, searched)) => {
                    let query = job.query.clone();
                    self.find_files.job = None;
                    self.show_results(ctx, query, files, searched);
                    if std::mem::take(&mut self.find_files.replace_when_done) {
                        self.confirm_replace_in_files(ctx);
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.find_files.job = None;
                    return;
                }
            }
        }
    }

    /// Puts the results in the Find Results tab, reusing it if it's open.
    fn show_results(&mut self, ctx: &egui::Context, query: Query, files: Vec<FileMatches>, searched: usize) {
        let report = search::report(&query, &files, searched);
        let existing = self.find_files.results.as_ref().and_then(|r| self.docs.iter().position(|d| d.id == r.doc));
        let index = match existing {
            Some(i) => i,
            None => {
                self.new_doc();
                self.sync_groups();
                self.active
            }
        };
        let doc = &mut self.docs[index];
        doc.name = Some("Find Results".into());
        doc.scratch = true;
        doc.buffer = Buffer::new(&report.text);
        doc.buffer.set_marks(RESULTS_MARKS, report.highlights);
        doc.view = ViewState::new(&doc.buffer);
        doc.view.reveal(false);
        let doc_id = doc.id;
        let mut targets: Vec<(usize, PathBuf, usize, usize)> =
            report.targets.into_iter().map(|(line, (path, l, col))| (line, path, l, col)).collect();
        targets.sort_by_key(|t| t.0);
        let count: usize = files.iter().map(|f| f.count).sum();
        self.find_files.status = format!("{count} match{} in {} file{}", if count == 1 { "" } else { "es" }, files.len(), if files.len() == 1 { "" } else { "s" });
        self.find_files.results = Some(Results { doc: doc_id, query, files, targets, at: None });
        self.activate(index);
        self.focus_editor(ctx);
    }

    /// A double click in Find Results opens the file at that result; on a
    /// file's name, the file itself.
    pub(super) fn open_result_at_caret(&mut self, ctx: &egui::Context, doc: u64) {
        let Some(i) = self.docs.iter().position(|d| d.id == doc) else {
            return;
        };
        let b = &self.docs[i].buffer;
        let line = b.line_of(b.primary().head);
        let Some(results) = &self.find_files.results else {
            return;
        };
        if let Some(at) = results.targets.iter().position(|t| t.0 == line) {
            self.go_to_result(ctx, at);
        } else if let Some(path) = b.line_text(line).strip_suffix(':').map(PathBuf::from).filter(|p| p.is_file()) {
            self.open_path(&path);
            self.focus_editor(ctx);
        }
    }

    /// F4 and ⇧F4: the next or previous result, wrapping around.
    pub(super) fn step_result(&mut self, ctx: &egui::Context, forward: bool) {
        let Some(results) = &self.find_files.results else {
            return;
        };
        let n = results.targets.len();
        if n == 0 {
            return;
        }
        let at = match (results.at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(at), true) => (at + 1) % n,
            (Some(at), false) => (at + n - 1) % n,
        };
        self.go_to_result(ctx, at);
    }

    /// Opens a result's file with the caret on the match, and moves the
    /// caret in Find Results to it too.
    fn go_to_result(&mut self, ctx: &egui::Context, at: usize) {
        let Some(results) = &mut self.find_files.results else {
            return;
        };
        results.at = Some(at);
        let (report_line, path, line, col) = results.targets[at].clone();
        let results_doc = results.doc;
        if let Some(doc) = self.docs.iter_mut().find(|d| d.id == results_doc) {
            let pos = doc.buffer.line_start(report_line);
            doc.buffer.set_single(Selection::caret(pos));
            doc.view.reveal(false);
        }
        self.open_path(&path);
        let b = &mut self.docs[self.active].buffer;
        let line = line.saturating_sub(1).min(b.line_count() - 1);
        let pos = (b.line_start(line) + col).min(b.line_end(line));
        b.set_single(Selection::caret(pos));
        self.docs[self.active].view.reveal(true);
        self.focus_editor(ctx);
    }

    /// Asks before replacing every match. Without results for what's in
    /// the panel now, searches first.
    pub(super) fn confirm_replace_in_files(&mut self, ctx: &egui::Context) {
        let query = self.find_files.query();
        let fresh = self.find_files.results.as_ref().is_some_and(|r| r.query == query);
        if !fresh {
            self.find_files.replace_when_done = true;
            self.search_files(ctx);
            return;
        }
        let Some(results) = &self.find_files.results else {
            return;
        };
        let count: usize = results.files.iter().map(|f| f.count).sum();
        if count == 0 {
            self.find_files.status = "Nothing to replace".into();
            return;
        }
        let answer = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Replace in Files")
            .set_description(format!(
                "Replace {count} match{} in {} file{} with \"{}\"?\n\nThe files open as tabs with the changes unsaved.",
                if count == 1 { "" } else { "es" },
                results.files.len(),
                if results.files.len() == 1 { "" } else { "s" },
                self.find_files.replace
            ))
            .set_buttons(MessageButtons::OkCancelCustom("Replace".into(), "Cancel".into()))
            .show();
        if answer == MessageDialogResult::Custom("Replace".into()) {
            self.replace_in_files_now(ctx);
        }
    }

    /// Replaces every match of the last search, in tabs (opening the files
    /// that aren't open), each file as one undo step. Nothing is saved.
    pub(super) fn replace_in_files_now(&mut self, ctx: &egui::Context) {
        let Some(results) = self.find_files.results.take() else {
            return;
        };
        let Ok(regex) = results.query.compile() else {
            return;
        };
        let replacement = self.find_files.replace.clone();
        let (mut replaced, mut changed) = (0, 0);
        for file in &results.files {
            let index = match self.docs.iter().position(|d| d.path.as_deref() == Some(file.path.as_path())) {
                Some(i) => i,
                None => {
                    self.open_path(&file.path);
                    self.sync_groups();
                    match self.docs.iter().position(|d| d.path.as_deref() == Some(file.path.as_path())) {
                        Some(i) => i,
                        None => continue,
                    }
                }
            };
            self.hold_text(index);
            let b = &mut self.docs[index].buffer;
            let text = b.text();
            let rope = b.rope();
            let edits: Vec<Edit> = search::find_matches(&regex, &text)
                .iter()
                .map(|m| Edit {
                    start: rope.byte_to_char(m.start),
                    end: rope.byte_to_char(m.end),
                    text: search::expand_replacement(&replacement, m, &text, results.query.regex),
                })
                .collect();
            if !edits.is_empty() {
                replaced += edits.len();
                changed += 1;
                b.apply_edits(edits);
            }
        }
        // The results no longer match the files; the tab stays as it was.
        self.find_files.status = format!("Replaced {replaced} in {changed} file{}", if changed == 1 { "" } else { "s" });
        self.message = Some(format!("{}: not saved yet (File > Save All)", self.find_files.status));
        self.focus_editor(ctx);
    }
}
