//! Key bindings (`.sublime-keymap`) and command palette entries
//! (`.sublime-commands`) from packages. Both name Sublime Text commands,
//! which the app maps onto its own; see `app::translate`.

use std::str::FromStr;

use eframe::egui::{Key, KeyboardShortcut, Modifiers};
use serde_json::Value;
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{Regex, Scope};

/// A command as packages name it, and its arguments, e.g. `toggle_setting` with
/// `{"setting": "word_wrap"}`.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedCommand {
    pub name: String,
    pub args: Value,
}

#[derive(Clone, Debug)]
pub struct Binding {
    /// One key, or a sequence pressed one after another (e.g. ⌘K then ⌘B).
    pub keys: Vec<KeyboardShortcut>,
    pub command: NamedCommand,
    /// All must hold for the binding to apply.
    pub context: Vec<Condition>,
}

/// One `context` entry, e.g. `{"key": "selector", "operand": "source.rust"}`.
#[derive(Clone, Debug)]
pub struct Condition {
    pub key: String,
    /// `equal` (the default), `not_equal`, `regex_match`, `not_regex_match`,
    /// `regex_contains` or `not_regex_contains`.
    pub operator: String,
    /// Defaults to `true`.
    pub operand: Value,
    /// For tests about each selection: whether all must pass, not just one.
    pub match_all: bool,
    /// The operand as a regex, for the regex operators.
    regex: Option<Regex>,
}

impl Condition {
    pub fn new(key: &str, operator: &str, operand: Value, match_all: bool) -> Self {
        let regex = operand.as_str().and_then(|pattern| {
            // `regex_match` must match the whole text; `regex_contains` anywhere in it.
            let pattern = if operator.ends_with("regex_match") { format!(r"\A(?:{pattern})\z") } else { pattern.to_owned() };
            Regex::try_compile(&pattern).is_none().then(|| Regex::new(pattern))
        });
        Self { key: key.to_owned(), operator: operator.to_owned(), operand, match_all, regex }
    }

    /// `equal` or `not_equal` against `value`; false for other operators.
    pub fn compare(&self, value: Value) -> bool {
        match self.operator.as_str() {
            "equal" => value == self.operand,
            "not_equal" => value != self.operand,
            _ => false,
        }
    }

    /// Any operator, against a piece of text.
    pub fn compare_text(&self, text: &str) -> bool {
        let matched = || self.regex.as_ref().is_some_and(|r| r.search(text, 0, text.len(), None));
        match self.operator.as_str() {
            "regex_match" | "regex_contains" => matched(),
            "not_regex_match" | "not_regex_contains" => self.regex.is_some() && !matched(),
            _ => self.compare(Value::String(text.to_owned())),
        }
    }

    /// For `selector`: whether the operand selector matches `scopes`.
    pub fn compare_selector(&self, scopes: &[Scope]) -> bool {
        let selector = self.operand.as_str().and_then(|s| ScopeSelectors::from_str(s).ok());
        let matches = selector.is_some_and(|s| s.does_match(scopes).is_some());
        match self.operator.as_str() {
            "equal" => matches,
            "not_equal" => !matches,
            _ => false,
        }
    }
}

pub struct PaletteEntry {
    pub caption: String,
    pub command: NamedCommand,
}

/// Reads a keymap. Bindings whose keys this platform can't express are left out.
pub fn parse_keymap(text: &str) -> Result<Vec<Binding>, String> {
    let json = crate::packages::parse_json(text)?;
    let entries = json.as_array().ok_or("a keymap is a JSON list")?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let keys: Option<Vec<KeyboardShortcut>> =
                entry.get("keys")?.as_array()?.iter().map(|k| parse_key(k.as_str()?)).collect();
            let keys = keys.filter(|k| !k.is_empty())?;
            let context = entry
                .get("context")
                .and_then(Value::as_array)
                .map(|conditions| {
                    conditions
                        .iter()
                        .filter_map(|c| {
                            Some(Condition::new(
                                c.get("key")?.as_str()?,
                                c.get("operator").and_then(Value::as_str).unwrap_or("equal"),
                                c.get("operand").cloned().unwrap_or(Value::Bool(true)),
                                c.get("match_all").and_then(Value::as_bool).unwrap_or(false),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Binding { keys, command: command(entry)?, context })
        })
        .collect())
}

/// Reads a `.sublime-commands` file: palette captions and their commands.
pub fn parse_commands(text: &str) -> Result<Vec<PaletteEntry>, String> {
    let json = crate::packages::parse_json(text)?;
    let entries = json.as_array().ok_or("a commands file is a JSON list")?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let caption = entry.get("caption")?.as_str()?.to_owned();
            Some(PaletteEntry { caption, command: command(entry)? })
        })
        .collect())
}

fn command(entry: &Value) -> Option<NamedCommand> {
    Some(NamedCommand {
        name: entry.get("command")?.as_str()?.to_owned(),
        args: entry.get("args").cloned().unwrap_or(Value::Null),
    })
}

/// A key as key binding files write it, e.g. `super+shift+d`, `ctrl+alt+up` or
/// `primary+k` (⌘ on macOS, Ctrl elsewhere).
pub fn parse_key(text: &str) -> Option<KeyboardShortcut> {
    let mac = cfg!(target_os = "macos");
    // `+` as the key itself: "ctrl++" or a lone "+".
    let (mods, key) = match text.strip_suffix("++") {
        Some(mods) => (mods, "+"),
        None if text == "+" => ("", "+"),
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };
    let mut modifiers = Modifiers::NONE;
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        modifiers = modifiers.plus(match m {
            "shift" => Modifiers::SHIFT,
            "alt" | "option" => Modifiers::ALT,
            "primary" => Modifiers::COMMAND,
            "super" | "cmd" | "command" if mac => Modifiers::COMMAND,
            // egui treats Ctrl as the command key outside macOS.
            "ctrl" | "control" => if mac { Modifiers::CTRL } else { Modifiers::COMMAND },
            _ => return None,
        });
    }
    let name = match key {
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "pageup" | "page_up" => "PageUp",
        "pagedown" | "page_down" => "PageDown",
        "keypad_enter" => "Enter",
        "forward_slash" => "/",
        "backquote" => "`",
        "plus" => "+",
        "minus" => "-",
        "equals" => "=",
        other => other,
    };
    // egui spells names in title case ("Escape", "F5", "A").
    let mut title = String::with_capacity(name.len());
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        title.extend(first.to_uppercase());
        title.push_str(chars.as_str());
    }
    let key = Key::from_name(&title).or_else(|| Key::from_name(name))?;
    Some(KeyboardShortcut::new(modifiers, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_names() {
        let mac = cfg!(target_os = "macos");
        let cmd = Modifiers::COMMAND;
        assert_eq!(parse_key("primary+shift+d"), Some(KeyboardShortcut::new(cmd.plus(Modifiers::SHIFT), Key::D)));
        assert_eq!(parse_key("alt+up"), Some(KeyboardShortcut::new(Modifiers::ALT, Key::ArrowUp)));
        assert_eq!(parse_key("f5"), Some(KeyboardShortcut::new(Modifiers::NONE, Key::F5)));
        assert_eq!(parse_key("primary+["), Some(KeyboardShortcut::new(cmd, Key::OpenBracket)));
        assert_eq!(parse_key("ctrl++"), Some(KeyboardShortcut::new(if mac { Modifiers::CTRL } else { cmd }, Key::Plus)));
        assert_eq!(parse_key("escape"), Some(KeyboardShortcut::new(Modifiers::NONE, Key::Escape)));
        assert_eq!(parse_key("hyper+x"), None);
        assert_eq!(parse_key("primary+nosuchkey"), None);
        if mac {
            assert_eq!(parse_key("super+k"), Some(KeyboardShortcut::new(cmd, Key::K)));
        }
    }

    #[test]
    fn conditions_compare_values_text_and_scopes() {
        let c = |op: &str, operand: Value| Condition::new("k", op, operand, false);
        assert!(c("equal", Value::Bool(true)).compare(Value::Bool(true)));
        assert!(c("not_equal", Value::from(1)).compare(Value::from(2)));
        assert!(c("regex_contains", "\\w$".into()).compare_text("let a"));
        assert!(!c("regex_contains", "\\w$".into()).compare_text("let a "));
        assert!(c("regex_match", "^\\s*$".into()).compare_text("   "));
        assert!(!c("regex_match", "a".into()).compare_text("ab"), "must match all of it");
        assert!(c("not_regex_match", "a".into()).compare_text("ab"));
        assert!(c("equal", "ab".into()).compare_text("ab"));
        assert!(!c("regex_contains", "(".into()).compare_text("("), "a broken regex never matches");
        let scopes = [Scope::new("source.rust").unwrap(), Scope::new("string.quoted").unwrap()];
        assert!(c("equal", "source.rust string".into()).compare_selector(&scopes));
        assert!(c("not_equal", "comment".into()).compare_selector(&scopes));
    }

    #[test]
    fn reads_keymaps_and_commands() {
        let bindings = parse_keymap(
            r#"[
                { "keys": ["primary+k", "primary+b"], "command": "toggle_side_bar" },
                { "keys": ["f6"], "command": "toggle_setting", "args": { "setting": "word_wrap" },
                  "context": [{ "key": "selector", "operand": "text.html" }] },
                { "keys": ["hyper+x"], "command": "unexpressible" },
                { "keys": ["f7"] },
            ]"#,
        )
        .unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].keys.len(), 2);
        assert_eq!(bindings[1].command.args["setting"], "word_wrap");
        assert_eq!(bindings[1].context[0].operator, "equal");
        assert_eq!(bindings[1].context[0].operand, "text.html");
        assert!(parse_keymap("{}").is_err());

        let entries = parse_commands(r#"[{ "caption": "Wrap: Toggle", "command": "toggle_setting" }, { "caption": "x" }]"#).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command.name, "toggle_setting");
    }
}
