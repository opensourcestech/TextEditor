//! Split views: the editor area divided into groups, each with its own tabs
//! and one of them in front (see `crate::layout`).

use eframe::egui::{self, CursorIcon, Rect, Sense, UiBuilder, pos2};

use std::collections::BTreeMap;

use super::{Action, EditorApp};
use crate::editor_view::{self, ViewContext, ViewOutput};
use crate::layout::{self, Layout};

/// A window set aside when a plugin opened a new one. This editor has one
/// window, so `new_window` puts its tabs, layout and side bar aside; they
/// come back when the new window's tabs are all closed, or on
/// `close_window`.
pub(super) struct Stash {
    layout: Layout,
    groups: Vec<layout::Group>,
    focus_group: usize,
    active: u64,
    sidebar: Option<bool>,
}

impl EditorApp {
    /// Whether a tab belongs to a window set aside.
    pub(super) fn stashed(&self, id: u64) -> bool {
        self.windows.iter().any(|w| w.groups.iter().any(|g| g.views.contains(&id)))
    }

    /// A plugin's `new_window`: an empty window, the current one set aside.
    pub(super) fn new_window(&mut self) {
        let active = self.docs[self.active].id;
        self.windows.push(Stash {
            layout: std::mem::take(&mut self.layout),
            groups: std::mem::replace(&mut self.groups, vec![layout::Group::default()]),
            focus_group: std::mem::take(&mut self.focus_group),
            active,
            sidebar: self.sidebar_override.take(),
        });
    }

    /// `close_window`: closes the window's tabs, which brings back the one
    /// set aside. The editor's own window isn't closed this way.
    pub(super) fn close_window(&mut self, ctx: &egui::Context) {
        if self.windows.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.groups.iter().flat_map(|g| g.views.clone()).collect();
        for id in ids {
            if let Some(i) = self.docs.iter().position(|d| d.id == id) {
                self.close(ctx, i);
            }
        }
        // A window that had no tabs left to close.
        if self.groups.iter().all(|g| g.views.is_empty()) && !self.windows.is_empty() {
            self.pop_window();
        }
    }

    /// The window set aside last comes back.
    pub(super) fn pop_window(&mut self) {
        let Some(stash) = self.windows.pop() else {
            return;
        };
        let ids: Vec<u64> = self.docs.iter().map(|d| d.id).collect();
        let open: Vec<u64> = self.groups.iter().flat_map(|g| g.views.clone()).collect();
        self.layout = stash.layout;
        self.groups = stash.groups;
        for group in &mut self.groups {
            group.views.retain(|v| ids.contains(v));
        }
        // Anything still open in the closed window joins the one coming back.
        self.focus_group = stash.focus_group.min(self.groups.len() - 1);
        self.groups[self.focus_group].views.extend(open);
        self.sidebar_override = stash.sidebar;
        if let Some(i) = self.docs.iter().position(|d| d.id == stash.active) {
            self.active = i;
        }
        self.sync_groups();
    }

    /// The layout and groups to save in the session: the editor's own
    /// window, with tabs of windows plugins opened added to its last group.
    pub(super) fn session_groups(&self) -> (Layout, Vec<layout::Group>) {
        let Some(base) = self.windows.first() else {
            return (self.layout.clone(), self.groups.clone());
        };
        let mut groups = base.groups.clone();
        let later = self.windows[1..].iter().flat_map(|w| w.groups.iter()).chain(self.groups.iter());
        let last = groups.len() - 1;
        groups[last].views.extend(later.flat_map(|g| g.views.iter().copied()));
        (base.layout.clone(), groups)
    }

    /// The group a tab is in.
    pub(super) fn group_of(&self, id: u64) -> Option<usize> {
        self.groups.iter().position(|g| g.views.contains(&id))
    }

    /// Keeps every tab in exactly one group and each group's front tab
    /// valid. New tabs join the focused group. Called every frame, and after
    /// tabs open or close.
    pub(super) fn sync_groups(&mut self) {
        // While starting up there may be no tabs yet.
        if self.docs.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.docs.iter().map(|d| d.id).collect();
        for group in &mut self.groups {
            let gone: Vec<u64> = group.views.iter().filter(|v| !ids.contains(v)).copied().collect();
            for id in gone {
                group.remove(id);
            }
        }
        self.focus_group = self.focus_group.min(self.groups.len() - 1);
        for &id in &ids {
            if self.group_of(id).is_none() && !self.stashed(id) {
                self.groups[self.focus_group].views.push(id);
            }
        }
        let active = self.docs[self.active].id;
        if let Some(g) = self.group_of(active) {
            self.groups[g].active = Some(active);
            // Focus follows the active tab, unless an empty group was clicked
            // to open the next file there.
            if !self.groups[self.focus_group].views.is_empty() {
                self.focus_group = g;
            }
        }
        for group in &mut self.groups {
            if group.active.is_none_or(|a| !group.views.contains(&a)) {
                group.active = group.views.first().copied();
            }
        }
    }

    /// File > New View into File: another tab showing the active tab's text.
    pub(super) fn clone_active_view(&mut self, ctx: &egui::Context) {
        self.sync_clones();
        let id = self.take_id();
        let mut doc = self.docs[self.active].clone_view(id);
        doc.view.reveal(true);
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        self.sync_groups();
        self.sync_clones();
        self.focus_editor(ctx);
    }

    /// Tabs showing the same text, by family, where there is more than one.
    fn families(&self) -> BTreeMap<u64, Vec<usize>> {
        let mut families: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (i, doc) in self.docs.iter().enumerate() {
            families.entry(doc.family).or_default().push(i);
        }
        families.retain(|_, members| members.len() > 1);
        families
    }

    /// Keeps tabs that show the same text in step: the copies replay the
    /// changes made through the tab holding the text, and the active tab
    /// becomes the one holding it, so it's what gets edited and undone.
    pub(super) fn sync_clones(&mut self) {
        let families = self.families();
        for doc in &mut self.docs {
            if !families.contains_key(&doc.family) {
                // Alone again: no copies to keep changes for.
                doc.holds_text = true;
                doc.buffer.set_logging(false);
            }
        }
        for members in families.values() {
            let holder = members.iter().copied().find(|&i| self.docs[i].holds_text).unwrap_or(members[0]);
            self.docs[holder].holds_text = true;
            self.docs[holder].buffer.set_logging(true);
            let changes = self.docs[holder].buffer.take_log();
            for &i in members.iter().filter(|&&i| i != holder) {
                let (from, to) = two_mut(&mut self.docs, holder, i);
                to.holds_text = false;
                to.buffer.replay(&changes);
                // A copy edited directly (it shouldn't be) starts over.
                if to.buffer.len() != from.buffer.len() {
                    let sels = to.buffer.selections().to_vec();
                    to.buffer = from.buffer.snapshot();
                    to.buffer.set_selections(sels, 0);
                }
                to.buffer.follow(&from.buffer);
            }
            if members.contains(&self.active) && self.active != holder {
                self.hold_text(self.active);
            }
        }
    }

    /// Makes tab `index` the one of its family holding the text, so it can
    /// be edited. Its copies are brought up to date first.
    pub(super) fn hold_text(&mut self, index: usize) {
        if self.docs[index].holds_text {
            return;
        }
        let family = self.docs[index].family;
        let Some(holder) = self.docs.iter().position(|d| d.family == family && d.holds_text) else {
            self.docs[index].holds_text = true;
            return;
        };
        let changes = self.docs[holder].buffer.take_log();
        for doc in self.docs.iter_mut().filter(|d| d.family == family && !d.holds_text) {
            doc.buffer.replay(&changes);
        }
        let (from, to) = two_mut(&mut self.docs, holder, index);
        from.buffer.trade_views(&mut to.buffer);
        std::mem::swap(&mut from.buffer, &mut to.buffer);
        from.holds_text = false;
        to.holds_text = true;
        to.buffer.set_logging(true);
        from.buffer.set_logging(false);
        from.buffer.follow(&to.buffer);
    }

    /// Brings a tab to the front of its group and makes it the active one.
    pub(super) fn activate(&mut self, index: usize) {
        self.active = index;
        if let Some(g) = self.group_of(self.docs[index].id) {
            self.focus_group = g;
            self.groups[g].active = Some(self.docs[index].id);
        }
    }

    /// Switches to a layout. Tabs of groups that go away join the last one left.
    pub(super) fn set_layout(&mut self, layout: Layout) {
        layout::regroup(&mut self.groups, layout.groups());
        self.layout = layout;
        self.focus_group = self.focus_group.min(self.groups.len() - 1);
        self.sync_groups();
    }

    /// ⌃1, ⌃2…: puts the keyboard in that group's front tab.
    pub(super) fn focus_group_now(&mut self, ctx: &egui::Context, g: usize) {
        if g >= self.groups.len() {
            return;
        }
        self.focus_group = g;
        if let Some(i) = self.groups[g].active.and_then(|id| self.docs.iter().position(|d| d.id == id)) {
            self.activate(i);
            self.focus_editor(ctx);
        }
    }

    /// ⌃⇧1, ⌃⇧2…: moves the active tab into that group.
    pub(super) fn move_to_group(&mut self, ctx: &egui::Context, g: usize) {
        if g >= self.groups.len() {
            return;
        }
        let id = self.docs[self.active].id;
        for group in &mut self.groups {
            group.remove(id);
        }
        self.groups[g].views.push(id);
        self.groups[g].active = Some(id);
        self.focus_group = g;
        self.sync_groups();
        self.focus_editor(ctx);
    }

    /// Next or previous tab, within the active tab's group.
    pub(super) fn step_tab(&mut self, ctx: &egui::Context, by: isize) {
        let id = self.docs[self.active].id;
        let Some(g) = self.group_of(id) else {
            return;
        };
        let views = &self.groups[g].views;
        let at = views.iter().position(|v| *v == id).unwrap_or(0) as isize;
        let next = views[(at + by).rem_euclid(views.len() as isize) as usize];
        if let Some(i) = self.docs.iter().position(|d| d.id == next) {
            self.activate(i);
            self.focus_editor(ctx);
        }
    }

    /// Draws every group: its tabs, and its front tab's editor. Clicking
    /// into a group's editor makes its tab the active one.
    pub(super) fn editor_area(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        // A tab whose editor got the keyboard becomes the active one before
        // it handles any keys, so it holds its text.
        self.activate_focused(&ctx);
        let area = ui.max_rect();
        let split = self.groups.len() > 1;
        let mut outputs = Vec::new();
        for (g, rect) in self.layout.cell_rects(area).into_iter().enumerate() {
            // A little room for the line between groups.
            let rect = if split { rect.shrink(1.0) } else { rect };
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("group", g)));
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            self.group_tab_bar(&mut child, g, actions);
            let front = self.groups[g].active.and_then(|id| self.docs.iter().position(|d| d.id == id));
            let Some(index) = front else {
                // An empty group: clicking it sends the next file there.
                let response = child.allocate_rect(child.available_rect_before_wrap(), Sense::click());
                if response.clicked() {
                    self.focus_group = g;
                }
                continue;
            };
            let out = self.show_doc(&mut child, index);
            outputs.push((self.docs[index].id, out));
        }
        if split {
            self.splitters(ui, area);
        }
        self.activate_focused(&ctx);
        for (id, out) in outputs {
            self.after_view(&ctx, id, out, actions);
        }
    }

    /// Clicking into another group's editor makes its tab the active one.
    fn activate_focused(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused());
        if let Some(i) = self.docs.iter().position(|d| Some(editor_view::editor_id(d.id)) == focused)
            && i != self.active
        {
            self.activate(i);
            self.sync_clones();
        }
    }

    /// One tab's editor, with its own settings, marks and phantoms.
    fn show_doc(&mut self, ui: &mut egui::Ui, index: usize) -> ViewOutput {
        let id = self.docs[index].id;
        let settings = self.settings_for(id);
        let highlights = self.highlights_for(index);
        let phantoms = self.phantoms_for(index);
        let find = (index == self.active && self.find.open).then_some((self.find.query.as_str(), self.find.match_case));
        let cx = ViewContext {
            settings: &settings,
            highlighter: &self.highlighter,
            find,
            snippets: &self.snippets,
            highlights: &highlights,
            phantoms: &phantoms,
        };
        editor_view::show(ui, &mut self.docs[index], &cx)
    }

    /// What a group's editor reported: a phantom's link clicked, a double
    /// click in Find Results, the right-click menu.
    fn after_view(&mut self, ctx: &egui::Context, id: u64, out: ViewOutput, actions: &mut Vec<Action>) {
        if let Some((phantom, href)) = out.navigate {
            self.phantom_navigate(phantom, href);
        }
        if out.double_clicked && self.find_files.is_results(id) {
            self.open_result_at_caret(ctx, id);
        }
        if let Some(response) = out.response {
            if response.secondary_clicked()
                && let Some(i) = self.docs.iter().position(|d| d.id == id)
            {
                self.activate(i);
                self.focus_editor(ctx);
            }
            let items = self.context_menu_items();
            response.context_menu(|ui| crate::menu::egui_items(ui, &items, actions));
        }
    }

    /// A group's row of tabs.
    fn group_tab_bar(&mut self, ui: &mut egui::Ui, g: usize, actions: &mut Vec<Action>) {
        let views = self.groups[g].views.clone();
        let front = self.groups[g].active;
        let focused = g == self.focus_group;
        let groups = self.groups.len();
        let mut switch_to = None;
        let extras = self.runnable_menu(&self.package_tab_menu);
        let mut pick = None;
        let fill = ui.visuals().panel_fill;
        egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::horizontal().id_salt(("tabs", g)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, id) in views.iter().enumerate() {
                        let Some(i) = self.docs.iter().position(|d| d.id == *id) else {
                            continue;
                        };
                        let doc = &self.docs[i];
                        let label = if doc.dirty() { format!("{} •", doc.title()) } else { doc.title() };
                        // The focused group's front tab is the active one; others' are dimmer.
                        let text = if Some(*id) == front && !focused { egui::RichText::new(label).weak() } else { label.into() };
                        let tab = ui.selectable_label(Some(*id) == front, text);
                        if tab.clicked() || tab.secondary_clicked() {
                            switch_to = Some(i);
                        }
                        if tab.middle_clicked() {
                            actions.push(Action::Close(i));
                        }
                        let tab = match &doc.path {
                            Some(path) => tab.on_hover_text(path.display().to_string()),
                            None => tab,
                        };
                        tab.context_menu(|ui| {
                            if ui.button("Close").clicked() {
                                actions.push(Action::Close(i));
                            }
                            if groups > 1 {
                                ui.separator();
                                for to in (0..groups).filter(|to| *to != g) {
                                    if ui.button(format!("Move to Group {}", to + 1)).clicked() {
                                        actions.push(Action::MoveToGroup(to));
                                    }
                                }
                            }
                            super::package_menus::tab_menu(ui, *id, doc.path.as_deref(), (g, index), &extras, &mut pick);
                        });
                        if ui.small_button("×").on_hover_text("Close").clicked() {
                            actions.push(Action::Close(i));
                        }
                        ui.separator();
                    }
                });
            });
        });
        if let Some(i) = switch_to {
            self.activate(i);
            self.focus_editor(ui.ctx());
        }
        if let Some(pick) = pick {
            self.menu_pick(ui.ctx(), pick);
        }
    }

    /// Lines between groups, which can be dragged to resize them.
    fn splitters(&mut self, ui: &mut egui::Ui, area: Rect) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        for vertical in [true, false] {
            let count = if vertical { self.layout.cols.len() } else { self.layout.rows.len() };
            for i in 1..count.saturating_sub(1) {
                let at = if vertical { self.layout.cols[i] } else { self.layout.rows[i] } as f32;
                let (line, handle) = if vertical {
                    let x = area.left() + at * area.width();
                    ([pos2(x, area.top()), pos2(x, area.bottom())], Rect::from_x_y_ranges(x - 3.0..=x + 3.0, area.y_range()))
                } else {
                    let y = area.top() + at * area.height();
                    ([pos2(area.left(), y), pos2(area.right(), y)], Rect::from_x_y_ranges(area.x_range(), y - 3.0..=y + 3.0))
                };
                ui.painter().line_segment(line, stroke);
                let response = ui.interact(handle, ui.scope_id().with(("splitter", vertical, i)), Sense::drag());
                if response.hovered() || response.dragged() {
                    ui.ctx().set_cursor_icon(if vertical { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical });
                }
                if response.dragged()
                    && let Some(p) = response.interact_pointer_pos()
                {
                    let at = if vertical { (p.x - area.left()) / area.width() } else { (p.y - area.top()) / area.height() };
                    self.layout.drag_edge(vertical, i, at as f64);
                }
            }
        }
    }
}

/// Two different elements of a slice, both changeable.
fn two_mut<T>(items: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    assert_ne!(a, b);
    if a < b {
        let (left, right) = items.split_at_mut(b);
        (&mut left[a], &mut right[0])
    } else {
        let (left, right) = items.split_at_mut(a);
        (&mut right[0], &mut left[b])
    }
}
