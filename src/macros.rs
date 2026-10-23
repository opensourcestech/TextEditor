//! Macros: Tools > Record Macro remembers what is typed
//! and done in the editor, Playback Macro does it all again, and Save
//! Macro writes it out as a `.sublime-macro` file. Packages' macro files
//! play the same way.

use eframe::egui::{Key, Modifiers};
use serde_json::{Value, json};

use crate::buffer::Motion;
use crate::editor_view::Command;
use crate::keymap::NamedCommand;

/// One thing a macro does.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Typed text, paired with closing brackets as when typing.
    Type(String),
    /// A key the editor handles itself: arrows, Backspace, Enter, Tab…
    Key(Key, Modifiers),
    Paste(String),
    /// A command, by name.
    Command(NamedCommand),
}

/// A macro file from a package, listed in Tools > Macros.
#[derive(Clone, Debug)]
pub struct MacroFile {
    /// `Package/path/Name.sublime-macro`.
    pub path: String,
    pub steps: Vec<Step>,
}

impl MacroFile {
    /// The file name without `.sublime-macro`.
    pub fn name(&self) -> &str {
        let file = self.path.rsplit('/').next().unwrap_or(&self.path);
        crate::packages::base_name(file)
    }
}

/// Reads a `.sublime-macro` file: a list of commands with their arguments.
pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let json = crate::packages::parse_json(text)?;
    let entries = json.as_array().ok_or("a macro is a JSON list")?;
    Ok(entries
        .iter()
        .filter_map(|e| {
            let name = e.get("command")?.as_str()?.to_owned();
            Some(Step::Command(NamedCommand { name, args: e.get("args").cloned().unwrap_or(Value::Null) }))
        })
        .collect())
}

/// The macro as a `.sublime-macro` file. Keys become the commands Sublime
/// binds them to; ones it has no command for are left out.
pub fn to_json(steps: &[Step]) -> String {
    let insert = |text: &str| json!({ "command": "insert", "args": { "characters": text } });
    let entries: Vec<Value> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Type(text) | Step::Paste(text) => Some(insert(text)),
            Step::Key(key, mods) => key_command(*key, *mods).map(|c| json!({ "command": c.name, "args": c.args })),
            Step::Command(c) => Some(json!({ "command": c.name, "args": c.args })),
        })
        .collect();
    serde_json::to_string_pretty(&entries).unwrap_or_default() + "\n"
}

/// The command a key press in the editor amounts to.
pub fn key_command(key: Key, mods: Modifiers) -> Option<NamedCommand> {
    let mac = cfg!(target_os = "macos");
    // As in the editor: ⌥ moves by word on macOS, Ctrl elsewhere; ⌘ to the ends.
    let word = if mac { mods.alt } else { mods.ctrl };
    let line = mac && mods.mac_cmd;
    let extend = mods.shift;
    let cmd = |name: &str, args: Value| Some(NamedCommand { name: name.to_owned(), args });
    let by = |by: &str, forward: bool| cmd("move", json!({ "by": by, "forward": forward, "extend": extend }));
    let to = |to: &str| cmd("move_to", json!({ "to": to, "extend": extend }));
    match key {
        Key::ArrowLeft if line => to("bol"),
        Key::ArrowRight if line => to("eol"),
        Key::ArrowLeft => by(if word { "words" } else { "characters" }, false),
        Key::ArrowRight => by(if word { "word_ends" } else { "characters" }, true),
        Key::ArrowUp if line => to("bof"),
        Key::ArrowDown if line => to("eof"),
        Key::ArrowUp => by("lines", false),
        Key::ArrowDown => by("lines", true),
        Key::Home => to(if mods.command { "bof" } else { "bol" }),
        Key::End => to(if mods.command { "eof" } else { "eol" }),
        Key::PageUp => by("pages", false),
        Key::PageDown => by("pages", true),
        Key::Backspace if line => cmd("delete_to_line_start", Value::Null),
        Key::Backspace if word => cmd("delete_word", json!({ "forward": false })),
        Key::Backspace => cmd("left_delete", Value::Null),
        Key::Delete if word => cmd("delete_word", json!({ "forward": true })),
        Key::Delete => cmd("right_delete", Value::Null),
        Key::Enter if mods.command && mods.shift => cmd("insert_line_before", Value::Null),
        Key::Enter if mods.command => cmd("insert_line_after", Value::Null),
        Key::Enter => cmd("insert", json!({ "characters": "\n" })),
        Key::Tab if mods.shift => cmd("unindent", Value::Null),
        Key::Tab => cmd("insert", json!({ "characters": "\t" })),
        Key::Escape => cmd("single_selection", Value::Null),
        Key::Z if mods.command && mods.shift => cmd("redo", Value::Null),
        Key::Z if mods.command => cmd("undo", Value::Null),
        Key::Y if mods.command && !mac => cmd("redo", Value::Null),
        Key::A if mods.command => cmd("select_all", Value::Null),
        _ => None,
    }
}

fn command(name: &str, args: Value) -> NamedCommand {
    NamedCommand { name: name.to_owned(), args }
}


/// An editor command by name, as macros save it.
pub fn editor_command(cmd: Command) -> NamedCommand {
    match cmd {
        Command::ExpandToLine => command("expand_selection", json!({ "to": "line" })),
        Command::SplitIntoLines => command("split_selection_into_lines", Value::Null),
        Command::AddNextOccurrence => command("find_under_expand", Value::Null),
        Command::SelectAllOccurrences => command("find_all_under", Value::Null),
        Command::AddCaretAbove => command("select_lines", json!({ "forward": false })),
        Command::AddCaretBelow => command("select_lines", json!({ "forward": true })),
        Command::SingleSelection => command("single_selection", Value::Null),
        Command::DuplicateLine => command("duplicate_line", Value::Null),
        Command::DeleteLine => command("delete_line", Value::Null),
        Command::ToggleComment => command("toggle_comment", json!({ "block": false })),
        Command::Indent => command("indent", Value::Null),
        Command::Outdent => command("unindent", Value::Null),
        Command::InsertLineAfter => command("insert_line_after", Value::Null),
        Command::InsertLineBefore => command("insert_line_before", Value::Null),
    }
}

/// Moving every caret (`extend`ing the selections), as a command.
pub fn motion_command(motion: Motion, extend: bool) -> NamedCommand {
    let by = |by: &str, forward: bool| command("move", json!({ "by": by, "forward": forward, "extend": extend }));
    let to = |to: &str| command("move_to", json!({ "to": to, "extend": extend }));
    match motion {
        Motion::Left => by("characters", false),
        Motion::Right => by("characters", true),
        Motion::WordLeft => by("words", false),
        Motion::WordRight => by("word_ends", true),
        Motion::Up => by("lines", false),
        Motion::Down => by("lines", true),
        Motion::PageUp(_) => by("pages", false),
        Motion::PageDown(_) => by("pages", true),
        Motion::LineStart => to("bol"),
        Motion::LineEnd => to("eol"),
        Motion::DocStart => to("bof"),
        Motion::DocEnd => to("eof"),
    }
}

/// Deleting from every caret to where `motion` moves it.
pub fn delete_command(motion: Motion) -> Option<NamedCommand> {
    Some(match motion {
        Motion::Left => command("left_delete", Value::Null),
        Motion::Right => command("right_delete", Value::Null),
        Motion::WordLeft => command("delete_word", json!({ "forward": false })),
        Motion::WordRight => command("delete_word", json!({ "forward": true })),
        Motion::LineStart => command("delete_to_line_start", Value::Null),
        Motion::LineEnd => command("delete_to_line_end", Value::Null),
        _ => return None,
    })
}

/// Whether the editor itself does something with this key, so recording
/// it is worthwhile.
pub fn is_editor_key(key: Key, mods: Modifiers) -> bool {
    key_command(key, mods).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_macro_files() {
        let steps = parse(
            r#"[
                // Macro files may have comments.
                { "command": "move_to", "args": { "to": "hardeol" } },
                { "command": "insert", "args": { "characters": "\n" } },
                { "args": {} },
            ]"#,
        )
        .unwrap();
        assert_eq!(steps.len(), 2);
        assert!(matches!(&steps[0], Step::Command(c) if c.name == "move_to" && c.args["to"] == "hardeol"));

        let recorded = vec![
            Step::Type("hi".into()),
            Step::Key(Key::Backspace, Modifiers::NONE),
            Step::Key(Key::ArrowLeft, Modifiers::SHIFT),
            Step::Key(Key::F1, Modifiers::NONE),
        ];
        let written: Value = serde_json::from_str(&to_json(&recorded)).unwrap();
        assert_eq!(written.as_array().unwrap().len(), 3, "F1 does nothing in the editor, so it's left out");
        assert_eq!(written[0]["args"]["characters"], "hi");
        assert_eq!(written[1]["command"], "left_delete");
        assert_eq!(written[2]["args"], json!({ "by": "characters", "forward": false, "extend": true }));
        assert!(parse("{}").is_err());

        let file = MacroFile { path: "User/Tidy Up.sublime-macro".into(), steps: Vec::new() };
        assert_eq!(file.name(), "Tidy Up");
    }
}
