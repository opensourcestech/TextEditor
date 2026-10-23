//! Menus from packages, as in Sublime Text: `Main.sublime-menu` adds to the
//! menu bar, `Context.sublime-menu` to the editor's right-click menu, and
//! `Side Bar.sublime-menu` and `Tab Context.sublime-menu` to those of the
//! side bar's files and folders and of tabs.
//! Each is a tree of captions and commands; an item with an `id` merges
//! into the item of the same id, which is how packages add to the
//! standard menus (`"id": "tools"`, `"id": "package-settings"`…).

use serde_json::Value;

use crate::packages::keymap::NamedCommand;

/// The menu files, by name: `Main.te-menu` (or `Main.sublime-menu`)…
pub const MAIN: &str = "Main";
pub const CONTEXT: &str = "Context";
pub const SIDE_BAR: &str = "Side Bar";
pub const TAB_CONTEXT: &str = "Tab Context";

/// Fills in the arguments for the item clicked: those of
/// `values` the command's arguments name, like `paths` in a side bar menu.
pub fn with_args(command: &NamedCommand, values: &[(&str, Value)]) -> NamedCommand {
    let mut command = command.clone();
    if let Value::Object(args) = &mut command.args {
        for (key, value) in values {
            if args.contains_key(*key) {
                args.insert((*key).to_owned(), value.clone());
            }
        }
    }
    command
}

/// Only the items whose commands `runs` says can run, and the submenus
/// left with any.
pub fn keep(items: &[MenuItem], runs: &dyn Fn(&NamedCommand) -> bool) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::new();
    for item in items {
        match item {
            MenuItem::Separator => {
                if !out.is_empty() && out.last() != Some(&MenuItem::Separator) {
                    out.push(MenuItem::Separator);
                }
            }
            MenuItem::Entry { caption, id, command, children } => {
                let children = keep(children, runs);
                let runnable = command.as_ref().is_some_and(runs);
                if runnable || !children.is_empty() {
                    out.push(MenuItem::Entry { caption: caption.clone(), id: id.clone(), command: command.clone(), children });
                }
            }
        }
    }
    while out.last() == Some(&MenuItem::Separator) {
        out.pop();
    }
    out
}

/// Draws menu items into an open egui menu, noting the command clicked.
pub fn show(ui: &mut eframe::egui::Ui, items: &[MenuItem], clicked: &mut Option<NamedCommand>) {
    for item in items {
        match item {
            MenuItem::Separator => {
                ui.separator();
            }
            MenuItem::Entry { caption, children, .. } if !children.is_empty() => {
                ui.menu_button(caption, |ui| show(ui, children, clicked));
            }
            MenuItem::Entry { caption, command: Some(command), .. } => {
                if ui.button(caption).clicked() {
                    *clicked = Some(command.clone());
                }
            }
            MenuItem::Entry { .. } => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    Separator,
    Entry {
        caption: String,
        id: Option<String>,
        command: Option<NamedCommand>,
        children: Vec<MenuItem>,
    },
}

/// Reads a menu file.
pub fn parse(text: &str) -> Result<Vec<MenuItem>, String> {
    let json = crate::packages::parse_json(text)?;
    let entries = json.as_array().ok_or("a menu is a JSON list")?;
    Ok(entries.iter().filter_map(item).collect())
}

fn item(entry: &Value) -> Option<MenuItem> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    let caption = text("caption");
    if caption.as_deref() == Some("-") {
        return Some(MenuItem::Separator);
    }
    let command = text("command").map(|name| NamedCommand { name, args: entry.get("args").cloned().unwrap_or(Value::Null) });
    let children: Vec<MenuItem> =
        entry.get("children").and_then(Value::as_array).map(|c| c.iter().filter_map(item).collect()).unwrap_or_default();
    let id = text("id");
    // Plugins can name their own items; this is the closest without asking.
    // An item that is only there to merge into others needs no caption.
    let caption = caption
        .or_else(|| command.as_ref().map(|c| humanize(&c.name)))
        .or_else(|| id.as_ref().map(|id| humanize(&id.replace('-', "_"))))?;
    if command.is_none() && children.is_empty() && id.is_none() {
        return None;
    }
    Some(MenuItem::Entry { caption, id, command, children })
}

/// `toggle_word_count` → "Toggle Word Count".
fn humanize(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Adds `items` to `into`: an item whose `id` is already there merges its
/// children into that one, anything else goes at the end.
pub fn merge(into: &mut Vec<MenuItem>, items: Vec<MenuItem>) {
    for item in items {
        let existing = match &item {
            MenuItem::Entry { id: Some(id), .. } => {
                into.iter_mut().find(|i| matches!(i, MenuItem::Entry { id: Some(other), .. } if other == id))
            }
            _ => None,
        };
        match (existing, item) {
            (Some(MenuItem::Entry { children, .. }), MenuItem::Entry { children: more, .. }) => merge(children, more),
            (_, item) => into.push(item),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_menus_and_merges_by_id() {
        let mut tree = parse(
            r#"[
                { "id": "tools", "caption": "Tools", "children": [
                    { "caption": "Build", "command": "build" },
                    { "caption": "-" },
                ] },
                { "id": "preferences", "children": [{ "id": "package-settings", "caption": "Package Settings", "children": [] }] },
            ]"#,
        )
        .unwrap();
        let more = parse(
            r#"[
                { "id": "tools", "children": [{ "command": "count_words" }] },
                { "id": "preferences", "children": [{ "id": "package-settings", "children": [
                    { "caption": "Linter", "children": [{ "caption": "Settings", "command": "edit_settings", "args": { "base_file": "x" } }] },
                ] }] },
                { "caption": "Nothing to do" },
            ]"#,
        )
        .unwrap();
        assert_eq!(more.len(), 2, "an item with no command, children or id is dropped");
        merge(&mut tree, more);
        assert_eq!(tree.len(), 2);
        let MenuItem::Entry { children: tools, .. } = &tree[0] else { panic!() };
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[1], MenuItem::Separator);
        assert!(matches!(&tools[2], MenuItem::Entry { caption, .. } if caption == "Count Words"));
        let MenuItem::Entry { children: prefs, .. } = &tree[1] else { panic!() };
        let MenuItem::Entry { children: settings, .. } = &prefs[0] else { panic!() };
        assert!(matches!(&settings[0], MenuItem::Entry { caption, .. } if caption == "Linter"));
    }

    #[test]
    fn side_bar_items_get_the_paths_and_unrunnable_ones_go() {
        let items = parse(
            r#"[
                { "caption": "Copy Name", "command": "copy_name", "args": { "paths": [] } },
                { "caption": "-" },
                { "caption": "More", "children": [{ "caption": "Gone", "command": "nope" }] },
            ]"#,
        )
        .unwrap();
        let kept = keep(&items, &|c| c.name != "nope");
        assert_eq!(kept.len(), 1, "the separator goes with the empty submenu after it");
        let MenuItem::Entry { command: Some(command), .. } = &kept[0] else { panic!() };
        let filled = with_args(command, &[("paths", serde_json::json!(["/a"])), ("group", 1.into())]);
        assert_eq!(filled.args, serde_json::json!({ "paths": ["/a"] }), "only arguments it names");
    }
}
