//! Pure state for the shared interactive sized-list — navigation, multi-select
//! (with dupe "protect the last copy" safety), and sorting. Unit-tested with no
//! terminal; `render.rs` draws it and `mod.rs` runs the event loop.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowMeta {
    /// A row with no meta column — part of the widget API for future consumers
    /// (explore/scan rows); currently exercised only by tests.
    #[allow(dead_code)]
    None,
    /// Days since modified (for `large`).
    Age(Option<u64>),
    /// A short tag column, e.g. "safe"/"review" for caches.
    Tag(String),
    /// Uninstall leftover confidence, e.g. "high"/"med?".
    Confidence(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub bytes: u64,
    pub label: String,
    pub path: PathBuf,
    pub selected: bool,
    pub meta: RowMeta,
    /// Rows sharing a group id are one dupe set; at least one stays unselected.
    pub group: Option<usize>,
}

impl Row {
    /// Convenience constructor for adapters; ungrouped, unselected.
    pub fn new(bytes: u64, label: impl Into<String>, path: PathBuf, meta: RowMeta) -> Self {
        Row {
            bytes,
            label: label.into(),
            path,
            selected: false,
            meta,
            group: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Age,
    Name,
}

pub struct ListState {
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub sort: Sort,
}

impl ListState {
    pub fn new(rows: Vec<Row>, sort: Sort) -> Self {
        let mut state = ListState {
            rows,
            cursor: 0,
            sort,
        };
        state.sort_rows();
        state
    }

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.rows.len() {
            self.cursor += 1;
        }
    }

    /// True unless selecting row `i` would leave its dupe group fully selected.
    fn can_select(&self, i: usize) -> bool {
        match self.rows[i].group {
            None => true,
            Some(g) => self
                .rows
                .iter()
                .enumerate()
                .any(|(j, r)| j != i && r.group == Some(g) && !r.selected),
        }
    }

    pub fn toggle(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        if row.selected {
            self.rows[self.cursor].selected = false;
        } else if self.can_select(self.cursor) {
            self.rows[self.cursor].selected = true;
        }
    }

    /// Select every redundant row: all ungrouped rows, and all-but-the-first of
    /// each dupe group (so one copy per set is always kept).
    pub fn select_all(&mut self) {
        let mut first_of_group: HashMap<usize, usize> = HashMap::new();
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(g) = row.group {
                first_of_group.entry(g).or_insert(i);
            }
        }
        for (i, row) in self.rows.iter_mut().enumerate() {
            let keep_one = row.group.is_some_and(|g| first_of_group[&g] == i);
            row.selected = !keep_one;
        }
    }

    pub fn select_none(&mut self) {
        for row in &mut self.rows {
            row.selected = false;
        }
    }

    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        let anchor = self.rows.get(self.cursor).map(|r| r.path.clone());
        self.sort_rows();
        self.cursor = anchor
            .and_then(|p| self.rows.iter().position(|r| r.path == p))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
    }

    fn sort_rows(&mut self) {
        match self.sort {
            Sort::Size => self.rows.sort_by_key(|r| std::cmp::Reverse(r.bytes)),
            Sort::Name => self.rows.sort_by_key(|r| r.label.to_lowercase()),
            Sort::Age => self.rows.sort_by_key(|r| {
                let days = match r.meta {
                    RowMeta::Age(d) => d.unwrap_or(0),
                    _ => 0,
                };
                std::cmp::Reverse(days)
            }),
        }
    }

    pub fn selected(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.selected)
    }

    pub fn selected_bytes(&self) -> u64 {
        self.selected().map(|r| r.bytes).sum()
    }

    pub fn selected_count(&self) -> usize {
        self.selected().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(bytes: u64, name: &str, group: Option<usize>) -> Row {
        Row {
            bytes,
            label: name.into(),
            path: PathBuf::from("/x").join(name),
            selected: false,
            meta: RowMeta::None,
            group,
        }
    }

    #[test]
    fn navigation_clamps() {
        let mut s = ListState::new(
            vec![row(3, "a", None), row(2, "b", None), row(1, "c", None)],
            Sort::Size,
        );
        s.move_up();
        assert_eq!(s.cursor, 0);
        s.move_down();
        s.move_down();
        s.move_down();
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn toggle_and_totals() {
        let mut s = ListState::new(vec![row(10, "a", None), row(5, "b", None)], Sort::Size);
        s.toggle(); // select "a" (cursor 0, largest)
        assert_eq!(s.selected_count(), 1);
        assert_eq!(s.selected_bytes(), 10);
        s.toggle(); // deselect
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn dupes_protect_last_copy() {
        // Two copies of one set; you may select one but never both.
        let mut s = ListState::new(
            vec![row(4, "copy1", Some(1)), row(4, "copy2", Some(1))],
            Sort::Size,
        );
        s.toggle(); // select copy1 (cursor 0)
        s.move_down();
        s.toggle(); // try to select copy2 — refused (would be all)
        assert_eq!(s.selected_count(), 1, "one copy must remain unselected");
    }

    #[test]
    fn select_all_keeps_one_per_group() {
        let mut s = ListState::new(
            vec![
                row(4, "c1", Some(1)),
                row(4, "c2", Some(1)),
                row(9, "solo", None),
            ],
            Sort::Size,
        );
        s.select_all();
        assert_eq!(s.selected_count(), 2, "solo + one redundant copy");
        // exactly one member of group 1 is unselected
        let unsel = s
            .rows
            .iter()
            .filter(|r| r.group == Some(1) && !r.selected)
            .count();
        assert_eq!(unsel, 1);
    }

    #[test]
    fn sort_keeps_cursor_on_row() {
        let mut s = ListState::new(vec![row(1, "a", None), row(9, "b", None)], Sort::Size);
        // cursor 0 == "b" (largest). Switch to Name → "a","b"; cursor should follow "b".
        s.set_sort(Sort::Name);
        assert_eq!(s.rows[s.cursor].label, "b");
    }
}
