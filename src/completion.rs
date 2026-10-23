//! Autocomplete: while a word is typed, a list offers
//! ways to finish it: words from the open files, snippets, and plugins'
//! completions. This is the list itself; the app shows it.

use std::collections::HashMap;

use serde_json::Value;

use crate::buffer::Buffer;
use crate::fuzzy::fuzzy_match;

/// Rows shown at most; the rest are worse matches nobody scrolls to.
const MAX_SHOWN: usize = 50;
/// Words are gathered from this many chars on each side of the caret, so
/// a huge file doesn't slow typing down.
const SCAN_CHARS: usize = 200_000;
/// Words shorter than this aren't worth offering.
const MIN_WORD_CHARS: usize = 3;

/// What accepting a completion does.
#[derive(Clone, Debug, PartialEq)]
pub enum Insert {
    /// Replace the typed prefix with this text.
    Text(String),
    /// Replace the prefix with this snippet template, fields and all.
    Snippet(String),
    /// Remove the prefix and run this command, as a plugin's
    /// `command_completion` asks.
    Command(String, Value),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    /// What's matched against the prefix and shown.
    pub trigger: String,
    /// Gray text on the right, like "snippet" or a type.
    pub annotation: String,
    pub insert: Insert,
}

impl Completion {
    pub fn word(word: &str) -> Self {
        Self { trigger: word.to_owned(), annotation: String::new(), insert: Insert::Text(word.to_owned()) }
    }

    /// A completion from a plugin, as the plugin host sends it.
    pub fn from_plugin(item: &Value) -> Option<Self> {
        let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        let trigger = text("trigger");
        if trigger.is_empty() {
            return None;
        }
        let completion = item.get("completion").and_then(Value::as_str).unwrap_or(&trigger).to_owned();
        let insert = match item.get("format").and_then(Value::as_u64).unwrap_or(0) {
            1 => Insert::Snippet(completion),
            2 => Insert::Command(completion, item.get("args").cloned().unwrap_or(Value::Null)),
            _ => Insert::Text(completion),
        };
        Some(Self { trigger, annotation: text("annotation"), insert })
    }
}

/// The open list: every candidate, and the ones that match the prefix.
pub struct Popup {
    pub items: Vec<Completion>,
    /// Indexes into `items` with the matched char positions, best first.
    pub shown: Vec<(usize, Vec<usize>)>,
    pub selected: usize,
    /// The document it's for, and where its prefix starts there.
    pub doc: u64,
    pub start: usize,
    pub prefix: String,
    /// The document's revision and caret when the list was last updated,
    /// to tell typing from moving the caret.
    pub seen: (u64, usize),
    /// The selected row moved and should be scrolled into view.
    pub reveal: bool,
}

impl Popup {
    /// A list for `prefix`; `None` if nothing matches, or the only match is
    /// what's already typed.
    pub fn new(items: Vec<Completion>, doc: u64, start: usize, prefix: &str) -> Option<Self> {
        let mut popup = Self { items, shown: Vec::new(), selected: 0, doc, start, prefix: String::new(), seen: (0, 0), reveal: false };
        popup.filter(prefix).then_some(popup)
    }

    /// Narrows the list to `prefix`. False if nothing is left to offer.
    pub fn filter(&mut self, prefix: &str) -> bool {
        self.prefix = prefix.to_owned();
        let first = prefix.chars().next().map(fold);
        let mut scored: Vec<(i32, usize, Vec<usize>)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, c)| c.trigger != prefix)
            // Only completions that start like the prefix are offered;
            // with nothing typed yet, everything.
            .filter(|(_, c)| first.is_none() || c.trigger.chars().next().map(fold) == first)
            .filter_map(|(i, c)| fuzzy_match(prefix, &c.trigger).map(|(score, at)| (score, i, at)))
            .collect();
        // Better matches first; among equals, earlier items (the nearest
        // words, a plugin's own order).
        scored.sort_by_key(|(score, i, _)| (std::cmp::Reverse(*score), *i));
        scored.truncate(MAX_SHOWN);
        self.shown = scored.into_iter().map(|(_, i, at)| (i, at)).collect();
        self.selected = 0;
        !self.shown.is_empty()
    }

    pub fn selected(&self) -> Option<&Completion> {
        self.shown.get(self.selected).map(|(i, _)| &self.items[*i])
    }

    /// Up (`-1`) or down (`1`) the list, wrapping around.
    pub fn step(&mut self, by: isize) {
        let n = self.shown.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
            self.reveal = true;
        }
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word being typed before `pos`: where it starts, and its text.
pub fn prefix_at(b: &Buffer, pos: usize) -> (usize, String) {
    let line_start = b.line_start(b.line_of(pos));
    let before: Vec<char> = b.slice(line_start, pos).chars().collect();
    let n = before.iter().rev().take_while(|c| is_word(**c)).count();
    (pos - n, before[before.len() - n..].iter().collect())
}

/// Words in `b` around `pos`, nearest first, leaving out the one being
/// typed (`skip` is its range).
pub fn buffer_words(b: &Buffer, pos: usize, skip: (usize, usize)) -> Vec<String> {
    let from = pos.saturating_sub(SCAN_CHARS);
    let to = (pos + SCAN_CHARS).min(b.len());
    let mut nearest: HashMap<String, usize> = HashMap::new();
    let mut word = String::new();
    let mut word_start = from;
    let mut finish = |word: &mut String, start: usize, end: usize| {
        let long_enough = word.chars().count() >= MIN_WORD_CHARS;
        if long_enough && !word.chars().all(|c| c.is_ascii_digit()) && (start, end) != skip {
            let distance = if end <= pos { pos - end } else { start.saturating_sub(pos) };
            let best = nearest.entry(std::mem::take(word)).or_insert(distance);
            *best = (*best).min(distance);
        }
        word.clear();
    };
    for (i, c) in b.rope().slice(from..to).chars().enumerate() {
        let at = from + i;
        if is_word(c) {
            if word.is_empty() {
                word_start = at;
            }
            word.push(c);
        } else if !word.is_empty() {
            finish(&mut word, word_start, at);
        }
    }
    if !word.is_empty() {
        finish(&mut word, word_start, to);
    }
    let mut words: Vec<(String, usize)> = nearest.into_iter().collect();
    words.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    words.into_iter().map(|(w, _)| w).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_the_prefix_and_nearby_words() {
        let b = Buffer::new("alpha beta\nalphabet al");
        assert_eq!(prefix_at(&b, b.len()), (20, "al".to_owned()));
        assert_eq!(prefix_at(&b, 5), (0, "alpha".to_owned()));
        let words = buffer_words(&b, b.len(), (20, 22));
        assert_eq!(words, vec!["alphabet", "beta", "alpha"], "nearest first, the typed word left out");
    }

    #[test]
    fn filters_and_ranks_matches() {
        let items = ["format", "for_each", "from", "afor"].iter().map(|w| Completion::word(w)).collect();
        let mut popup = Popup::new(items, 1, 0, "for").unwrap();
        let shown: Vec<&str> = popup.shown.iter().map(|(i, _)| popup.items[*i].trigger.as_str()).collect();
        assert_eq!(shown, vec!["format", "for_each"], "only those starting like the prefix, in order");
        popup.step(-1);
        assert_eq!(popup.selected().unwrap().trigger, "for_each");
        assert!(popup.filter("fe"), "fuzzy: f…e");
        assert!(!popup.filter("format"), "nothing left but what's typed");
        assert!(Popup::new(vec![Completion::word("x")], 1, 0, "y").is_none());
        let all = Popup::new(vec![Completion::word("b"), Completion::word("a")], 1, 0, "").unwrap();
        assert_eq!(all.shown.len(), 2, "nothing typed: everything, in order");
    }

    #[test]
    fn reads_plugin_completions() {
        let snippet = json!({ "trigger": "fn", "annotation": "function", "completion": "fn $1() {}", "format": 1 });
        assert_eq!(Completion::from_plugin(&snippet).unwrap().insert, Insert::Snippet("fn $1() {}".into()));
        let plain = json!({ "trigger": "print" });
        assert_eq!(Completion::from_plugin(&plain).unwrap().insert, Insert::Text("print".into()));
        let command = json!({ "trigger": "go", "completion": "goto", "format": 2, "args": { "x": 1 } });
        assert_eq!(Completion::from_plugin(&command).unwrap().insert, Insert::Command("goto".into(), json!({ "x": 1 })));
        assert!(Completion::from_plugin(&json!({})).is_none());
    }
}
