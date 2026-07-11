# deepscan Interactive Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `large`, `dupes`, `clean` (renamed from `reclaim`), and `uninstall` an interactive terminal UI backed by one shared select→Trash sized-list, and route every deletion through the macOS Trash.

**Architecture:** `deepscan-core` stays the data layer; one new TUI layer in `deepscan-cli` (`tui/widget.rs` pure state, `tui/action.rs` Trash+confirm, `tui/render.rs`+`tui/mod.rs` ratatui) owns all interaction. Each command is a thin adapter that builds `Vec<Row>` and either launches the widget (TTY) or prints today's report (piped / `--json` / `--plain`).

**Tech Stack:** Rust (edition 2021), ratatui 0.29 + crossterm, `trash` 5, clap 4, rayon, serde_json.

## Global Constraints

- Toolchain pinned to `1.96.0` via `rust-toolchain.toml`; CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, release build. Every task must end clippy-clean under `-D warnings`.
- All deletions go to the **macOS Trash** (`trash::delete`) — never `std::fs::remove_dir_all`. Every delete path is checked by `deepscan_core::is_safe_to_delete` first.
- Interactive UI launches only when `std::io::stdout().is_terminal()` and neither `--json` nor `--plain` is set. Piped/redirected output and `--json` keep their exact current form.
- Colors/spinner already gate on TTY + `NO_COLOR`; do not regress that.
- `human(bytes)` from `deepscan_core` is the only byte formatter. Reuse it.
- Follow existing style: pure state machines are unit-tested without a terminal (see `tui.rs` `navigation_clamps_and_pops`); the ratatui layer is a thin renderer.

---

### Task 1: Restructure `tui.rs` into a `tui/` module (no behavior change)

Rust cannot have both `tui.rs` and `tui/`. Convert first so later tasks add sibling files. Explore's code moves verbatim into `tui/tree.rs`; `tui::run` keeps working.

**Files:**
- Move: `crates/deepscan-cli/src/tui.rs` → `crates/deepscan-cli/src/tui/tree.rs`
- Create: `crates/deepscan-cli/src/tui/mod.rs`

**Interfaces:**
- Produces: `deepscan_cli::tui::run(root: PathBuf) -> anyhow::Result<()>` (unchanged signature, now re-exported from `tui/mod.rs`).

- [ ] **Step 1: Move the file**

```bash
cd crates/deepscan-cli/src
mkdir tui
git mv tui.rs tui/tree.rs
```

- [ ] **Step 2: Create `tui/mod.rs` that re-exports explore's `run`**

```rust
//! Interactive terminal views. `tree` is the explore browser; `widget`,
//! `action`, and `render` (added in later tasks) back the shared sized-list.

mod tree;

pub use tree::run;
```

- [ ] **Step 3: Make `tree.rs` items visible to the module**

In `tui/tree.rs`, the only public item is `pub fn run`. Leave it `pub`; `mod tree;` in `mod.rs` keeps it crate-private, and `pub use tree::run;` re-exports it. No other edits.

- [ ] **Step 4: Build and test**

Run: `cargo build -p deepscan-cli && cargo test -q`
Expected: builds; all existing tests pass (explore's `navigation_clamps_and_pops`, `sort_by_size_keeps_cursor` still run).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(tui): split tui.rs into tui/ module (tree.rs), no behavior change"
```

---

### Task 2: Core — single Trash delete primitive + reroute `execute_reclaim`

One delete path for the whole app. Injectable so tests never touch the real Trash.

**Files:**
- Create: `crates/deepscan-core/src/deletion.rs`
- Modify: `crates/deepscan-core/src/engine.rs` (`execute_reclaim`, ~lines 129-169, and its test ~220-254)
- Modify: `crates/deepscan-core/src/lib.rs` (exports)

**Interfaces:**
- Produces: `deepscan_core::move_to_trash(path: &Path) -> Result<(), String>`
- Produces: `deepscan_core::execute_reclaim(targets: &[ReclaimTarget], home: Option<&Path>) -> ReclaimResult` (unchanged signature; now Trash-backed)

- [ ] **Step 1: Write the failing test for `move_to_trash` guard behavior**

Add to a new `crates/deepscan-core/src/deletion.rs`:

```rust
//! The single deletion path: move to the macOS Trash (recoverable). Everything
//! that removes files — `reclaim`/`clean`, `uninstall`, and the interactive
//! views — routes through here so behavior and safety are uniform.

use std::path::Path;

/// Move `path` to the Trash. Returns a human-readable error on failure.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trashes_a_real_file() {
        let base = std::env::temp_dir().join(format!("deepscan-trash-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let victim = base.join("junk.bin");
        std::fs::write(&victim, vec![0u8; 32]).unwrap();

        move_to_trash(&victim).expect("trash should succeed");
        assert!(!victim.exists(), "file left its original location");

        let _ = std::fs::remove_dir_all(&base);
    }
}
```

- [ ] **Step 2: Register the module and export**

In `crates/deepscan-core/src/lib.rs`, add `pub mod deletion;` with the other `pub mod` lines, and add to the re-exports:

```rust
pub use deletion::move_to_trash;
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p deepscan-core deletion -- --nocapture`
Expected: PASS (moves a temp file to Trash). This is the one test allowed to touch the real Trash; keep it tiny.

- [ ] **Step 4: Make `execute_reclaim` Trash-backed and injectable**

In `crates/deepscan-core/src/engine.rs`, replace the body of `execute_reclaim` (currently uses `std::fs::remove_dir_all`) with a thin wrapper over an injectable deleter:

```rust
/// Move each target to the Trash, refusing any path that fails the safety
/// guard. Delegates to [`execute_reclaim_with`] using [`crate::move_to_trash`].
pub fn execute_reclaim(targets: &[ReclaimTarget], home: Option<&Path>) -> ReclaimResult {
    execute_reclaim_with(targets, home, |path| crate::move_to_trash(path))
}

/// Testable core: `delete` is injected so tests assert the guard without
/// touching the real Trash.
pub fn execute_reclaim_with(
    targets: &[ReclaimTarget],
    home: Option<&Path>,
    delete: impl Fn(&Path) -> Result<(), String>,
) -> ReclaimResult {
    let mut deleted = Vec::new();
    let mut freed_bytes = 0u64;

    for target in targets {
        if !is_safe_to_delete(&target.path, home) {
            deleted.push(ReclaimOutcome {
                name: target.name.clone(),
                path: target.path.clone(),
                bytes: target.bytes,
                ok: false,
                error: Some("refused: failed safety guard".to_string()),
            });
            continue;
        }
        match delete(&target.path) {
            Ok(()) => {
                freed_bytes += target.bytes;
                deleted.push(ReclaimOutcome {
                    name: target.name.clone(),
                    path: target.path.clone(),
                    bytes: target.bytes,
                    ok: true,
                    error: None,
                });
            }
            Err(err) => deleted.push(ReclaimOutcome {
                name: target.name.clone(),
                path: target.path.clone(),
                bytes: target.bytes,
                ok: false,
                error: Some(err),
            }),
        }
    }

    ReclaimResult { deleted, freed_bytes }
}
```

- [ ] **Step 5: Update the existing reclaim test to use a fake deleter**

In `engine.rs` `execute_reclaim_deletes_only_safe_targets`, replace the call
`let result = execute_reclaim(&targets, Some(Path::new("/nonexistent-home")));`
with a fake deleter that removes the fixture instead of trashing it, so the test asserts the guard without polluting Trash:

```rust
    let result = execute_reclaim_with(&targets, Some(Path::new("/nonexistent-home")), |path| {
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    });
```

The assertions (`freed_bytes == 1024`, `!victim.exists()`, `deleted[0].ok`, `!deleted[1].ok`) stay unchanged.

- [ ] **Step 6: Export `execute_reclaim_with`**

In `lib.rs`, add `execute_reclaim_with` to the `pub use engine::{...}` list.

- [ ] **Step 7: Run tests + clippy**

Run: `cargo test -q && cargo clippy --all-targets -- -D warnings`
Expected: PASS, clippy clean.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(core): route reclaim delete through Trash; add move_to_trash + injectable execute_reclaim_with"
```

---

### Task 3: Widget state — `ListState` pure state machine

The heart of the shared widget. No terminal, fully unit-tested.

**Files:**
- Create: `crates/deepscan-cli/src/tui/widget.rs`
- Modify: `crates/deepscan-cli/src/tui/mod.rs` (add `mod widget;`)

**Interfaces:**
- Produces: `Row { bytes: u64, label: String, path: PathBuf, selected: bool, meta: RowMeta, group: Option<usize> }`
- Produces: `enum RowMeta { None, Age(Option<u64>), Tag(String), Confidence(String) }`
- Produces: `enum Sort { Size, Age, Name }`
- Produces: `struct ListState { rows: Vec<Row>, cursor: usize, sort: Sort }` with `new`, `move_up`, `move_down`, `toggle`, `select_all`, `select_none`, `set_sort`, `selected`, `selected_bytes`, `selected_count`.

- [ ] **Step 1: Write the failing tests**

Create `crates/deepscan-cli/src/tui/widget.rs`:

```rust
//! Pure state for the shared interactive sized-list — navigation, multi-select
//! (with dupe "protect the last copy" safety), and sorting. Unit-tested with no
//! terminal; `render.rs` draws it and `mod.rs` runs the event loop.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowMeta {
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
        Row { bytes, label: label.into(), path, selected: false, meta, group: None }
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
        let mut state = ListState { rows, cursor: 0, sort };
        state.apply_sort();
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
        self.apply_sort();
    }

    fn apply_sort(&mut self) {
        let anchor = self.rows.get(self.cursor).map(|r| r.path.clone());
        match self.sort {
            Sort::Size => self.rows.sort_by_key(|r| std::cmp::Reverse(r.bytes)),
            Sort::Name => self.rows.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase())),
            Sort::Age => self.rows.sort_by_key(|r| {
                let days = match r.meta {
                    RowMeta::Age(d) => d.unwrap_or(0),
                    _ => 0,
                };
                std::cmp::Reverse(days)
            }),
        }
        self.cursor = anchor
            .and_then(|p| self.rows.iter().position(|r| r.path == p))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
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
        Row { bytes, label: name.into(), path: PathBuf::from("/x").join(name),
              selected: false, meta: RowMeta::None, group }
    }

    #[test]
    fn navigation_clamps() {
        let mut s = ListState::new(vec![row(3, "a", None), row(2, "b", None), row(1, "c", None)], Sort::Size);
        s.move_up();
        assert_eq!(s.cursor, 0);
        s.move_down(); s.move_down(); s.move_down();
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
        let mut s = ListState::new(vec![row(4, "copy1", Some(1)), row(4, "copy2", Some(1))], Sort::Size);
        s.toggle();                 // select copy1 (cursor 0)
        s.move_down();
        s.toggle();                 // try to select copy2 — refused (would be all)
        assert_eq!(s.selected_count(), 1, "one copy must remain unselected");
    }

    #[test]
    fn select_all_keeps_one_per_group() {
        let mut s = ListState::new(
            vec![row(4, "c1", Some(1)), row(4, "c2", Some(1)), row(9, "solo", None)],
            Sort::Size,
        );
        s.select_all();
        assert_eq!(s.selected_count(), 2, "solo + one redundant copy");
        // exactly one member of group 1 is unselected
        let unsel = s.rows.iter().filter(|r| r.group == Some(1) && !r.selected).count();
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
```

- [ ] **Step 2: Register the module**

In `tui/mod.rs`, add `mod widget;` (below `mod tree;`).

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p deepscan-cli widget -- --nocapture`
Expected: 5 tests PASS.

- [ ] **Step 4: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings
git add -A
git commit -m "feat(tui): pure ListState — nav, multi-select, dupe protect-last, sort"
```

---

### Task 4: Action layer — guarded Trash + reveal + confirm gate

**Files:**
- Create: `crates/deepscan-cli/src/tui/action.rs`
- Modify: `crates/deepscan-cli/src/tui/mod.rs` (add `mod action;`)

**Interfaces:**
- Produces: `struct TrashOutcome { freed_bytes: u64, failures: Vec<(PathBuf, String)> }`
- Produces: `trash_selected(items: &[(PathBuf, u64)], home: Option<&Path>) -> TrashOutcome`
- Produces: `trash_selected_with(items, home, delete: impl Fn(&Path) -> Result<(), String>) -> TrashOutcome`
- Produces: `reveal_in_finder(path: &Path)`

- [ ] **Step 1: Write the failing test**

Create `crates/deepscan-cli/src/tui/action.rs`:

```rust
//! Side-effecting actions for the interactive views: move selected items to the
//! Trash (guarded by `is_safe_to_delete`), and reveal a path in Finder. The
//! Trash call is injectable so the guard is unit-tested without touching Trash.

use std::path::{Path, PathBuf};
use std::process::Command;

use deepscan_core::{is_safe_to_delete, move_to_trash};

#[derive(Debug, Default)]
pub struct TrashOutcome {
    pub freed_bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

/// Trash each `(path, bytes)`, refusing any path that fails the safety guard.
pub fn trash_selected(items: &[(PathBuf, u64)], home: Option<&Path>) -> TrashOutcome {
    trash_selected_with(items, home, |path| move_to_trash(path))
}

pub fn trash_selected_with(
    items: &[(PathBuf, u64)],
    home: Option<&Path>,
    delete: impl Fn(&Path) -> Result<(), String>,
) -> TrashOutcome {
    let mut outcome = TrashOutcome::default();
    for (path, bytes) in items {
        if !is_safe_to_delete(path, home) {
            outcome.failures.push((path.clone(), "refused: failed safety guard".into()));
            continue;
        }
        match delete(path) {
            Ok(()) => outcome.freed_bytes += bytes,
            Err(err) => outcome.failures.push((path.clone(), err)),
        }
    }
    outcome
}

/// Open Finder with `path` selected. Best-effort; failures are ignored.
pub fn reveal_in_finder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_refuses_dangerous_paths_and_trashes_safe_ones() {
        let home = PathBuf::from("/Users/tester");
        let items = vec![
            (PathBuf::from("/Users/tester/Library/Caches/foo"), 100u64),
            (PathBuf::from("/"), 0u64), // must be refused
        ];
        let deleted = std::cell::RefCell::new(Vec::new());
        let outcome = trash_selected_with(&items, Some(&home), |p| {
            deleted.borrow_mut().push(p.to_path_buf());
            Ok(())
        });
        assert_eq!(outcome.freed_bytes, 100);
        assert_eq!(outcome.failures.len(), 1, "root refused");
        assert_eq!(deleted.borrow().len(), 1, "only the safe path was deleted");
    }
}
```

- [ ] **Step 2: Register the module**

In `tui/mod.rs`, add `mod action;`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p deepscan-cli action -- --nocapture`
Expected: PASS.

- [ ] **Step 4: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings
git add -A
git commit -m "feat(tui): guarded Trash action + reveal-in-Finder"
```

---

### Task 5: Render + `run_list` event loop (with confirm modal)

**Files:**
- Create: `crates/deepscan-cli/src/tui/render.rs`
- Modify: `crates/deepscan-cli/src/tui/mod.rs` (add `mod render;`, add `run_list`, `ListConfig`)

**Interfaces:**
- Consumes: `widget::{ListState, Row, RowMeta, Sort}`, `action::{trash_selected, reveal_in_finder, TrashOutcome}`
- Produces: `pub struct ListConfig { pub title: String, pub home: Option<PathBuf> }`
- Produces: `pub fn run_list(rows: Vec<Row>, sort: Sort, config: ListConfig) -> anyhow::Result<TrashOutcome>`

- [ ] **Step 1: Write the renderer**

Create `crates/deepscan-cli/src/tui/render.rs`. Model the drawing on `tree.rs::draw` (bar + size style). It draws the header, the list, an optional confirm modal, and the footer keymap:

```rust
//! ratatui drawing for the shared sized-list and its confirm modal. Pure view
//! over `ListState`; all state transitions live in `widget.rs`/`mod.rs`.

use deepscan_core::human;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState as UiListState, Paragraph};
use ratatui::Frame;

use super::widget::{ListState, RowMeta};

const BAR_WIDTH: usize = 14;

pub(super) fn draw(frame: &mut Frame, state: &ListState, title: &str, confirming: bool) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let header = format!(
        " {title}   [selected: {} · {}]",
        state.selected_count(),
        human(state.selected_bytes())
    );
    frame.render_widget(Paragraph::new(Line::from(header)).bold(), chunks[0]);

    let max = state.rows.iter().map(|r| r.bytes).max().unwrap_or(1).max(1);
    let items: Vec<ListItem> = state
        .rows
        .iter()
        .map(|row| {
            let filled = (((row.bytes as f64 / max as f64) * BAR_WIDTH as f64).round() as usize)
                .min(BAR_WIDTH);
            let bar = format!("{}{}", "█".repeat(filled), "·".repeat(BAR_WIDTH - filled));
            let check = if row.selected { "[x]" } else { "[ ]" };
            let col = match &row.meta {
                RowMeta::Age(Some(d)) => format!("{d}d"),
                RowMeta::Age(None) => "?".into(),
                RowMeta::Tag(t) | RowMeta::Confidence(t) => t.clone(),
                RowMeta::None => String::new(),
            };
            ListItem::new(Line::from(format!(
                " {check} {bar}  {:>10}  {:>6}  {}",
                human(row.bytes),
                col,
                row.label
            )))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶");
    let mut ui = UiListState::default();
    if !state.rows.is_empty() {
        ui.select(Some(state.cursor));
    }
    frame.render_stateful_widget(list, chunks[1], &mut ui);

    let footer = " ↑↓ move  space select  a all  n none  s sort  d Trash  f reveal  q quit";
    frame.render_widget(Paragraph::new(footer).dim(), chunks[2]);

    if confirming {
        draw_confirm(frame, state);
    }
}

fn draw_confirm(frame: &mut Frame, state: &ListState) {
    let area = centered(60, 7, frame.area());
    frame.render_widget(Clear, area);
    let text = format!(
        "Move {} item(s) ({}) to the Trash?\n\n[y] confirm    [n] cancel",
        state.selected_count(),
        human(state.selected_bytes())
    );
    let block = Block::default().borders(Borders::ALL).title(" confirm ");
    frame.render_widget(Paragraph::new(text).block(block).bold(), area);
}

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w.min(area.width), height: h.min(area.height) }
}
```

- [ ] **Step 2: Write `run_list` in `tui/mod.rs`**

Add to `crates/deepscan-cli/src/tui/mod.rs` (model the terminal lifecycle on `tree.rs::run`):

```rust
mod action;
mod render;
mod tree;
mod widget;

pub use tree::run;
pub use widget::{Row, RowMeta, Sort};

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use action::{reveal_in_finder, trash_selected, TrashOutcome};
use widget::ListState;

pub struct ListConfig {
    pub title: String,
    pub home: Option<PathBuf>,
}

/// Run the interactive sized-list. Returns what was trashed (empty if the user
/// quit without deleting).
pub fn run_list(rows: Vec<Row>, sort: Sort, config: ListConfig) -> anyhow::Result<TrashOutcome> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut state = ListState::new(rows, sort);
    let mut confirming = false;
    let mut outcome = TrashOutcome::default();

    loop {
        terminal.draw(|f| render::draw(f, &state, &config.title, confirming))?;
        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if confirming {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let items: Vec<(PathBuf, u64)> =
                        state.selected().map(|r| (r.path.clone(), r.bytes)).collect();
                    outcome = trash_selected(&items, config.home.as_deref());
                    let trashed: std::collections::HashSet<PathBuf> =
                        items.iter().map(|(p, _)| p.clone()).collect();
                    let failed: std::collections::HashSet<PathBuf> =
                        outcome.failures.iter().map(|(p, _)| p.clone()).collect();
                    state.rows.retain(|r| !trashed.contains(&r.path) || failed.contains(&r.path));
                    for r in &mut state.rows {
                        r.selected = false;
                    }
                    state.cursor = state.cursor.min(state.rows.len().saturating_sub(1));
                    confirming = false;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => confirming = false,
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => state.move_up(),
            KeyCode::Down | KeyCode::Char('j') => state.move_down(),
            KeyCode::Char(' ') => state.toggle(),
            KeyCode::Char('a') => state.select_all(),
            KeyCode::Char('n') => state.select_none(),
            KeyCode::Char('s') => {
                let next = match state.sort {
                    Sort::Size => Sort::Age,
                    Sort::Age => Sort::Name,
                    Sort::Name => Sort::Size,
                };
                state.set_sort(next);
            }
            KeyCode::Char('f') => {
                if let Some(row) = state.rows.get(state.cursor) {
                    reveal_in_finder(&row.path);
                }
            }
            KeyCode::Char('d') | KeyCode::Enter => {
                if state.selected_count() > 0 {
                    confirming = true;
                }
            }
            _ => {}
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(outcome)
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p deepscan-cli`
Expected: builds (no warnings).

- [ ] **Step 4: Smoke-test the lifecycle with a faked TTY**

Add a throwaway binary check by driving an existing command once wiring lands (Task 6). For now verify compilation only. (`run_list` is exercised end-to-end in Task 6's smoke test.)

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings
git add -A
git commit -m "feat(tui): run_list event loop + renderer with confirm modal"
```

---

### Task 6: Wire `large` to the interactive list (+ auto-TTY dispatch helper)

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`Large` command: add `--plain`; `run_large`; add `interactive()` helper and `--plain` fields)

**Interfaces:**
- Consumes: `tui::{run_list, Row, RowMeta, Sort, ListConfig}`
- Produces: `fn interactive(json: bool, plain: bool) -> bool` (true → launch TUI)

- [ ] **Step 1: Add the dispatch helper**

In `main.rs`, near `palette()`:

```rust
/// Launch the interactive TUI only on a real terminal, and only when the user
/// hasn't asked for machine/plain output.
fn interactive(json: bool, plain: bool) -> bool {
    !json && !plain && std::io::stdout().is_terminal()
}
```

- [ ] **Step 2: Add `--plain` to the `Large` command**

In the `Large { .. }` variant add:

```rust
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
```

And thread `plain` through the `Commands::Large { .. } => run_large(path, top, min_mb, older, json, plain)` call and `run_large`'s signature.

- [ ] **Step 3: Branch `run_large` to the TUI**

In `run_large`, after computing `files` (and before the JSON branch), insert:

```rust
    if interactive(json, plain) && !files.is_empty() {
        let rows: Vec<tui::Row> = files
            .iter()
            .map(|f| tui::Row::new(f.bytes, f.path.display().to_string(),
                                   f.path.clone(), tui::RowMeta::Age(f.modified_days)))
            .collect();
        let title = match older {
            Some(days) => format!("deepscan large · not modified in {days}+ days"),
            None => "deepscan large".to_string(),
        };
        let outcome = tui::run_list(rows, tui::Sort::Size,
            tui::ListConfig { title, home: home_dir() })?;
        if outcome.freed_bytes > 0 {
            println!("Moved {} to the Trash.", human(outcome.freed_bytes));
        }
        return Ok(ExitCode::SUCCESS);
    }
```

Because this branch is placed *before* the existing `if json { … }` return and gated on `interactive(json, plain)`, all three paths resolve correctly: `--json` and `--plain` both make `interactive(...)` false and fall through to today's JSON/plain output; a bare TTY run launches the list. `run_large` gains a `plain: bool` parameter (Step 2).

- [ ] **Step 4: Build + smoke-test the TUI end to end**

Run:
```bash
cargo build -q --release -p deepscan-cli
printf 'q' | script -q /dev/null ./target/release/deepscan large --top 5 >/dev/null 2>&1 && echo "launched + quit clean"
./target/release/deepscan large --top 3 --plain 2>/dev/null   # still prints
./target/release/deepscan large --top 3 --json 2>/dev/null | python3 -c "import json,sys;json.load(sys.stdin);print('json ok')"
```
Expected: "launched + quit clean", the plain list, and "json ok".

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings
git add -A
git commit -m "feat(large): interactive sized-list with select→Trash; --plain fallback"
```

---

### Task 7: Wire `uninstall` and rename `reclaim` → `clean` (both interactive)

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`Uninstall` interactive; rename `Reclaim`→`Clean` with `reclaim` alias; `run_clean` caches list)

**Interfaces:**
- Consumes: `tui::{run_list, Row, RowMeta, Sort, ListConfig}`

- [ ] **Step 1: Interactive `uninstall` (leftovers pre-selected)**

In `run_uninstall`, in the non-`--apply` branch, before the current dry-run print, add:

```rust
    if interactive(json, false) {
        let mut rows: Vec<tui::Row> = Vec::new();
        if let Some(app) = &plan.app_path {
            let mut r = tui::Row::new(plan.app_bytes, app.display().to_string(), app.clone(),
                tui::RowMeta::Confidence("app".into()));
            r.selected = true;
            rows.push(r);
        }
        for l in &plan.leftovers {
            let tag = match l.confidence {
                Confidence::High => "high",
                Confidence::Medium => "med?",
            };
            let mut r = tui::Row::new(l.bytes, l.path.display().to_string(), l.path.clone(),
                tui::RowMeta::Confidence(tag.into()));
            r.selected = true;
            rows.push(r);
        }
        let outcome = tui::run_list(rows, tui::Sort::Size,
            tui::ListConfig { title: format!("deepscan uninstall · {}", plan.app_name),
                              home: home_dir() })?;
        if outcome.freed_bytes > 0 {
            println!("Moved {} to the Trash.", human(outcome.freed_bytes));
        }
        return Ok(ExitCode::SUCCESS);
    }
```

Note: `Row::selected` is `pub` (Task 3), so pre-selecting is a field set.

- [ ] **Step 2: Rename the command `Reclaim` → `Clean` with a `reclaim` alias**

In the `Commands` enum, rename the `Reclaim { .. }` variant to `Clean { .. }` and add the clap alias:

```rust
    /// Reclaim regenerable caches. Dry-run unless --apply is passed.
    #[command(alias = "reclaim")]
    Clean {
        #[arg(long)] apply: bool,
        #[arg(long)] yes: bool,
        #[arg(long)] json: bool,
        #[arg(long, value_name = "NAME")] only: Vec<String>,
        /// Force the plain printed report even in a terminal.
        #[arg(long)] plain: bool,
    },
```

Update the `match` arm to `Commands::Clean { .. } => run_clean(apply, yes, json, only, plain)`. Rename `run_reclaim` → `run_clean` (keep `execute_reclaim`/`build_reclaim_plan` core names).

- [ ] **Step 3: Interactive caches list in `run_clean` (dry-run branch)**

At the top of `run_clean`, after building/filtering `plan`, add before the `if !apply` block:

```rust
    if !apply && interactive(json, plain) {
        let mut rows: Vec<tui::Row> = plan
            .auto_targets
            .iter()
            .map(|t| tui::Row::new(t.bytes, t.name.clone(), t.path.clone(),
                                   tui::RowMeta::Tag("safe".into())))
            .collect();
        rows.extend(plan.manual_targets.iter().map(|t| {
            tui::Row::new(t.bytes, t.name.clone(), t.path.clone(), tui::RowMeta::Tag("review".into()))
        }));
        let outcome = tui::run_list(rows, tui::Sort::Size,
            tui::ListConfig { title: "deepscan clean · caches".to_string(), home: home_dir() })?;
        if outcome.freed_bytes > 0 {
            println!("Moved {} to the Trash.", human(outcome.freed_bytes));
        }
        return Ok(());
    }
```

Headless behavior (`--apply`, `--json`, `--plain`, piped) is unchanged — caches only, as the spec requires.

- [ ] **Step 4: Build + smoke-test**

```bash
cargo build -q --release -p deepscan-cli
printf 'q' | script -q /dev/null ./target/release/deepscan clean >/dev/null 2>&1 && echo "clean TUI ok"
./target/release/deepscan reclaim --plain 2>/dev/null | head -1   # alias still works, plain
printf 'q' | script -q /dev/null ./target/release/deepscan uninstall Cursor >/dev/null 2>&1 && echo "uninstall TUI ok"
```
Expected: "clean TUI ok", the alias prints the dry-run header, "uninstall TUI ok".

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings
git add -A
git commit -m "feat(clean,uninstall): interactive lists; rename reclaim→clean (alias kept)"
```

---

### Task 8: Wire `dupes` (grouped rows + protect-last end to end)

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`Dupes` command: add `--plain`; interactive branch in `run_dupes`)

**Interfaces:**
- Consumes: `tui::{run_list, Row, RowMeta, Sort, ListConfig}`; `DuplicateGroup`

- [ ] **Step 1: Add `--plain` to `Dupes` and thread it through**

Add `#[arg(long)] plain: bool` to the `Dupes` variant and to `run_dupes`'s signature and its call site.

- [ ] **Step 2: Interactive branch — one row per copy, grouped by set**

In `run_dupes`, after computing `groups` and before the JSON branch:

```rust
    if interactive(json, plain) && !groups.is_empty() {
        let mut rows: Vec<tui::Row> = Vec::new();
        for (gid, group) in groups.iter().take(top).enumerate() {
            for path in &group.paths {
                rows.push(tui::Row {
                    bytes: group.bytes,
                    label: path.display().to_string(),
                    path: path.clone(),
                    selected: false,
                    meta: tui::RowMeta::Tag(format!("×{}", group.paths.len())),
                    group: Some(gid),
                });
            }
        }
        let title = format!("deepscan dupes · {} reclaimable", human(groups.iter().map(|g| g.wasted).sum()));
        let outcome = tui::run_list(rows, tui::Sort::Size,
            tui::ListConfig { title, home: home_dir() })?;
        if outcome.freed_bytes > 0 {
            println!("Moved {} of duplicates to the Trash.", human(outcome.freed_bytes));
        }
        return Ok(ExitCode::SUCCESS);
    }
```

`Row` is constructed with an explicit `group: Some(gid)` so the widget's protect-last rule keeps one copy of every set. `tui::Row` fields are all `pub` (Task 3).

- [ ] **Step 3: Build + smoke-test with protect-last**

```bash
cargo build -q --release -p deepscan-cli
printf 'aq' | script -q /dev/null ./target/release/deepscan dupes "$HOME/Downloads" --top 5 >/dev/null 2>&1 && echo "dupes TUI ok"
./target/release/deepscan dupes "$HOME/Downloads" --top 3 --plain 2>/dev/null | head -1
```
Expected: "dupes TUI ok" (the `a` selects all-but-one per set, `q` quits without deleting), and the plain header.

- [ ] **Step 4: clippy + full test + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A
git commit -m "feat(dupes): interactive grouped list, protects one copy per set"
```

---

## Phase 1 exit criteria

- `large`, `dupes`, `clean`, `uninstall` open an interactive select→Trash list on a TTY; `--plain`/`--json`/piped output unchanged.
- Every deletion (interactive and `clean --apply`) goes to the Trash; the `is_safe_to_delete` guard is enforced on every path.
- `reclaim` still works as an alias for `clean`.
- All pure state (`ListState`, action guard, `execute_reclaim_with`) is unit-tested; TUI lifecycle smoke-tested via faked TTY; CI green.

Deferred to later plans: Phase 2 `scan` dashboard (+ `space`/`anomalies` folded in as views/aliases), Phase 3 `explore` gains actions, Phase 4 the guided multi-source `clean` hub.
