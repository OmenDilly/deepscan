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

use deepscan_core::{human, Anomaly, AnomalyKind, DiskSpace, LargeFile, ScanReport, Severity};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        sections.push(Section {
            title: title.to_string(),
            rows,
        });
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

    Dashboard {
        root,
        disk: report.disk.clone(),
        snapshots,
        sections,
        nav,
        cursor: 0,
    }
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
        items.push(ListItem::new(
            Line::from(format!(" {}", section.title)).bold(),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

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
        LargeFile {
            path: PathBuf::from("/x").join(name),
            bytes,
            modified_days: Some(5),
            accessed_days: None,
        }
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
        assert!(titles.contains(&"Biggest files"));
        assert!(!titles.contains(&"Reclaimable"));
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
    }
}
