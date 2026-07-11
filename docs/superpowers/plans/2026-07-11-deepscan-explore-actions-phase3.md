# deepscan Interactive Phase 3 — `explore` gains select → Trash + reveal

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** The `explore` tree browser (and, for free, the dashboard's drilled-in explore from Phase 2) gains multi-select → Trash with a confirm modal, plus reveal-in-Finder — reusing the Phase-1 `action.rs` (guarded `trash_selected` via `is_safe_to_trash`, `reveal_in_finder`).

**Architecture:** One cohesive change to `crates/deepscan-cli/src/tui/tree.rs` (which already holds the explore state + render + loop together). Add a `selected` flag per `Entry`, selection/target/removal methods on `Explorer` (unit-tested pure), and wire them into `event_loop` (keymap) + `draw` (checkboxes + confirm modal + footer). Deletion goes to the Trash through the existing guarded action layer. `deepscan-core` is unchanged.

**Tech Stack:** Rust 2021, ratatui 0.29 + crossterm, the Phase-1 `action.rs`.

## Global Constraints

- Toolchain 1.96.0; ends fmt-clean, `cargo clippy --all-targets -- -D warnings` clean, all tests passing.
- Deletion routes through `super::action::trash_selected(targets, home)` — which guards every path with `deepscan_core::is_safe_to_trash` (human-confirmed model: allows deep/app paths, refuses root/near-root/home/`..`). Trash only after the confirm modal.
- Selection + Trash act on the CURRENT level only (the folder you're looking at). `Enter`/`→`/`l` stays descend; `d` (not Enter) opens the confirm.
- The explore navigation/streaming-sizing behavior and the root volume-reconciliation line are unchanged. Terminal still always restored on exit.

---

### Task 1: `explore` select → Trash + reveal (state, keymap, render)

**Files:**
- Modify: `crates/deepscan-cli/src/tui/tree.rs` (Entry, Explorer methods, `event_loop`, `draw`, imports, tests)

**Interfaces:**
- Consumes: `super::action::{trash_selected, reveal_in_finder}`, `deepscan_core::home_dir`.

- [ ] **Step 1: Add imports and the `selected` field**

At the top of `tree.rs`, add to the existing imports:
- `use std::collections::HashSet;` (next to the existing `HashMap` import)
- `use super::action::{reveal_in_finder, trash_selected};`
- extend the ratatui widgets import to include `Block, Borders, Clear`: `use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};`
- extend the layout import to include `Rect`: `use ratatui::layout::{Constraint, Layout, Rect};`

Add `selected: bool` to `Entry`:
```rust
#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    /// Files know their size at load; directories are `None` until sized.
    bytes: Option<u64>,
    /// Toggled by the user for a Trash action (current level only).
    selected: bool,
}
```

In `build_level`, set `selected: false` on the pushed `Entry` (the `items.push(Entry { name: …, path: p, is_dir, bytes })` becomes `Entry { name: …, path: p, is_dir, bytes, selected: false }`).

- [ ] **Step 2: Write the failing tests for the pure selection methods**

In `tree.rs`'s `#[cfg(test)] mod tests`, the `dir_entry` helper builds an `Entry`; add `selected: false` to it. Then add these tests:

```rust
    #[test]
    fn toggle_and_selected_targets() {
        let mut explorer = explorer(vec![
            dir_entry("a", Some(30)),
            dir_entry("b", Some(20)),
        ]);
        explorer.toggle(); // select "a" (cursor 0)
        assert_eq!(explorer.selected_count(), 1);
        assert_eq!(explorer.selected_bytes(), 30);
        let targets = explorer.selected_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].1, 30);
        explorer.toggle(); // deselect
        assert_eq!(explorer.selected_count(), 0);
    }

    #[test]
    fn remove_trashed_drops_entries_and_clamps() {
        let mut explorer = explorer(vec![
            dir_entry("a", Some(30)),
            dir_entry("b", Some(20)),
            dir_entry("c", Some(10)),
        ]);
        explorer.current_mut().selected = 2; // cursor on "c"
        let trashed: HashSet<PathBuf> =
            [PathBuf::from("/root/c")].into_iter().collect();
        explorer.remove_trashed(&trashed);
        assert_eq!(explorer.current().items.len(), 2);
        assert_eq!(explorer.current().selected, 1, "cursor clamps to last row");
    }
```

(The existing `explorer(...)`/`dir_entry(...)`/`level(...)` test helpers already build the state.)

- [ ] **Step 3: Run the tests to verify they fail (methods not defined)**

Run: `cargo test -p deepscan-cli -- tree 2>&1 | head -20` (or `cargo build -p deepscan-cli`)
Expected: compile error — `toggle`, `selected_count`, `selected_bytes`, `selected_targets`, `remove_trashed` not found.

- [ ] **Step 4: Implement the selection methods on `Explorer`**

Add to `impl Explorer` (after `drain_all`):

```rust
    /// Toggle the highlighted entry's selection (current level only).
    fn toggle(&mut self) {
        let level = self.current_mut();
        if let Some(entry) = level.items.get_mut(level.selected) {
            entry.selected = !entry.selected;
        }
    }

    fn selected_count(&self) -> usize {
        self.current().items.iter().filter(|e| e.selected).count()
    }

    fn selected_bytes(&self) -> u64 {
        self.current()
            .items
            .iter()
            .filter(|e| e.selected)
            .filter_map(|e| e.bytes)
            .sum()
    }

    /// Paths + sizes of the selected entries in the current level.
    fn selected_targets(&self) -> Vec<(PathBuf, u64)> {
        self.current()
            .items
            .iter()
            .filter(|e| e.selected)
            .map(|e| (e.path.clone(), e.bytes.unwrap_or(0)))
            .collect()
    }

    /// Drop entries whose path was trashed; clamp the cursor.
    fn remove_trashed(&mut self, trashed: &HashSet<PathBuf>) {
        let level = self.current_mut();
        level.items.retain(|e| !trashed.contains(&e.path));
        level.selected = level.selected.min(level.items.len().saturating_sub(1));
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p deepscan-cli -- tree`
Expected: the 2 new tests + existing explore tests PASS.

- [ ] **Step 6: Wire the keymap into `event_loop`**

Replace `event_loop`'s body with a version that adds a `confirming` flag, the Trash/reveal keys, and the confirm sub-mode. Keep the `tick` spinner + `drain_all` + `terminal.draw` structure; `draw` now takes `confirming`:

```rust
fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut explorer: Explorer,
) -> anyhow::Result<()> {
    let mut tick = 0usize;
    let mut confirming = false;
    let home = deepscan_core::home_dir();
    loop {
        explorer.drain_all();
        terminal.draw(|frame| draw(frame, &explorer, tick, confirming))?;

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if confirming {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                let targets = explorer.selected_targets();
                                let outcome = trash_selected(&targets, home.as_deref());
                                let failed: HashSet<PathBuf> =
                                    outcome.failures.iter().map(|(p, _)| p.clone()).collect();
                                let trashed: HashSet<PathBuf> = targets
                                    .iter()
                                    .map(|(p, _)| p.clone())
                                    .filter(|p| !failed.contains(p))
                                    .collect();
                                explorer.remove_trashed(&trashed);
                                confirming = false;
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                confirming = false;
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Up | KeyCode::Char('k') => explorer.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => explorer.move_down(),
                            KeyCode::Left | KeyCode::Char('h') => explorer.back(),
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                                explorer.descend()
                            }
                            KeyCode::Char(' ') => explorer.toggle(),
                            KeyCode::Char('f') => {
                                let level = explorer.current();
                                if let Some(entry) = level.items.get(level.selected) {
                                    reveal_in_finder(&entry.path);
                                }
                            }
                            KeyCode::Char('d') => {
                                if explorer.selected_count() > 0 {
                                    confirming = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        tick = tick.wrapping_add(1);
    }
    Ok(())
}
```

- [ ] **Step 7: Update `draw` — checkboxes, footer, confirm modal**

Change `draw`'s signature to `fn draw(frame: &mut Frame, explorer: &Explorer, tick: usize, confirming: bool)`.

In the row-building closure, prefix each row with a checkbox. Replace the two `format!` lines:
```rust
                Some(bytes) => {
                    let filled = (((bytes as f64 / max as f64) * BAR_WIDTH as f64).round()
                        as usize)
                        .min(BAR_WIDTH);
                    let bar = format!("{}{}", "█".repeat(filled), "·".repeat(BAR_WIDTH - filled));
                    let check = if entry.selected { "[x]" } else { "[ ]" };
                    format!(" {check} {bar}  {:>10}  {}{}", human(bytes), entry.name, suffix)
                }
                None => {
                    let spinner = SPINNER[tick % SPINNER.len()];
                    let check = if entry.selected { "[x]" } else { "[ ]" };
                    format!(
                        " {check} {:<width$}  {:>10}  {}{}",
                        format!("{spinner} sizing"),
                        "—",
                        entry.name,
                        suffix,
                        width = BAR_WIDTH
                    )
                }
```

Replace the footer render with a selection-aware footer:
```rust
    let footer = if explorer.selected_count() > 0 {
        format!(
            " {} selected · {}   space toggle · d Trash · f reveal · ←→ nav · q quit",
            explorer.selected_count(),
            human(explorer.selected_bytes())
        )
    } else {
        " ↑↓ move   → enter   ← back   space select   d Trash   f reveal   q quit".to_string()
    };
    frame.render_widget(Paragraph::new(footer).dim(), chunks[3]);

    if confirming {
        draw_confirm(frame, explorer);
    }
```

Add the modal + centering helpers at the end of the file (before `#[cfg(test)]`):
```rust
fn draw_confirm(frame: &mut Frame, explorer: &Explorer) {
    let area = centered(60, 7, frame.area());
    frame.render_widget(Clear, area);
    let text = format!(
        "Move {} item(s) ({}) to the Trash?\n\n[y] confirm    [n] cancel",
        explorer.selected_count(),
        human(explorer.selected_bytes())
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

- [ ] **Step 8: Build, smoke-test, gate, commit**

```bash
cd /Users/dmitrijacenko/projects/deepscan
cargo build -q --release -p deepscan-cli
# explore still launches + quits clean (restores terminal); selection/Trash keys are live:
printf 'q' | script -q /dev/null ./target/release/deepscan explore "$HOME/projects/deepscan" >/dev/null 2>&1 && echo "explore ok"
# the dashboard's drilled-in explore inherits the same loop — dashboard still launches:
( sleep 3; printf 'q' ) | script -q /dev/null ./target/release/deepscan scan "$HOME/projects/deepscan" >/dev/null 2>&1 && echo "dashboard ok"
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
```
Expected: "explore ok", "dashboard ok", clippy clean, tests pass.

```bash
git add -A && git commit -m "feat(explore): multi-select → Trash (confirm) + reveal-in-Finder"
```

---

## Phase 3 exit criteria

- In `explore` (standalone and drilled-in from the `scan` dashboard): `space` toggles selection, `d` opens a Trash confirm modal, `y` moves the selected entries in the current level to the recoverable Trash (guarded by `is_safe_to_trash`), `f` reveals; the entries disappear on success, guard-refused ones remain.
- Navigation, streaming sizing, the root volume-reconciliation line, and terminal restore are unchanged; selection methods are unit-tested; CI green.

Deferred: Phase 4 — the guided multi-source `clean` hub.
