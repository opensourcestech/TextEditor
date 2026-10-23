//! Split views: the editor area is divided into cells
//! (columns, rows or a grid), and each cell is a group with its own tabs.

use eframe::egui::{Rect, pos2};
use serde_json::{Value, json};

/// A layout, in the format plugins use: the column and row edges as fractions of the
/// editor area, from 0 to 1, and each cell as the edges it spans,
/// `[left col, top row, right col, bottom row]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub cols: Vec<f64>,
    pub rows: Vec<f64>,
    pub cells: Vec<[usize; 4]>,
}

impl Default for Layout {
    fn default() -> Self {
        Self::columns(1)
    }
}

/// The layouts the View menu offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Single,
    Columns2,
    Columns3,
    Columns4,
    Rows2,
    Rows3,
    Grid4,
}

impl Preset {
    pub const ALL: [Preset; 7] =
        [Preset::Single, Preset::Columns2, Preset::Columns3, Preset::Columns4, Preset::Rows2, Preset::Rows3, Preset::Grid4];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Single => "Single",
            Preset::Columns2 => "Columns: 2",
            Preset::Columns3 => "Columns: 3",
            Preset::Columns4 => "Columns: 4",
            Preset::Rows2 => "Rows: 2",
            Preset::Rows3 => "Rows: 3",
            Preset::Grid4 => "Grid: 4",
        }
    }

    pub fn layout(self) -> Layout {
        match self {
            Preset::Single => Layout::columns(1),
            Preset::Columns2 => Layout::columns(2),
            Preset::Columns3 => Layout::columns(3),
            Preset::Columns4 => Layout::columns(4),
            Preset::Rows2 => Layout::rows(2),
            Preset::Rows3 => Layout::rows(3),
            Preset::Grid4 => Layout {
                cols: vec![0.0, 0.5, 1.0],
                rows: vec![0.0, 0.5, 1.0],
                cells: vec![[0, 0, 1, 1], [1, 0, 2, 1], [0, 1, 1, 2], [1, 1, 2, 2]],
            },
        }
    }
}

fn even(n: usize) -> Vec<f64> {
    (0..=n).map(|i| i as f64 / n as f64).collect()
}

impl Layout {
    /// `n` columns side by side, of equal width.
    pub fn columns(n: usize) -> Self {
        let n = n.max(1);
        Self { cols: even(n), rows: vec![0.0, 1.0], cells: (0..n).map(|i| [i, 0, i + 1, 1]).collect() }
    }

    /// `n` rows one above the other, of equal height.
    pub fn rows(n: usize) -> Self {
        let n = n.max(1);
        Self { cols: vec![0.0, 1.0], rows: even(n), cells: (0..n).map(|i| [0, i, 1, i + 1]).collect() }
    }

    pub fn groups(&self) -> usize {
        self.cells.len()
    }

    /// The preset with the same cells, whatever the splitters' positions.
    pub fn preset(&self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| {
            let l = p.layout();
            l.cells == self.cells && l.cols.len() == self.cols.len() && l.rows.len() == self.rows.len()
        })
    }

    /// Reads a layout as plugins give it to `set_layout`. `None` if it
    /// doesn't make sense: edges out of order, or cells outside them.
    pub fn from_json(value: &Value) -> Option<Self> {
        let edges = |key: &str| -> Option<Vec<f64>> {
            let edges: Vec<f64> = value.get(key)?.as_array()?.iter().map(Value::as_f64).collect::<Option<_>>()?;
            let ordered = edges.len() >= 2 && edges.windows(2).all(|w| w[0] < w[1]);
            (ordered && edges[0] >= 0.0 && edges[edges.len() - 1] <= 1.0).then_some(edges)
        };
        let (cols, rows) = (edges("cols")?, edges("rows")?);
        let cells: Vec<[usize; 4]> = value
            .get("cells")?
            .as_array()?
            .iter()
            .map(|c| {
                let c: Vec<usize> = c.as_array()?.iter().map(|n| n.as_u64().map(|n| n as usize)).collect::<Option<_>>()?;
                let cell: [usize; 4] = c.try_into().ok()?;
                let fits = cell[0] < cell[2] && cell[2] < cols.len() && cell[1] < cell[3] && cell[3] < rows.len();
                fits.then_some(cell)
            })
            .collect::<Option<_>>()?;
        (!cells.is_empty()).then_some(Self { cols, rows, cells })
    }

    pub fn to_json(&self) -> Value {
        json!({ "cols": self.cols, "rows": self.rows, "cells": self.cells })
    }

    /// Where each cell goes in `area`, in group order.
    pub fn cell_rects(&self, area: Rect) -> Vec<Rect> {
        let x = |i: usize| area.left() + self.cols[i] as f32 * area.width();
        let y = |i: usize| area.top() + self.rows[i] as f32 * area.height();
        self.cells.iter().map(|c| Rect::from_min_max(pos2(x(c[0]), y(c[1])), pos2(x(c[2]), y(c[3])))).collect()
    }

    /// Moves an inner column (`vertical`) or row edge to `at`, keeping a
    /// little room on either side of it.
    pub fn drag_edge(&mut self, vertical: bool, index: usize, at: f64) {
        let edges = if vertical { &mut self.cols } else { &mut self.rows };
        if index == 0 || index + 1 >= edges.len() {
            return;
        }
        const ROOM: f64 = 0.05;
        edges[index] = at.clamp(edges[index - 1] + ROOM, edges[index + 1] - ROOM);
    }
}

/// One group's tabs, by document id in tab order, and the one in front.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub views: Vec<u64>,
    pub active: Option<u64>,
}

impl Group {
    /// Takes `id` out, putting the tab beside it in front if it was.
    pub fn remove(&mut self, id: u64) {
        let Some(at) = self.views.iter().position(|v| *v == id) else {
            return;
        };
        self.views.remove(at);
        if self.active == Some(id) {
            self.active = self.views.get(at.min(self.views.len().saturating_sub(1))).copied();
        }
    }
}

/// Fits `groups` to a layout with `count` groups: tabs of groups that no
/// longer exist join the last one that does.
pub fn regroup(groups: &mut Vec<Group>, count: usize) {
    let count = count.max(1);
    if groups.len() > count {
        let extra: Vec<Group> = groups.drain(count..).collect();
        let last = &mut groups[count - 1];
        for group in extra {
            last.views.extend(group.views);
        }
        if last.active.is_none() {
            last.active = last.views.first().copied();
        }
    }
    groups.resize_with(count, Group::default);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_split_the_area_evenly() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 200.0));
        let rects = Preset::Columns2.layout().cell_rects(area);
        assert_eq!(rects, vec![
            Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 200.0)),
            Rect::from_min_max(pos2(200.0, 0.0), pos2(400.0, 200.0)),
        ]);
        let grid = Preset::Grid4.layout().cell_rects(area);
        assert_eq!(grid[3], Rect::from_min_max(pos2(200.0, 100.0), pos2(400.0, 200.0)));
        assert_eq!(Preset::Rows3.layout().preset(), Some(Preset::Rows3));
    }

    #[test]
    fn reads_layouts_as_plugins_write_them() {
        let l = Layout::from_json(&json!({ "cols": [0.0, 0.3, 1.0], "rows": [0.0, 1.0], "cells": [[0, 0, 1, 1], [1, 0, 2, 1]] }));
        assert_eq!(l.unwrap().cols, vec![0.0, 0.3, 1.0]);
        assert!(Layout::from_json(&json!({ "cols": [0.0, 1.0], "rows": [0.0, 1.0], "cells": [[0, 0, 2, 1]] })).is_none());
        assert!(Layout::from_json(&json!({ "cols": [0.5, 0.2], "rows": [0.0, 1.0], "cells": [[0, 0, 1, 1]] })).is_none());
        let round = Preset::Grid4.layout();
        assert_eq!(Layout::from_json(&round.to_json()), Some(round));
    }

    #[test]
    fn dragging_keeps_edges_in_order() {
        let mut l = Layout::columns(3);
        l.drag_edge(true, 1, 0.9);
        assert!(l.cols[1] < l.cols[2]);
        l.drag_edge(true, 0, 0.5);
        assert_eq!(l.cols[0], 0.0, "the outer edges stay put");
    }

    #[test]
    fn fewer_groups_move_tabs_into_the_last() {
        let mut groups = vec![
            Group { views: vec![1], active: Some(1) },
            Group { views: vec![2, 3], active: Some(3) },
            Group { views: vec![4], active: Some(4) },
        ];
        regroup(&mut groups, 1);
        assert_eq!(groups, vec![Group { views: vec![1, 2, 3, 4], active: Some(1) }]);
        regroup(&mut groups, 2);
        assert_eq!(groups[1], Group::default());
        let mut g = Group { views: vec![1, 2, 3], active: Some(3) };
        g.remove(3);
        assert_eq!(g.active, Some(2));
    }
}
