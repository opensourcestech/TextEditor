use eframe::egui::{
    self, Align2, AtomExt, Button, Color32, Key, Modifiers, Order, TextFormat, TextStyle, text::LayoutJob,
};

use crate::ui::fuzzy::fuzzy_match;

/// Only the best matches are drawn; nobody scrolls past this many.
const MAX_ROWS: usize = 200;
const MATCH_COLOR: Color32 = Color32::from_rgb(250, 200, 100);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Goto Anything: open tabs and files in the folder; `name:42` jumps to a line.
    #[default]
    Files,
    Commands,
    /// A list a plugin asked to choose from (its quick panel).
    Quick,
    /// A line of text a plugin asked for (its input panel); no list.
    Input,
}

/// One row the palette can show.
pub struct Candidate {
    /// What the query is matched against, e.g. `src/app.rs` or `File: Save`.
    pub text: String,
    /// Character index where the bold title begins (the file name in a path).
    pub title_start: usize,
    /// Gray text on the right: the folder of a file, or a command's shortcut.
    pub hint: String,
}

pub enum Pick {
    /// Index into the candidates, plus a line from a `name:42` query.
    Item { index: usize, line: Option<usize> },
    /// `:42` with no file name: go to that line in the current file.
    Line(usize),
    /// Enter in `Mode::Input`: the text typed.
    Text(String),
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub mode: Mode,
    query: String,
    selected: usize,
    /// Candidate index and matched character positions, best first.
    ranked: Vec<(usize, Vec<usize>)>,
    ranked_for: Option<(String, usize)>,
    /// Gray text in the empty field, for plugin prompts.
    prompt: String,
}

impl Palette {
    pub fn show(&mut self, mode: Mode, query: &str) {
        *self = Self {
            open: true,
            mode,
            query: query.to_owned(),
            ..Self::default()
        };
    }

    /// Opens for a plugin: a list to choose from, or a line to type.
    pub fn show_prompt(&mut self, mode: Mode, prompt: &str, query: &str, selected: usize) {
        self.show(mode, query);
        self.prompt = prompt.to_owned();
        self.selected = selected;
    }

    /// Call when the candidate list changed while open (e.g. a folder scan finished).
    pub fn invalidate(&mut self) {
        self.ranked_for = None;
    }

    pub fn ui(&mut self, ctx: &egui::Context, candidates: &[Candidate]) -> Option<Pick> {
        // Taken before anything else is drawn so the editor and find bar never see them.
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if escape {
            self.open = false;
            return None;
        }

        if self.mode == Mode::Input {
            return self.input_ui(ctx, enter);
        }
        let (search, line, line_only) = self.split_query();
        self.rank(&search, candidates);
        let rows = if line_only { 1 } else { self.ranked.len() };
        if rows > 0 {
            if up {
                self.selected = (self.selected + rows - 1) % rows;
            }
            if down {
                self.selected = (self.selected + 1) % rows;
            }
        }
        self.selected = self.selected.min(rows.saturating_sub(1));

        let mut clicked = None;
        let hint = match self.mode {
            Mode::Files => "Search files by name  ·  name:42 or :42 for a line",
            Mode::Commands => "Type a command",
            Mode::Quick | Mode::Input => &self.prompt,
        };
        let area = egui::Area::new(egui::Id::unique("palette"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, [0.0, 48.0])
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(560.0);
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .desired_width(f32::INFINITY)
                            .hint_text(hint),
                    );
                    field.request_focus();
                    if field.changed() {
                        self.selected = 0;
                    }
                    ui.separator();

                    if line_only {
                        let text = match line {
                            Some(n) => format!("Go to line {n}"),
                            None => "Type a line number".to_owned(),
                        };
                        if ui.add(Button::selectable(true, text)).clicked() {
                            clicked = Some(0);
                        }
                        return;
                    }
                    if self.ranked.is_empty() {
                        ui.weak(if candidates.is_empty() {
                            "Nothing to show. Open a folder with File > Open Folder…"
                        } else {
                            "No matches"
                        });
                        return;
                    }
                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for (row, (index, positions)) in self.ranked.iter().enumerate() {
                            let candidate = &candidates[*index];
                            let button = Button::selectable(
                                row == self.selected,
                                title_job(ui, candidate, positions),
                            )
                            // A long hint is cut short (…), never the title.
                            .right_text(egui::RichText::new(&candidate.hint).weak().atom_shrink(true))
                            .truncate()
                            .min_size(egui::vec2(ui.available_width(), 0.0));
                            let response = ui.add(button);
                            if response.clicked() {
                                clicked = Some(row);
                            }
                            if row == self.selected && (up || down) {
                                response.scroll_to_me(None);
                            }
                        }
                    });
                });
            });

        // A click anywhere outside the palette dismisses it.
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p))
        });
        if outside {
            self.open = false;
            return None;
        }

        let chosen = clicked.or(if enter && rows > 0 { Some(self.selected) } else { None })?;
        self.open = false;
        if line_only {
            return line.map(Pick::Line);
        }
        Some(Pick::Item {
            index: self.ranked[chosen].0,
            line,
        })
    }

    /// `Mode::Input`: just the text field, with the prompt above it.
    fn input_ui(&mut self, ctx: &egui::Context, enter: bool) -> Option<Pick> {
        let area = egui::Area::new(egui::Id::unique("palette-input"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, [0.0, 48.0])
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(560.0);
                    if !self.prompt.is_empty() {
                        ui.label(&self.prompt);
                    }
                    let field = ui.add(egui::TextEdit::singleline(&mut self.query).desired_width(f32::INFINITY));
                    field.request_focus();
                });
            });
        let outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p))
        });
        if outside {
            self.open = false;
            return None;
        }
        if !enter {
            return None;
        }
        self.open = false;
        Some(Pick::Text(self.query.clone()))
    }

    /// Splits `main:42` into the search text and a line number. The flag is
    /// set for a query that is only `:` and digits.
    fn split_query(&self) -> (String, Option<usize>, bool) {
        if self.mode == Mode::Files
            && let Some((search, digits)) = self.query.rsplit_once(':')
            && digits.chars().all(|c| c.is_ascii_digit())
        {
            let line = digits.parse().ok().filter(|&n| n > 0);
            let search = search.trim().to_owned();
            let line_only = search.is_empty();
            return (search, line, line_only);
        }
        (self.query.clone(), None, false)
    }

    fn rank(&mut self, search: &str, candidates: &[Candidate]) {
        let key = (search.to_owned(), candidates.len());
        if self.ranked_for.as_ref() == Some(&key) {
            return;
        }
        let mut scored: Vec<(i32, usize, Vec<usize>)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let (score, positions) = fuzzy_match(search, &c.text)?;
                // Matching the file name beats matching the folders above it.
                let in_title = positions.iter().filter(|&&p| p >= c.title_start).count();
                Some((score + 3 * in_title as i32, i, positions))
            })
            .collect();
        if !search.is_empty() {
            scored.sort_by_key(|(score, i, _)| {
                (std::cmp::Reverse(*score), candidates[*i].text.len(), *i)
            });
        }
        scored.truncate(MAX_ROWS);
        self.ranked = scored.into_iter().map(|(_, i, p)| (i, p)).collect();
        self.ranked_for = Some(key);
    }
}

/// The candidate's title with matched characters highlighted.
fn title_job(ui: &egui::Ui, candidate: &Candidate, positions: &[usize]) -> LayoutJob {
    let font = TextStyle::Body.resolve(ui.style());
    let mut job = LayoutJob::default();
    for (i, c) in candidate.text.chars().enumerate().skip(candidate.title_start) {
        let color = if positions.contains(&i) {
            MATCH_COLOR
        } else {
            // Filled in with the button's normal text color.
            Color32::PLACEHOLDER
        };
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, TextFormat::simple(font.clone(), color));
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(query: &str) -> (String, Option<usize>, bool) {
        let mut p = Palette::default();
        p.show(Mode::Files, query);
        p.split_query()
    }

    #[test]
    fn splits_line_numbers_from_file_queries() {
        assert_eq!(files("app"), ("app".into(), None, false));
        assert_eq!(files("app:42"), ("app".into(), Some(42), false));
        assert_eq!(files(":7"), ("".into(), Some(7), true));
        assert_eq!(files(":"), ("".into(), None, true));
        // Not a line number, so the colon is part of the search.
        assert_eq!(files("a:b"), ("a:b".into(), None, false));
    }

    #[test]
    fn ranks_file_name_matches_first() {
        let c = |text: &str| Candidate {
            title_start: text.rfind('/').map_or(0, |i| i + 1),
            text: text.to_owned(),
            hint: String::new(),
        };
        let candidates = [c("app/main.rs"), c("src/app.rs"), c("README.md")];
        let mut p = Palette::default();
        p.show(Mode::Files, "");
        p.rank("app", &candidates);
        let order: Vec<usize> = p.ranked.iter().map(|(i, _)| *i).collect();
        assert_eq!(order, vec![1, 0]);
    }
}
