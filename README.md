# TextEditor

A fast, native text editor written in Rust. TextEditor focuses on the things
that make a code editor pleasant to live in: multiple cursors, fuzzy
**Goto Anything**, a **command palette**, syntax highlighting for nearly 200
languages, and opening multi-megabyte files without breaking a sweat.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.95+](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
![Platform: macOS | Windows](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey.svg)

---

## Contents

- [Features](#features)
- [Installation](#installation)
- [Getting started](#getting-started)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Settings](#settings)
- [Themes and languages](#themes-and-languages)
- [Packages](#packages)
  - [Python plugins](#python-plugins)
- [Building a macOS app](#building-a-macos-app)
- [Building for Windows](#building-for-windows)
- [Development](#development)
- [Known limitations](#known-limitations)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgements](#acknowledgements)

## Features

**Editing**

- **Multiple cursors** — ⌘-click to add a cursor, ⌘D to select the next
  occurrence, ⌃⌘G to select every occurrence, ⌃⇧↑/↓ to add cursors above or
  below, ⇧⌘L to split a selection into one cursor per line. Typing, deleting,
  copying and pasting all work across every cursor.
- **Smart editing** — auto-closing brackets and quotes, indentation kept on
  Enter, block indent/unindent, toggle line comments for 70+ languages,
  duplicate/delete lines, whole-line copy and paste when nothing is selected.
- **Undo that makes sense** — typing is grouped into natural steps, and
  undoing back to the saved state marks the file clean again.
- **Autocomplete** — as you type a word, a list offers words from your open
  files, snippets and plugins' completions; ↑/↓ to choose, Enter or Tab to
  take one, Esc to dismiss. ⌃Space opens it on demand.
- **Macros** — **Tools → Record Macro** (⌃Q) remembers what you type and do,
  **Playback Macro** (⌃⇧Q) does it again as one undo step, and **Save
  Macro…** keeps it as a `.te-macro` file in **Tools → Macros**.
- **Word wrap**, line numbers, indent guides and current-line highlighting,
  each switchable from the View menu.

**Navigation**

- **Goto Anything (⌘P)** — fuzzy-find any file in the open folder or any open
  tab. Add `:42` to jump straight to a line (`main:42`), or type `:42` alone
  for the current file.
- **Command palette (⇧⌘P)** — every menu command, searchable, with its
  shortcut.
- **Side bar** — open files plus a folder tree; files are indexed in the
  background so large folders never freeze the window.
- **Find** with match highlighting, wrap-around, match case, and *Find All*,
  which turns every match into a cursor.
- **Find in Files (⇧⌘F)** — search the open folder, other folders or the open
  files, as text or a regex, filtered with patterns like `*.rs, -target`.
  Matches are listed in a *Find Results* tab: double-click a line, or press F4
  and ⇧F4, to go to each one. **Replace** changes every match across the files
  (with `$1`-style groups for regexes), leaving them unsaved to review.
- **Split views** — **View → Layout** splits the window into 2–4 columns,
  rows or a grid, each group with its own tabs. Drag the lines between them
  to resize; ⌃1–⌃4 move between groups and ⌃⇧1–⌃⇧4 move a tab to one.
  **File → New View into File** shows a file in a second tab, say beside
  the first; both share its text and undo history.
- **Open Recent** — the last 15 files and folders, kept between launches.

**Built for real work**

- **Large files** — the editor draws only the visible lines, keeps text in a
  rope, and colors code incrementally. A 34 MB, 940,000-line file opens and
  scrolls smoothly.
- **Hot exit** — quit without being nagged; unsaved and untitled tabs come back
  exactly as you left them. The session is also saved continuously, so a crash
  or Force Quit loses at most a second of work.
- **Native macOS integration** — real menu bar; closing the window keeps
  the app running in the Dock until ⌘Q, as Mac apps do; open files from Finder
  ("Open With", double-click, drop on the Dock icon), universal binary for
  Intel and Apple Silicon, input-method support for languages like Japanese
  and Chinese.
- **Packages** — packages add color schemes, languages,
  snippets, key bindings, command palette entries, menus, macros and Python
  plugins, on top of 7 bundled themes and 192 languages. Install packages
  from a package index or a `.te-package` file, switch them on
  and off, or override one file of a package.
- Windows (CRLF) line endings are detected and preserved on save.

## Installation

### Download

Download the latest release from the [Releases](../../releases) page:

- **macOS:** `TextEditor-<version>.dmg`. Open it and drag **TextEditor** into
  **Applications**.
- **Windows:** `TextEditor-<version>-windows.zip`. Extract it and run
  **TextEditor.exe**. Nothing else needs installing.

#### Opening it the first time

TextEditor isn't signed with an Apple or Microsoft developer certificate, so
the first time you open a downloaded copy, the system warns that it can't
check the app. This happens once; after that it opens normally.

**macOS 15 (Sequoia) and later**

1. Open **TextEditor** from Applications. macOS says it can't verify the app;
   click **Done**.
2. Open **System Settings → Privacy & Security** and scroll down to
   **Security**.
3. Next to *"TextEditor" was blocked to protect your Mac*, click
   **Open Anyway**, then confirm with your password or Touch ID.

**macOS 14 (Sonoma) and earlier**

1. In **Applications**, right-click (or Control-click) **TextEditor** and
   choose **Open**.
2. Click **Open** in the dialog.

**If macOS says the app "is damaged" or still won't open**, run this in
Terminal, then open the app again:

```sh
xattr -dr com.apple.quarantine /Applications/TextEditor.app
```

**Windows**

If Windows says *"Windows protected your PC"*, click **More info**, then
**Run anyway**.

### Build from source

Requirements:

- [Rust](https://rustup.rs) **1.95 or newer**
- macOS: the Xcode Command Line Tools (`xcode-select --install`)

```sh
git clone https://github.com/opensourcestech/TextEditor.git
cd TextEditor
cargo run --release
```

To build the `.app` bundle and disk image, see
[Building a macOS app](#building-a-macos-app).

## Getting started

Open files and folders from the **File** menu, by dragging them onto the
window or the Dock icon, with Finder's **Open With**, or from the command line:

```sh
open -a TextEditor .            # open the current folder in the side bar
open -a TextEditor notes.md     # open a file in a tab

# From a source build:
target/release/text_editor src/main.rs README.md
```

A few things worth trying first:

1. **⌘P** and type a few letters of a file name — Goto Anything matches the
   letters of a name in order (`edv` finds `editor_view.rs`).
2. **⇧⌘P** and type `theme` to switch color themes.
3. Put the cursor on a word and press **⌘D** a few times, then type — you are
   now editing every copy at once.
4. Select a few lines and press **⇧⌘L** to put a cursor at the end of each.

## Keyboard shortcuts

Shortcuts are shown for macOS. Every command is also in the menus and the
command palette.

| Files | | Editing | |
|---|---|---|---|
| New file | ⌘N | Undo / Redo | ⌘Z / ⇧⌘Z |
| Open / Open folder | ⌘O / ⇧⌘O | Duplicate line | ⇧⌘D |
| Save / Save as | ⌘S / ⇧⌘S | Delete line | ⌃⇧K |
| Save all | ⌥⌘S | Insert line after / before | ⌘↩ / ⇧⌘↩ |
| Close tab | ⌘W | Indent / Unindent | ⌘] / ⌘[ |
| Next / previous tab | ⌥⌘→ / ⌥⌘← | Toggle comment | ⌘/ |
| Settings | ⌘, | Show completions | ⌃Space |

| Selection and cursors | | Find and go to | |
|---|---|---|---|
| Add cursor | ⌘-click | Find | ⌘F |
| Select next occurrence | ⌘D | Find next / previous | ⌘G / ⇧⌘G |
| Select all occurrences | ⌃⌘G | Find all (in the find bar) | ⌥↩ |
| Add cursor above / below | ⌃⇧↑ / ⌃⇧↓ | Find in files | ⇧⌘F |
| Split selection into lines | ⇧⌘L | Next / previous result | F4 / ⇧F4 |
| Expand selection to line | ⌘L | Goto Anything | ⌘P |
| Back to one cursor | Esc | Go to line | ⌃G |
| | | Command palette | ⇧⌘P |

| View | | Moving around | |
|---|---|---|---|
| Toggle side bar | ⌘\\ | Word left / right | ⌥← / ⌥→ |
| Larger / smaller font | ⌘= / ⌘- | Line start / end | ⌘← / ⌘→ |
| Reset font size | ⌘0 | File start / end | ⌘↑ / ⌘↓ |
| Layout: single / 2, 3, 4 columns | ⌥⌘1 / ⌥⌘2–4 | | |
| Layout: 2, 3 rows / grid | ⇧⌥⌘2–3 / ⌥⌘5 | **Macros** | |
| Focus group 1–4 | ⌃1–⌃4 | Record / stop recording | ⌃Q |
| Move tab to group 1–4 | ⌃⇧1–⌃⇧4 | Play back | ⌃⇧Q |

Double-click selects a word, triple-click a line, and Shift-click extends the
selection. Home toggles between the first non-blank character and column 0.

## Settings

Open **Settings** with ⌘, for the common options, or choose
**TextEditor → Edit settings.json** to edit the file directly. Saving the file
applies it immediately.

| Key | Default | Description |
|---|---|---|
| `theme` | `"base16-ocean.dark"` | Color theme name |
| `font_size` | `14.0` | Editor font size (8–40) |
| `tab_size` | `4` | Width of a tab, in columns |
| `translate_tabs_to_spaces` | `true` | Indent with spaces instead of tabs |
| `word_wrap` | `false` | Wrap long lines at the window edge |
| `line_numbers` | `true` | Show the line-number gutter |
| `highlight_line` | `true` | Highlight the line with the cursor |
| `indent_guides` | `true` | Faint lines at each indentation level |
| `auto_match_enabled` | `true` | Auto-close brackets and quotes |
| `auto_indent` | `true` | Keep indentation when pressing Enter |
| `auto_complete` | `true` | Offer completions while typing a word |
| `caret_blink` | `true` | Blink the cursor |
| `show_sidebar` | `true` | Show the side bar |
| `hot_exit` | `true` | Keep unsaved changes when quitting instead of asking |
| `ignored_packages` | `[]` | Packages that are installed but switched off |
| `python_path` | `""` | Python 3 for plugins; empty finds one |
| `package_channels` | Package Control's | Channels **Install Package…** lists packages from |
| `package_repositories` | `[]` | More repositories of packages, as Package Control has them |

Settings and app data live in one folder:

| Platform | Location |
|---|---|
| macOS | `~/Library/Application Support/TextEditor/` |
| Linux | `~/.config/TextEditor/` |
| Windows | `%APPDATA%\TextEditor\` |

It contains `settings.json`, `recent.json`, the `session/` folder used by hot
exit, the `Packages/` and `Installed Packages/` folders described in
[Packages](#packages), and `Cache/`, which holds prepared package languages
and can be deleted at any time.

## Themes and languages

**Bundled themes:** base16-ocean (dark and light), base16-eighties,
base16-mocha, InspiredGitHub, and Solarized (dark and light). The window
chrome follows the theme, including the macOS title bar.

**Languages:** 192 syntaxes, from the extended set
maintained by [bat](https://github.com/sharkdp/bat) — including Rust, C/C++,
Go, Python, JavaScript, TypeScript/TSX, Swift, Kotlin, Java, C#, Ruby, PHP,
HTML, CSS/SCSS, Vue, Svelte, Markdown, JSON, YAML, TOML, SQL, GraphQL,
Terraform, Nix, Dockerfile, Zig and many more. The language is detected from
the file name or a `#!` line, and can be changed with **View → Syntax**.

More themes and languages come from [packages](#packages).

## Packages

A package is a folder, or a zip file renamed to `.te-package`, that holds
any of these:

| File | What it adds |
|---|---|
| `.te-color-scheme`, `.tmTheme` | A color theme, listed under its file name |
| `.te-syntax` | A language |
| `.te-snippet` | A snippet: type its trigger and press Tab |
| `.te-commands` | Command palette entries |
| `Default.te-keymap`, `Default (OSX).te-keymap` | Key bindings (`(Linux)` / `(Windows)` on those systems) |
| `.te-macro` | A macro, listed in **Tools → Macros** |
| `.te-settings` | Settings that the package's plugins read with `load_settings` |
| `Main.te-menu` | Menu bar items, merged into the menus by `id` (`tools`, `preferences`…) |
| `Context.te-menu` | Items for the editor's right-click menu |
| `Side Bar.te-menu`, `Tab Context.te-menu` | Items for the right-click menus of the side bar's files and folders, and of tabs |
| `dependencies.json` | The libraries the package's plugins import (see below) |
| `.py` at the top of the package | Python plugins: commands and event listeners (`import texteditor`) |

Other files are ignored.

**Sublime Text packages work too.** For compatibility, each `.te-` file has
a `.sublime-` twin that TextEditor reads the same way (`.sublime-package`,
`.sublime-syntax`, `Default.sublime-keymap`…), and Python plugins can
`import sublime` and `sublime_plugin`. So most packages written for Sublime
Text, and those on Package Control, can be used as they are. Packages
written for TextEditor use the `.te-` names and `import texteditor`.

Packages live in two folders inside the settings folder:

| Folder | Holds |
|---|---|
| `Installed Packages/` | Packages as `<Name>.te-package` files (or `.sublime-package`) |
| `Packages/` | Packages as plain folders, including your own `User` folder |

**Managing packages.** Everything is under **TextEditor → Packages** (**File →
Packages** on other systems) and in the command palette (⇧⌘P, type
`package`):

- **Browse Packages…** opens the `Packages` folder in Finder.
- **Install Package…** lists the packages in
  [Package Control](https://packagecontrol.io)'s channel (and any in
  `package_channels` and `package_repositories`), with each one's newest
  release that works here; pick one to download and install it. The list is
  kept for a day, so it opens quickly after the first time.
- **Upgrade Packages** installs a newer release of every package installed
  from the index (or by Sublime Text's Package Control), checking the index
  afresh.
- **Install Missing Libraries** installs the libraries (like `requests` or
  `certifi`) that packages' plugins import, as listed in their
  `dependencies.json`, from Package Control's library channel, choosing the
  release for the Python that runs the plugins. Installing a package from
  the index does this for it too, and the status bar says when a package
  needs libraries that aren't installed. They go in `Lib/python3` in the
  settings folder.
- **Install Package from File…** copies a `.te-package`, `.sublime-package`
  or `.zip` into `Installed Packages/` as `<Name>.te-package`, replacing any
  older copy. Zips downloaded from GitHub
  work too: the `-main` or `-master` suffix is dropped from the name and the
  zip's top-level folder is skipped.
- **Disable Package** / **Enable Package** switch a package off and on without
  deleting it, by adding it to or removing it from `ignored_packages`. If your
  theme comes from a package you switch off, another theme is shown meanwhile,
  and yours comes back when the package does.
- **Remove Package** deletes an installed package's archive.

Changes take effect right away, with no restart. Files added or edited in the
package folders by hand are picked up when the TextEditor window comes back to
the front, or with **Reload Packages**.

**Your own additions.** Put files anywhere in `Packages/User/`, which loads
last, so its files win. For example, to rebind keys, create
`Packages/User/Default (OSX).te-keymap`:

```json
[
    { "keys": ["super+shift+d"], "command": "find_under_expand" },
    { "keys": ["f6"], "command": "toggle_setting", "args": { "setting": "word_wrap" } },
    { "keys": ["super+k", "super+b"], "command": "toggle_side_bar" },
    { "keys": ["super+alt+t"], "command": "insert_snippet", "args": { "contents": "TODO(${1:me}): $0" } }
]
```

**Overriding a file.** A file in `Packages/<Name>/` replaces the file at the
same path inside the package's archive, so you can change one theme of an
installed package without unpacking it. Packages load in alphabetical order
with `User` last; a later theme, language, snippet trigger or key binding
wins over an earlier one.

**Snippets.** Type a snippet's `tabTrigger` and press Tab. With several
cursors, it is inserted at each. The fields still to fill in are outlined;
Tab and Shift-Tab move between them (`$1`, `${2:placeholder}`), fields with
the same number are typed into together, and the caret ends at `$0`; Esc
stops early. Variables such as `$TM_FILENAME`, `$TM_LINE_NUMBER`,
`$SELECTION` and `${NAME:default}` work, and so do transforms like
`${1/(.*)/\U$1/}`, which update as you type (`$1` groups, `\u` `\l` `\U`
`\L` `\E` case changes, `(?1:yes:no)`, and the `g` and `i` options). A
snippet's `<scope>` is matched against the scopes where the trigger starts,
so `source.python - string` keeps a snippet out of strings. Snippets also
appear in the command palette as *Snippet: …*.

**Key bindings and commands.** Bindings, palette entries and menu items name
commands. TextEditor has these (the same names as Sublime Text's, so its
packages' bindings work too):
opening, saving and closing files; find; Goto Anything and the command palette
(`show_overlay`); tabs; undo, clipboard and selection commands; line
duplicate/delete/insert (`duplicate_line`, `delete_line`, `insert_line_after`,
`insert_line_before`, `delete_to_line_start`, `delete_to_line_end`); indent and comment;
font size; `toggle_setting` for its own settings; `move`, `move_to`,
`left_delete`, `right_delete` and `delete_word`; output panels
(`show_panel`); `insert`; `insert_snippet`; `save_all`; Find in Files
(`show_panel` with `find_in_files`, `next_result`, `prev_result`); layouts
and groups (`set_layout`, `focus_group`, `move_to_group`); completions
(`auto_complete`, `commit_completion`, `hide_auto_complete`); macros
(`toggle_record_macro`, `run_macro`, `save_macro`, `run_macro_file`);
`edit_settings` (a package's settings beside your own copy;
the layout goes back once both are closed); `clone_file`; `new_window` and
`close_window`; `install_package`, `upgrade_all_packages` and
`satisfy_libraries`;
`open_file` and `open_url`; and every command of the
[Python plugins](#python-plugins) that are running. Bindings for any other
command are skipped. Two-key sequences like `["super+k", "super+b"]` work.

Menu items from `.te-menu` (and `.sublime-menu`) files run commands the same way. In
the side bar's menu, a command's `paths`, `files` and `dirs` arguments are
filled in with the file or folder clicked, and in a tab's menu its `group`
and `index`. Items whose command TextEditor already has a menu item for are
left out, and so are the menus of Sublime Text's own `Default` package,
which describe menus TextEditor already has.

A binding's `context` conditions work with the `equal`,
`not_equal`, `regex_match`, `not_regex_match`, `regex_contains` and
`not_regex_contains` operators and `match_all`. They can test the text
around each cursor (`preceding_text`, `following_text`, `text`), the scope
there (`selector`, `eol_selector`), `selection_empty`, `num_selections`,
`has_next_field`, `panel`, `panel_visible`, `panel_has_focus`, `overlay`,
`overlay_visible`, `auto_complete_visible`, `popup_visible`,
`is_recording_macro` and `setting.<name>`; plugins answer any other condition
through `on_query_context`. When a binding takes a key that a menu item uses,
the menu item loses it; when it gives a menu command a new key, the menu
shows that key.

### Writing your own package

Make a folder in `Packages/`, say `Packages/MyTools/`, and put files in it.
It loads at once (or with **Reload Packages**). For example, a command
with a key, a palette entry and a menu item:

`my_tools.py`:

```python
import texteditor


class InsertDateCommand(texteditor.TextCommand):
    def run(self, edit):
        import datetime
        for region in self.view.sel():
            self.view.insert(edit, region.begin(), datetime.date.today().isoformat())
```

`Default.te-keymap`:

```json
[{ "keys": ["primary+shift+t"], "command": "insert_date" }]
```

`MyTools.te-commands`:

```json
[{ "caption": "MyTools: Insert Date", "command": "insert_date" }]
```

`Main.te-menu`:

```json
[{ "id": "tools", "children": [{ "caption": "Insert Date", "command": "insert_date" }] }]
```

Commands are named after their class: `InsertDateCommand` is
`insert_date`. To share the package, zip the folder's contents and rename
the zip to `MyTools.te-package`; **Install Package from File…** installs it.

### Python plugins

Python plugins are the `.py` files at the top of a package. They're
imported in a separate process, the plugin host, where `import texteditor`
gives them the API: commands, event listeners, views, regions, settings and
the rest. Sublime Text's plugins use the same API under the names `sublime`
and `sublime_plugin`, which TextEditor also has. A package's plugins can
import each other (`from .helpers import x`), from a folder or a zipped
package.

They need Python 3.8 or newer. TextEditor uses the first it finds (Homebrew,
python.org, or the Xcode command line tools on macOS; `python3` elsewhere),
or the one set in `python_path`. Packages without plugins never start Python.

**What works:**

- `TextCommand`, `WindowCommand` and `ApplicationCommand`, run from key
  bindings, `.te-commands` palette entries, or other plugins. A text
  command's edits undo in one step. Commands with input handlers ask for
  their arguments with a quick or input panel.
- `EventListener` and `ViewEventListener` with `on_new`, `on_load`,
  `on_activated`, `on_deactivated`, `on_pre_save` (formatters can change the
  text before it's written), `on_post_save`, `on_modified`,
  `on_selection_modified`, `on_close`, `on_query_context` and
  `on_query_completions`, plus their `_async` versions.
- The API for reading and changing text and selections, `find` and
  `find_all`, lines and words, scopes and selectors, view and package
  settings (`load_settings`, `save_settings`), resources, the clipboard,
  dialogs, `show_quick_panel`, `show_input_panel`, output panels,
  `set_status`, `status_message`, `open_file`, `set_timeout` and more.
- **Regions** from `add_regions` follow edits, and are drawn as their flags
  say (filled, outlined, solid, stippled or squiggly underlines, hidden) in
  the theme's color for their scope.
- **View settings** a plugin sets, like `tab_size`, `translate_tabs_to_spaces`,
  `word_wrap` or `draw_indent_guides`, change how that view is edited and
  shown.
- **Popups** (`show_popup`, `update_popup`, `hide_popup`) and **phantoms**
  (`add_phantom`, `PhantomSet`; at the end of the line, or with
  `LAYOUT_BELOW` and `LAYOUT_BLOCK` in rows of their own under it) show
  minihtml — bold, italics,
  code, colors, lists and clickable links that call `on_navigate`.
- **Completions** from `on_query_completions`: strings, `[trigger, snippet]`
  pairs, `CompletionItem`s (text, snippet and command completions) and
  `CompletionList`s, including ones filled in later with `set_completions`,
  with the flags that leave out word or snippet completions.
- **Windows**: TextEditor has one window, so when a plugin opens a new one
  (as diff tools do), your tabs, layout and side bar are set aside until its
  tabs are closed, or **File → Close Window**. Plugins can name tabs
  (`set_name`) and make them read-only (`set_read_only`), and key bindings'
  `setting.<name>` conditions see settings plugins set on a view.
- Commands can take as long as they need: after half a second the editor
  stops waiting and lets them finish in the background, with a spinner in
  the status bar.
- **Layouts and groups**: `set_layout`, `get_layout`, `num_groups`,
  `focus_group`, `views_in_group`, `get_view_index`, `set_view_index` and
  the rest, plus the view's geometry (`visible_region`, `viewport_position`,
  `text_to_layout`, `line_height`…) and `show` / `show_at_center`.

**Plugin console.** What plugins `print`, and their errors, go to
**Packages → Show Plugin Console**. When a plugin fails to load, the status
bar says so.

**Not supported:** the minimap, folding, hover events, and Sublime Text's
private `sublime_api` module; these are present so plugins load,
but do nothing (or, for `sublime_api`, fail when called). Libraries in
`Lib/python3` or `Lib/python38` in the settings folder can be imported; see
**Install Missing Libraries** under [Packages](#packages).

### More about packages

**Languages load in the background.** Adding package languages means
preparing every language again, which takes a moment, so it happens on a
background thread and the result is saved in `Cache/` in the settings folder.
Later launches with the same packages load it straight from there.

**Metadata.** If a package has a `package-metadata.json` (as packages
installed from the index, or by Sublime Text's Package Control, do), its `version` is shown next to the
package's name in the menus.

**Problems.** A package or file that can't be read is skipped, and the first
problem is shown in the status bar. Some newer `.te-syntax`/`.sublime-syntax` files
use features (such as `extends`) that the highlighting engine doesn't support
yet; those files are reported and skipped while the rest of the package loads.

## Building a macOS app

`packaging/macos/bundle.sh` builds a universal (Intel + Apple Silicon)
`dist/TextEditor.app` and `dist/TextEditor-<version>.dmg`:

```sh
packaging/macos/bundle.sh             # ad-hoc signed, runs on this Mac
packaging/macos/bundle.sh --install   # …and copy it to /Applications
```

The script builds the editor once for each architecture, so the first build
takes a while. You don't need an Apple Developer account to build the app for
yourself: an app you build opens on your own Mac without Gatekeeper warnings,
because macOS only checks apps that were downloaded.

The macOS and Windows apps share one icon, kept ready-made in the repository:
`packaging/macos/AppIcon.icns` for macOS, and `packaging/windows/AppIcon.ico`
and `AppIcon.png` for Windows.

To distribute builds that open without Gatekeeper warnings, sign them with a
Developer ID and have Apple notarize them. Store notarization credentials once:

```sh
xcrun notarytool store-credentials texteditor-notary \
  --apple-id you@example.com --team-id TEAMID
```

then build:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_PROFILE=texteditor-notary \
packaging/macos/bundle.sh
```

The script signs with the hardened runtime, notarizes and staples both the app
and the disk image.

## Building for Windows

`packaging/windows/build.sh` builds `dist/TextEditor.exe` for 64-bit Windows,
and `dist/TextEditor-<version>-windows.zip` to share it. It runs on macOS or
Linux and needs the MinGW-w64 cross-compiler:

```sh
brew install mingw-w64
packaging/windows/build.sh
```

The `.exe` is a single file that runs on Windows 10 and later without
installing anything else. It has the same icon as the macOS app, built in by
`build.rs`.

## Development

```sh
cargo run -- some/file.rs   # debug build
cargo test                  # unit and UI tests
cargo clippy --all-targets
```

Dependencies are optimized even in debug builds (see `Cargo.toml`), so syntax
highlighting stays fast while developing; the first build takes a while.

The test suite includes UI tests that drive the real app with simulated
clicks and keystrokes using [`egui_kittest`](https://crates.io/crates/egui_kittest).
They run headless and use a temporary settings folder, so they never touch
your own settings or session. To see the app as the tests do,
`cargo test screenshots -- --ignored` renders pictures of split views,
menus, Find in Files, autocomplete and plugins' phantoms and popups into
the folder in `TEXTEDITOR_SCREENSHOTS` (or the temp folder).

### Project layout

The source is grouped into folders by area:

```
src/
  main.rs       startup
  app.rs, app/  the application window and what it puts on screen
  text/         the text model
  ui/           drawing and input
  language/     highlighting, autocomplete and snippets
  packages/     packages and the files they hold
  plugins/      Python plugins
  workspace/    the open folder, and what's kept between runs
```

| File | Purpose |
|---|---|
| `src/main.rs` | Startup and window options |
| `src/app.rs` | The application: tabs, menus, find bar, side bar, settings window, session |
| `src/app/groups.rs`, `src/ui/layout.rs` | Split views: layouts, groups and their tabs |
| `src/app/find_in_files.rs`, `src/workspace/search.rs` | Find in Files: the panel, searching, Find Results, replace |
| `src/app/autocomplete.rs`, `src/language/completion.rs` | Autocomplete: the list and where its completions come from |
| `src/app/macro_ui.rs`, `src/packages/macros.rs` | Recording, playing and saving macros |
| `src/app/package_menus.rs`, `src/packages/menu_file.rs` | Menus from packages |
| `src/app/package_index_ui.rs`, `src/packages/package_index.rs` | Install Package… and Upgrade Packages from Package Control's index |
| `src/app/plugin_api.rs` | The editor's answers to the plugin API |
| `src/text/buffer.rs` | Text model: rope, multiple selections, undo/redo |
| `src/text/document.rs` | One open file: loading, saving, line endings |
| `src/text/wrap.rs` | Word wrap: line-to-row mapping, updated incrementally on edits |
| `src/ui/editor_view.rs` | The text area widget: drawing, keyboard, mouse, clipboard, IME |
| `src/ui/menu.rs` | One menu definition, shown as the macOS menu bar or in-window menus |
| `src/ui/palette.rs`, `src/ui/fuzzy.rs` | Goto Anything / command palette and fuzzy matching |
| `src/ui/minihtml.rs` | minihtml, for plugins' popups and phantoms |
| `src/ui/mac_open.rs` | Files opened from Finder and the Dock |
| `src/language/highlight.rs` | Incremental syntax highlighting and themes |
| `src/language/color_scheme.rs` | The `.te-color-scheme` format |
| `src/language/snippets.rs` | Snippet files and template expansion |
| `src/packages.rs` | Packages: finding, reading, installing and removing them |
| `src/packages/keymap.rs` | Key binding and command palette files |
| `src/plugins.rs`, `src/plugins/plugin_host/` | Python plugins: the host process and its `texteditor` API |
| `src/workspace.rs` | Open folder: side bar tree and background file index |
| `src/workspace/settings.rs`, `src/workspace/recent.rs`, `src/workspace/session.rs` | Settings, recent files, hot exit |
| `packaging/macos/` | App bundle and disk-image build, and the app icon (`AppIcon.icns`) |
| `packaging/windows/` | Windows `.exe` and zip build, and the app icon (`AppIcon.ico`, `AppIcon.png`) |
| `build.rs` | Builds the icon and version details into the Windows `.exe` |

### Design notes

- **Text model.** Text lives in a [`ropey`](https://crates.io/crates/ropey)
  rope, so edits anywhere in a large file are cheap. Selections are kept
  sorted and merged; every edit is recorded so undo restores both text and
  cursors.
- **Drawing.** The editor is a custom egui widget that lays out only the rows
  on screen. Syntax highlighting saves parser state every 32 lines, so an
  edit re-colors from the nearest checkpoint rather than the top of the file,
  and far-away regions are colored a little at a time.
- **Word wrap.** Each edit reports exactly which lines it replaced; only those
  are re-measured.
- **Menus.** A single menu description drives the macOS menu bar, the
  in-window menus on other platforms, the command palette and the keyboard
  shortcuts, so they can never disagree.

## Known limitations

- **Platforms.** macOS is the supported and tested platform. The code is
  cross-platform (egui/eframe) and has in-window menus for other systems, but
  Linux and Windows builds have not been tested yet.
- Only **UTF-8** files can be opened.
- Text is laid out on a fixed-width grid, so very wide characters (CJK, emoji)
  can make columns line up slightly off.
- Undo history is not kept across restarts (unsaved text is).
- Turning word wrap on, or resizing the window with wrap on, re-measures the
  whole file: instant for normal files, about a second for a 30 MB file.
- Plugins get the API described under [Python plugins](#python-plugins),
  and they need a Python 3.8+ on the computer. Libraries with compiled code
  need a release built for that Python's version.
- Installing from the package index uses `curl`, which macOS, Windows 10 and
  later, and most Linux systems have.

## Contributing

Contributions are welcome — bug reports, ideas and pull requests.

1. Open an issue describing the bug or feature before large changes.
2. Keep pull requests focused, and add a test for the behavior you change.
   UI behavior can be tested with `egui_kittest`; see the tests in
   `src/app.rs` and `src/ui/editor_view.rs`.
3. Make sure `cargo test` and `cargo clippy --all-targets` pass cleanly.

Unless you explicitly state otherwise, any contribution you submit for
inclusion in this project shall be dual licensed as below, without any
additional terms or conditions.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

TextEditor bundles third-party components under their own licenses, all
compatible with the above. The bundled syntax definitions and themes, which
come from Sublime Text's open-source packages, bat and syntect, carry their
own licenses; see the [`two-face`](https://crates.io/crates/two-face) and
[`syntect`](https://crates.io/crates/syntect) crates for details.

## Acknowledgements

TextEditor is built on excellent open-source work:

- [egui / eframe](https://github.com/emilk/egui) — immediate-mode GUI
- [syntect](https://github.com/trishume/syntect) and
  [two-face](https://github.com/CosmicHorrorDev/two-face) — syntax highlighting
  and the extended language set from [bat](https://github.com/sharkdp/bat)
- [ropey](https://github.com/cessen/ropey) — rope text storage
- [muda](https://github.com/tauri-apps/muda) — native macOS menus
- [rfd](https://github.com/PolyMeilex/rfd) — native file dialogs
- [objc2](https://github.com/madsmtm/objc2) — macOS integration

TextEditor can use Sublime Text's packages. It is
an independent project written from scratch; it is not affiliated with or
endorsed by Sublime HQ, and "Sublime Text" is a trademark of Sublime HQ Pty Ltd.
