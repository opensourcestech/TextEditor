//! Word wrap: where each line breaks into screen rows, and the mapping
//! between rows, lines and positions that drawing and the mouse rely on.
//! Without wrapping every line is exactly one row. Plugins' phantoms can
//! add rows of their own below a line.

use crate::buffer::{Buffer, LineSplice};

/// Character widths, with the x position so tabs can reach the next stop.
pub type Width<'a> = &'a dyn Fn(char, f32) -> f32;

/// Rows a line needs: start offsets (in chars) of each row, beginning with 0.
/// Breaks after a space or tab when it can, mid-word only for words wider
/// than the whole row.
pub fn breaks(text: &str, max_width: f32, width: Width) -> Vec<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut starts = vec![0];
    let mut row_start = 0;
    let mut x = 0.0;
    // Index just after the last space or tab in this row: a good break point.
    let mut after_space = None;
    let mut i = 0;
    while i < chars.len() {
        let w = width(chars[i], x);
        if x + w > max_width && i > row_start {
            let at = after_space.filter(|&b| b > row_start).unwrap_or(i);
            starts.push(at);
            row_start = at;
            after_space = None;
            // Re-measure what moved down to the new row.
            x = chars[at..i].iter().fold(0.0, |x, &c| x + width(c, x));
            continue;
        }
        x += w;
        if chars[i] == ' ' || chars[i] == '\t' {
            after_space = Some(i + 1);
        }
        i += 1;
    }
    starts
}

/// How many rows a line needs. Most lines fit in one row, which is checked
/// in place without copying the line out of the buffer.
fn row_count(b: &Buffer, line: usize, max_width: f32, width: Width) -> u32 {
    let mut x = 0.0;
    for c in b.rope().line(line).chars() {
        if c == '\n' {
            break;
        }
        x += width(c, x);
        if x > max_width {
            return breaks(&b.line_text(line), max_width, width).len() as u32;
        }
    }
    1
}

/// Rows-per-line bookkeeping for one tab, kept up to date as the text changes.
pub struct RowMap {
    /// Row width when wrapping; `None` means one row per line.
    wrap: Option<f32>,
    /// What `rows` was measured with: wrap width, font size and tab size.
    measured_for: Option<(u32, u32, usize)>,
    rows: Vec<u32>,
    /// Rows before each line; one longer than `rows`.
    prefix: Vec<usize>,
    /// Rows added below lines (for phantoms), by line, sorted, and the sum
    /// of those before each entry.
    extra: Vec<(usize, usize)>,
    extra_before: Vec<usize>,
    /// The buffer's line count as of the last `sync`.
    lines: usize,
}

/// Placeholder for a line whose rows must be measured again.
const DIRTY: u32 = 0;

impl RowMap {
    pub fn new() -> Self {
        Self { wrap: None, measured_for: None, rows: Vec::new(), prefix: vec![0], extra: Vec::new(), extra_before: vec![0], lines: 1 }
    }

    /// Rows to add below lines, as `(line, rows)`.
    pub fn set_extra(&mut self, mut extra: Vec<(usize, usize)>) {
        extra.sort_unstable();
        extra.dedup_by(|b, a| {
            let same = a.0 == b.0;
            if same {
                a.1 += b.1;
            }
            same
        });
        extra.retain(|e| e.1 > 0);
        if extra == self.extra {
            return;
        }
        self.extra_before = std::iter::once(0).chain(extra.iter().scan(0, |sum, e| {
            *sum += e.1;
            Some(*sum)
        }))
        .collect();
        self.extra = extra;
    }

    /// Rows added below lines before `line`.
    fn extra_above(&self, line: usize) -> usize {
        self.extra_before[self.extra.partition_point(|e| e.0 < line)]
    }

    /// Rows a line's own text takes.
    pub fn text_rows(&self, line: usize) -> usize {
        if self.wrapping() { self.rows.get(line).map_or(1, |r| *r as usize) } else { 1 }
    }

    /// Whether `row` is one added below a line rather than one of its text.
    pub fn is_extra_row(&self, row: usize) -> bool {
        let (line, k) = self.line_of_row(row);
        k >= self.text_rows(line)
    }

    /// Brings the map up to date after edits (`splices`) or a change of
    /// width, font or tab size. Only re-measures lines that changed.
    pub fn sync(
        &mut self,
        b: &Buffer,
        splices: Vec<LineSplice>,
        wrap: Option<f32>,
        font_size: f32,
        tab_size: usize,
        width: Width,
    ) {
        self.wrap = wrap;
        self.lines = b.line_count();
        let Some(wrap_width) = wrap else {
            self.measured_for = None;
            self.rows.clear();
            return;
        };
        let key = (wrap_width.to_bits(), font_size.to_bits(), tab_size);
        let fresh = self.measured_for != Some(key);
        if !fresh {
            for s in &splices {
                let end = (s.line + s.removed + 1).min(self.rows.len());
                let start = s.line.min(end);
                self.rows.splice(start..end, std::iter::repeat_n(DIRTY, s.inserted + 1));
            }
        }
        if fresh || self.rows.len() != b.line_count() {
            self.rows = vec![DIRTY; b.line_count()];
        } else if splices.is_empty() {
            return;
        }
        for (line, rows) in self.rows.iter_mut().enumerate() {
            if *rows == DIRTY {
                *rows = row_count(b, line, wrap_width, width);
            }
        }
        self.prefix.clear();
        self.prefix.push(0);
        let mut total = 0;
        for &r in &self.rows {
            total += r as usize;
            self.prefix.push(total);
        }
        self.measured_for = Some(key);
    }

    pub fn wrap_width(&self) -> Option<f32> {
        self.wrap
    }

    pub fn wrapping(&self) -> bool {
        self.wrap.is_some()
    }

    pub fn total_rows(&self, b: &Buffer) -> usize {
        let text = if self.wrapping() { *self.prefix.last().unwrap() } else { b.line_count() };
        text + self.extra_before.last().copied().unwrap_or(0)
    }

    /// The row a line starts on.
    pub fn first_row(&self, line: usize) -> usize {
        let text = if self.wrapping() { self.prefix[line.min(self.prefix.len() - 1)] } else { line };
        text + self.extra_above(line)
    }

    /// The line a row belongs to, and which of that line's rows it is. Rows
    /// added below a line count after its text's rows.
    pub fn line_of_row(&self, row: usize) -> (usize, usize) {
        if self.extra.is_empty() {
            if !self.wrapping() {
                return (row, 0);
            }
            let line = self.prefix.partition_point(|&p| p <= row).saturating_sub(1).min(self.rows.len() - 1);
            return (line, row - self.prefix[line]);
        }
        let lines = if self.wrapping() { self.rows.len() } else { self.lines.max(1) };
        // The last line starting at or before `row`.
        let (mut low, mut high) = (0, lines);
        while high - low > 1 {
            let mid = (low + high) / 2;
            if self.first_row(mid) <= row { low = mid } else { high = mid }
        }
        (low, row.saturating_sub(self.first_row(low)))
    }

    /// Row start offsets within a line (see [`breaks`]).
    pub fn line_breaks(&self, b: &Buffer, line: usize, width: Width) -> Vec<usize> {
        match self.wrap {
            Some(max) => breaks(&b.line_text(line), max, width),
            None => vec![0],
        }
    }

    /// The row a position is drawn on, and its x within that row.
    pub fn locate(&self, b: &Buffer, pos: usize, width: Width) -> (usize, f32) {
        let line = b.line_of(pos);
        let start = b.line_start(line);
        let col = pos - start;
        let starts = self.line_breaks(b, line, width);
        let k = starts.partition_point(|&s| s <= col) - 1;
        let x = b.slice(start + starts[k], pos).chars().fold(0.0, |x, c| x + width(c, x));
        (self.first_row(line) + k, x)
    }

    /// The position in `row` closest to x.
    pub fn pos_at(&self, b: &Buffer, row: usize, x: f32, width: Width) -> usize {
        let row = row.min(self.total_rows(b).saturating_sub(1));
        let (line, k) = self.line_of_row(row);
        let starts = self.line_breaks(b, line, width);
        // A row added below the line: as if past the end of its last row.
        let (k, x) = if k >= starts.len() { (starts.len() - 1, f32::INFINITY) } else { (k, x) };
        let line_start = b.line_start(line);
        let text = b.line_text(line);
        let row_start = starts[k.min(starts.len() - 1)];
        let row_end = starts.get(k + 1).copied().unwrap_or(text.chars().count());
        let mut left = 0.0;
        for (i, c) in text.chars().skip(row_start).take(row_end - row_start).enumerate() {
            let w = width(c, left);
            if x < left + w / 2.0 {
                return line_start + row_start + i;
            }
            left += w;
        }
        // Past the end of a wrapped row: stay on this row rather than jumping
        // to the start of the next one.
        let end = if k + 1 < starts.len() { row_end.saturating_sub(1).max(row_start) } else { row_end };
        line_start + end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{EditKind, Selection};

    const W: Width = &|_, _| 1.0;

    #[test]
    fn rows_added_below_lines_shift_the_rest() {
        let b = Buffer::new("a\nb\nc");
        let mut map = RowMap::new();
        map.sync(&b, Vec::new(), None, 14.0, 4, W);
        map.set_extra(vec![(1, 2), (0, 1)]);
        assert_eq!(map.total_rows(&b), 6);
        assert_eq!((map.first_row(0), map.first_row(1), map.first_row(2)), (0, 2, 5));
        assert_eq!(map.line_of_row(1), (0, 1));
        assert!(map.is_extra_row(1) && map.is_extra_row(4) && !map.is_extra_row(5));
        assert_eq!(map.line_of_row(5), (2, 0));
        assert_eq!(map.locate(&b, 4, W).0, 5, "c is on row 5");
        assert_eq!(map.pos_at(&b, 3, 0.0, W), 3, "a phantom row lands at its line's end");
        map.set_extra(Vec::new());
        assert_eq!(map.first_row(2), 2);
    }

    #[test]
    fn breaks_after_spaces() {
        assert_eq!(breaks("aaa bbb ccc", 6.0, W), vec![0, 4, 8]);
        assert_eq!(breaks("aaa bbb ccc", 8.0, W), vec![0, 8]);
        assert_eq!(breaks("short", 80.0, W), vec![0]);
        assert_eq!(breaks("", 8.0, W), vec![0]);
    }

    #[test]
    fn long_words_break_mid_word() {
        assert_eq!(breaks("abcdefghij", 4.0, W), vec![0, 4, 8]);
        assert_eq!(breaks("ab abcdefghij", 4.0, W), vec![0, 3, 7, 11]);
    }

    #[test]
    fn maps_rows_lines_and_positions() {
        let b = Buffer::new("aaa bbb ccc\nx\nddd eee");
        let mut map = RowMap::new();
        map.sync(&b, Vec::new(), Some(6.0), 14.0, 4, W);
        // "aaa |bbb |ccc", "x", "ddd |eee"
        assert_eq!(map.total_rows(&b), 3 + 1 + 2);
        assert_eq!(map.line_of_row(2), (0, 2));
        assert_eq!(map.line_of_row(3), (1, 0));
        assert_eq!(map.first_row(2), 4);
        // "bbb" starts row 1; position 5 is its second char.
        assert_eq!(map.locate(&b, 5, W), (1, 1.0));
        assert_eq!(map.pos_at(&b, 1, 1.0, W), 5);
        // Clicking past the end of a wrapped row stays on that row.
        assert_eq!(map.pos_at(&b, 0, 50.0, W), 3);
        // ...but past the end of a line's last row lands at the line end.
        assert_eq!(map.pos_at(&b, 2, 50.0, W), 11);
    }

    #[test]
    fn without_wrap_rows_are_lines() {
        let b = Buffer::new("one two three\nfour");
        let mut map = RowMap::new();
        map.sync(&b, Vec::new(), None, 14.0, 4, W);
        assert_eq!(map.total_rows(&b), 2);
        assert_eq!(map.locate(&b, 16, W), (1, 2.0));
    }

    /// Updating after edits must match measuring everything from scratch.
    #[test]
    fn edits_update_only_changed_lines_correctly() {
        let mut b = Buffer::new("aaa bbb ccc\nx\nddd eee fff ggg\ny");
        let mut map = RowMap::new();
        let splices = b.take_line_splices();
        map.sync(&b, splices, Some(8.0), 14.0, 4, W);

        let edits: [(&[usize], &str); 4] = [
            (&[12], "joined "),
            (&[0, 14], "new line\n"),
            (&[3], "\n\n"),
            (&[1, 20], "zzzzzzzzzzzzzzzz "),
        ];
        for (carets, text) in edits {
            b.set_selections(carets.iter().map(|&c| Selection::caret(c)).collect(), 0);
            b.insert(text, EditKind::Other);
            let splices = b.take_line_splices();
            map.sync(&b, splices, Some(8.0), 14.0, 4, W);

            let mut fresh = RowMap::new();
            fresh.sync(&b, Vec::new(), Some(8.0), 14.0, 4, W);
            assert_eq!(map.rows, fresh.rows, "after inserting {text:?}");
            assert_eq!(map.prefix, fresh.prefix);
        }
        b.undo();
        let splices = b.take_line_splices();
        map.sync(&b, splices, Some(8.0), 14.0, 4, W);
        let mut fresh = RowMap::new();
        fresh.sync(&b, Vec::new(), Some(8.0), 14.0, 4, W);
        assert_eq!(map.rows, fresh.rows, "after undo");
    }
}
