//! Interactive size explorer — the "CLI DaisyDisk" legibility wedge.
//!
//! Opens **instantly**: a directory's immediate children are listed from one
//! `read_dir` (no waiting), then their sizes are computed in a background thread
//! and filled in, with live progress shown in the header. So a huge root (a
//! full home directory) no longer blocks behind a bare spinner — you see the
//! structure right away and can navigate while sizes compute. Sized levels are
//! cached, so revisiting is instant. Read-only: this is for *exploring* where
//! the space went, not deleting (that's `reclaim`/`uninstall`).
//!
//! Navigation is a pure state machine ([`Explorer`]) so it's unit-testable
//! without a terminal; the ratatui layer is a thin renderer.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use deepscan_core::{human, progress, reset_progress, scan_children, ChildSize};
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

const BAR_WIDTH: usize = 16;

#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    /// `None` until the background sizing for this level finishes.
    bytes: Option<u64>,
}

struct Level {
    path: PathBuf,
    items: Vec<Entry>,
    selected: usize,
    loading: bool,
}

struct Explorer {
    stack: Vec<Level>,
    cache: HashMap<PathBuf, Vec<ChildSize>>,
}

type PendingLoad = (PathBuf, mpsc::Receiver<io::Result<(u64, Vec<ChildSize>)>>);

/// List a directory's immediate children instantly (sizes unknown for now).
fn load_level(path: PathBuf) -> Level {
    let mut items: Vec<Entry> = match std::fs::read_dir(&path) {
        Ok(read_dir) => read_dir
            .filter_map(Result::ok)
            .map(|entry| Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                bytes: None,
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    items.sort_by_key(|entry| entry.name.to_lowercase());
    Level {
        path,
        items,
        selected: 0,
        loading: true,
    }
}

fn level_from_sized(path: PathBuf, sized: Vec<ChildSize>) -> Level {
    let items = sized
        .into_iter()
        .map(|c| Entry {
            name: c.name,
            path: c.path,
            bytes: Some(c.bytes),
        })
        .collect();
    Level {
        path,
        items,
        selected: 0,
        loading: false,
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

    fn back(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        let level = self.current();
        level.items.get(level.selected).map(|e| e.path.clone())
    }

    /// Drop the sized (and sorted) results into the matching level + cache them,
    /// keeping the cursor on the same item if it survived.
    fn apply_sizes(&mut self, path: &PathBuf, sized: Vec<ChildSize>) {
        self.cache.insert(path.clone(), sized.clone());
        if let Some(level) = self.stack.iter_mut().find(|l| &l.path == path) {
            let previously_selected = level.items.get(level.selected).map(|e| e.path.clone());
            level.items = sized
                .into_iter()
                .map(|c| Entry {
                    name: c.name,
                    path: c.path,
                    bytes: Some(c.bytes),
                })
                .collect();
            level.loading = false;
            level.selected = previously_selected
                .and_then(|p| level.items.iter().position(|e| e.path == p))
                .unwrap_or(0)
                .min(level.items.len().saturating_sub(1));
        }
    }

    /// Descend into the selected entry if it's a directory.
    fn descend(&mut self) -> bool {
        let Some(target) = self.selected_path() else {
            return false;
        };
        if !target.is_dir() {
            return false;
        }
        if let Some(sized) = self.cache.get(&target).cloned() {
            self.stack.push(level_from_sized(target, sized));
        } else {
            self.stack.push(load_level(target));
        }
        true
    }
}

/// Set up the terminal, run the explorer, and always restore the terminal.
pub fn run(root: PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let explorer = Explorer {
        stack: vec![load_level(root)],
        cache: HashMap::new(),
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
    let mut pending: Option<PendingLoad> = None;

    loop {
        // Kick off sizing for the current level if it still needs it.
        if pending.is_none() && explorer.current().loading {
            let path = explorer.current().path.clone();
            if let Some(sized) = explorer.cache.get(&path).cloned() {
                explorer.apply_sizes(&path, sized);
            } else {
                reset_progress();
                let (tx, rx) = mpsc::channel();
                let scan_path = path.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(scan_children(&scan_path));
                });
                pending = Some((path, rx));
            }
        }

        terminal.draw(|frame| draw(frame, &explorer))?;

        if let Some((path, rx)) = &pending {
            if let Ok(result) = rx.try_recv() {
                if let Ok((_, sized)) = result {
                    let path = path.clone();
                    explorer.apply_sizes(&path, sized);
                }
                pending = None;
            }
        }

        if !event::poll(Duration::from_millis(80))? {
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
            KeyCode::Up | KeyCode::Char('k') => explorer.move_up(),
            KeyCode::Down | KeyCode::Char('j') => explorer.move_down(),
            KeyCode::Left | KeyCode::Char('h') => {
                explorer.back();
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                explorer.descend();
            }
            _ => {}
        }

        // If we navigated to a different level that still needs sizing, re-target
        // the background scan to it — so drilling in doesn't wait for the parent.
        let retarget = pending.as_ref().is_some_and(|(path, _)| {
            explorer.current().loading && path != &explorer.current().path
        });
        if retarget {
            pending = None;
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, explorer: &Explorer) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let level = explorer.current();

    let header = if level.loading {
        let (dirs, bytes) = progress();
        format!(
            " {}   computing… {} scanned · {} dirs",
            level.path.display(),
            human(bytes),
            dirs
        )
    } else {
        let total: u64 = level.items.iter().filter_map(|e| e.bytes).sum();
        format!(
            " {}   {}  ({} items)",
            level.path.display(),
            human(total),
            level.items.len()
        )
    };
    frame.render_widget(Paragraph::new(Line::from(header)).bold(), chunks[0]);

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
            let line = match entry.bytes {
                Some(bytes) => {
                    let filled = (((bytes as f64 / max as f64) * BAR_WIDTH as f64).round()
                        as usize)
                        .min(BAR_WIDTH);
                    let bar = format!("{}{}", "█".repeat(filled), "·".repeat(BAR_WIDTH - filled));
                    format!(" {bar}  {:>10}  {}", human(bytes), entry.name)
                }
                None => format!(" {}  {:>10}  {}", " ".repeat(BAR_WIDTH), "—", entry.name),
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
    frame.render_stateful_widget(list, chunks[1], &mut state);

    let footer = if level.loading {
        " ↑↓ move   → enter   ← back   q quit    (sizes still computing…)"
    } else {
        " ↑↓ move   → enter   ← back   q quit"
    };
    frame.render_widget(Paragraph::new(footer).dim(), chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(name: &str, bytes: u64) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/root").join(name),
            bytes: Some(bytes),
        }
    }

    fn explorer_with(items: Vec<Entry>) -> Explorer {
        Explorer {
            stack: vec![Level {
                path: PathBuf::from("/root"),
                items,
                selected: 0,
                loading: false,
            }],
            cache: HashMap::new(),
        }
    }

    #[test]
    fn navigation_clamps_and_stacks() {
        let mut explorer = explorer_with(vec![sized("a", 30), sized("b", 20), sized("c", 10)]);
        explorer.move_up();
        assert_eq!(explorer.current().selected, 0, "clamps at top");
        explorer.move_down();
        explorer.move_down();
        explorer.move_down();
        assert_eq!(explorer.current().selected, 2, "clamps at bottom");

        explorer.stack.push(Level {
            path: PathBuf::from("/root/a"),
            items: vec![sized("x", 5)],
            selected: 0,
            loading: false,
        });
        assert!(explorer.back());
        assert_eq!(explorer.current().path, PathBuf::from("/root"));
        assert!(!explorer.back(), "cannot pop the root");
    }

    #[test]
    fn apply_sizes_fills_and_keeps_cursor() {
        let no_size = |name: &str| Entry {
            name: name.to_string(),
            path: PathBuf::from("/root").join(name),
            bytes: None,
        };
        let mut explorer = explorer_with(vec![no_size("a"), no_size("b")]);
        explorer.current_mut().loading = true;
        explorer.current_mut().selected = 0; // on "a"

        // scan_children returns sorted-desc; "b" is bigger.
        let results = vec![
            ChildSize {
                name: "b".into(),
                path: PathBuf::from("/root/b"),
                bytes: 99,
            },
            ChildSize {
                name: "a".into(),
                path: PathBuf::from("/root/a"),
                bytes: 1,
            },
        ];
        explorer.apply_sizes(&PathBuf::from("/root"), results);

        assert!(!explorer.current().loading);
        assert_eq!(explorer.current().items[0].name, "b", "sorted by size");
        assert_eq!(explorer.current().items[0].bytes, Some(99));
        assert_eq!(
            explorer.current().selected,
            1,
            "cursor follows 'a' to its new row"
        );
        assert!(explorer.cache.contains_key(&PathBuf::from("/root")));
    }
}
