use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ropey::Rope;

/// Typing pauses longer than this start a new undo step.
const COALESCE_WINDOW: Duration = Duration::from_millis(1000);

/// A selection, or a caret when `anchor == head`. Positions are char indices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Remembered x position for up/down moves, so the caret keeps its column
    /// across shorter lines.
    pub goal: Option<f32>,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos, goal: None }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    /// Typed characters; consecutive ones merge into one undo step.
    Typing,
    /// Backspace/Delete presses; also merge.
    Deleting,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// First non-blank character, or column 0 if already there.
    LineStart,
    LineEnd,
    Up,
    Down,
    PageUp(usize),
    PageDown(usize),
    DocStart,
    DocEnd,
}

/// Which lines one change replaced: `removed + 1` lines starting at `line`
/// became `inserted + 1` lines. Lets word wrap re-measure only those lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSplice {
    pub line: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// A snippet field: chars `start..end`, which follow edits like a selection
/// would, except that typing at either edge grows the field. Fields with
/// the same `group` are filled in together; group 0 is where the caret goes
/// last. A field with a `transform` isn't typed into: it shows its group's
/// text changed by that transform, kept up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub start: usize,
    pub end: usize,
    pub group: u32,
    /// Index into the transforms given with the snippet.
    pub transform: Option<u16>,
}

/// One change to the text, as other views of the same text replay it:
/// `removed` chars at `at` replaced by `inserted`.
#[derive(Clone, Debug, PartialEq)]
pub struct Replay {
    at: usize,
    removed: usize,
    inserted: String,
}

/// Turns a field's text into a transformed copy (see `snippets::Transform`).
pub type FieldTransform = Box<dyn Fn(&str) -> String>;

/// One replacement, applied at `at` in the text as it was at that moment.
#[derive(Clone, Debug)]
struct Change {
    at: usize,
    removed: String,
    inserted: String,
}

struct Transaction {
    /// In the order they were applied.
    changes: Vec<Change>,
    before: Vec<Selection>,
    after: Vec<Selection>,
    kind: EditKind,
    id: u64,
}

/// A replacement of `start..end` (char indices in the current text).
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// How selections are placed after an edit.
enum After {
    /// One caret per edit, `offset` chars into its inserted text.
    Carets(Vec<usize>),
    /// Existing selections shifted to follow the text around them.
    Map,
}

/// Text plus selections and undo history: the model behind one editor tab.
pub struct Buffer {
    rope: Rope,
    /// Sorted by position and never overlapping.
    sels: Vec<Selection>,
    /// The selection that was added last; the view keeps it on screen.
    primary: usize,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Typing can merge into the last undo step until the cursor moves.
    can_merge: bool,
    last_edit: Instant,
    next_id: u64,
    saved_id: u64,
    /// Lowest line changed since `take_changed_line`, for re-highlighting.
    changed_from: Option<usize>,
    /// Every change's lines since `take_line_splices`, in order.
    splices: Vec<LineSplice>,
    /// Fields of the snippet being filled in, if any.
    fields: Vec<Field>,
    /// Field groups in the order Tab visits them (1, 2, …, then 0), and
    /// which one is selected.
    field_order: Vec<u32>,
    field_at: usize,
    transforms: Vec<FieldTransform>,
    /// While above zero, every change joins one undo step (a plugin
    /// command's edits, say); the step's id once it exists.
    group_depth: usize,
    group_id: Option<u64>,
    /// Ranges that follow edits, by key: plugins' regions and phantoms.
    /// Unlike snippet fields, text typed at their end stays outside them.
    marks: BTreeMap<String, Vec<(usize, usize)>>,
    /// Changes since `take_log`, kept while other views show this text.
    log: Option<Vec<Replay>>,
    /// Bumped on every change, so caches know when to refresh.
    pub revision: u64,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            sels: vec![Selection::caret(0)],
            primary: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            can_merge: false,
            last_edit: Instant::now(),
            next_id: 1,
            saved_id: 0,
            changed_from: None,
            splices: Vec::new(),
            fields: Vec::new(),
            field_order: Vec::new(),
            field_at: 0,
            transforms: Vec::new(),
            group_depth: 0,
            group_id: None,
            marks: BTreeMap::new(),
            log: None,
            revision: 0,
        }
    }

    // ----- Other views of the same text --------------------------------------

    /// Starts (or stops) keeping changes for other views to replay.
    pub fn set_logging(&mut self, on: bool) {
        match (on, &self.log) {
            (true, None) => self.log = Some(Vec::new()),
            (false, _) => self.log = None,
            _ => {}
        }
    }

    /// The changes since the last call, for other views to replay.
    pub fn take_log(&mut self) -> Vec<Replay> {
        self.log.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Another view of this text: the same text and saved state, with its
    /// own selections and marks, and no undo history of its own.
    pub fn snapshot(&self) -> Buffer {
        let mut b = Buffer::new("");
        b.rope = self.rope.clone();
        b.sels = self.sels.clone();
        b.primary = self.primary;
        b.follow(self);
        b
    }

    /// Makes the changes another view made, moving selections and marks
    /// along, as for anyone else's edits. They don't go in the undo history.
    pub fn replay(&mut self, changes: &[Replay]) {
        for c in changes {
            let edit = [Edit { start: c.at, end: c.at + c.removed, text: c.inserted.clone() }];
            for s in &mut self.sels {
                *s = Selection::new(map_pos(&edit, s.anchor), map_pos(&edit, s.head));
            }
            self.replace(c.at, c.at + c.removed, &c.inserted);
        }
        if !changes.is_empty() {
            self.normalize();
            self.can_merge = false;
        }
    }

    /// Takes another view's saved state and revision.
    pub fn follow(&mut self, other: &Buffer) {
        self.saved_id = if other.is_dirty() { u64::MAX } else { self.current_id() };
        self.revision = other.revision;
    }

    /// Swaps what belongs to the view rather than the text — selections,
    /// marks, snippet fields, and the edits its wrapping hasn't seen — with
    /// another view of the same text.
    pub fn trade_views(&mut self, other: &mut Buffer) {
        std::mem::swap(&mut self.sels, &mut other.sels);
        std::mem::swap(&mut self.primary, &mut other.primary);
        std::mem::swap(&mut self.marks, &mut other.marks);
        std::mem::swap(&mut self.fields, &mut other.fields);
        std::mem::swap(&mut self.field_order, &mut other.field_order);
        std::mem::swap(&mut self.field_at, &mut other.field_at);
        std::mem::swap(&mut self.transforms, &mut other.transforms);
        std::mem::swap(&mut self.splices, &mut other.splices);
        std::mem::swap(&mut self.changed_from, &mut other.changed_from);
        self.can_merge = false;
        other.can_merge = false;
    }

    // ----- Marks ---------------------------------------------------------

    /// Remembers ranges under `key`, replacing any it had; none removes it.
    pub fn set_marks(&mut self, key: &str, mut ranges: Vec<(usize, usize)>) {
        if ranges.is_empty() {
            self.marks.remove(key);
            return;
        }
        let len = self.len();
        for r in &mut ranges {
            *r = (r.0.min(r.1).min(len), r.0.max(r.1).min(len));
        }
        self.marks.insert(key.to_owned(), ranges);
    }

    /// The ranges under `key`, where the edits since have moved them.
    pub fn marks(&self, key: &str) -> &[(usize, usize)] {
        self.marks.get(key).map_or(&[], Vec::as_slice)
    }


    // ----- Reading -------------------------------------------------------

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line_of(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.len()))
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.line_count() - 1))
    }

    /// End of the line's text, before its newline.
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 < self.line_count() {
            self.rope.line_to_char(line + 1) - 1
        } else {
            self.len()
        }
    }

    /// The line's text without its newline.
    pub fn line_text(&self, line: usize) -> String {
        self.rope.slice(self.line_start(line)..self.line_end(line)).to_string()
    }

    pub fn slice(&self, start: usize, end: usize) -> String {
        self.rope.slice(start..end).to_string()
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.len()).then(|| self.rope.char(pos))
    }

    pub fn selections(&self) -> &[Selection] {
        &self.sels
    }

    pub fn primary(&self) -> Selection {
        self.sels[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn is_dirty(&self) -> bool {
        self.current_id() != self.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.saved_id = self.current_id();
        // Further typing must not merge into the saved step, or it would
        // look unchanged.
        self.can_merge = false;
    }

    /// Marks the text as differing from the file on disk, e.g. unsaved
    /// changes restored after a restart.
    pub fn mark_unsaved(&mut self) {
        self.saved_id = u64::MAX;
    }

    fn current_id(&self) -> u64 {
        self.undo.last().map_or(0, |t| t.id)
    }

    /// Lowest line edited since the last call.
    pub fn take_changed_line(&mut self) -> Option<usize> {
        self.changed_from.take()
    }

    /// Lines replaced by each change since the last call, in the order applied.
    pub fn take_line_splices(&mut self) -> Vec<LineSplice> {
        std::mem::take(&mut self.splices)
    }

    // ----- Selections ----------------------------------------------------

    pub fn set_selections(&mut self, sels: Vec<Selection>, primary: usize) {
        self.sels = if sels.is_empty() { vec![Selection::caret(0)] } else { sels };
        self.primary = primary.min(self.sels.len() - 1);
        self.normalize();
        self.can_merge = false;
    }

    pub fn set_single(&mut self, sel: Selection) {
        self.set_selections(vec![sel], 0);
    }

    pub fn add_selection(&mut self, sel: Selection) {
        let mut sels = self.sels.clone();
        sels.push(sel);
        let primary = sels.len() - 1;
        self.set_selections(sels, primary);
    }

    /// Esc: keep only the primary selection.
    pub fn single_selection(&mut self) {
        let mut sel = self.primary();
        if self.sels.len() == 1 {
            sel.anchor = sel.head;
        }
        self.set_single(sel);
    }

    pub fn select_all(&mut self) {
        self.set_single(Selection::new(0, self.len()));
    }

    /// Sorts selections and merges any that overlap, keeping track of the primary.
    fn normalize(&mut self) {
        let primary_head = self.sels[self.primary].head;
        let mut sels = std::mem::take(&mut self.sels);
        let len = self.len();
        for s in &mut sels {
            s.anchor = s.anchor.min(len);
            s.head = s.head.min(len);
        }
        sels.sort_by_key(|s| (s.start(), s.end()));
        let mut merged: Vec<Selection> = Vec::with_capacity(sels.len());
        for s in sels {
            if let Some(last) = merged.last_mut() {
                let overlaps = s.start() < last.end() || s.start() == last.start();
                if overlaps || (s.is_empty() && last.is_empty() && s.head == last.head) {
                    let (start, end) = (last.start(), last.end().max(s.end()));
                    // Keep the direction of the earlier selection.
                    *last = if last.head < last.anchor {
                        Selection::new(end, start)
                    } else {
                        Selection::new(start, end)
                    };
                    continue;
                }
            }
            merged.push(s);
        }
        self.primary = merged
            .iter()
            .position(|s| s.start() <= primary_head && primary_head <= s.end())
            .unwrap_or(merged.len() - 1);
        self.sels = merged;
    }

    /// Moves every selection. `width` gives each character's width, for up/down.
    pub fn move_selections(&mut self, motion: Motion, extend: bool, width: &dyn Fn(char, f32) -> f32) {
        let sels: Vec<Selection> = self
            .sels
            .iter()
            .map(|&s| {
                // Left/Right on a selection just collapse it to that side.
                if !extend && !s.is_empty() {
                    match motion {
                        Motion::Left => return Selection::caret(s.start()),
                        Motion::Right => return Selection::caret(s.end()),
                        _ => {}
                    }
                }
                let (head, goal) = self.target(s, motion, width);
                let anchor = if extend { s.anchor } else { head };
                Selection { anchor, head, goal }
            })
            .collect();
        let primary = self.primary;
        self.set_selections(sels, primary);
    }

    /// Moves every selection's head to wherever `to` says, e.g. by screen
    /// row when word wrap is on. `to` returns the new head and goal x.
    pub fn move_with(&mut self, extend: bool, to: impl Fn(&Self, Selection) -> (usize, Option<f32>)) {
        let sels = self
            .sels
            .iter()
            .map(|&s| {
                let (head, goal) = to(self, s);
                Selection { anchor: if extend { s.anchor } else { head }, head, goal }
            })
            .collect();
        let primary = self.primary;
        self.set_selections(sels, primary);
    }

    fn target(&self, s: Selection, motion: Motion, width: &dyn Fn(char, f32) -> f32) -> (usize, Option<f32>) {
        let pos = s.head;
        let line = self.line_of(pos);
        let vertical = |lines: isize| {
            let goal = s.goal.unwrap_or_else(|| self.x_of(pos, width));
            let target = line as isize + lines;
            let head = if target < 0 {
                0
            } else if target as usize >= self.line_count() {
                self.len()
            } else {
                self.pos_at_x(target as usize, goal, width)
            };
            (head, Some(goal))
        };
        match motion {
            Motion::Left => (pos.saturating_sub(1), None),
            Motion::Right => ((pos + 1).min(self.len()), None),
            Motion::WordLeft => (self.word_left(pos), None),
            Motion::WordRight => (self.word_right(pos), None),
            Motion::LineStart => {
                let start = self.line_start(line);
                let first = start + self.line_text(line).chars().take_while(|c| c.is_whitespace()).count();
                (if pos == first { start } else { first }, None)
            }
            Motion::LineEnd => (self.line_end(line), None),
            Motion::Up => vertical(-1),
            Motion::Down => vertical(1),
            Motion::PageUp(n) => vertical(-(n as isize)),
            Motion::PageDown(n) => vertical(n as isize),
            Motion::DocStart => (0, None),
            Motion::DocEnd => (self.len(), None),
        }
    }

    /// Horizontal position of `pos` within its line.
    pub fn x_of(&self, pos: usize, width: &dyn Fn(char, f32) -> f32) -> f32 {
        let start = self.line_start(self.line_of(pos));
        self.rope.slice(start..pos).chars().fold(0.0, |x, c| x + width(c, x))
    }

    /// The position in `line` closest to x.
    pub fn pos_at_x(&self, line: usize, x: f32, width: &dyn Fn(char, f32) -> f32) -> usize {
        let start = self.line_start(line);
        let mut left = 0.0;
        for (i, c) in self.rope.slice(start..self.line_end(line)).chars().enumerate() {
            let w = width(c, left);
            if x < left + w / 2.0 {
                return start + i;
            }
            left += w;
        }
        self.line_end(line)
    }

    fn word_left(&self, pos: usize) -> usize {
        let mut p = pos;
        while p > 0 && matches!(self.rope.char(p - 1), ' ' | '\t') {
            p -= 1;
        }
        if p == 0 {
            return 0;
        }
        let kind = class(self.rope.char(p - 1));
        if kind == Class::Newline {
            return p - 1;
        }
        while p > 0 && class(self.rope.char(p - 1)) == kind {
            p -= 1;
        }
        p
    }

    fn word_right(&self, pos: usize) -> usize {
        let len = self.len();
        let mut p = pos;
        while p < len && matches!(self.rope.char(p), ' ' | '\t') {
            p += 1;
        }
        if p == len {
            return len;
        }
        let kind = class(self.rope.char(p));
        if kind == Class::Newline {
            return p + 1;
        }
        while p < len && class(self.rope.char(p)) == kind {
            p += 1;
        }
        p
    }

    /// The word (or run of punctuation) around `pos`.
    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let len = self.len();
        let kind = match (self.char_at(pos), pos.checked_sub(1).and_then(|p| self.char_at(p))) {
            (Some(c), _) if class(c) == Class::Word => Class::Word,
            (_, Some(c)) if class(c) == Class::Word => Class::Word,
            (Some(c), _) => class(c),
            (None, _) => return (pos, pos),
        };
        if kind == Class::Newline {
            return (pos, pos);
        }
        let mut start = pos;
        while start > 0 && class(self.rope.char(start - 1)) == kind {
            start -= 1;
        }
        let mut end = pos;
        while end < len && class(self.rope.char(end)) == kind {
            end += 1;
        }
        (start, end)
    }

    /// The whole line including its newline.
    pub fn line_range(&self, line: usize) -> (usize, usize) {
        let end = if line + 1 < self.line_count() {
            self.line_start(line + 1)
        } else {
            self.len()
        };
        (self.line_start(line), end)
    }

    /// ⌘L: extend each selection to whole lines; repeat to take the next line too.
    pub fn expand_to_lines(&mut self) {
        let sels = self
            .sels
            .iter()
            .map(|s| {
                let first = self.line_of(s.start());
                let last_pos = if s.end() > s.start() && self.line_start(self.line_of(s.end())) == s.end() {
                    s.end() - 1
                } else {
                    s.end()
                };
                let last = self.line_of(last_pos);
                let (start, _) = self.line_range(first);
                let (_, end) = self.line_range(last);
                // Already whole lines: take the next one too.
                let end = if s.start() == start && s.end() == end && !s.is_empty() {
                    self.line_range(last + 1).1
                } else {
                    end
                };
                Selection::new(start, end)
            })
            .collect();
        self.set_selections(sels, self.primary);
    }

    /// ⇧⌘L: one caret at the end of every line in each selection.
    pub fn split_into_lines(&mut self) {
        let mut out = Vec::new();
        for s in &self.sels {
            if s.is_empty() {
                out.push(*s);
                continue;
            }
            let (first, last) = (self.line_of(s.start()), self.line_of(s.end()));
            for line in first..=last {
                let start = s.start().max(self.line_start(line));
                let end = s.end().min(self.line_end(line));
                if line == last && end == self.line_start(line) && line != first {
                    continue; // selection ended at the very start of this line
                }
                out.push(Selection::new(start, end));
            }
        }
        let primary = out.len().saturating_sub(1);
        self.set_selections(out, primary);
    }

    /// ⌃⇧↑ / ⌃⇧↓: add a caret on the line above the first (or below the last) one.
    pub fn add_caret_vertically(&mut self, down: bool, width: &dyn Fn(char, f32) -> f32) {
        let from = if down { *self.sels.last().unwrap() } else { self.sels[0] };
        let line = self.line_of(from.head);
        if (down && line + 1 >= self.line_count()) || (!down && line == 0) {
            return;
        }
        let goal = from.goal.unwrap_or_else(|| self.x_of(from.head, width));
        let target = if down { line + 1 } else { line - 1 };
        let pos = self.pos_at_x(target, goal, width);
        let mut sels = self.sels.clone();
        sels.push(Selection { anchor: pos, head: pos, goal: Some(goal) });
        let primary = sels.len() - 1;
        // Keep the goal column of existing carets too.
        let goal_of = |s: &Selection| s.goal.or(Some(goal));
        let sels = sels.iter().map(|s| Selection { goal: goal_of(s), ..*s }).collect();
        self.set_selections(sels, primary);
    }

    /// ⌘D: select the word under each caret, or add the next copy of the
    /// primary selection's text. Returns false when there is no further copy.
    pub fn add_next_occurrence(&mut self) -> bool {
        if self.sels.iter().any(|s| s.is_empty()) {
            let sels = self
                .sels
                .iter()
                .map(|s| {
                    if s.is_empty() {
                        let (a, b) = self.word_at(s.head);
                        Selection::new(a, b)
                    } else {
                        *s
                    }
                })
                .collect();
            self.set_selections(sels, self.primary);
            return true;
        }
        let needle = self.slice(self.primary().start(), self.primary().end());
        let after = self.sels.last().unwrap().end();
        let found = self
            .find(&needle, after, true)
            .filter(|&(s, e)| !self.sels.iter().any(|x| x.start() == s && x.end() == e));
        match found {
            Some((s, e)) => {
                self.add_selection(Selection::new(s, e));
                true
            }
            None => false,
        }
    }

    /// ⌃⌘G: select every copy of the primary selection's text (or word under it).
    pub fn select_all_occurrences(&mut self) {
        let p = self.primary();
        let (start, end) = if p.is_empty() { self.word_at(p.head) } else { (p.start(), p.end()) };
        if start == end {
            return;
        }
        let needle = self.slice(start, end);
        let sels: Vec<Selection> = self
            .find_all(&needle, true)
            .into_iter()
            .map(|(s, e)| Selection::new(s, e))
            .collect();
        let primary = sels.iter().position(|s| s.start() == start).unwrap_or(0);
        self.set_selections(sels, primary);
    }

    /// First match at or after `from`, wrapping to the top.
    pub fn find(&self, needle: &str, from: usize, case_sensitive: bool) -> Option<(usize, usize)> {
        let all = self.find_all(needle, case_sensitive);
        all.iter().find(|m| m.0 >= from).or(all.first()).copied()
    }

    /// Every non-overlapping match as char ranges.
    pub fn find_all(&self, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let hay = self.text();
        let needle_len = needle.chars().count();
        if case_sensitive {
            return hay
                .match_indices(needle)
                .map(|(b, _)| {
                    let s = self.rope.byte_to_char(b);
                    (s, s + needle_len)
                })
                .collect();
        }
        // Case-folding can change byte lengths, so compare char by char.
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let hay: Vec<char> = hay.chars().map(fold).collect();
        let needle: Vec<char> = needle.chars().map(fold).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= hay.len() {
            if hay[i..i + needle.len()] == needle[..] {
                out.push((i, i + needle.len()));
                i += needle.len();
            } else {
                i += 1;
            }
        }
        out
    }

    // ----- Editing -------------------------------------------------------

    /// Replaces every selection with `text`.
    pub fn insert(&mut self, text: &str, kind: EditKind) {
        let texts = vec![(text.to_owned(), text.chars().count()); self.sels.len()];
        self.replace_selections(texts, kind);
    }

    /// Replaces selection i with `texts[i].0` and puts its caret `texts[i].1`
    /// chars into that text.
    pub fn replace_selections(&mut self, texts: Vec<(String, usize)>, kind: EditKind) {
        let (edits, offsets): (Vec<Edit>, Vec<usize>) = self
            .sels
            .iter()
            .zip(texts)
            .map(|(s, (text, offset))| (Edit { start: s.start(), end: s.end(), text }, offset))
            .unzip();
        self.apply(edits, kind, After::Carets(offsets));
    }

    /// Deletes each selection, or for an empty one the range to `motion`
    /// (e.g. Backspace is `Motion::Left`).
    pub fn delete(&mut self, motion: Motion, width: &dyn Fn(char, f32) -> f32) {
        let ranges = self
            .sels
            .iter()
            .map(|&s| {
                if s.is_empty() {
                    let (other, _) = self.target(s, motion, width);
                    (other.min(s.head), other.max(s.head))
                } else {
                    (s.start(), s.end())
                }
            })
            .collect();
        self.delete_ranges(ranges);
    }

    /// Deletes the given ranges as one Backspace/Delete step.
    pub fn delete_ranges(&mut self, mut ranges: Vec<(usize, usize)>) {
        // Neighbouring carets can produce overlapping ranges; join them.
        ranges.sort();
        let mut joined: Vec<Edit> = Vec::new();
        for (start, end) in ranges {
            match joined.last_mut() {
                Some(last) if start <= last.end => last.end = last.end.max(end),
                _ => joined.push(Edit { start, end, text: String::new() }),
            }
        }
        joined.retain(|e| e.start < e.end);
        if !joined.is_empty() {
            self.apply(joined, EditKind::Deleting, After::Map);
        }
    }

    /// Applies arbitrary edits (sorted, non-overlapping) and keeps selections in place.
    pub fn apply_edits(&mut self, edits: Vec<Edit>) {
        self.apply(edits, EditKind::Other, After::Map);
    }

    // ----- Snippet fields ------------------------------------------------

    /// Replaces each edit's range with its text in one undo step, then
    /// selects the first of the snippet fields. `fields[i]` are relative to
    /// the start of `edits[i]`'s text and index `transforms[i]`. Edits must
    /// be sorted and not overlap.
    pub fn insert_snippet(&mut self, edits: Vec<Edit>, fields: Vec<Vec<Field>>, transforms: Vec<Vec<FieldTransform>>) {
        self.clear_fields();
        let mut shift: isize = 0;
        let mut absolute = Vec::new();
        for ((e, fields), transforms) in edits.iter().zip(fields).zip(transforms) {
            let base = (e.start as isize + shift) as usize;
            let first = self.transforms.len() as u16;
            absolute.extend(fields.into_iter().map(|f| Field {
                start: base + f.start,
                end: base + f.end,
                transform: f.transform.map(|t| first + t),
                ..f
            }));
            self.transforms.extend(transforms);
            shift += e.text.chars().count() as isize - (e.end - e.start) as isize;
        }
        self.apply(edits, EditKind::Other, After::Map);
        // Tab visits the groups that can be typed into.
        let mut order: Vec<u32> =
            absolute.iter().filter(|f| f.group != 0 && f.transform.is_none()).map(|f| f.group).collect();
        order.sort_unstable();
        order.dedup();
        order.push(0);
        self.fields = absolute;
        self.field_order = order;
        self.field_at = 0;
        self.select_field();
    }

    /// Tab (or Shift-Tab, `back`) in a snippet: selects the next field.
    /// False if no snippet is being filled in, or the caret has left it.
    pub fn next_field(&mut self, back: bool) -> bool {
        let head = self.primary().head;
        if !self.fields.iter().any(|f| f.start <= head && head <= f.end) {
            self.clear_fields();
            return false;
        }
        self.field_at = if back { self.field_at.saturating_sub(1) } else { self.field_at + 1 };
        self.select_field();
        true
    }

    pub fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    pub fn clear_fields(&mut self) {
        self.fields.clear();
        self.field_order.clear();
        self.transforms.clear();
    }

    /// The snippet fields being filled in, for drawing.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Rewrites each transformed field from its group's text, as part of
    /// the last undo step.
    fn refresh_transforms(&mut self) {
        for i in 0..self.fields.len() {
            let f = self.fields[i];
            let Some(t) = f.transform else {
                continue;
            };
            let Some(source) = self.fields.iter().find(|s| s.group == f.group && s.transform.is_none()) else {
                continue;
            };
            let text = (self.transforms[t as usize])(&self.slice(source.start, source.end));
            if text == self.slice(f.start, f.end) {
                continue;
            }
            let edit = Edit { start: f.start, end: f.end, text };
            for s in &mut self.sels {
                *s = Selection { anchor: map_pos(std::slice::from_ref(&edit), s.anchor), head: map_pos(std::slice::from_ref(&edit), s.head), ..*s };
            }
            let change = self.replace(edit.start, edit.end, &edit.text);
            if let Some(last) = self.undo.last_mut() {
                last.changes.push(change);
                last.after = self.sels.clone();
            }
        }
    }

    /// Selects every field of the current group; reaching group 0 ends the snippet.
    fn select_field(&mut self) {
        let group = self.field_order.get(self.field_at).copied().unwrap_or(0);
        let mut sels: Vec<Selection> = self
            .fields
            .iter()
            .filter(|f| f.group == group && f.transform.is_none())
            .map(|f| Selection::new(f.start, f.end))
            .collect();
        sels.sort_by_key(|s| s.start());
        if !sels.is_empty() {
            let primary = sels.len() - 1;
            self.set_selections(sels, primary);
        }
        if group == 0 {
            self.clear_fields();
        }
    }

    /// Lines touched by any selection, each once, in order.
    pub fn selected_lines(&self) -> Vec<usize> {
        let mut lines = Vec::new();
        for s in &self.sels {
            let first = self.line_of(s.start());
            let mut last = self.line_of(s.end());
            // A selection ending at column 0 doesn't include that line.
            if last > first && self.line_start(last) == s.end() {
                last -= 1;
            }
            for l in first..=last {
                if lines.last() != Some(&l) {
                    lines.push(l);
                }
            }
        }
        lines
    }

    fn apply(&mut self, edits: Vec<Edit>, kind: EditKind, after: After) {
        let before = self.sels.clone();
        let mut changes = Vec::with_capacity(edits.len());
        // Apply from the end so earlier positions stay valid.
        for e in edits.iter().rev() {
            changes.push(self.replace(e.start, e.end, &e.text));
        }

        let sels: Vec<Selection> = match after {
            After::Carets(offsets) => {
                let mut shift: isize = 0;
                edits
                    .iter()
                    .zip(offsets)
                    .map(|(e, offset)| {
                        let start = (e.start as isize + shift) as usize;
                        let inserted = e.text.chars().count();
                        shift += inserted as isize - (e.end - e.start) as isize;
                        Selection::caret(start + offset.min(inserted))
                    })
                    .collect()
            }
            After::Map => before
                .iter()
                .map(|s| Selection::new(map_pos(&edits, s.anchor), map_pos(&edits, s.head)))
                .collect(),
        };
        self.sels = sels;
        self.normalize();
        self.record(changes, before, kind);
        if !self.transforms.is_empty() {
            self.refresh_transforms();
        }
    }

    fn replace(&mut self, start: usize, end: usize, text: &str) -> Change {
        let line = self.line_of(start);
        self.changed_from = Some(self.changed_from.map_or(line, |l| l.min(line)));
        let removed = self.slice(start, end);
        self.splices.push(LineSplice {
            line,
            removed: removed.matches('\n').count(),
            inserted: text.matches('\n').count(),
        });
        self.rope.remove(start..end);
        self.rope.insert(start, text);
        self.revision += 1;
        let inserted = text.chars().count();
        for f in &mut self.fields {
            *f = move_field(*f, start, end, inserted);
        }
        for range in self.marks.values_mut().flatten() {
            *range = move_mark(*range, start, end, inserted);
        }
        if let Some(log) = &mut self.log {
            log.push(Replay { at: start, removed: end - start, inserted: text.to_owned() });
        }
        Change { at: start, removed, inserted: text.to_owned() }
    }

    fn record(&mut self, changes: Vec<Change>, before: Vec<Selection>, kind: EditKind) {
        self.redo.clear();
        let now = Instant::now();
        let grouped = self.group_depth > 0 && self.group_id.is_some_and(|id| self.undo.last().is_some_and(|t| t.id == id));
        let merge = grouped
            || (self.can_merge
                && kind != EditKind::Other
                && now.duration_since(self.last_edit) < COALESCE_WINDOW
                && self.undo.last().is_some_and(|t| t.kind == kind));
        self.last_edit = now;
        if merge {
            let last = self.undo.last_mut().unwrap();
            last.changes.extend(changes);
            last.after = self.sels.clone();
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.undo.push(Transaction { changes, before, after: self.sels.clone(), kind, id });
            if self.group_depth > 0 {
                self.group_id = Some(id);
            }
        }
        // Typing a newline ends the step, like most editors.
        self.can_merge = kind != EditKind::Other
            && !self.undo.last().unwrap().changes.last().is_some_and(|c| c.inserted.contains('\n'));
    }

    /// Until the matching `end_undo_group`, every change joins one undo step.
    pub fn begin_undo_group(&mut self) {
        if self.group_depth == 0 {
            self.group_id = None;
        }
        self.group_depth += 1;
    }

    pub fn end_undo_group(&mut self) {
        self.group_depth = self.group_depth.saturating_sub(1);
        if self.group_depth == 0 {
            self.group_id = None;
            self.can_merge = false;
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(t) = self.undo.pop() else {
            return false;
        };
        for c in t.changes.iter().rev() {
            let end = c.at + c.inserted.chars().count();
            self.replace(c.at, end, &c.removed);
        }
        self.sels = t.before.clone();
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.redo.push(t);
        self.can_merge = false;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(t) = self.redo.pop() else {
            return false;
        };
        for c in &t.changes {
            let end = c.at + c.removed.chars().count();
            self.replace(c.at, end, &c.inserted);
        }
        self.sels = t.after.clone();
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.undo.push(t);
        self.can_merge = false;
        true
    }
}

/// Where a field ends up after `start..end` is replaced by `inserted` chars.
fn move_field(f: Field, start: usize, end: usize, inserted: usize) -> Field {
    let delta = inserted as isize - (end - start) as isize;
    let shift = |p: usize| (p as isize + delta) as usize;
    if f.start <= start && end <= f.end {
        // Inside the field, or typing at one of its edges: it grows or shrinks.
        Field { end: shift(f.end), ..f }
    } else if end <= f.start {
        Field { start: shift(f.start), end: shift(f.end), ..f }
    } else if f.end < start {
        f
    } else {
        // Overlaps an edge: the field covers what is left of both.
        Field { start: f.start.min(start), end: shift(f.end.max(end)), ..f }
    }
}

/// Where a mark ends up after `start..end` is replaced by `inserted` chars,
/// as plugins expect regions to move: text inserted at the start pushes the mark
/// along, text inserted at its end stays outside it, and an edit inside it
/// grows or shrinks it.
fn move_mark((a, z): (usize, usize), start: usize, end: usize, inserted: usize) -> (usize, usize) {
    let delta = inserted as isize - (end - start) as isize;
    let shift = |p: usize| (p as isize + delta) as usize;
    let a = if a < start {
        a
    } else if a >= end {
        shift(a)
    } else {
        start
    };
    let z = if z <= start {
        z
    } else if z >= end {
        shift(z)
    } else {
        start
    };
    // An empty mark where text goes in moves along with it.
    (a, z.max(a))
}

/// Where `pos` ends up after `edits` (sorted, in pre-edit coordinates).
fn map_pos(edits: &[Edit], pos: usize) -> usize {
    let mut shift: isize = 0;
    for e in edits {
        let inserted = e.text.chars().count() as isize;
        let removed = (e.end - e.start) as isize;
        if e.end < pos || (e.end == pos && (e.start < e.end || inserted > 0)) {
            shift += inserted - removed;
        } else if e.start < pos && pos < e.end {
            // Inside removed text: land at the start of the replacement.
            return (e.start as isize + shift) as usize;
        } else {
            break;
        }
    }
    (pos as isize + shift) as usize
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Newline,
    Punct,
}

fn class(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &dyn Fn(char, f32) -> f32 = &|_, _| 1.0;

    fn carets(b: &Buffer) -> Vec<usize> {
        b.selections().iter().map(|s| s.head).collect()
    }

    #[test]
    fn typing_at_several_carets() {
        let mut b = Buffer::new("ab\ncd\nef");
        b.set_selections(vec![Selection::caret(1), Selection::caret(4), Selection::caret(7)], 0);
        b.insert("X", EditKind::Typing);
        assert_eq!(b.text(), "aXb\ncXd\neXf");
        assert_eq!(carets(&b), vec![2, 6, 10]);
        b.delete(Motion::Left, W);
        assert_eq!(b.text(), "ab\ncd\nef");
        assert_eq!(carets(&b), vec![1, 4, 7]);
    }

    #[test]
    fn snippet_fields_follow_typing_and_tab_through() {
        // "fn ${1:x}($2) {$0}" at two places, each replacing a trigger "fn".
        let mut b = Buffer::new("fn\nfn");
        let field = |start, end, group| Field { start, end, group, transform: None };
        let snippet = || vec![field(3, 4, 1), field(5, 5, 2), field(8, 8, 0)];
        let edit = |start: usize| Edit { start, end: start + 2, text: "fn x() {}".into() };
        b.insert_snippet(vec![edit(0), edit(3)], vec![snippet(), snippet()], vec![vec![], vec![]]);
        assert_eq!(b.text(), "fn x() {}\nfn x() {}");
        let sels: Vec<(usize, usize)> = b.selections().iter().map(|s| (s.start(), s.end())).collect();
        assert_eq!(sels, [(3, 4), (13, 14)], "both copies of field 1 selected");

        b.insert("name", EditKind::Typing);
        assert_eq!(b.text(), "fn name() {}\nfn name() {}");
        assert!(b.next_field(false));
        assert_eq!(carets(&b), [8, 21], "field 2 moved along");
        b.insert("a", EditKind::Typing);
        assert!(b.next_field(true), "Shift-Tab goes back");
        assert_eq!(b.slice(b.primary().start(), b.primary().end()), "name");
        assert!(b.next_field(false) && b.next_field(false));
        assert_eq!(carets(&b), [12, 26], "ends at $0");
        assert!(!b.has_fields());
        assert!(!b.next_field(false), "Tab works normally again");
    }

    #[test]
    fn transformed_fields_follow_their_group() {
        // "${1:ab} = ${1/(.*)/\U$1/};" with the transform as plain upper-casing.
        let mut b = Buffer::new("");
        let fields = vec![
            Field { start: 0, end: 2, group: 1, transform: None },
            Field { start: 5, end: 7, group: 1, transform: Some(0) },
            Field { start: 8, end: 8, group: 0, transform: None },
        ];
        let upper: FieldTransform = Box::new(|s| s.to_uppercase());
        b.insert_snippet(vec![Edit { start: 0, end: 0, text: "ab = AB;".into() }], vec![fields], vec![vec![upper]]);
        assert_eq!(carets(&b), [2], "only the typed field is selected");
        b.insert("xyz", EditKind::Typing);
        assert_eq!(b.text(), "xyz = XYZ;");
        b.delete(Motion::Left, W);
        assert_eq!(b.text(), "xy = XY;");
        assert!(b.next_field(false));
        assert_eq!(carets(&b), [8]);
        b.undo();
        assert_eq!(b.text(), "xyz = XYZ;", "one undo step reverts the text and its copy");
    }

    #[test]
    fn grouped_changes_undo_together() {
        let mut b = Buffer::new("abc");
        b.begin_undo_group();
        b.apply_edits(vec![Edit { start: 0, end: 0, text: "1".into() }]);
        b.begin_undo_group();
        b.apply_edits(vec![Edit { start: 4, end: 4, text: "2".into() }]);
        b.end_undo_group();
        b.insert("x", EditKind::Typing);
        b.end_undo_group();
        b.insert("y", EditKind::Other);
        assert!(b.undo());
        assert!(b.undo());
        assert_eq!(b.text(), "abc", "everything in the group went in one step");
    }

    #[test]
    fn leaving_a_snippet_ends_it() {
        let mut b = Buffer::new("");
        let fields = vec![Field { start: 0, end: 1, group: 1, transform: None }, Field { start: 2, end: 2, group: 0, transform: None }];
        b.insert_snippet(vec![Edit { start: 0, end: 0, text: "x; more".into() }], vec![fields], vec![vec![]]);
        assert!(b.has_fields());
        b.set_single(Selection::caret(5));
        assert!(!b.next_field(false), "caret is outside every field");
        assert!(!b.has_fields());
    }

    #[test]
    fn marks_follow_edits_without_growing_at_their_end() {
        let mut b = Buffer::new("one two three");
        b.set_marks("word", vec![(4, 7), (8, 8)]);
        let edit = |b: &mut Buffer, start, end, text: &str| {
            b.apply_edits(vec![Edit { start, end, text: text.into() }]);
        };
        edit(&mut b, 0, 0, ">> ");
        assert_eq!(b.marks("word"), [(7, 10), (11, 11)], "both pushed along");
        edit(&mut b, 10, 10, "!");
        assert_eq!(b.marks("word"), [(7, 10), (12, 12)], "typing at the end stays outside");
        edit(&mut b, 8, 9, "WW");
        assert_eq!(b.marks("word"), [(7, 11), (13, 13)], "an edit inside grows it");
        edit(&mut b, 5, 13, "");
        assert_eq!(b.marks("word"), [(5, 5), (5, 5)], "deleted around");
        b.undo();
        assert_eq!(b.marks("word"), [(13, 13), (13, 13)], "undo is an edit like any other");
        b.set_marks("word", Vec::new());
        assert!(b.marks("word").is_empty());
    }

    #[test]
    fn another_view_replays_changes_with_its_own_selections() {
        let mut main = Buffer::new("one two");
        main.set_logging(true);
        let mut other = main.snapshot();
        other.set_single(Selection::caret(7));
        main.set_single(Selection::caret(0));
        main.insert("zero ", EditKind::Typing);
        main.undo();
        main.redo();
        other.replay(&main.take_log());
        other.follow(&main);
        assert_eq!(other.text(), "zero one two");
        assert_eq!(carets(&other), [12], "its caret moved along");
        assert!(other.is_dirty() && other.revision == main.revision);
        main.mark_saved();
        other.follow(&main);
        assert!(!other.is_dirty());

        // The view in front holds the text and its undo history.
        main.trade_views(&mut other);
        std::mem::swap(&mut main, &mut other);
        assert_eq!(carets(&other), [12], "each view keeps its own caret");
        assert_eq!(carets(&main), [5]);
        assert!(other.undo(), "the history went along with the text");
        assert!(!main.undo());
    }

    #[test]
    fn undo_merges_typing_and_restores_carets() {
        let mut b = Buffer::new("");
        for c in ["h", "i", " "] {
            b.insert(c, EditKind::Typing);
        }
        assert!(b.is_dirty());
        b.undo();
        assert_eq!(b.text(), "");
        assert!(!b.is_dirty());
        b.redo();
        assert_eq!(b.text(), "hi ");
        assert_eq!(carets(&b), vec![3]);
    }

    #[test]
    fn moving_the_cursor_starts_a_new_undo_step() {
        let mut b = Buffer::new("");
        b.insert("a", EditKind::Typing);
        b.move_selections(Motion::Left, false, W);
        b.insert("b", EditKind::Typing);
        b.undo();
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn saving_marks_clean_and_later_typing_dirty() {
        let mut b = Buffer::new("");
        b.insert("a", EditKind::Typing);
        b.mark_saved();
        assert!(!b.is_dirty());
        b.insert("b", EditKind::Typing);
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty());
    }

    #[test]
    fn backspace_keeps_carets_that_cannot_move() {
        let mut b = Buffer::new("ab");
        b.set_selections(vec![Selection::caret(0), Selection::caret(2)], 1);
        b.delete(Motion::Left, W);
        assert_eq!(b.text(), "a");
        assert_eq!(carets(&b), vec![0, 1]);
    }

    #[test]
    fn carets_that_meet_are_merged() {
        let mut b = Buffer::new("abc");
        b.set_selections(vec![Selection::caret(1), Selection::caret(2)], 1);
        b.move_selections(Motion::Left, false, W);
        b.move_selections(Motion::Left, false, W);
        assert_eq!(carets(&b), vec![0]);
    }

    #[test]
    fn vertical_moves_keep_the_goal_column() {
        let mut b = Buffer::new("abcdef\nab\nabcdef");
        b.set_single(Selection::caret(5));
        b.move_selections(Motion::Down, false, W);
        assert_eq!(carets(&b), vec![9]); // end of "ab"
        b.move_selections(Motion::Down, false, W);
        assert_eq!(carets(&b), vec![15]); // back to column 5
    }

    #[test]
    fn word_motions() {
        let b = Buffer::new("foo.bar  baz\nx");
        assert_eq!(b.word_right(0), 3);
        assert_eq!(b.word_right(3), 4);
        assert_eq!(b.word_right(7), 12);
        assert_eq!(b.word_right(12), 13);
        assert_eq!(b.word_left(12), 9);
        assert_eq!(b.word_left(9), 4);
        assert_eq!(b.word_at(5), (4, 7));
    }

    #[test]
    fn smart_home_toggles_between_indent_and_column_zero() {
        let mut b = Buffer::new("    code");
        b.set_single(Selection::caret(8));
        b.move_selections(Motion::LineStart, false, W);
        assert_eq!(carets(&b), vec![4]);
        b.move_selections(Motion::LineStart, false, W);
        assert_eq!(carets(&b), vec![0]);
    }

    #[test]
    fn next_occurrence_selects_word_then_copies() {
        let mut b = Buffer::new("foo bar foo foo");
        b.set_single(Selection::caret(1));
        b.add_next_occurrence();
        assert_eq!(b.selections(), &[Selection::new(0, 3)]);
        b.add_next_occurrence();
        b.add_next_occurrence();
        let ranges: Vec<_> = b.selections().iter().map(|s| (s.start(), s.end())).collect();
        assert_eq!(ranges, vec![(0, 3), (8, 11), (12, 15)]);
        assert!(!b.add_next_occurrence());
        b.insert("x", EditKind::Typing);
        assert_eq!(b.text(), "x bar x x");
    }

    #[test]
    fn split_selection_into_lines() {
        let mut b = Buffer::new("one\ntwo\nthree");
        b.select_all();
        b.split_into_lines();
        let ranges: Vec<_> = b.selections().iter().map(|s| (s.start(), s.end())).collect();
        assert_eq!(ranges, vec![(0, 3), (4, 7), (8, 13)]);
    }

    #[test]
    fn add_caret_below_uses_column() {
        let mut b = Buffer::new("abcd\nab\nabcd");
        b.set_single(Selection::caret(3));
        b.add_caret_vertically(true, W);
        b.add_caret_vertically(true, W);
        assert_eq!(carets(&b), vec![3, 7, 11]);
    }

    #[test]
    fn mapped_edits_shift_selections() {
        let mut b = Buffer::new("a\nb");
        b.set_selections(vec![Selection::caret(0), Selection::caret(2)], 0);
        // Indent both lines.
        b.apply_edits(vec![
            Edit { start: 0, end: 0, text: "\t".into() },
            Edit { start: 2, end: 2, text: "\t".into() },
        ]);
        assert_eq!(b.text(), "\ta\n\tb");
        assert_eq!(carets(&b), vec![1, 4]);
        b.undo();
        assert_eq!(b.text(), "a\nb");
    }

    #[test]
    fn expand_to_lines_grows_on_repeat() {
        let mut b = Buffer::new("a\nb\nc");
        b.set_single(Selection::caret(0));
        b.expand_to_lines();
        assert_eq!(b.primary(), Selection::new(0, 2));
        b.expand_to_lines();
        assert_eq!(b.primary(), Selection::new(0, 4));
    }

    #[test]
    fn tracks_the_first_changed_line() {
        let mut b = Buffer::new("a\nb\nc");
        b.set_single(Selection::caret(4));
        b.insert("x", EditKind::Typing);
        assert_eq!(b.take_changed_line(), Some(2));
        assert_eq!(b.take_changed_line(), None);
    }

    #[test]
    fn case_insensitive_find_uses_char_indices() {
        let b = Buffer::new("Héllo HÉLLO");
        assert_eq!(b.find_all("héllo", false), vec![(0, 5), (6, 11)]);
        assert_eq!(b.find_all("héllo", true), vec![]);
        assert_eq!(b.find_all("HÉ", true), vec![(6, 8)]);
    }
}
