//! One menu description, shown as the macOS menu bar or, elsewhere, as a
//! menu bar drawn inside the window.

#[cfg(any(target_os = "macos", test))]
use eframe::egui::Key;
use eframe::egui::{self, Button, KeyboardShortcut};

#[derive(Clone)]
pub struct Menu<A> {
    pub title: String,
    pub items: Vec<Item<A>>,
}

#[derive(Clone)]
pub enum Item<A> {
    Command {
        label: String,
        action: A,
        shortcut: Option<KeyboardShortcut>,
        enabled: bool,
        /// `Some` makes this a check-mark item.
        checked: Option<bool>,
    },
    Separator,
    Submenu {
        label: String,
        items: Vec<Item<A>>,
    },
    /// Standard macOS items (Hide, Services, Minimize…); skipped elsewhere.
    Native(#[cfg_attr(not(target_os = "macos"), allow(dead_code))] NativeItem),
}

#[derive(Clone, Copy, Hash)]
pub enum NativeItem {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
    Fullscreen,
    BringAllToFront,
}

impl<A> Item<A> {
    pub fn cmd(label: impl Into<String>, action: A, shortcut: Option<KeyboardShortcut>) -> Self {
        Item::Command { label: label.into(), action, shortcut, enabled: true, checked: None }
    }

    pub fn check(label: impl Into<String>, action: A, checked: bool) -> Self {
        Item::Command { label: label.into(), action, shortcut: None, enabled: true, checked: Some(checked) }
    }

    pub fn enabled(mut self, on: bool) -> Self {
        if let Item::Command { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    pub fn shortcut(mut self, key: KeyboardShortcut) -> Self {
        if let Item::Command { shortcut, .. } = &mut self {
            *shortcut = Some(key);
        }
        self
    }
}

/// Draws the menus inside the window (used when there is no native menu bar).
pub fn egui_menu_bar<A: Copy>(ui: &mut egui::Ui, menus: &[Menu<A>], actions: &mut Vec<A>) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menus {
            ui.menu_button(&menu.title, |ui| egui_items(ui, &menu.items, actions));
        }
    });
}

/// Draws menu items into an open egui menu (the menu bar's, or a right-click menu).
pub fn egui_items<A: Copy>(ui: &mut egui::Ui, items: &[Item<A>], actions: &mut Vec<A>) {
    for item in items {
        match item {
            Item::Command { label, action, shortcut, enabled, checked } => {
                // Ticked items show highlighted; egui's font has no check-mark glyph.
                let mut button = match checked {
                    Some(on) => Button::selectable(*on, label.as_str()),
                    None => Button::new(label.as_str()),
                };
                if let Some(key) = shortcut {
                    button = button.shortcut_text(ui.ctx().format_shortcut(key));
                }
                if ui.add_enabled(*enabled, button).clicked() {
                    actions.push(*action);
                }
            }
            Item::Separator => {
                ui.separator();
            }
            Item::Submenu { label, items } => {
                ui.menu_button(label, |ui| {
                    egui::ScrollArea::vertical().max_height(500.0).show(ui, |ui| egui_items(ui, items, actions));
                });
            }
            Item::Native(_) => {}
        }
    }
}

/// The macOS menu bar, rebuilt whenever the menu description changes.
#[cfg(target_os = "macos")]
pub struct NativeMenuBar<A> {
    menu: Option<muda::Menu>,
    /// Action for each menu item id ("0", "1", …) of the current menu.
    actions: Vec<A>,
    signature: u64,
    events: std::sync::mpsc::Receiver<muda::MenuEvent>,
}

#[cfg(target_os = "macos")]
impl<A: Copy> NativeMenuBar<A> {
    /// `None` off the main thread (in tests), where AppKit menus can't be made.
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        objc2::MainThreadMarker::new()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        // Menu clicks arrive outside egui, so wake the app up for each one.
        muda::MenuEvent::set_event_handler(Some(move |event| {
            let _ = tx.send(event);
            ctx.request_repaint();
        }));
        Some(Self { menu: None, actions: Vec::new(), signature: 0, events: rx })
    }

    pub fn update(&mut self, menus: &[Menu<A>]) {
        let signature = signature_of(menus);
        if self.menu.is_some() && signature == self.signature {
            return;
        }
        let mut actions = Vec::new();
        let menu = muda::Menu::new();
        for m in menus {
            let sub = muda::Submenu::new(&m.title, true);
            native_items(&sub, &m.items, &mut actions);
            if m.title == "Window" {
                sub.set_as_windows_menu_for_nsapp();
            }
            let _ = menu.append(&sub);
        }
        menu.init_for_nsapp();
        self.menu = Some(menu);
        self.actions = actions;
        self.signature = signature;
    }

    /// Actions picked from the menu bar since the last call.
    pub fn poll(&self) -> Vec<A> {
        self.events
            .try_iter()
            .filter_map(|e| e.id().0.parse::<usize>().ok())
            .filter_map(|i| self.actions.get(i).copied())
            .collect()
    }
}

#[cfg(target_os = "macos")]
fn native_items<A: Copy>(parent: &muda::Submenu, items: &[Item<A>], actions: &mut Vec<A>) {
    use muda::{CheckMenuItem, MenuItem, PredefinedMenuItem as P, Submenu};
    for item in items {
        let _ = match item {
            Item::Command { label, action, shortcut, enabled, checked } => {
                let id = actions.len().to_string();
                actions.push(*action);
                let accel = shortcut.and_then(|s| accelerator(&s));
                match checked {
                    Some(on) => parent.append(&CheckMenuItem::with_id(id, label, *enabled, *on, accel)),
                    None => parent.append(&MenuItem::with_id(id, label, *enabled, accel)),
                }
            }
            Item::Separator => parent.append(&P::separator()),
            Item::Submenu { label, items } => {
                let sub = Submenu::new(label, true);
                native_items(&sub, items, actions);
                parent.append(&sub)
            }
            Item::Native(kind) => parent.append(&match kind {
                NativeItem::About => P::about(None, None),
                NativeItem::Services => P::services(None),
                NativeItem::Hide => P::hide(None),
                NativeItem::HideOthers => P::hide_others(None),
                NativeItem::ShowAll => P::show_all(None),
                NativeItem::Minimize => P::minimize(None),
                NativeItem::Zoom => P::maximize(None),
                NativeItem::Fullscreen => P::fullscreen(None),
                NativeItem::BringAllToFront => P::bring_all_to_front(None),
            }),
        };
    }
}

/// egui shortcut → muda accelerator, e.g. ⇧⌘S → "cmd+shift+KeyS".
#[cfg(target_os = "macos")]
fn accelerator(s: &KeyboardShortcut) -> Option<muda::accelerator::Accelerator> {
    let mut parts = Vec::new();
    if s.modifiers.mac_cmd || s.modifiers.command {
        parts.push("cmd".to_owned());
    }
    if s.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if s.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if s.modifiers.shift {
        parts.push("shift".to_owned());
    }
    parts.push(key_code(s.logical_key)?);
    parts.join("+").parse().ok()
}

#[cfg(any(target_os = "macos", test))]
fn key_code(key: Key) -> Option<String> {
    let name = key.name();
    Some(match key {
        Key::ArrowUp => "ArrowUp".into(),
        Key::ArrowDown => "ArrowDown".into(),
        Key::ArrowLeft => "ArrowLeft".into(),
        Key::ArrowRight => "ArrowRight".into(),
        Key::Backslash => "Backslash".into(),
        Key::Slash => "Slash".into(),
        Key::Comma => "Comma".into(),
        Key::Equals | Key::Plus => "Equal".into(),
        Key::Minus => "Minus".into(),
        Key::OpenBracket => "BracketLeft".into(),
        Key::CloseBracket => "BracketRight".into(),
        Key::Enter => "Enter".into(),
        Key::Tab => "Tab".into(),
        Key::Escape => "Escape".into(),
        _ if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic()) => format!("Key{name}"),
        _ if name.len() == 1 && name.chars().all(|c| c.is_ascii_digit()) => format!("Digit{name}"),
        _ => return None,
    })
}

/// A hash of everything visible in the menus, to know when to rebuild.
#[cfg(target_os = "macos")]
fn signature_of<A>(menus: &[Menu<A>]) -> u64 {
    use std::hash::{Hash, Hasher};
    fn items<A>(h: &mut std::collections::hash_map::DefaultHasher, list: &[Item<A>]) {
        for item in list {
            match item {
                Item::Command { label, shortcut, enabled, checked, .. } => {
                    (0u8, label, enabled, checked).hash(h);
                    shortcut.map(|s| format!("{s:?}")).hash(h);
                }
                Item::Separator => 1u8.hash(h),
                Item::Submenu { label, items: sub } => {
                    (2u8, label).hash(h);
                    items(h, sub);
                }
                Item::Native(kind) => (3u8, kind).hash(h),
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in menus {
        m.title.hash(&mut h);
        items(&mut h, &m.items);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_map_to_accelerator_codes() {
        assert_eq!(key_code(Key::S).as_deref(), Some("KeyS"));
        assert_eq!(key_code(Key::Num0).as_deref(), Some("Digit0"));
        assert_eq!(key_code(Key::Backslash).as_deref(), Some("Backslash"));
        assert_eq!(key_code(Key::ArrowUp).as_deref(), Some("ArrowUp"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shortcuts_parse_as_accelerators() {
        use eframe::egui::Modifiers;
        let save_as = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
        assert!(accelerator(&save_as).is_some());
        let goto_line = KeyboardShortcut::new(Modifiers::CTRL, Key::G);
        assert!(accelerator(&goto_line).is_some());
        let caret = KeyboardShortcut::new(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::ArrowUp);
        assert!(accelerator(&caret).is_some());
    }
}
