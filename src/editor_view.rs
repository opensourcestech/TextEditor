//! The text area: a custom widget that draws only the visible lines, so files
//! of many megabytes stay fast, and supports any number of cursors.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CursorIcon, Event, EventFilter, FontId, ImeEvent, Key, Modifiers, Pos2, Rect,
    Sense, Stroke, TextFormat, Vec2, pos2, text::LayoutJob, vec2,
    scroll_area::{DragScroll, ScrollSource},
};

use crate::buffer::{Buffer, Edit, EditKind, FieldTransform, Motion, Selection};
use crate::document::Document;
use crate::highlight::{EditorColors, Highlighter, Span, blend};
use crate::minihtml;
use crate::settings::Settings;
use crate::snippets::{self, Snippet};
use crate::wrap::RowMap;

/// Time per frame spent coloring lines far below what was colored so far.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(8);
const BLINK_SECS: f32 = 0.53;
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Editor commands the app triggers from menus, shortcuts or the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    ExpandToLine,
    SplitIntoLines,
    AddNextOccurrence,
    SelectAllOccurrences,
    AddCaretAbove,
    AddCaretBelow,
    SingleSelection,
    DuplicateLine,
    DeleteLine,
    ToggleComment,
    Indent,
    Outdent,
    InsertLineAfter,
    InsertLineBefore,
}

/// What the view needs from the app each frame.
pub struct ViewContext<'a> {
    pub settings: &'a Settings,
    pub highlighter: &'a Highlighter,
    /// The find bar's text and match-case flag, to mark every match.
    pub find: Option<(&'a str, bool)>,
    /// Snippets from packages; Tab after a trigger inserts one.
    pub snippets: &'a [Snippet],
    /// Ranges marked by plugins (their `add_regions`) and Find Results.
    pub highlights: &'a [Highlight],
    /// Plugins' phantoms: notes shown after the end of a line.
    pub phantoms: &'a [Phantom],
}

/// A marked range of text, drawn as the `add_regions` flags say.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
    pub color: Color32,
    pub flags: u32,
}

/// The flags for drawing regions (`DRAW_NO_FILL` and the rest).
pub mod draw {
    pub const EMPTY: u32 = 1;
    pub const EMPTY_AS_OVERWRITE: u32 = 4;
    pub const NO_FILL: u32 = 32;
    pub const HIDDEN: u32 = 128;
    pub const NO_OUTLINE: u32 = 256;
    pub const SOLID_UNDERLINE: u32 = 512;
    pub const STIPPLED_UNDERLINE: u32 = 1024;
    pub const SQUIGGLY_UNDERLINE: u32 = 2048;
}

/// A plugin's phantom: minihtml shown at the end of the line that holds
/// `at` (`LAYOUT_INLINE`), or in rows of its own below that line, starting
/// under `at` (`LAYOUT_BELOW`) or at the left edge (`LAYOUT_BLOCK`).
pub struct Phantom {
    pub id: u64,
    pub at: usize,
    pub content: minihtml::Lines,
    pub layout: PhantomLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhantomLayout {
    Inline,
    Below,
    Block,
}

impl PhantomLayout {
    /// The plugin API's `LAYOUT_*` numbers.
    pub fn from_number(n: u64) -> Self {
        match n {
            1 => Self::Below,
            2 => Self::Block,
            _ => Self::Inline,
        }
    }
}

/// Rows a phantom below a line takes at most.
const MAX_PHANTOM_ROWS: usize = 30;

/// Rows a phantom takes below its line: one per line of its content.
fn phantom_rows(p: &Phantom) -> usize {
    if p.layout == PhantomLayout::Inline { 0 } else { p.content.len().clamp(1, MAX_PHANTOM_ROWS) }
}

/// What happened in the editor this frame that the app acts on.
#[derive(Default)]
pub struct ViewOutput {
    /// The text area, for its right-click menu.
    pub response: Option<egui::Response>,
    /// A link in a phantom was clicked: the phantom, and where it leads.
    pub navigate: Option<(u64, String)>,
    pub double_clicked: bool,
}

/// Where the text was drawn last frame, for popups and plugins.
#[derive(Clone, Copy, Debug)]
pub struct ViewGeometry {
    /// Screen position of the top-left of line 0, which moves when scrolling.
    pub origin: Pos2,
    pub text_left: f32,
    pub row: f32,
    pub space: f32,
}

#[derive(Clone, Copy)]
enum Reveal {
    /// Scroll as little as needed to show the primary caret.
    Nearby,
    /// Put the primary caret in the middle (after Find or Go to Line).
    Center,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

/// A mouse drag in progress.
struct Drag {
    /// Selections kept while dragging (other carets when ⌘-clicking).
    base: Vec<Selection>,
    /// The char, word or line first clicked.
    origin: (usize, usize),
    unit: Unit,
}

/// Per-tab view state: scrolling, mouse and caret blink.
pub struct ViewState {
    /// Widest line seen so far, in columns, for the horizontal scroll range.
    max_columns: usize,
    reveal: Option<Reveal>,
    /// What to bring into view instead of the primary caret.
    reveal_at: Option<usize>,
    /// A scroll position a plugin asked for.
    scroll_to: Option<Vec2>,
    pub geometry: Option<ViewGeometry>,
    drag: Option<Drag>,
    last_click: Option<(Instant, Pos2, u8)>,
    last_activity: Instant,
    /// Text being composed with an input method (e.g. Japanese), not yet typed.
    preedit: String,
    /// Text that was copied as whole lines; pasting it inserts lines above.
    line_copy: Option<String>,
    offset: Vec2,
    viewport: Vec2,
    page_rows: usize,
    /// Screen rows of each line (more than one when word wrap is on).
    rows: RowMap,
}

impl ViewState {
    pub fn new(buffer: &Buffer) -> Self {
        let max_columns = buffer
            .rope()
            .lines()
            .map(|l| columns(l.chars().filter(|&c| c != '\n'), 4))
            .max()
            .unwrap_or(0);
        Self {
            max_columns,
            reveal: None,
            reveal_at: None,
            scroll_to: None,
            geometry: None,
            drag: None,
            last_click: None,
            last_activity: Instant::now(),
            preedit: String::new(),
            line_copy: None,
            offset: Vec2::ZERO,
            viewport: Vec2::ZERO,
            page_rows: 30,
            rows: RowMap::new(),
        }
    }

    /// Scrolls the primary caret into view on the next frame.
    pub fn reveal(&mut self, center: bool) {
        self.reveal = Some(if center { Reveal::Center } else { Reveal::Nearby });
        self.reveal_at = None;
        self.last_activity = Instant::now();
    }

    /// Scrolls char `pos` into view on the next frame, as a plugin's `show` asks.
    pub fn reveal_pos(&mut self, pos: usize, center: bool) {
        self.reveal = Some(if center { Reveal::Center } else { Reveal::Nearby });
        self.reveal_at = Some(pos);
    }

    /// Scrolls to this offset on the next frame.
    pub fn scroll_to(&mut self, offset: Vec2) {
        self.scroll_to = Some(offset);
    }

    /// The scroll offset and the size of the visible area, as of last frame.
    pub fn viewport(&self) -> (Vec2, Vec2) {
        (self.offset, self.viewport)
    }
}

pub fn editor_id(doc_id: u64) -> egui::Id {
    egui::Id::unique(("editor", doc_id))
}

/// Font measurements for one frame. Text is laid out on a monospace grid
/// with tab stops every `tab_size` columns.
pub struct Metrics {
    pub font: FontId,
    pub row: f32,
    pub space: f32,
    ascii: [f32; 128],
    tab: f32,
    ctx: egui::Context,
}

impl Metrics {
    pub fn new(ctx: &egui::Context, settings: &Settings) -> Self {
        let font = FontId::monospace(settings.font_size);
        let mut ascii = [0.0; 128];
        let row = ctx.fonts_mut(|f| {
            for (i, w) in ascii.iter_mut().enumerate() {
                *w = f.glyph_width(&font, i as u8 as char);
            }
            f.row_height(&font)
        });
        let space = ascii[b' ' as usize];
        Self { font, row, space, ascii, tab: space * settings.tab_size as f32, ctx: ctx.clone() }
    }

    /// Width of `c` drawn at `x`; a tab reaches the next tab stop.
    pub fn width(&self, c: char, x: f32) -> f32 {
        if c == '\t' {
            return ((x / self.tab).floor() + 1.0) * self.tab - x;
        }
        match self.ascii.get(c as usize) {
            Some(&w) => w,
            None => self.ctx.fonts_mut(|f| f.glyph_width(&self.font, c)),
        }
    }
}

/// Draws the editor for `doc` and handles its keyboard and mouse input.
pub fn show(ui: &mut egui::Ui, doc: &mut Document, cx: &ViewContext) -> ViewOutput {
    let ctx = ui.ctx().clone();
    let id = editor_id(doc.id);
    let m = Metrics::new(&ctx, cx.settings);
    let colors = *cx.highlighter.colors();
    let is_mac = ctx.os().is_mac();

    let digits = doc.buffer.line_count().to_string().len().max(3);
    let gutter = if cx.settings.line_numbers { (digits as f32 + 3.0) * m.space } else { m.space };
    let text_left = gutter + m.space;
    let wrap = cx.settings.word_wrap.then(|| (ui.available_width() - text_left - 2.0 * m.space).max(10.0 * m.space));
    // Room below lines for phantoms that go there.
    let extra = cx
        .phantoms
        .iter()
        .map(|p| (doc.buffer.line_of(p.at.min(doc.buffer.len())), phantom_rows(p)))
        .filter(|e| e.1 > 0)
        .collect();
    doc.view.rows.set_extra(extra);
    sync_rows(doc, wrap, &m, cx.settings);

    let focused = ctx.memory(|mem| mem.has_focus(id));
    if focused {
        // Keep Tab, arrows and Esc for the editor instead of moving focus.
        let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
        ctx.memory_mut(|mem| mem.set_focus_lock_filter(id, filter));
        handle_keyboard(&ctx, doc, cx, &m, is_mac);
    }
    after_edit(doc, cx.settings);
    sync_rows(doc, wrap, &m, cx.settings);

    // Wrapped text never needs to scroll sideways.
    let area = if wrap.is_some() { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    let mut area = area
        .id_salt(("editor-scroll", doc.id))
        .auto_shrink(false)
        .scroll_source(ScrollSource { drag: DragScroll::Never, ..ScrollSource::ALL });
    if let Some(reveal) = doc.view.reveal.take()
        && let Some(offset) = reveal_offset(doc, &m, reveal, text_left)
    {
        area = area.scroll_offset(offset);
    }
    if let Some(offset) = doc.view.scroll_to.take() {
        area = area.scroll_offset(offset);
    }

    let mut out = ViewOutput::default();
    let output = area.show_viewport(ui, |ui, viewport| {
        let total_rows = doc.view.rows.total_rows(&doc.buffer);
        let content_width = if wrap.is_some() {
            viewport.width()
        } else {
            (text_left + doc.view.max_columns as f32 * m.space + 4.0 * m.space).max(viewport.width())
        };
        let size = vec2(
            content_width,
            // Allow scrolling the last line up to the top.
            total_rows as f32 * m.row + (viewport.height() - 3.0 * m.row).max(0.0),
        );
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        // Registered under the editor's own id: egui drops keyboard focus from
        // any id that no focusable widget claims during the frame.
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let clip = ui.clip_rect();
        let geometry = Geometry { origin: rect.min, text_left, gutter, clip };
        doc.view.geometry = Some(ViewGeometry { origin: rect.min, text_left, row: m.row, space: m.space });
        out.double_clicked = handle_mouse(ui, doc, &response, &geometry, &m, id, is_mac);

        let last_row = ((viewport.max.y / m.row).ceil().max(0.0) as usize).min(total_rows - 1);
        let first_row = ((viewport.min.y / m.row).floor().max(0.0) as usize).min(last_row);
        let first_line = doc.view.rows.line_of_row(first_row).0;
        let last_line = doc.view.rows.line_of_row(last_row).0;
        if !cx.highlighter.prepare(&mut doc.highlight, doc.buffer.rope(), first_line, last_line, HIGHLIGHT_BUDGET) {
            ctx.request_repaint();
        }
        let focused = ctx.memory(|mem| mem.has_focus(id));
        paint(ui, doc, cx, &m, &colors, &geometry, first_row..=last_row, focused);
        out.navigate = draw_phantoms(ui, doc, cx, &m, &colors, &geometry, first_line..=last_line);
        if focused {
            report_ime_position(&ctx, doc, &m, &geometry);
        }
        out.response = Some(response);
        viewport
    });
    doc.view.offset = output.state.clamped_offset();
    doc.view.viewport = output.inner.size();
    doc.view.page_rows = ((output.inner.height() / m.row) as usize).saturating_sub(1).max(1);
    out
}

/// Where char `pos` was drawn last frame: one character cell, on screen.
pub fn char_rect(ctx: &egui::Context, doc: &Document, settings: &Settings, pos: usize) -> Option<Rect> {
    let g = doc.view.geometry?;
    let m = Metrics::new(ctx, settings);
    let (row, x) = doc.view.rows.locate(&doc.buffer, pos.min(doc.buffer.len()), &|c, x| m.width(c, x));
    Some(Rect::from_min_size(pos2(g.origin.x + g.text_left + x, g.origin.y + row as f32 * g.row), vec2(g.space, g.row)))
}

/// The char nearest a point in the text's own coordinates (0, 0 being the
/// top-left of line 0), as plugins' `layout_to_text` asks.
pub fn char_at_layout(ctx: &egui::Context, doc: &Document, settings: &Settings, x: f32, y: f32) -> usize {
    let m = Metrics::new(ctx, settings);
    let row = (y / m.row).floor().max(0.0) as usize;
    doc.view.rows.pos_at(&doc.buffer, row, x, &|c, x| m.width(c, x))
}

/// Where char `pos` is in the text's own coordinates, as plugins'
/// `text_to_layout` asks.
pub fn layout_of_char(ctx: &egui::Context, doc: &Document, settings: &Settings, pos: usize) -> (f32, f32) {
    let m = Metrics::new(ctx, settings);
    let (row, x) = doc.view.rows.locate(&doc.buffer, pos.min(doc.buffer.len()), &|c, x| m.width(c, x));
    (x, row as f32 * m.row)
}

/// The first and last char on screen as of last frame.
pub fn visible_range(doc: &Document) -> (usize, usize) {
    let b = &doc.buffer;
    let Some(g) = doc.view.geometry else {
        return (0, b.len());
    };
    let rows = &doc.view.rows;
    let last = rows.total_rows(b).saturating_sub(1);
    let first_row = ((doc.view.offset.y / g.row).floor().max(0.0) as usize).min(last);
    let last_row = (((doc.view.offset.y + doc.view.viewport.y) / g.row).ceil() as usize).min(last);
    (b.line_start(rows.line_of_row(first_row).0), b.line_end(rows.line_of_row(last_row).0))
}

/// Presses `key` in the editor as if typed, for playing back macros.
pub fn press_key(ctx: &egui::Context, doc: &mut Document, key: Key, mods: Modifiers, cx: &ViewContext) -> bool {
    let m = Metrics::new(ctx, cx.settings);
    let acted = key_press(doc, key, mods, cx, &m, ctx.os().is_mac());
    if acted {
        doc.view.reveal(false);
    }
    acted
}

/// Runs a menu/palette command on the document.
pub fn run(ctx: &egui::Context, doc: &mut Document, cmd: Command, settings: &Settings) {
    let m = Metrics::new(ctx, settings);
    let width = |c: char, x: f32| m.width(c, x);
    let b = &mut doc.buffer;
    match cmd {
        Command::ExpandToLine => b.expand_to_lines(),
        Command::SplitIntoLines => b.split_into_lines(),
        Command::AddNextOccurrence => {
            b.add_next_occurrence();
        }
        Command::SelectAllOccurrences => b.select_all_occurrences(),
        Command::AddCaretAbove => b.add_caret_vertically(false, &width),
        Command::AddCaretBelow => b.add_caret_vertically(true, &width),
        Command::SingleSelection => b.single_selection(),
        Command::DuplicateLine => duplicate(b),
        Command::DeleteLine => delete_lines(b),
        Command::ToggleComment => {
            if let Some(token) = line_comment(&doc.highlight.syntax) {
                toggle_comment(b, token);
            }
        }
        Command::Indent => indent(b, settings),
        Command::Outdent => outdent(b, settings),
        Command::InsertLineAfter => insert_line(b, true, settings, &width),
        Command::InsertLineBefore => insert_line(b, false, settings, &width),
    }
    doc.view.reveal(false);
}

/// Moves every caret by `motion` (extending the selections if `extend`), or
/// with `delete`, deletes to there, as the `move`, `move_to`,
/// `left_delete` and `right_delete` commands do.
pub fn motion(ctx: &egui::Context, doc: &mut Document, motion: Motion, extend: bool, delete: bool, settings: &Settings) {
    let m = Metrics::new(ctx, settings);
    let width = |c: char, x: f32| m.width(c, x);
    let page = doc.view.page_rows;
    let motion = match motion {
        Motion::PageUp(_) => Motion::PageUp(page),
        Motion::PageDown(_) => Motion::PageDown(page),
        other => other,
    };
    if delete {
        doc.buffer.delete(motion, &width);
    } else {
        doc.buffer.move_selections(motion, extend, &width);
    }
    doc.view.reveal(false);
}

/// Where the content sits on screen this frame.
struct Geometry {
    /// Screen position of the top-left of line 0 (moves when scrolling).
    origin: Pos2,
    /// Content x where column 0 starts, after the gutter.
    text_left: f32,
    gutter: f32,
    clip: Rect,
}

impl Geometry {
    fn text_x(&self) -> f32 {
        self.origin.x + self.text_left
    }

    fn row_y(&self, row: usize, m: &Metrics) -> f32 {
        self.origin.y + row as f32 * m.row
    }

    /// The char index under a screen position.
    fn hit(&self, b: &Buffer, rows: &RowMap, p: Pos2, m: &Metrics) -> usize {
        let row = ((p.y - self.origin.y) / m.row).floor().max(0.0) as usize;
        rows.pos_at(b, row, p.x - self.text_x(), &|c, x| m.width(c, x))
    }
}

/// Re-measures wrapped rows for edits since the last call, or everything if
/// the wrap width, font or tab size changed.
fn sync_rows(doc: &mut Document, wrap: Option<f32>, m: &Metrics, settings: &Settings) {
    let splices = doc.buffer.take_line_splices();
    let width = |c: char, x: f32| m.width(c, x);
    doc.view.rows.sync(&doc.buffer, splices, wrap, settings.font_size, settings.tab_size, &width);
}

/// Up/Down/Page keys with word wrap: move by screen row, keeping the x position.
fn move_rows(doc: &mut Document, delta: isize, extend: bool, m: &Metrics, settings: &Settings) {
    sync_rows(doc, doc.view.rows.wrap_width(), m, settings);
    let width = |c: char, x: f32| m.width(c, x);
    let rows = &doc.view.rows;
    doc.buffer.move_with(extend, |b, s| {
        let (row, x) = rows.locate(b, s.head, &width);
        let goal = s.goal.unwrap_or(x);
        let mut target = row as isize + delta;
        // Rows of phantoms below a line have no text to stop in.
        while target > 0 && (target as usize) < rows.total_rows(b) && rows.is_extra_row(target as usize) {
            target += delta.signum();
        }
        let head = if target < 0 {
            0
        } else if target as usize >= rows.total_rows(b) {
            b.len()
        } else {
            rows.pos_at(b, target as usize, goal, &width)
        };
        (head, Some(goal))
    });
}

/// Bookkeeping after the text changed: re-color from the edit down and
/// widen the scroll range for lines that grew.
fn after_edit(doc: &mut Document, settings: &Settings) {
    if let Some(line) = doc.buffer.take_changed_line() {
        doc.highlight.invalidate_from(line);
        let b = &doc.buffer;
        for s in b.selections() {
            let cols = columns(b.line_text(b.line_of(s.head)).chars(), settings.tab_size);
            doc.view.max_columns = doc.view.max_columns.max(cols);
        }
    }
}

/// The scroll offset that brings the primary caret into view, if it isn't.
fn reveal_offset(doc: &Document, m: &Metrics, reveal: Reveal, text_left: f32) -> Option<Vec2> {
    let b = &doc.buffer;
    let at = doc.view.reveal_at.unwrap_or(b.primary().head).min(b.len());
    let (row, x) = doc.view.rows.locate(b, at, &|c, x| m.width(c, x));
    let y = row as f32 * m.row;
    let x = text_left + x;
    let (mut off, view) = (doc.view.offset, doc.view.viewport);
    if view.y <= 0.0 {
        // First frame: no size known yet, so just put the caret near the top.
        return Some(vec2(0.0, (y - 3.0 * m.row).max(0.0)));
    }
    match reveal {
        Reveal::Center => off.y = (y - view.y / 2.0).max(0.0),
        Reveal::Nearby => {
            if y < off.y {
                off.y = y;
            } else if y + m.row > off.y + view.y {
                off.y = y + m.row - view.y;
            }
        }
    }
    // The gutter covers the left part of the viewport.
    if doc.view.rows.wrapping() {
        off.x = 0.0;
    } else if x - text_left < off.x {
        off.x = (x - text_left - 4.0 * m.space).max(0.0);
    } else if x + 2.0 * m.space > off.x + view.x {
        off.x = x + 6.0 * m.space - view.x;
    }
    (off != doc.view.offset).then_some(off)
}

// ----- Input -------------------------------------------------------------

fn handle_keyboard(ctx: &egui::Context, doc: &mut Document, cx: &ViewContext, m: &Metrics, is_mac: bool) {
    let events = ctx.input(|i| i.events.clone());
    for event in events {
        // A read-only view can be read, selected and copied, not changed.
        let event = match event {
            Event::Cut if doc.read_only => Event::Copy,
            e if doc.read_only && changes_text(&e) => continue,
            e => e,
        };
        let acted = match event {
            Event::Text(text) => {
                type_text(&mut doc.buffer, &text, cx.settings);
                true
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => {
                doc.view.preedit = text;
                true
            }
            Event::Ime(ImeEvent::Commit(text)) => {
                doc.view.preedit.clear();
                if !text.is_empty() {
                    type_text(&mut doc.buffer, &text, cx.settings);
                }
                true
            }
            Event::Copy => {
                copy(ctx, doc, false);
                false
            }
            Event::Cut => {
                copy(ctx, doc, true);
                true
            }
            Event::Paste(text) => {
                paste(doc, &text);
                true
            }
            Event::Key { key, pressed: true, modifiers, .. } => key_press(doc, key, modifiers, cx, m, is_mac),
            _ => false,
        };
        if acted {
            doc.view.reveal(false);
        }
    }
}

/// Whether an input event would change the text.
fn changes_text(event: &Event) -> bool {
    match event {
        Event::Text(_) | Event::Paste(_) | Event::Cut | Event::Ime(ImeEvent::Commit(_)) => true,
        Event::Key { key, pressed: true, modifiers, .. } => {
            matches!(key, Key::Backspace | Key::Delete | Key::Enter | Key::Tab)
                || (modifiers.command && matches!(key, Key::Z | Key::Y))
        }
        _ => false,
    }
}

impl Command {
    /// Whether it only moves or adds selections, which read-only views allow.
    pub fn only_selects(self) -> bool {
        matches!(
            self,
            Command::ExpandToLine
                | Command::SplitIntoLines
                | Command::AddNextOccurrence
                | Command::SelectAllOccurrences
                | Command::AddCaretAbove
                | Command::AddCaretBelow
                | Command::SingleSelection
        )
    }
}

fn key_press(doc: &mut Document, key: Key, mods: Modifiers, cx: &ViewContext, m: &Metrics, is_mac: bool) -> bool {
    let settings = cx.settings;
    // Snippets: Tab inserts one after its trigger or moves to the next field,
    // Shift-Tab goes back, Esc stops filling it in.
    if key == Key::Tab && !mods.command && !mods.alt && snippet_tab(doc, cx, mods.shift) {
        return true;
    }
    if key == Key::Escape && doc.buffer.has_fields() {
        doc.buffer.clear_fields();
        return true;
    }
    let width = |c: char, x: f32| m.width(c, x);
    // macOS: ⌥ moves by word and ⌘ to line/file ends. Elsewhere Ctrl moves by word.
    let word = if is_mac { mods.alt } else { mods.ctrl };
    let line = is_mac && mods.mac_cmd;
    let page = doc.view.page_rows;
    if doc.view.rows.wrapping() && !line {
        let rows = match key {
            Key::ArrowUp => Some(-1),
            Key::ArrowDown => Some(1),
            Key::PageUp => Some(-(page as isize)),
            Key::PageDown => Some(page as isize),
            _ => None,
        };
        if let Some(delta) = rows {
            move_rows(doc, delta, mods.shift, m, settings);
            return true;
        }
    }
    let b = &mut doc.buffer;
    let mut mv = |motion| b.move_selections(motion, mods.shift, &width);
    match key {
        Key::ArrowLeft => mv(if line { Motion::LineStart } else if word { Motion::WordLeft } else { Motion::Left }),
        Key::ArrowRight => mv(if line { Motion::LineEnd } else if word { Motion::WordRight } else { Motion::Right }),
        Key::ArrowUp => mv(if line { Motion::DocStart } else { Motion::Up }),
        Key::ArrowDown => mv(if line { Motion::DocEnd } else { Motion::Down }),
        Key::Home => mv(if mods.command { Motion::DocStart } else { Motion::LineStart }),
        Key::End => mv(if mods.command { Motion::DocEnd } else { Motion::LineEnd }),
        Key::PageUp => mv(Motion::PageUp(page)),
        Key::PageDown => mv(Motion::PageDown(page)),
        Key::Backspace => backspace(b, if line { Some(Motion::LineStart) } else if word { Some(Motion::WordLeft) } else { None }, settings),
        Key::Delete => b.delete(if word { Motion::WordRight } else { Motion::Right }, &width),
        Key::Enter if mods.command => insert_line(b, !mods.shift, settings, &width),
        Key::Enter => newline(b, settings),
        Key::Tab if mods.shift => outdent(b, settings),
        Key::Tab => tab(b, settings),
        Key::Escape => b.single_selection(),
        Key::Z if mods.command && mods.shift => {
            b.redo();
        }
        Key::Z if mods.command => {
            b.undo();
        }
        Key::Y if mods.command && !is_mac => {
            b.redo();
        }
        Key::A if mods.command => b.select_all(),
        _ => return false,
    }
    true
}

/// Clicks and drags in the text. Returns true for a double click.
fn handle_mouse(ui: &egui::Ui, doc: &mut Document, response: &egui::Response, g: &Geometry, m: &Metrics, id: egui::Id, is_mac: bool) -> bool {
    let ctx = ui.ctx();
    let (pressed, down, pointer, mods) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers));
    if !down {
        doc.view.drag = None;
    }
    let Some(p) = pointer else {
        return false;
    };
    let mut double = false;
    let in_gutter = p.x < g.clip.left() + g.gutter;
    if response.hovered() && !in_gutter {
        ctx.set_cursor_icon(CursorIcon::Text);
    }

    if pressed && response.hovered() {
        ctx.memory_mut(|mem| mem.request_focus(id));
        let now = Instant::now();
        let count = match doc.view.last_click {
            Some((t, q, n)) if now - t < MULTI_CLICK && q.distance(p) < 4.0 => n % 3 + 1,
            _ => 1,
        };
        doc.view.last_click = Some((now, p, count));
        double = count == 2;
        let unit = match (in_gutter, count) {
            (true, _) | (_, 3) => Unit::Line,
            (_, 2) => Unit::Word,
            _ => Unit::Char,
        };
        let b = &doc.buffer;
        let pos = g.hit(b, &doc.view.rows, p, m);
        // ⌘-click (Ctrl-click elsewhere) adds a caret; Shift-click extends.
        let add = if is_mac { mods.mac_cmd } else { mods.ctrl };
        let (base, origin) = if mods.shift && !add {
            let primary = b.primary();
            let others = b
                .selections()
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != b.primary_index())
                .map(|(_, s)| *s)
                .collect();
            (others, (primary.anchor, primary.anchor))
        } else if add {
            (b.selections().to_vec(), unit_range(b, pos, unit))
        } else {
            (Vec::new(), unit_range(b, pos, unit))
        };
        doc.view.drag = Some(Drag { base, origin, unit });
        drag_to(doc, pos);
        doc.view.last_activity = now;
    } else if down && doc.view.drag.is_some() {
        let pos = g.hit(&doc.buffer, &doc.view.rows, p, m);
        drag_to(doc, pos);
        // Dragging past the edge scrolls.
        if !g.clip.contains(p) {
            ui.scroll_to_rect(Rect::from_center_size(p, vec2(m.space, m.row)), None);
            ctx.request_repaint();
        }
    }
    double
}

fn unit_range(b: &Buffer, pos: usize, unit: Unit) -> (usize, usize) {
    match unit {
        Unit::Char => (pos, pos),
        Unit::Word => b.word_at(pos),
        Unit::Line => b.line_range(b.line_of(pos)),
    }
}

fn drag_to(doc: &mut Document, pos: usize) {
    let Some(drag) = &doc.view.drag else {
        return;
    };
    let (start, end) = unit_range(&doc.buffer, pos, drag.unit);
    let (o_start, o_end) = drag.origin;
    let sel = if start < o_start {
        Selection::new(o_end, start)
    } else {
        Selection::new(o_start, end.max(o_end))
    };
    let mut sels = drag.base.clone();
    sels.push(sel);
    let primary = sels.len() - 1;
    doc.buffer.set_selections(sels, primary);
}

// ----- Editing commands ----------------------------------------------------

fn closing_of(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn snippet_tab(doc: &mut Document, cx: &ViewContext, back: bool) -> bool {
    if back {
        return doc.buffer.next_field(true);
    }
    expand_trigger(doc, cx) || doc.buffer.next_field(false)
}

/// Tab after a snippet's trigger replaces it with the snippet, at every
/// caret if the same trigger is in front of each.
fn expand_trigger(doc: &mut Document, cx: &ViewContext) -> bool {
    let b = &doc.buffer;
    if cx.snippets.is_empty() || b.selections().iter().any(|s| !s.is_empty()) {
        return false;
    }
    let before = |pos: usize| b.slice(b.line_start(b.line_of(pos)), pos);
    let head = b.primary().head;
    // The scope where the trigger starts, so `source.python - string` works.
    let scopes = |len: usize| scopes_at(doc, cx.highlighter, head.saturating_sub(len));
    let Some(snippet) = snippets::find_trigger(cx.snippets, &before(head), &scopes) else {
        return false;
    };
    if !b.selections().iter().all(|s| snippets::ends_with_trigger(&before(s.head), &snippet.trigger)) {
        return false;
    }
    let trigger = snippet.trigger.chars().count();
    insert_snippet(doc, &snippet.content, cx.settings, trigger);
    true
}

/// The scopes at a char position, outermost first: the full stack where it
/// can be worked out quickly, else just the language's.
pub fn scopes_at(doc: &Document, highlighter: &Highlighter, pos: usize) -> Vec<syntect::parsing::Scope> {
    let b = &doc.buffer;
    let line = b.line_of(pos);
    highlighter
        .scopes_at(&doc.highlight, b.rope(), line, pos - b.line_start(line))
        .or_else(|| highlighter.base_scope(doc.syntax()).map(|s| vec![s]))
        .unwrap_or_default()
}

/// Inserts a snippet template at every selection, replacing the selected
/// text (which `$SELECTION` stands for), or the `trigger` chars before each
/// caret. Then selects the first field.
pub fn insert_snippet(doc: &mut Document, template: &str, settings: &Settings, trigger: usize) {
    let b = &doc.buffer;
    let tab = settings.indent_unit();
    let path = doc.path.as_deref();
    let mut edits = Vec::new();
    let mut fields = Vec::new();
    let mut transforms = Vec::new();
    for s in b.selections() {
        let (start, end) = if s.is_empty() { (s.head.saturating_sub(trigger), s.head) } else { (s.start(), s.end()) };
        let line = b.line_of(start);
        let line_text = b.line_text(line);
        let indent: String = line_text.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let selected = b.slice(s.start(), s.end());
        let var = |name: &str| -> Option<String> {
            let file = |f: fn(&std::path::Path) -> Option<String>| path.and_then(f);
            match name {
                "SELECTION" | "TM_SELECTED_TEXT" => Some(selected.clone()),
                "TM_CURRENT_LINE" => Some(line_text.clone()),
                "TM_CURRENT_WORD" => {
                    let (a, z) = b.word_at(start);
                    Some(b.slice(a, z))
                }
                "TM_LINE_INDEX" => Some((start - b.line_start(line)).to_string()),
                "TM_LINE_NUMBER" => Some((line + 1).to_string()),
                "TM_FILENAME" => file(|p| Some(p.file_name()?.to_string_lossy().into_owned())),
                "TM_FILEPATH" => file(|p| Some(p.display().to_string())),
                "TM_DIRECTORY" => file(|p| Some(p.parent()?.display().to_string())),
                "TM_SOFT_TABS" => Some(if settings.translate_tabs_to_spaces { "YES" } else { "NO" }.to_owned()),
                "TM_TAB_SIZE" => Some(settings.tab_size.to_string()),
                _ => None,
            }
        };
        let cx = snippets::Context { var: &var, indent: &indent, tab: &tab };
        let expanded = snippets::expand(template, &cx);
        edits.push(Edit { start, end, text: expanded.text });
        fields.push(expanded.fields);
        let boxed = expanded.transforms.into_iter().map(|t| Box::new(move |text: &str| t.apply(text)) as FieldTransform);
        transforms.push(boxed.collect());
    }
    doc.buffer.insert_snippet(edits, fields, transforms);
    doc.view.reveal(false);
}

/// Inserts text at every caret as if typed: newlines keep the indentation
/// and tabs follow the indentation settings.
pub fn insert_typed(b: &mut Buffer, text: &str, settings: &Settings) {
    let mut plain = String::new();
    for c in text.chars() {
        if c != '\n' && c != '\t' {
            plain.push(c);
            continue;
        }
        if !plain.is_empty() {
            b.insert(&std::mem::take(&mut plain), EditKind::Other);
        }
        if c == '\n' {
            newline(b, settings);
        } else {
            tab(b, settings);
        }
    }
    if !plain.is_empty() {
        b.insert(&plain, EditKind::Other);
    }
}

/// Types text at every caret, pairing brackets and quotes.
pub fn type_text(b: &mut Buffer, text: &str, settings: &Settings) {
    let mut chars = text.chars();
    if let (Some(c), None, true) = (chars.next(), chars.next(), settings.auto_match_enabled) {
        let sels = b.selections().to_vec();
        let all_empty = sels.iter().all(|s| s.is_empty());
        // Typing the closer that is already there just steps over it.
        if all_empty && matches!(c, ')' | ']' | '}' | '"' | '\'' | '`') && sels.iter().all(|s| b.char_at(s.head) == Some(c)) {
            b.move_selections(Motion::Right, false, &|_, _| 0.0);
            return;
        }
        if let Some(close) = closing_of(c) {
            if sels.iter().all(|s| !s.is_empty()) {
                // Wrap each selection.
                let texts = sels
                    .iter()
                    .map(|s| {
                        let inner = b.slice(s.start(), s.end());
                        let n = inner.chars().count();
                        (format!("{c}{inner}{close}"), n + 1)
                    })
                    .collect();
                b.replace_selections(texts, EditKind::Other);
                return;
            }
            let pairs_ok = all_empty
                && sels.iter().all(|s| {
                    let next = b.char_at(s.head);
                    let prev = s.head.checked_sub(1).and_then(|p| b.char_at(p));
                    let next_ok = next.is_none_or(|n| n.is_whitespace() || matches!(n, ')' | ']' | '}' | ',' | ';'));
                    // Don't pair a quote typed right after a word (it's an apostrophe).
                    let prev_ok = c != close || prev.is_none_or(|p| !is_word(p) && p != c);
                    next_ok && prev_ok
                });
            if pairs_ok {
                b.replace_selections(vec![(format!("{c}{close}"), 1); sels.len()], EditKind::Typing);
                return;
            }
        }
    }
    b.insert(text, EditKind::Typing);
}

/// Enter: a new line keeping the indentation, indenting one more after an
/// opening bracket, and splitting `{|}` onto three lines.
pub fn newline(b: &mut Buffer, settings: &Settings) {
    let unit = settings.indent_unit();
    let texts = b
        .selections()
        .iter()
        .map(|s| {
            if !settings.auto_indent {
                return ("\n".to_owned(), 1);
            }
            let line_start = b.line_start(b.line_of(s.start()));
            let before = b.slice(line_start, s.start());
            let indent: String = before.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let prev = before.trim_end().chars().last();
            let next = b.char_at(s.end());
            let opens = prev.filter(|p| matches!(p, '(' | '[' | '{'));
            match opens {
                Some(open) if next == closing_of(open) => {
                    let text = format!("\n{indent}{unit}\n{indent}");
                    (text, 1 + indent.chars().count() + unit.chars().count())
                }
                Some(_) => {
                    let text = format!("\n{indent}{unit}");
                    let n = text.chars().count();
                    (text, n)
                }
                None => {
                    let text = format!("\n{indent}");
                    let n = text.chars().count();
                    (text, n)
                }
            }
        })
        .collect();
    b.replace_selections(texts, EditKind::Other);
}

/// ⌘Enter / ⇧⌘Enter: start a new line below or above, wherever the caret is.
fn insert_line(b: &mut Buffer, after: bool, settings: &Settings, width: &dyn Fn(char, f32) -> f32) {
    if after {
        b.move_selections(Motion::LineEnd, false, width);
        newline(b, settings);
        return;
    }
    let texts: Vec<(String, usize)> = b
        .selections()
        .iter()
        .map(|s| {
            let indent: String =
                b.line_text(b.line_of(s.head)).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let n = indent.chars().count();
            (indent + "\n", n)
        })
        .collect();
    let starts = b.selections().iter().map(|s| Selection::caret(b.line_start(b.line_of(s.head)))).collect();
    b.set_selections(starts, b.primary_index());
    b.replace_selections(texts, EditKind::Other);
}

/// Tab: indent selected lines, or insert a tab / spaces up to the next tab stop.
fn tab(b: &mut Buffer, settings: &Settings) {
    if b.selections().iter().any(|s| b.line_of(s.start()) != b.line_of(s.end())) {
        indent(b, settings);
        return;
    }
    let texts = b
        .selections()
        .iter()
        .map(|s| {
            if !settings.translate_tabs_to_spaces {
                return ("\t".to_owned(), 1);
            }
            let start = b.line_start(b.line_of(s.start()));
            let col = columns(b.slice(start, s.start()).chars(), settings.tab_size);
            let n = settings.tab_size - col % settings.tab_size;
            (" ".repeat(n), n)
        })
        .collect();
    b.replace_selections(texts, EditKind::Typing);
}

fn indent(b: &mut Buffer, settings: &Settings) {
    let unit = settings.indent_unit();
    let edits = b
        .selected_lines()
        .into_iter()
        .filter(|&l| b.line_start(l) != b.line_end(l))
        .map(|l| Edit { start: b.line_start(l), end: b.line_start(l), text: unit.clone() })
        .collect();
    b.apply_edits(edits);
}

fn outdent(b: &mut Buffer, settings: &Settings) {
    let edits = b
        .selected_lines()
        .into_iter()
        .filter_map(|l| {
            let text = b.line_text(l);
            let n = if text.starts_with('\t') {
                1
            } else {
                text.chars().take(settings.tab_size).take_while(|c| *c == ' ').count()
            };
            (n > 0).then(|| Edit { start: b.line_start(l), end: b.line_start(l) + n, text: String::new() })
        })
        .collect();
    b.apply_edits(edits);
}

/// Backspace, also removing an empty bracket pair and a whole soft tab.
pub fn backspace(b: &mut Buffer, motion: Option<Motion>, settings: &Settings) {
    if let Some(motion) = motion {
        b.delete(motion, &|_, _| 0.0);
        return;
    }
    let ranges = b
        .selections()
        .iter()
        .map(|s| {
            if !s.is_empty() {
                return (s.start(), s.end());
            }
            let h = s.head;
            if h == 0 {
                return (0, 0);
            }
            let prev = b.char_at(h - 1).unwrap();
            if settings.auto_match_enabled && closing_of(prev).is_some() && b.char_at(h) == closing_of(prev) {
                return (h - 1, h + 1);
            }
            let start = b.line_start(b.line_of(h));
            let before = b.slice(start, h);
            if settings.translate_tabs_to_spaces && !before.is_empty() && before.chars().all(|c| c == ' ') {
                let n = (before.len() - 1) % settings.tab_size + 1;
                return (h - n, h);
            }
            (h - 1, h)
        })
        .collect();
    b.delete_ranges(ranges);
}

/// ⇧⌘D: duplicate the line (or the selected text).
fn duplicate(b: &mut Buffer) {
    let mut edits: Vec<Edit> = Vec::new();
    let mut seen = HashSet::new();
    for s in b.selections() {
        if s.is_empty() {
            let line = b.line_of(s.head);
            if seen.insert(line) {
                let start = b.line_start(line);
                edits.push(Edit { start, end: start, text: b.line_text(line) + "\n" });
            }
        } else {
            edits.push(Edit { start: s.start(), end: s.start(), text: b.slice(s.start(), s.end()) });
        }
    }
    edits.sort_by_key(|e| e.start);
    edits.dedup_by_key(|e| e.start);
    b.apply_edits(edits);
}

/// ⌃⇧K: delete every line a selection touches.
fn delete_lines(b: &mut Buffer) {
    let mut ranges: Vec<(usize, usize)> = b.selected_lines().into_iter().map(|l| b.line_range(l)).collect();
    // The last line has no newline after it, so take the one before it instead.
    if let Some(last) = ranges.last_mut()
        && last.1 == b.len()
        && last.0 > 0
        && b.char_at(last.1.saturating_sub(1)) != Some('\n')
    {
        last.0 -= 1;
    }
    b.delete_ranges(ranges);
}

/// The line-comment marker for a syntax, if it has one.
pub fn line_comment(syntax: &str) -> Option<&'static str> {
    Some(match syntax {
        "Rust" | "C" | "C++" | "C#" | "Java" | "JavaScript" | "JavaScript (Babel)" | "TypeScript"
        | "TypeScriptReact" | "Go" | "Swift" | "Scala" | "Objective-C" | "Objective-C++" | "PHP" | "Kotlin" | "D"
        | "Groovy" | "JSON" | "Dart" | "Zig" | "Odin" | "Solidity" | "Protocol Buffer" | "GLSL" | "WGSL"
        | "SCSS" | "Less" | "Stylus" | "QML" | "jsonnet" | "F#" | "Typst" | "Verilog" | "SystemVerilog" => "//",
        "Python" | "Ruby" | "Perl" | "Bourne Again Shell (bash)" | "Fish" | "Makefile" | "YAML" | "TOML" | "R"
        | "Git Ignore" | "Git Config" | "Dockerfile" | "CMake" | "Nim" | "Elixir" | "Julia" | "PowerShell"
        | "Crystal" | "CoffeeScript" | "Nix" | "Terraform" | "GraphQL" | "INI" | "DotENV" | "AWK" | "Puppet"
        | "nginx" | "Apache Conf" | "Crontab" | "SSH Config" | "Requirements.txt" | "GDScript (Godot Engine)"
        | "Salt State (SLS)" | "Robot Framework" | "Rego" | "Vyper" | "Ninja" | "gnuplot" | "JQ" | "Tcl" => "#",
        "SQL" | "Lua" | "Haskell" | "Elm" | "Ada" | "VHDL" | "PureScript" | "Idris" | "Cabal" | "Lean 4" => "--",
        "LaTeX" | "TeX" | "Erlang" | "MATLAB" => "%",
        "Lisp" | "Clojure" | "Scheme" | "Racket" | "ARM Assembly" | "x86_64 Assembly" | "LLVM" => ";",
        "VimL" => "\"",
        "Batch File" => "REM",
        _ => return None,
    })
}

/// ⌘/: comment out the selected lines, or uncomment them if all are commented.
pub fn toggle_comment(b: &mut Buffer, token: &str) {
    let lines: Vec<usize> =
        b.selected_lines().into_iter().filter(|&l| !b.line_text(l).trim().is_empty()).collect();
    if lines.is_empty() {
        return;
    }
    let indent_of = |l: usize| b.line_text(l).chars().take_while(|c| c.is_whitespace()).count();
    let commented = lines.iter().all(|&l| b.line_text(l).trim_start().starts_with(token));
    let edits = if commented {
        lines
            .iter()
            .map(|&l| {
                let start = b.line_start(l) + indent_of(l);
                let after = b.line_text(l).chars().nth(indent_of(l) + token.chars().count());
                let len = token.chars().count() + usize::from(after == Some(' '));
                Edit { start, end: start + len, text: String::new() }
            })
            .collect()
    } else {
        // Line the markers up at the least-indented line.
        let col = lines.iter().map(|&l| indent_of(l)).min().unwrap_or(0);
        lines
            .iter()
            .map(|&l| {
                let at = b.line_start(l) + col;
                Edit { start: at, end: at, text: format!("{token} ") }
            })
            .collect()
    };
    b.apply_edits(edits);
}

pub fn copy(ctx: &egui::Context, doc: &mut Document, cut: bool) {
    let b = &mut doc.buffer;
    if b.selections().iter().all(|s| s.is_empty()) {
        // Nothing selected: copy (or cut) the whole lines.
        let lines = b.selected_lines();
        let mut text: String = lines.iter().map(|&l| b.line_text(l) + "\n").collect();
        if text.is_empty() {
            text.push('\n');
        }
        doc.view.line_copy = Some(text.clone());
        ctx.copy_text(text);
        if cut {
            delete_lines(b);
        }
        return;
    }
    let text = b
        .selections()
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| b.slice(s.start(), s.end()))
        .collect::<Vec<_>>()
        .join("\n");
    doc.view.line_copy = None;
    ctx.copy_text(text);
    if cut {
        b.insert("", EditKind::Other);
    }
}

pub fn paste(doc: &mut Document, text: &str) {
    let text = text.replace("\r\n", "\n");
    let b = &mut doc.buffer;
    let all_empty = b.selections().iter().all(|s| s.is_empty());
    if all_empty && doc.view.line_copy.as_deref() == Some(text.as_str()) {
        // Whole lines go above the caret's line, as they were copied.
        let mut starts: Vec<usize> = b.selections().iter().map(|s| b.line_start(b.line_of(s.head))).collect();
        starts.dedup();
        let edits = starts.into_iter().map(|at| Edit { start: at, end: at, text: text.clone() }).collect();
        b.apply_edits(edits);
        return;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let count = b.selections().len();
    if count > 1 && lines.len() == count {
        // One line per caret, e.g. after copying from several carets.
        let texts = lines.iter().map(|l| (l.to_string(), l.chars().count())).collect();
        b.replace_selections(texts, EditKind::Other);
    } else {
        b.insert(&text, EditKind::Other);
    }
}

// ----- Drawing -----------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &egui::Ui,
    doc: &Document,
    cx: &ViewContext,
    m: &Metrics,
    colors: &EditorColors,
    g: &Geometry,
    rows: std::ops::RangeInclusive<usize>,
    focused: bool,
) {
    let painter = ui.painter();
    let b = &doc.buffer;
    let map = &doc.view.rows;
    let settings = cx.settings;
    let width = |c: char, x: f32| m.width(c, x);
    let (first_row, last_row) = (*rows.start(), *rows.end());
    let (first_line, last_line) = (map.line_of_row(first_row).0, map.line_of_row(last_row).0);
    let (vis_start, vis_end) = (b.line_start(first_line), b.line_end(last_line));
    let sels: Vec<Selection> =
        b.selections().iter().filter(|s| s.end() >= vis_start && s.start() <= vis_end).copied().collect();
    let caret_lines: HashSet<usize> = sels.iter().map(|s| b.line_of(s.head)).collect();
    // The screen row of each visible caret.
    let carets: Vec<(usize, usize)> = sels.iter().map(|s| (map.locate(b, s.head, &width).0, s.head)).collect();

    let elapsed = doc.view.last_activity.elapsed().as_secs_f32();
    let caret_on = !settings.caret_blink || ((elapsed / BLINK_SECS) as u32).is_multiple_of(2);
    if focused && settings.caret_blink {
        let next = BLINK_SECS - elapsed % BLINK_SECS;
        ui.ctx().request_repaint_after(Duration::from_secs_f32(next));
    }

    // The line being drawn, measured once even when it spans several rows.
    let mut current: Option<LineInfo> = None;
    for row in rows {
        let (line, k) = map.line_of_row(row);
        if current.as_ref().is_none_or(|c| c.line != line) {
            current = Some(LineInfo::new(b, map, line, cx.find, &width));
        }
        let info = current.as_ref().unwrap();
        // A row of a phantom below the line: drawn with the phantoms.
        if k >= info.starts.len() {
            continue;
        }
        let text = &info.text;
        let (row_start, row_end) = (info.starts[k], info.starts.get(k + 1).copied().unwrap_or(info.char_len));
        let last_row_of_line = k + 1 == info.starts.len();
        let (byte_start, byte_end) = (info.byte_of(row_start), info.byte_of(row_end));
        let y = g.row_y(row, m);
        let row_rect = Rect::from_min_size(pos2(g.clip.left(), y), vec2(g.clip.width(), m.row));
        // x of every char boundary in this row, measured once.
        let xs: Vec<f32> = std::iter::once(0.0)
            .chain(text[byte_start..byte_end].chars().scan(0.0, |x, c| {
                *x += width(c, *x);
                Some(*x)
            }))
            .collect();
        let x_at = |col: usize| g.text_x() + xs[col.saturating_sub(row_start).min(xs.len() - 1)];
        let (abs_start, abs_end) = (info.start + row_start, info.start + row_end);

        let caret_here = carets.iter().any(|&(r, _)| r == row);
        if settings.highlight_line && caret_here && sels.iter().all(|s| s.is_empty()) {
            painter.rect_filled(row_rect, 0.0, colors.line_highlight);
        }
        for h in cx.highlights.iter().filter(|h| h.flags & draw::HIDDEN == 0) {
            if h.start == h.end {
                // An empty region shows only if asked to, as a bar or an underscore.
                let starts_after = h.start > abs_end || (h.start == abs_end && !last_row_of_line);
                if h.flags & (draw::EMPTY | draw::EMPTY_AS_OVERWRITE) == 0 || h.start < abs_start || starts_after {
                    continue;
                }
                let x = x_at(h.start - info.start);
                let stroke = Stroke::new(1.0, h.color);
                if h.flags & draw::EMPTY_AS_OVERWRITE != 0 {
                    painter.line_segment([pos2(x, y + m.row - 1.0), pos2(x + m.space, y + m.row - 1.0)], stroke);
                } else {
                    painter.line_segment([pos2(x, y), pos2(x, y + m.row)], stroke);
                }
                continue;
            }
            let (a, z) = (h.start.max(abs_start), h.end.min(abs_end));
            if a >= z {
                continue;
            }
            let r = Rect::from_min_max(pos2(x_at(a - info.start), y), pos2(x_at(z - info.start), y + m.row));
            paint_region(painter, r, h);
        }
        for &(a, z) in &info.matches {
            let (a, z) = (a.max(row_start), z.min(row_end));
            if a < z {
                let r = Rect::from_min_max(pos2(x_at(a), y), pos2(x_at(z), y + m.row));
                painter.rect_filled(r, 2.0, colors.find_highlight);
            }
        }
        for s in &sels {
            // A selection starting where a wrapped row ends belongs to the next row.
            let starts_after = s.start() > abs_end || (s.start() == abs_end && !last_row_of_line);
            if s.is_empty() || s.end() < abs_start || starts_after {
                continue;
            }
            let x0 = x_at(s.start().max(abs_start) - info.start);
            let mut x1 = x_at(s.end().min(abs_end) - info.start);
            if s.end() > abs_end && last_row_of_line {
                x1 += m.space * 0.6; // the selected newline
            }
            painter.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + m.row)), 2.0, colors.selection);
        }
        // Snippet fields still to fill in are outlined.
        for f in b.fields().iter().filter(|f| f.group != 0 && f.transform.is_none()) {
            let starts_after = f.start > abs_end || (f.start == abs_end && !last_row_of_line);
            if f.end < abs_start || starts_after {
                continue;
            }
            let x0 = x_at(f.start.max(abs_start) - info.start);
            // An empty field still shows, as a thin box.
            let x1 = x_at(f.end.min(abs_end) - info.start).max(x0 + 3.0);
            let r = Rect::from_min_max(pos2(x0 - 1.0, y), pos2(x1 + 1.0, y + m.row));
            painter.rect_stroke(r, 2.0, Stroke::new(1.0, colors.gutter_foreground), egui::StrokeKind::Inside);
        }
        if settings.indent_guides && k == 0 {
            let indent = columns(text.chars().take_while(|c| *c == ' ' || *c == '\t'), settings.tab_size);
            let mut col = settings.tab_size;
            while col < indent {
                let x = g.text_x() + col as f32 * m.space;
                painter.line_segment([pos2(x, y), pos2(x, y + m.row)], Stroke::new(1.0, colors.guide));
                col += settings.tab_size;
            }
        }

        let spans = doc.highlight.spans(line);
        let job = line_job(text, byte_start..byte_end, spans, colors.foreground, &m.font, settings.tab_size);
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        painter.galley(pos2(g.text_x(), y), galley, colors.foreground);

        if focused {
            for &(_, head) in carets.iter().filter(|&&(r, _)| r == row) {
                let x = x_at(head - info.start);
                if caret_on {
                    let caret = Rect::from_min_size(pos2(x - 1.0, y), vec2(2.0, m.row));
                    painter.rect_filled(caret, 0.0, colors.caret);
                }
                if !doc.view.preedit.is_empty() && head == b.primary().head {
                    let r = painter.text(pos2(x, y), egui::Align2::LEFT_TOP, &doc.view.preedit, m.font.clone(), colors.foreground);
                    painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, colors.foreground));
                }
            }
        }
    }

    // The gutter stays put when scrolling sideways, drawn over the text.
    let gutter = Rect::from_min_max(g.clip.left_top(), pos2(g.clip.left() + g.gutter, g.clip.bottom()));
    painter.rect_filled(gutter, 0.0, colors.background);
    if settings.line_numbers {
        for row in first_row..=last_row {
            // Numbers go on a line's first row only, not its wrapped rows.
            let (line, k) = map.line_of_row(row);
            if k != 0 {
                continue;
            }
            let color = if caret_lines.contains(&line) { colors.gutter_active } else { colors.gutter_foreground };
            let pos = pos2(gutter.right() - m.space * 1.5, g.row_y(row, m));
            painter.text(pos, egui::Align2::RIGHT_TOP, (line + 1).to_string(), m.font.clone(), color);
        }
    }
}

/// One row's part of a marked region: filled, outlined and underlined as
/// its flags say. Unstyled regions are filled and outlined.
fn paint_region(painter: &egui::Painter, r: Rect, h: &Highlight) {
    // An opaque color is far too strong as a fill.
    let fill = if h.color.a() == 255 { h.color.gamma_multiply(0.3) } else { h.color };
    if h.flags & draw::NO_FILL == 0 {
        painter.rect_filled(r, 2.0, fill);
    }
    if h.flags & draw::NO_OUTLINE == 0 {
        painter.rect_stroke(r, 2.0, Stroke::new(1.0, h.color), egui::StrokeKind::Inside);
    }
    let stroke = Stroke::new(1.0, h.color);
    let y = r.bottom() - 1.5;
    if h.flags & draw::SQUIGGLY_UNDERLINE != 0 {
        let mut points = Vec::new();
        let mut x = r.left();
        let mut up = true;
        while x <= r.right() {
            points.push(pos2(x, if up { y - 1.5 } else { y + 0.5 }));
            x += 2.0;
            up = !up;
        }
        painter.add(egui::Shape::line(points, stroke));
    } else if h.flags & draw::STIPPLED_UNDERLINE != 0 {
        let mut x = r.left();
        while x < r.right() {
            painter.line_segment([pos2(x, y), pos2((x + 2.0).min(r.right()), y)], stroke);
            x += 4.0;
        }
    } else if h.flags & draw::SOLID_UNDERLINE != 0 {
        painter.line_segment([pos2(r.left(), y), pos2(r.right(), y)], stroke);
    }
}

/// Draws phantoms after the end of their lines, in a box, with clickable
/// links. Returns the phantom and link clicked, if any.
fn draw_phantoms(
    ui: &mut egui::Ui,
    doc: &Document,
    cx: &ViewContext,
    m: &Metrics,
    colors: &EditorColors,
    g: &Geometry,
    lines: std::ops::RangeInclusive<usize>,
) -> Option<(u64, String)> {
    let b = &doc.buffer;
    let width = |c: char, x: f32| m.width(c, x);
    let font = FontId::proportional((m.font.size * 0.9).max(8.0));
    let code_font = FontId::monospace((m.font.size * 0.9).max(8.0));
    let text_color = blend(colors.foreground, colors.background, 0.3);
    let link_color = ui.visuals().hyperlink_color;
    let mut clicked = None;
    // Several phantoms on one line go one after another.
    let mut line_end_x: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    // Phantoms below a line stack up in the rows added for them.
    let mut rows_used: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let map = &doc.view.rows;
    let lay_out = |ui: &egui::Ui, runs: &[&minihtml::Run]| -> Vec<Arc<egui::Galley>> {
        runs.iter()
            .map(|run| {
                let color = if run.link.is_some() { link_color } else { run.color.unwrap_or(text_color) };
                let format = TextFormat {
                    font_id: if run.code { code_font.clone() } else { font.clone() },
                    color,
                    italics: run.italic,
                    underline: if run.link.is_some() { Stroke::new(1.0, color) } else { Stroke::NONE },
                    ..Default::default()
                };
                let mut job = LayoutJob::default();
                job.append(&run.text, 0.0, format);
                ui.ctx().fonts_mut(|f| f.layout_job(job))
            })
            .collect()
    };
    let pad = m.space * 0.5;
    for phantom in cx.phantoms {
        let line = b.line_of(phantom.at.min(b.len()));
        if !lines.contains(&line) {
            continue;
        }
        // Each row of the phantom: its runs, and where the row starts.
        let gap = minihtml::Run { text: "   ".into(), ..Default::default() };
        let (rows, x, first_row): (Vec<Vec<&minihtml::Run>>, f32, usize) = match phantom.layout {
            PhantomLayout::Inline => {
                // All on one row: lines become runs with a gap between them.
                let mut runs: Vec<&minihtml::Run> = Vec::new();
                for (i, l) in phantom.content.iter().enumerate() {
                    if i > 0 {
                        runs.push(&gap);
                    }
                    runs.extend(l.iter());
                }
                let (row, x) = map.locate(b, b.line_end(line), &width);
                (vec![runs], line_end_x.get(&line).copied().unwrap_or(g.text_x() + x + 2.0 * m.space), row)
            }
            layout => {
                let used = rows_used.entry(line).or_default();
                let first = map.first_row(line) + map.text_rows(line) + *used;
                *used += phantom_rows(phantom);
                let x = if layout == PhantomLayout::Block { g.text_x() } else { g.text_x() + map.locate(b, phantom.at.min(b.len()), &width).1 };
                let rows = phantom.content.iter().take(MAX_PHANTOM_ROWS).map(|l| l.iter().collect()).collect();
                (rows, x, first)
            }
        };
        let laid: Vec<Vec<Arc<egui::Galley>>> = rows.iter().map(|runs| lay_out(ui, runs)).collect();
        let widest = laid.iter().map(|r| r.iter().map(|g| g.size().x).sum::<f32>()).fold(0.0, f32::max);
        let top = g.row_y(first_row, m);
        let frame = Rect::from_min_size(pos2(x, top + 1.0), vec2(widest + 2.0 * pad, laid.len().max(1) as f32 * m.row - 2.0));
        ui.painter().rect_filled(frame, 3.0, blend(colors.background, colors.foreground, 0.08));
        for (r, (runs, galleys)) in rows.iter().zip(laid).enumerate() {
            let y = top + r as f32 * m.row;
            let mut left = x + pad;
            for (i, (run, galley)) in runs.iter().zip(galleys).enumerate() {
                let size = galley.size();
                let rect = Rect::from_min_size(pos2(left, y + (m.row - size.y) / 2.0), size);
                if let Some(href) = &run.link {
                    let id = ui.scope_id().with(("phantom-link", phantom.id, r, i));
                    let link = ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                    if link.clicked() {
                        clicked = Some((phantom.id, href.clone()));
                    }
                }
                ui.painter().galley(rect.min, galley, text_color);
                left += size.x;
            }
        }
        if phantom.layout == PhantomLayout::Inline {
            line_end_x.insert(line, frame.right() + m.space);
        }
    }
    clicked
}

/// A line's text and wrap points, kept while drawing its rows.
struct LineInfo {
    line: usize,
    /// Char index where the line starts in the buffer.
    start: usize,
    text: String,
    char_len: usize,
    /// Row start offsets in chars, and the matching byte offsets.
    starts: Vec<usize>,
    start_bytes: Vec<usize>,
    /// Find-bar matches in this line, as char offsets.
    matches: Vec<(usize, usize)>,
}

impl LineInfo {
    fn new(b: &Buffer, map: &RowMap, line: usize, find: Option<(&str, bool)>, width: crate::wrap::Width) -> Self {
        let text = b.line_text(line);
        let starts = map.line_breaks(b, line, width);
        let mut start_bytes = Vec::with_capacity(starts.len());
        let mut next = starts.iter().peekable();
        let mut char_len = 0;
        for (i, (byte, _)) in text.char_indices().enumerate() {
            while next.next_if(|&&s| s == i).is_some() {
                start_bytes.push(byte);
            }
            char_len = i + 1;
        }
        // Rows starting at the very end (an empty line).
        start_bytes.resize(starts.len(), text.len());
        let matches = match find {
            Some((query, case)) if !query.is_empty() => find_matches(&text, query, case),
            _ => Vec::new(),
        };
        Self { line, start: b.line_start(line), text, char_len, starts, start_bytes, matches }
    }

    /// Byte offset of a row boundary (a row start, or the end of the line).
    fn byte_of(&self, col: usize) -> usize {
        match self.starts.iter().position(|&s| s == col) {
            Some(i) => self.start_bytes[i],
            None => self.text.len(),
        }
    }
}

/// Tells the OS where the caret is, so input-method windows appear next to it.
fn report_ime_position(ctx: &egui::Context, doc: &Document, m: &Metrics, g: &Geometry) {
    let (row, x) = doc.view.rows.locate(&doc.buffer, doc.buffer.primary().head, &|c, x| m.width(c, x));
    let cursor = Rect::from_min_size(pos2(g.text_x() + x, g.row_y(row, m)), vec2(1.0, m.row));
    ctx.output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Normal,
            rect: g.clip,
            cursor_rect: cursor,
            should_interrupt_composition: false,
        });
    });
}

/// Part of a line (`range`, in bytes) as colored text, with tabs expanded
/// to the next tab stop.
fn line_job(
    text: &str,
    range: std::ops::Range<usize>,
    spans: Option<Arc<[Span]>>,
    fg: Color32,
    font: &FontId,
    tab_size: usize,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut col = 0;
    let mut push = |piece: &str, color: Color32, italics: bool, underline: bool| {
        let format = TextFormat {
            font_id: font.clone(),
            color,
            italics,
            underline: if underline { Stroke::new(1.0, color) } else { Stroke::NONE },
            ..Default::default()
        };
        if piece.contains('\t') {
            let mut out = String::with_capacity(piece.len() + 8);
            for c in piece.chars() {
                if c == '\t' {
                    let n = tab_size - col % tab_size;
                    out.extend(std::iter::repeat_n(' ', n));
                    col += n;
                } else {
                    out.push(c);
                    col += 1;
                }
            }
            job.append(&out, 0.0, format);
        } else {
            col += piece.chars().count();
            job.append(piece, 0.0, format);
        }
    };
    let usable = spans.filter(|spans| {
        spans.iter().all(|s| s.range.end <= text.len() && text.is_char_boundary(s.range.start) && text.is_char_boundary(s.range.end))
    });
    match usable {
        Some(spans) => {
            let mut at = range.start;
            for s in spans.iter() {
                let (start, end) = (s.range.start.max(range.start), s.range.end.min(range.end));
                if end <= start {
                    continue;
                }
                if start > at {
                    push(&text[at..start], fg, false, false);
                }
                push(&text[start..end], s.color, s.italic, s.underline);
                at = end;
            }
            if at < range.end {
                push(&text[at..range.end], fg, false, false);
            }
        }
        None => push(&text[range], fg, false, false),
    }
    job
}

/// Display width in columns, with tabs reaching the next tab stop.
fn columns(chars: impl Iterator<Item = char>, tab_size: usize) -> usize {
    chars.fold(0, |col, c| if c == '\t' { (col / tab_size + 1) * tab_size } else { col + 1 })
}

/// All matches of `query` in `text`, as (start, end) character indices.
pub fn find_matches(text: &str, query: &str, match_case: bool) -> Vec<(usize, usize)> {
    let fold = |c: char| if match_case { c } else { c.to_lowercase().next().unwrap_or(c) };
    let hay: Vec<char> = text.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    use egui_kittest::Harness;

    struct Fixture {
        doc: Document,
        settings: Settings,
        highlighter: Highlighter,
    }

    /// The real editor widget in a test window, driven by simulated input.
    fn harness(text: &str) -> Harness<'static, Fixture> {
        let mut doc = Document::new_untitled(1);
        doc.buffer = Buffer::new(text);
        let fixture = Fixture { doc, settings: Settings::default(), highlighter: Highlighter::new(crate::highlight::DEFAULT_THEME, &Default::default()) };
        Harness::new_ui_state(
            |ui, f: &mut Fixture| {
                let cx = ViewContext { settings: &f.settings, highlighter: &f.highlighter, find: None, snippets: &[], highlights: &[], phantoms: &[] };
                show(ui, &mut f.doc, &cx);
            },
            fixture,
        )
    }

    /// A narrow window with word wrap on.
    fn wrapped(text: &str) -> Harness<'static, Fixture> {
        let mut doc = Document::new_untitled(1);
        doc.buffer = Buffer::new(text);
        let settings = Settings { word_wrap: true, ..Settings::default() };
        let fixture = Fixture { doc, settings, highlighter: Highlighter::new(crate::highlight::DEFAULT_THEME, &Default::default()) };
        let mut h = Harness::builder().with_size(egui::vec2(320.0, 300.0)).build_ui_state(
            |ui, f: &mut Fixture| {
                let cx = ViewContext { settings: &f.settings, highlighter: &f.highlighter, find: None, snippets: &[], highlights: &[], phantoms: &[] };
                show(ui, &mut f.doc, &cx);
            },
            fixture,
        );
        h.run_steps(2);
        h
    }

    /// Screen position of a row's text, a little in from the left edge.
    fn row_pos(h: &Harness<'_, Fixture>, row: usize) -> Pos2 {
        let m = Metrics::new(&h.ctx, &h.state().settings);
        let gutter = (3.0 + 3.0) * m.space;
        pos2(gutter + m.space + 2.0 * m.space, (row as f32 + 0.5) * m.row)
    }

    /// A tab whose first line has a two-line phantom below it.
    fn with_phantom(wrap: bool) -> Harness<'static, (Fixture, Vec<Phantom>)> {
        let mut doc = Document::new_untitled(1);
        doc.buffer = Buffer::new("one\ntwo\nthree");
        let settings = Settings { word_wrap: wrap, ..Settings::default() };
        let fixture = Fixture { doc, settings, highlighter: Highlighter::new(crate::highlight::DEFAULT_THEME, &Default::default()) };
        let content = minihtml::parse("<b>error</b>: here<br>and <a href='x'>fix</a>");
        let phantoms = vec![Phantom { id: 7, at: 1, content, layout: PhantomLayout::Below }];
        let mut h = Harness::builder().with_size(egui::vec2(500.0, 300.0)).build_ui_state(
            |ui, (f, phantoms): &mut (Fixture, Vec<Phantom>)| {
                let cx = ViewContext { settings: &f.settings, highlighter: &f.highlighter, find: None, snippets: &[], highlights: &[], phantoms };
                show(ui, &mut f.doc, &cx);
            },
            (fixture, phantoms),
        );
        h.run_steps(2);
        h
    }

    #[test]
    fn phantoms_below_a_line_take_rows_of_their_own() {
        for wrap in [false, true] {
            let mut h = with_phantom(wrap);
            let rows = &h.state().0.doc.view.rows;
            let b = &h.state().0.doc.buffer;
            assert_eq!(rows.total_rows(b), 5, "three lines and two phantom rows");
            assert_eq!(rows.first_row(1), 3);
            // A click on the phantom puts the caret at the end of its line.
            let p = row_pos_in(&h, 1);
            click_in(&mut h, p);
            assert_eq!(h.state().0.doc.buffer.primary().head, 3);
            // Down goes past the phantom to the next line.
            h.key_press(Key::ArrowDown);
            h.step();
            let b = &h.state().0.doc.buffer;
            assert_eq!(b.line_of(b.primary().head), 1, "wrap {wrap}");
        }
    }

    fn row_pos_in(h: &Harness<'_, (Fixture, Vec<Phantom>)>, row: usize) -> Pos2 {
        let m = Metrics::new(&h.ctx, &h.state().0.settings);
        let gutter = (3.0 + 3.0) * m.space;
        pos2(gutter + m.space + 30.0 * m.space, (row as f32 + 0.5) * m.row)
    }

    fn click_in<S>(h: &mut Harness<'_, S>, pos: Pos2) {
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.step();
        }
    }

    #[test]
    fn word_wrap_splits_long_lines_into_rows() {
        let h = wrapped(&("word ".repeat(40) + "\nshort"));
        let f = h.state();
        let rows = f.doc.view.rows.total_rows(&f.doc.buffer);
        assert!(rows > 4, "200 chars in a 320 px window should wrap, got {rows} rows");
        // The short line after it is a single row.
        assert_eq!(f.doc.view.rows.line_of_row(rows - 1), (1, 0));
    }

    #[test]
    fn arrow_down_moves_by_screen_row_when_wrapped() {
        let mut h = wrapped(&("word ".repeat(40) + "\nnext"));
        let start = row_pos(&h, 0);
        click(&mut h, start);
        let before = h.state().doc.buffer.primary().head;
        h.key_press(Key::ArrowDown);
        h.step();
        let b = &h.state().doc.buffer;
        let head = b.primary().head;
        assert_eq!(b.line_of(head), 0, "still in the long line, one row down");
        assert!(head > before + 10);
    }

    #[test]
    fn clicking_a_wrapped_row_lands_in_that_row() {
        let mut h = wrapped(&"word ".repeat(40));
        let p = row_pos(&h, 2);
        click(&mut h, p);
        let f = h.state();
        let m = Metrics::new(&h.ctx, &f.settings);
        let width = |c: char, x: f32| m.width(c, x);
        let (row, _) = f.doc.view.rows.locate(&f.doc.buffer, f.doc.buffer.primary().head, &width);
        assert_eq!(row, 2);
    }

    #[test]
    fn typing_in_a_wrapped_line_keeps_rows_in_step() {
        let mut h = wrapped(&"word ".repeat(10));
        let p = row_pos(&h, 0);
        click(&mut h, p);
        for _ in 0..30 {
            h.event(Event::Text("abc ".into()));
            h.step();
        }
        let f = h.state();
        let m = Metrics::new(&h.ctx, &f.settings);
        let width = |c: char, x: f32| m.width(c, x);
        let wrap = f.doc.view.rows.wrap_width();
        let mut fresh = RowMap::new();
        fresh.sync(&f.doc.buffer, Vec::new(), wrap, f.settings.font_size, f.settings.tab_size, &width);
        assert_eq!(f.doc.view.rows.total_rows(&f.doc.buffer), fresh.total_rows(&f.doc.buffer));
    }

    fn click(h: &mut Harness<'_, Fixture>, pos: Pos2) {
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.step();
        }
    }

    #[test]
    fn clicking_then_typing_edits_the_text() {
        let mut h = harness("");
        h.step();
        click(&mut h, pos2(200.0, 10.0));
        h.event(Event::Text("hello".into()));
        h.step();
        // Focus must survive later frames, not just the click.
        h.step();
        h.event(Event::Text(" world".into()));
        h.step();
        assert_eq!(h.state().doc.buffer.text(), "hello world");
    }

    #[test]
    fn keys_move_and_delete_after_clicking() {
        let mut h = harness("abc");
        h.step();
        click(&mut h, pos2(400.0, 10.0)); // past the end of the line
        h.key_press(Key::Backspace);
        h.step();
        h.key_press(Key::ArrowLeft);
        h.step();
        h.event(Event::Text("X".into()));
        h.step();
        h.key_press(Key::Enter);
        h.step();
        assert_eq!(h.state().doc.buffer.text(), "aX\nb");
    }

    #[test]
    fn typing_does_nothing_before_the_editor_is_focused() {
        let mut h = harness("");
        h.step();
        h.event(Event::Text("ignored".into()));
        h.step();
        assert_eq!(h.state().doc.buffer.text(), "");
    }

    fn buf(text: &str, carets: &[usize]) -> Buffer {
        let mut b = Buffer::new(text);
        b.set_selections(carets.iter().map(|&c| Selection::caret(c)).collect(), 0);
        b
    }

    fn carets(b: &Buffer) -> Vec<usize> {
        b.selections().iter().map(|s| s.head).collect()
    }

    #[test]
    fn brackets_pair_and_step_over() {
        let s = Settings::default();
        let mut b = buf("", &[0]);
        type_text(&mut b, "(", &s);
        assert_eq!((b.text().as_str(), carets(&b)), ("()", vec![1]));
        type_text(&mut b, ")", &s);
        assert_eq!((b.text().as_str(), carets(&b)), ("()", vec![2]));
    }

    #[test]
    fn apostrophes_after_words_are_not_paired() {
        let s = Settings::default();
        let mut b = buf("don", &[3]);
        type_text(&mut b, "'", &s);
        assert_eq!(b.text(), "don'");
    }

    #[test]
    fn typing_a_bracket_wraps_selections() {
        let s = Settings::default();
        let mut b = Buffer::new("a b");
        b.set_selections(vec![Selection::new(0, 1), Selection::new(2, 3)], 0);
        type_text(&mut b, "[", &s);
        assert_eq!(b.text(), "[a] [b]");
    }

    #[test]
    fn enter_keeps_indent_and_splits_braces() {
        let s = Settings::default();
        let mut b = buf("    fn x() {}", &[12]);
        newline(&mut b, &s);
        assert_eq!(b.text(), "    fn x() {\n        \n    }");
        assert_eq!(carets(&b), vec![12 + 1 + 8]);
    }

    #[test]
    fn backspace_removes_soft_tabs_and_empty_pairs() {
        let s = Settings::default();
        let mut b = buf("        x", &[8]);
        backspace(&mut b, None, &s);
        assert_eq!(b.text(), "    x");
        let mut b = buf("()", &[1]);
        backspace(&mut b, None, &s);
        assert_eq!(b.text(), "");
    }

    #[test]
    fn tab_inserts_spaces_to_the_next_stop() {
        let s = Settings::default();
        let mut b = buf("ab", &[2]);
        tab(&mut b, &s);
        assert_eq!(b.text(), "ab  ");
    }

    #[test]
    fn indent_and_outdent_selected_lines() {
        let s = Settings::default();
        let mut b = Buffer::new("a\n\nb");
        b.select_all();
        tab(&mut b, &s);
        assert_eq!(b.text(), "    a\n\n    b");
        outdent(&mut b, &s);
        assert_eq!(b.text(), "a\n\nb");
    }

    #[test]
    fn comments_toggle_at_the_shallowest_indent() {
        let mut b = Buffer::new("  a\n    b\n");
        b.set_selections(vec![Selection::new(0, 9)], 0);
        toggle_comment(&mut b, "//");
        assert_eq!(b.text(), "  // a\n  //   b\n");
        toggle_comment(&mut b, "//");
        assert_eq!(b.text(), "  a\n    b\n");
    }

    #[test]
    fn duplicate_and_delete_lines() {
        let mut b = buf("one\ntwo", &[5]);
        duplicate(&mut b);
        assert_eq!(b.text(), "one\ntwo\ntwo");
        assert_eq!(b.line_of(carets(&b)[0]), 2);
        delete_lines(&mut b);
        assert_eq!(b.text(), "one\ntwo");
    }

    #[test]
    fn insert_line_above_keeps_indent() {
        let s = Settings::default();
        let mut b = buf("  x", &[3]);
        insert_line(&mut b, false, &s, &|_, _| 1.0);
        assert_eq!(b.text(), "  \n  x");
        assert_eq!(carets(&b), vec![2]);
    }

    #[test]
    fn line_jobs_expand_tabs_to_stops() {
        let job = line_job("a\tb", 0..3, None, Color32::WHITE, &FontId::monospace(12.0), 4);
        assert_eq!(job.text, "a   b");
        assert_eq!(columns("a\tb".chars(), 4), 5);
    }

    #[test]
    fn finds_matches_as_char_indices() {
        assert_eq!(find_matches("aXa xa", "xa", false), vec![(1, 3), (4, 6)]);
        assert_eq!(find_matches("aXa xa", "xa", true), vec![(4, 6)]);
        assert_eq!(find_matches("héllo héllo", "llo", true), vec![(2, 5), (8, 11)]);
        assert!(find_matches("abc", "", false).is_empty());
    }
}
