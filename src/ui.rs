//! Drawing and input: the text area widget, split views, menus, Goto
//! Anything and the command palette, and plugins' popups.

pub mod editor_view;
pub mod fuzzy;
pub mod layout;
#[cfg(target_os = "macos")]
pub mod mac_open;
pub mod menu;
pub mod minihtml;
pub mod palette;
