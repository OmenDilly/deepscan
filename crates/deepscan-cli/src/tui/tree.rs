//! Interactive size explorer — the "CLI DaisyDisk".
//!
//! Opens instantly (one `read_dir` per level), then sizes **every folder in
//! parallel** — each folder's size streams into its own row the moment it's
//! computed, with a per-row spinner while it's still working. So you watch the
//! small folders resolve immediately and the big ones (Library) fill in last,
//! instead of staring at a single global counter. Files are sized instantly
//! from their metadata; only directories need the recursive walk. Sized levels
//! are cached (instant revisit) and parent levels keep sizing in the background
//! while you drill deeper. Read-only — exploration, not deletion.
//!
//! Navigation is a pure state machine ([`Explorer`]), unit-tested without a
//! terminal; the ratatui layer is a thin renderer.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use deepscan_core::{disk_space, human, measure_path_serial, DiskSpace};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use rayon::prelude::*;

const BAR_WIDTH: usize = 16;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    /// Files know their size at load; directories are `None` until sized.
    bytes: Option<u64>,
}

struct Level {
    path: PathBuf,
    items: Vec<Entry>,
    selected: usize,
    rx: Option<mpsc::Receiver<(PathBuf, u64)>>,
    pending: usize,
}

struct Explorer {
    stack: Vec<Level>,
    cache: HashMap<PathBuf, Vec<Entry>>,
    /// True capacity of the volume the root lives on — so the root view can be
    /// honest that a folder tree is only *part* of the disk macOS reports.
    disk: Option<DiskSpace>,
}

/// Size every directory in `dirs` in parallel, streaming each result back as it
/// finishes. Each folder is measured single-threaded so we never nest rayon.
fn spawn_sizer(dirs: Vec<PathBuf>) -> mpsc::Receiver<(PathBuf, u64)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        dirs.into_par_iter().for_each_with(tx, |tx, path| {
            let size = measure_path_serial(&path);
            let _ = tx.send((path, size));
        });
    });
    rx
}

/// Re-sort a level largest-first, keeping the cursor on the same item.
fn sort_by_size(level: &mut Level) {
    let cursor = level.items.get(level.selected).map(|e| e.path.clone());
    level
        .items
        .sort_by_key(|e| std::cmp::Reverse(e.bytes.unwrap_or(0)));
    level.selected = cursor
        .and_then(|p| level.items.iter().position(|e| e.path == p))
        .unwrap_or(0)
        .min(level.items.len().saturating_sub(1));
}

/// List a directory instantly (files sized, directories queued for sizing).
fn build_level(path: PathBuf, cache: &HashMap<PathBuf, Vec<Entry>>) -> Level {
    if let Some(items) = cache.get(&path) {
        return Level {
            path,
            items: items.clone(),
            selected: 0,
            rx: None,
            pending: 0,
        };
    }

    let mut items: Vec<Entry> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&path) {
        for entry in read_dir.filter_map(Result::ok) {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            let file_type = meta.file_type();
            let (is_dir, bytes) = if file_type.is_symlink() {
                continue;
            } else if file_type.is_dir() {
                (true, None)
            } else if file_type.is_file() {
                (false, Some(meta.len()))
            } else {
                continue;
            };
            items.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: p,
                is_dir,
                bytes,
            });
        }
    }
    items.sort_by_key(|e| e.name.to_lowercase());

    let dirs: Vec<PathBuf> = items
        .iter()
        .filter(|e| e.is_dir)
        .map(|e| e.path.clone())
        .collect();
    let pending = dirs.len();
    let rx = if dirs.is_empty() {
        None
    } else {
        Some(spawn_sizer(dirs))
    };

    let mut level = Level {
        path,
        items,
        selected: 0,
        rx,
        pending,
    };
    if level.rx.is_none() {
        sort_by_size(&mut level); // all files → final order is known immediately
    }
    level
}

/// Pull any finished folder sizes into the level. Returns the now-complete item
/// list (to cache) the moment the level finishes sizing.
fn drain_level(level: &mut Level) -> Option<Vec<Entry>> {
    let finished = {
        let Some(rx) = &level.rx else {
            return None;
        };
        loop {
            match rx.try_recv() {
                Ok((path, size)) => {
                    if let Some(item) = level.items.iter_mut().find(|e| e.path == path) {
                        item.bytes = Some(size);
                    }
                    level.pending = level.pending.saturating_sub(1);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    level.pending = 0;
                    break;
                }
            }
        }
        level.pending == 0
    };

    if finished {
        level.rx = None;
        sort_by_size(level);
        Some(level.items.clone())
    } else {
        None
    }
}

impl Explorer {
    fn current(&self) -> &Level {
        self.stack.last().expect("stack is never empty")
    }

    fn current_mut(&mut self) -> &mut Level {
        self.stack.last_mut().expect("stack is never empty")
    }

    fn move_up(&mut self) {
        let level = self.current_mut();
        level.selected = level.selected.saturating_sub(1);
    }

    fn move_down(&mut self) {
        let level = self.current_mut();
        if level.selected + 1 < level.items.len() {
            level.selected += 1;
        }
    }

    fn back(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    fn descend(&mut self) {
        let target = {
            let level = self.current();
            level
                .items
                .get(level.selected)
                .filter(|e| e.is_dir)
                .map(|e| e.path.clone())
        };
        if let Some(target) = target {
            let level = build_level(target, &self.cache);
            self.stack.push(level);
        }
    }

    /// Drain finished sizes from every level (parents keep sizing in the
    /// background), caching any that just completed.
    fn drain_all(&mut self) {
        let mut to_cache = Vec::new();
        for level in &mut self.stack {
            if let Some(items) = drain_level(level) {
                to_cache.push((level.path.clone(), items));
            }
        }
        for (path, items) in to_cache {
            self.cache.insert(path, items);
        }
    }
}

/// Set up the terminal, run the explorer, and always restore the terminal.
pub fn run(root: PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let disk = disk_space(&root);
    let explorer = Explorer {
        stack: vec![build_level(root, &HashMap::new())],
        cache: HashMap::new(),
        disk,
    };
    let outcome = event_loop(&mut terminal, explorer);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    outcome
}

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut explorer: Explorer,
) -> anyhow::Result<()> {
    let mut tick = 0usize;
    loop {
        explorer.drain_all();
        terminal.draw(|frame| draw(frame, &explorer, tick))?;

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Up | KeyCode::Char('k') => explorer.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => explorer.move_down(),
                        KeyCode::Left | KeyCode::Char('h') => explorer.back(),
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => explorer.descend(),
                        _ => {}
                    }
                }
            }
        }
        tick = tick.wrapping_add(1);
    }
    Ok(())
}

fn draw(frame: &mut Frame, explorer: &Explorer, tick: usize) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // path + tree total
        Constraint::Length(1), // reconciliation against the whole volume
        Constraint::Min(1),    // entries
        Constraint::Length(1), // key hints
    ])
    .split(frame.area());

    let level = explorer.current();
    let header = if level.pending > 0 {
        let known: u64 = level.items.iter().filter_map(|e| e.bytes).sum();
        format!(
            " {}   {} so far · sizing {} folder(s)…   ({} items)",
            level.path.display(),
            human(known),
            level.pending,
            level.items.len()
        )
    } else {
        let total: u64 = level.items.iter().filter_map(|e| e.bytes).sum();
        format!(
            " {}   {}   ({} items)",
            level.path.display(),
            human(total),
            level.items.len()
        )
    };
    frame.render_widget(Paragraph::new(Line::from(header)).bold(), chunks[0]);

    // At the root, reconcile this tree against the whole volume so we never
    // silently imply the folder total *is* the disk. Deeper levels leave it
    // blank — "elsewhere" only means something relative to the root.
    let at_root = explorer.stack.len() == 1;
    let recon = match (&explorer.disk, at_root) {
        (Some(disk), true) if level.pending == 0 => {
            let tree: u64 = level.items.iter().filter_map(|e| e.bytes).sum();
            let pct = tree.saturating_mul(100).checked_div(disk.used).unwrap_or(0);
            let elsewhere = disk.used.saturating_sub(tree);
            format!(
                " volume {} used of {} · this tree is {}% · {} elsewhere (system files + snapshots — see: deepscan space)",
                human(disk.used),
                human(disk.total),
                pct,
                human(elsewhere),
            )
        }
        (Some(disk), true) => format!(
            " volume {} used of {} · sizing this tree…",
            human(disk.used),
            human(disk.total),
        ),
        _ => String::new(),
    };
    frame.render_widget(Paragraph::new(Line::from(recon)).dim(), chunks[1]);

    let max = level
        .items
        .iter()
        .filter_map(|e| e.bytes)
        .max()
        .unwrap_or(1)
        .max(1);
    let rows: Vec<ListItem> = level
        .items
        .iter()
        .map(|entry| {
            let suffix = if entry.is_dir { "/" } else { "" };
            let line = match entry.bytes {
                Some(bytes) => {
                    let filled = (((bytes as f64 / max as f64) * BAR_WIDTH as f64).round()
                        as usize)
                        .min(BAR_WIDTH);
                    let bar = format!("{}{}", "█".repeat(filled), "·".repeat(BAR_WIDTH - filled));
                    format!(" {bar}  {:>10}  {}{}", human(bytes), entry.name, suffix)
                }
                None => {
                    let spinner = SPINNER[tick % SPINNER.len()];
                    format!(
                        " {:<width$}  {:>10}  {}{}",
                        format!("{spinner} sizing"),
                        "—",
                        entry.name,
                        suffix,
                        width = BAR_WIDTH
                    )
                }
            };
            ListItem::new(Line::from(line))
        })
        .collect();

    let list = List::new(rows)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶");
    let mut state = ListState::default();
    if !level.items.is_empty() {
        state.select(Some(level.selected));
    }
    frame.render_stateful_widget(list, chunks[2], &mut state);

    frame.render_widget(
        Paragraph::new(" ↑↓ move   → enter   ← back   q quit   (explore / for whole disk)").dim(),
        chunks[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_entry(name: &str, bytes: Option<u64>) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/root").join(name),
            is_dir: true,
            bytes,
        }
    }

    fn level(items: Vec<Entry>) -> Level {
        Level {
            path: PathBuf::from("/root"),
            items,
            selected: 0,
            rx: None,
            pending: 0,
        }
    }

    fn explorer(items: Vec<Entry>) -> Explorer {
        Explorer {
            stack: vec![level(items)],
            cache: HashMap::new(),
            disk: None,
        }
    }

    #[test]
    fn navigation_clamps_and_pops() {
        let mut explorer = explorer(vec![
            dir_entry("a", Some(30)),
            dir_entry("b", Some(20)),
            dir_entry("c", Some(10)),
        ]);
        explorer.move_up();
        assert_eq!(explorer.current().selected, 0, "clamps at top");
        explorer.move_down();
        explorer.move_down();
        explorer.move_down();
        assert_eq!(explorer.current().selected, 2, "clamps at bottom");

        explorer.stack.push(level(vec![dir_entry("x", Some(5))]));
        explorer.back();
        assert_eq!(explorer.current().path, PathBuf::from("/root"));
        explorer.back(); // no-op at root
        assert_eq!(explorer.stack.len(), 1);
    }

    #[test]
    fn sort_by_size_keeps_cursor() {
        let mut lvl = level(vec![dir_entry("a", Some(1)), dir_entry("b", Some(99))]);
        lvl.selected = 0; // cursor on "a"
        sort_by_size(&mut lvl);
        assert_eq!(lvl.items[0].name, "b", "largest first");
        assert_eq!(lvl.selected, 1, "cursor follows 'a' to its new row");
    }
}
