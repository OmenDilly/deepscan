# deepscan Interactive Phase 2 — the `scan` dashboard

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Interactive `scan` on a terminal opens a navigable **dashboard** — disk accounting + reclaimable buckets + size anomalies + biggest files + leak signatures in one screen — where Enter drills into `explore` at a folder (reusing the explore loop on the same terminal) or reveals a file in Finder.

**Architecture:** A new self-contained `tui/dashboard.rs` (pure state + ratatui render + run loop, following the `tree.rs` precedent). Drill-in reuses the existing explore event loop via a new `pub(crate) explore_at(&mut terminal, root)`. Non-interactive `scan` is UNCHANGED and stays fast — the dashboard performs the extra anomaly + large-file walk only when actually opened (opening it is an interactive, spinner-covered action). `deepscan-core` does not change.

**Tech Stack:** Rust 2021, ratatui 0.29 + crossterm.

## Global Constraints

- Toolchain 1.96.0; every task ends fmt-clean, `cargo clippy --all-targets -- -D warnings` clean, all tests passing.
- Non-interactive `scan` (piped / `--json` / `--plain`) output and performance are UNCHANGED — do not add the anomaly/large-file walk to the fast default path. `--json` schema is unchanged.
- Interactive launches only when `interactive(json, plain)` is true (the helper added in Phase 1). Deletion is not part of this phase (the dashboard reveals/drills only; no Trash from the dashboard itself — drilling into `explore` gets Phase 3's actions later).
- `human(bytes)` is the only byte formatter. Follow `tree.rs`'s pure-state-machine + thin-renderer pattern.

---

### Task 1: Expose `explore_at` for nested drill-in (no behavior change)

**Files:**
- Modify: `crates/deepscan-cli/src/tui/tree.rs` (`run`, add `explore_at`)

**Interfaces:**
- Produces: `pub(crate) fn explore_at<B: ratatui::backend::Backend>(terminal: &mut ratatui::Terminal<B>, root: std::path::PathBuf) -> anyhow::Result<()>` — runs the explore browser on an already-set-up terminal (raw mode + alt screen owned by the caller).

- [ ] **Step 1: Add `explore_at` and refactor `run` to use it**

Replace `tree.rs`'s `run` with a version that factors the Explorer construction + loop into `explore_at`:

```rust
pub fn run(root: PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let outcome = explore_at(&mut terminal, root);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    outcome
}

/// Run the explore browser on a terminal the caller already set up (raw mode +
/// alternate screen). Lets the dashboard drill in without re-initializing the
/// terminal — no flicker. Returns when the user quits the browser (`q`/Esc).
pub(crate) fn explore_at<B: Backend>(terminal: &mut Terminal<B>, root: PathBuf) -> anyhow::Result<()> {
    let disk = disk_space(&root);
    let explorer = Explorer {
        stack: vec![build_level(root, &HashMap::new())],
        cache: HashMap::new(),
        disk,
    };
    event_loop(terminal, explorer)
}
```

(`event_loop`, `build_level`, `Explorer`, `disk_space` are all already in this file.)

- [ ] **Step 2: Build + verify explore unchanged**

Run: `cargo build -p deepscan-cli && cargo test -q`
Then smoke-test explore still launches/quits:
```bash
cargo build -q --release -p deepscan-cli
printf 'q' | script -q /dev/null ./target/release/deepscan explore "$HOME/projects/deepscan" >/dev/null 2>&1 && echo "explore ok"
```
Expected: builds, tests pass, "explore ok".

- [ ] **Step 3: clippy + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "refactor(tui): extract explore_at for nested drill-in (no behavior change)"
```

---

### Task 2: Dashboard state — model, assembly, navigation (pure)

**Files:**
- Create: `crates/deepscan-cli/src/tui/dashboard.rs`
- Modify: `crates/deepscan-cli/src/tui/mod.rs` (add `mod dashboard;`)

**Interfaces:**
- Produces: `pub struct Dashboard`
- Produces: `pub fn assemble_dashboard(root: PathBuf, report: &ScanReport, anomalies: &[Anomaly], largest: &[LargeFile], snapshots: usize) -> Dashboard`
- Produces (crate-internal, used by Task 3): `RowKind`, `DashRow`, `Section`, and `Dashboard::{move_up, move_down, current}`.

- [ ] **Step 1: Write the module with the pure state + tests**

Create `crates/deepscan-cli/src/tui/dashboard.rs`:

```rust
//! The interactive `scan` dashboard — a navigable overview folding disk
//! accounting, reclaimable buckets, size anomalies, biggest files, and leak
//! signatures into one screen. Enter drills into `explore` at a folder (reusing
//! the explore loop on the same terminal) or reveals a file in Finder.
//!
//! Non-interactive `scan` is unchanged and stays fast: the extra anomaly /
//! large-file walk runs only when the dashboard is actually opened. Navigation
//! is a pure state machine (unit-tested without a terminal); the ratatui layer
//! (Task 3) is a thin renderer.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use deepscan_core::{
    human, Anomaly, AnomalyKind, DiskSpace, LargeFile, ScanReport, Severity,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState as UiListState, Paragraph};
use ratatui::{Frame, Terminal};

use super::action::reveal_in_finder;
use super::tree::explore_at;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    Dir,
    File,
}

pub(crate) struct DashRow {
    pub bytes: u64,
    pub label: String,
    pub tag: Option<String>,
    pub path: PathBuf,
    pub kind: RowKind,
}

pub(crate) struct Section {
    pub title: String,
    pub rows: Vec<DashRow>,
}

pub struct Dashboard {
    root: PathBuf,
    disk: Option<DiskSpace>,
    snapshots: usize,
    sections: Vec<Section>,
    /// (section, row) positions in reading order — the navigable rows.
    nav: Vec<(usize, usize)>,
    cursor: usize,
}

fn sev_tag(severity: Severity) -> String {
    match severity {
        Severity::Critical => "CRIT".to_string(),
        Severity::Warn => "WARN".to_string(),
        Severity::Info => "info".to_string(),
    }
}

fn push_section(sections: &mut Vec<Section>, title: &str, rows: Vec<DashRow>) {
    if !rows.is_empty() {
        sections.push(Section { title: title.to_string(), rows });
    }
}

/// Assemble the dashboard from a (fast) scan report plus the anomaly and
/// largest-file walks. Empty sections are dropped.
pub fn assemble_dashboard(
    root: PathBuf,
    report: &ScanReport,
    anomalies: &[Anomaly],
    largest: &[LargeFile],
    snapshots: usize,
) -> Dashboard {
    let mut sections = Vec::new();

    let reclaimable = report
        .buckets
        .iter()
        .map(|b| DashRow {
            bytes: b.bytes,
            label: b.name.clone(),
            tag: Some(if b.safe_auto { "safe" } else { "review" }.to_string()),
            path: b.path.clone(),
            kind: RowKind::Dir,
        })
        .collect();
    push_section(&mut sections, "Reclaimable", reclaimable);

    let app_data = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::AppData)
        .map(|a| DashRow {
            bytes: a.bytes,
            label: a.name.clone(),
            tag: Some(sev_tag(a.severity)),
            path: a.path.clone(),
            kind: RowKind::Dir,
        })
        .collect();
    push_section(&mut sections, "App data — review before removing", app_data);

    let caches = anomalies
        .iter()
        .filter(|a| a.kind == AnomalyKind::Cache)
        .map(|a| DashRow {
            bytes: a.bytes,
            label: a.name.clone(),
            tag: Some("cache".to_string()),
            path: a.path.clone(),
            kind: RowKind::Dir,
        })
        .collect();
    push_section(&mut sections, "Caches — safe to clear", caches);

    let files = largest
        .iter()
        .map(|f| DashRow {
            bytes: f.bytes,
            label: f.path.display().to_string(),
            tag: f.modified_days.map(|d| format!("{d}d")),
            path: f.path.clone(),
            kind: RowKind::File,
        })
        .collect();
    push_section(&mut sections, "Biggest files", files);

    let leaks = report
        .findings
        .iter()
        .map(|f| DashRow {
            bytes: f.bytes,
            label: f.name.clone(),
            tag: Some(sev_tag(f.severity)),
            path: f.path.clone(),
            kind: RowKind::Dir,
        })
        .collect();
    push_section(&mut sections, "Leak signatures", leaks);

    let mut nav = Vec::new();
    for (si, section) in sections.iter().enumerate() {
        for ri in 0..section.rows.len() {
            nav.push((si, ri));
        }
    }

    Dashboard { root, disk: report.disk.clone(), snapshots, sections, nav, cursor: 0 }
}

impl Dashboard {
    fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_down(&mut self) {
        if self.cursor + 1 < self.nav.len() {
            self.cursor += 1;
        }
    }

    fn current(&self) -> Option<&DashRow> {
        self.nav
            .get(self.cursor)
            .map(|&(si, ri)| &self.sections[si].rows[ri])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn anomaly(name: &str, bytes: u64, kind: AnomalyKind) -> Anomaly {
        Anomaly {
            zone: "z".into(),
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            bytes,
            kind,
            median_bytes: 1,
            ratio: None,
            siblings: 2,
            severity: Severity::Info,
        }
    }

    fn large(name: &str, bytes: u64) -> LargeFile {
        LargeFile { path: PathBuf::from("/x").join(name), bytes, modified_days: Some(5), accessed_days: None }
    }

    fn empty_report() -> ScanReport {
        ScanReport {
            root: PathBuf::from("/x"),
            disk: None,
            total_bytes: 0,
            children: vec![],
            tree: None,
            reclaimable_bytes: 0,
            buckets: vec![],
            findings: vec![],
        }
    }

    #[test]
    fn assembles_only_nonempty_sections_and_splits_anomalies() {
        let report = empty_report();
        let anomalies = vec![
            anomaly("Claude", 20, AnomalyKind::AppData),
            anomaly("CocoaPods", 4, AnomalyKind::Cache),
        ];
        let largest = vec![large("big.bin", 99)];
        let d = assemble_dashboard(PathBuf::from("/x"), &report, &anomalies, &largest, 3);
        // App data, Caches, Biggest files — Reclaimable + Leaks empty, dropped.
        let titles: Vec<&str> = d.sections.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.starts_with("App data")));
        assert!(titles.iter().any(|t| t.starts_with("Caches")));
        assert!(titles.iter().any(|t| *t == "Biggest files"));
        assert!(!titles.iter().any(|t| *t == "Reclaimable"));
        assert_eq!(d.nav.len(), 3, "three navigable rows");
        assert_eq!(d.snapshots, 3);
    }

    #[test]
    fn navigation_clamps_and_reports_current() {
        let report = empty_report();
        let anomalies = vec![
            anomaly("a", 3, AnomalyKind::AppData),
            anomaly("b", 2, AnomalyKind::AppData),
        ];
        let mut d = assemble_dashboard(PathBuf::from("/x"), &report, &anomalies, &[], 0);
        assert_eq!(d.current().map(|r| r.label.as_str()), Some("a"));
        d.move_up();
        assert_eq!(d.cursor, 0, "clamps at top");
        d.move_down();
        assert_eq!(d.current().map(|r| r.label.as_str()), Some("b"));
        d.move_down();
        assert_eq!(d.cursor, 1, "clamps at bottom");
        assert_eq!(d.current().map(|r| r.kind), Some(RowKind::Dir));
        let _ = Path::new("/x");
    }
}
```

- [ ] **Step 2: Register the module**

In `tui/mod.rs` add `mod dashboard;` (keep alphabetical-ish with the others) and `pub use dashboard::{assemble_dashboard, Dashboard};`. (The render/run loop `run_dashboard` is added in Task 3; export it then.)

- [ ] **Step 3: Run the tests**

Run: `cargo test -p deepscan-cli dashboard`
Expected: 2 tests PASS.

- [ ] **Step 4: clippy + commit**

Note: `Dashboard::{move_up,move_down,current}`, `RowKind`/`DashRow`/`Section`, and the ratatui/render imports at the top of the file are consumed by Task 3 (same module); Task 2 alone leaves them unused (dead code AND unused imports). Add `#![allow(dead_code, unused_imports)]` at the top of `dashboard.rs` (after the `//!` doc) with `// TODO(phase2): remove once Task 3 adds the render + run loop.`, then remove it in Task 3.

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(tui): dashboard state — assembly, sections, navigation"
```

---

### Task 3: Dashboard render + run loop (with drill-in)

**Files:**
- Modify: `crates/deepscan-cli/src/tui/dashboard.rs` (add `draw`, `run_dashboard`, `dashboard_loop`; remove the Task-2 dead-code allow)
- Modify: `crates/deepscan-cli/src/tui/mod.rs` (export `run_dashboard`)

**Interfaces:**
- Produces: `pub fn run_dashboard(dashboard: Dashboard) -> anyhow::Result<()>`

- [ ] **Step 1: Add the renderer and loop; drop the dead-code allow**

Remove the `#![allow(dead_code, unused_imports)]` line added in Task 2 (everything is consumed now), then append to `dashboard.rs`:

```rust
/// Set up the terminal, run the dashboard, and always restore the terminal
/// (mirrors tree.rs::run — restore happens even if the loop errors).
pub fn run_dashboard(mut dashboard: Dashboard) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let outcome = dashboard_loop(&mut terminal, &mut dashboard);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    outcome
}

fn dashboard_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    dashboard: &mut Dashboard,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, dashboard))?;
        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => dashboard.move_up(),
            KeyCode::Down | KeyCode::Char('j') => dashboard.move_down(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                // Clone out of the borrow before acting, so we can pass the
                // terminal on for a nested explore.
                let target = dashboard.current().map(|r| (r.kind, r.path.clone()));
                if let Some((kind, path)) = target {
                    match kind {
                        RowKind::Dir => explore_at(terminal, path)?,
                        RowKind::File => reveal_in_finder(&path),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, dashboard: &Dashboard) {
    let chunks = Layout::vertical([
        Constraint::Length(2), // path + disk accounting
        Constraint::Min(1),    // sections
        Constraint::Length(1), // key hints
    ])
    .split(frame.area());

    let disk_line = match &dashboard.disk {
        Some(disk) => format!(
            " {} used of {}  ·  {} free  ·  {} snapshot(s)",
            human(disk.used),
            human(disk.total),
            human(disk.free),
            dashboard.snapshots
        ),
        None => format!(" {} snapshot(s)", dashboard.snapshots),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(" deepscan · {}", dashboard.root.display())).bold(),
            Line::from(disk_line).dim(),
        ]),
        chunks[0],
    );

    // Build a flat item list (section headers + rows); map the nav cursor to the
    // matching item index so the highlight lands on the right row.
    let mut items: Vec<ListItem> = Vec::new();
    let mut cursor_item = 0usize;
    let mut nav_idx = 0usize;
    for section in &dashboard.sections {
        items.push(ListItem::new(Line::from(format!(" {}", section.title)).bold()));
        for row in &section.rows {
            if nav_idx == dashboard.cursor {
                cursor_item = items.len();
            }
            let tag = row.tag.as_deref().unwrap_or("");
            items.push(ListItem::new(Line::from(format!(
                "   {:>10}  {:>6}  {}",
                human(row.bytes),
                tag,
                row.label
            ))));
            nav_idx += 1;
        }
    }

    let list = List::new(items)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶");
    let mut state = UiListState::default();
    if !dashboard.nav.is_empty() {
        state.select(Some(cursor_item));
    }
    frame.render_stateful_widget(list, chunks[1], &mut state);

    frame.render_widget(
        Paragraph::new(" ↑↓ move   → / enter  explore folder · reveal file   q quit").dim(),
        chunks[2],
    );
}
```

- [ ] **Step 2: Export `run_dashboard`**

In `tui/mod.rs`, extend the dashboard re-export to `pub use dashboard::{assemble_dashboard, run_dashboard, Dashboard};`.

- [ ] **Step 3: Build**

Run: `cargo build -p deepscan-cli`
Expected: builds (no warnings; the Task-2 dead-code allow is gone and everything is now consumed).

- [ ] **Step 4: clippy + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A && git commit -m "feat(tui): dashboard renderer + run loop with explore drill-in"
```

---

### Task 4: Wire interactive `scan` to the dashboard

**Files:**
- Modify: `crates/deepscan-cli/src/main.rs` (`Scan` command: add `--plain`; interactive branch in the `Commands::Scan` arm)

**Interfaces:**
- Consumes: `tui::{assemble_dashboard, run_dashboard}`, `detect_anomalies`, `default_zones`, `find_large_files`, `space_report`, `build_report` (all already imported in main.rs).

- [ ] **Step 1: Add `--plain` to the `Scan` command**

In the `Scan { .. }` variant add:
```rust
        /// Force the plain printed report even in a terminal.
        #[arg(long)]
        plain: bool,
```
and destructure `plain` in the `Commands::Scan { .. }` match arm.

- [ ] **Step 2: Insert the interactive dashboard branch**

In `main`, in the `Commands::Scan` arm, AFTER `let root = ...; let signatures = ...;` and the `depth`/`include_tree` lines, but BEFORE the existing `let report = with_spinner("scanning", ...)`, insert:

```rust
            if interactive(json, plain) {
                // The dashboard does extra work (anomalies + biggest files) the
                // fast default scan skips — fine, opening it is interactive.
                let report = build_report(&root, top, false, 0, &signatures)?;
                let root_for_walk = root.clone();
                let (anomalies, largest, snapshots) = with_spinner("scanning", move || {
                    let anomalies = detect_anomalies(&default_zones());
                    let mut largest = find_large_files(&root_for_walk, 100 * 1024 * 1024);
                    largest.truncate(12);
                    let snapshots = space_report(&root_for_walk).snapshots.len();
                    (anomalies, largest, snapshots)
                });
                let dashboard =
                    tui::assemble_dashboard(root, &report, &anomalies, &largest, snapshots);
                tui::run_dashboard(dashboard)?;
                return Ok(ExitCode::SUCCESS);
            }
```

(`main` returns `anyhow::Result<ExitCode>`, so `?` and the early `return Ok(...)` are valid. The existing non-interactive report/render/exit-code code below is untouched.)

- [ ] **Step 3: Build + smoke-test (interactive opens; non-interactive unchanged)**

```bash
cargo build -q --release -p deepscan-cli
# Interactive dashboard launches + quits cleanly (feed q after the scan spinner):
( sleep 3; printf 'q' ) | script -q /dev/null ./target/release/deepscan scan "$HOME/projects/deepscan" >/dev/null 2>&1 && echo "dashboard ok"
# Non-interactive unchanged (fast, same output, valid json):
./target/release/deepscan scan "$HOME/projects/deepscan" --plain 2>/dev/null | head -3
./target/release/deepscan scan "$HOME/projects/deepscan" --json 2>/dev/null | python3 -c "import json,sys;json.load(sys.stdin);print('scan json ok')"
```
Expected: "dashboard ok" (launches, drill-in available, quits restoring terminal), the plain report prints, "scan json ok". (`scan` on a small path so anomalies/large are quick; the dashboard scans the zones regardless — that's expected.)

- [ ] **Step 4: clippy + full test + commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test -q
git add -A && git commit -m "feat(scan): interactive dashboard on a TTY; plain/--json/piped unchanged"
```

---

## Phase 2 exit criteria

- `deepscan scan` on a terminal opens a navigable dashboard (disk accounting + reclaimable + app-data/cache anomalies + biggest files + leaks); Enter drills into `explore` at a folder (same terminal, no flicker) or reveals a file; `q` quits.
- Piped / `--json` / `--plain` `scan` output and speed are unchanged.
- Terminal is always restored; nav is a unit-tested pure state machine; CI green.

Deferred: Phase 3 (`explore` gains select→Trash) — the dashboard's drilled-in explore inherits those actions automatically once Phase 3 lands. Phase 4 (guided multi-source `clean` hub).
