//! Interactive size explorer — the "CLI DaisyDisk" legibility wedge.
//!
//! A lazy, cached, navigable tree. Each level's child sizes are computed on
//! demand in a background thread (so the UI never freezes) and cached, so
//! revisiting a directory is instant. Read-only: this is for *exploring* where
//! the space went, not deleting — that's what `reclaim`/`uninstall` are for.
//!
//! The navigation is a plain state machine ([`Explorer`]) so it's unit-testable
//! without a terminal; the ratatui layer below it is a thin renderer.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use deepscan_core::{human, scan_children, ChildSize};
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

struct Level {
    path: PathBuf,
    items: Vec<ChildSize>,
    selected: usize,
}

/// The explorer's navigation state — a stack of visited levels plus a size
/// cache. Pure (no terminal), so it's unit-tested below.
struct Explorer {
    stack: Vec<Level>,
    cache: HashMap<PathBuf, Vec<ChildSize>>,
}

impl Explorer {
    fn new(root: PathBuf, items: Vec<ChildSize>) -> Self {
        let mut cache = HashMap::new();
        cache.insert(root.clone(), items.clone());
        Explorer {
            stack: vec![Level {
                path: root,
                items,
                selected: 0,
            }],
            cache,
        }
    }

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

    /// Pop one level; returns false if already at the root.
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
        level.items.get(level.selected).map(|c| c.path.clone())
    }

    fn push(&mut self, path: PathBuf, items: Vec<ChildSize>) {
        self.stack.push(Level {
            path,
            items,
            selected: 0,
        });
    }
}

/// Set up the terminal, run the explorer, and always restore the terminal.
pub fn run(root: PathBuf, items: Vec<ChildSize>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let outcome = event_loop(&mut terminal, Explorer::new(root, items));

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    outcome
}

type PendingLoad = (PathBuf, mpsc::Receiver<io::Result<(u64, Vec<ChildSize>)>>);

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut explorer: Explorer,
) -> anyhow::Result<()> {
    let mut pending: Option<PendingLoad> = None;

    loop {
        terminal.draw(|frame| draw(frame, &explorer, pending.is_some()))?;

        // A background scan finished → cache + descend into it.
        if let Some((path, rx)) = &pending {
            if let Ok(result) = rx.try_recv() {
                if let Ok((_, items)) = result {
                    explorer.cache.insert(path.clone(), items.clone());
                    explorer.push(path.clone(), items);
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
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter if pending.is_none() => {
                descend(&mut explorer, &mut pending);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Descend into the selected directory — instantly if cached, else kick off a
/// background scan whose result is picked up next loop.
fn descend(explorer: &mut Explorer, pending: &mut Option<PendingLoad>) {
    let Some(target) = explorer.selected_path() else {
        return;
    };
    if !target.is_dir() {
        return;
    }
    if let Some(cached) = explorer.cache.get(&target).cloned() {
        explorer.push(target, cached);
        return;
    }
    let (tx, rx) = mpsc::channel();
    let scan_target = target.clone();
    std::thread::spawn(move || {
        let _ = tx.send(scan_children(&scan_target));
    });
    *pending = Some((target, rx));
}

fn draw(frame: &mut Frame, explorer: &Explorer, loading: bool) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let level = explorer.current();
    let total: u64 = level.items.iter().map(|item| item.bytes).sum();

    let header = Paragraph::new(Line::from(format!(
        " {}   {}",
        level.path.display(),
        human(total)
    )))
    .bold();
    frame.render_widget(header, chunks[0]);

    let max = level.items.first().map(|i| i.bytes).unwrap_or(1).max(1);
    let rows: Vec<ListItem> = level
        .items
        .iter()
        .map(|child| {
            let filled = (((child.bytes as f64 / max as f64) * BAR_WIDTH as f64).round() as usize)
                .min(BAR_WIDTH);
            let bar = format!("{}{}", "█".repeat(filled), "·".repeat(BAR_WIDTH - filled));
            ListItem::new(Line::from(format!(
                " {bar}  {:>10}  {}",
                human(child.bytes),
                child.name
            )))
        })
        .collect();

    let list = List::new(rows)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶");
    let mut state = ListState::default();
    state.select(Some(level.selected));
    frame.render_stateful_widget(list, chunks[1], &mut state);

    let footer = if loading {
        " scanning…    q quit"
    } else {
        " ↑↓ move    → enter    ← back    q quit"
    };
    frame.render_widget(Paragraph::new(footer).dim(), chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn child(name: &str, bytes: u64) -> ChildSize {
        ChildSize {
            name: name.to_string(),
            path: Path::new("/root").join(name),
            bytes,
        }
    }

    #[test]
    fn navigation_clamps_and_stacks() {
        let items = vec![child("a", 30), child("b", 20), child("c", 10)];
        let mut explorer = Explorer::new(PathBuf::from("/root"), items);

        assert_eq!(explorer.current().selected, 0);
        explorer.move_up();
        assert_eq!(explorer.current().selected, 0, "clamps at top");
        explorer.move_down();
        explorer.move_down();
        explorer.move_down();
        assert_eq!(explorer.current().selected, 2, "clamps at bottom");

        explorer.push(PathBuf::from("/root/a"), vec![child("x", 5)]);
        assert_eq!(explorer.current().path, PathBuf::from("/root/a"));
        assert_eq!(explorer.current().selected, 0, "new level starts at top");

        assert!(explorer.back());
        assert_eq!(explorer.current().path, PathBuf::from("/root"));
        assert!(!explorer.back(), "cannot pop the root");
    }
}
