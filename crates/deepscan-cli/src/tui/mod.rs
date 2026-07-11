//! Interactive terminal views. `tree` is the explore browser; `widget`,
//! `action`, and `render` back the shared sized-list.

mod action;
mod dashboard;
mod render;
mod tree;
mod widget;

pub use action::TrashOutcome;
// TODO(phase2): consumed by Task 4's dashboard entry point; unused in this
// bin crate until then.
#[allow(unused_imports)]
pub use dashboard::{assemble_dashboard, run_dashboard, Dashboard};
pub use tree::run;
pub use widget::{Row, RowMeta, Sort};

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use action::{reveal_in_finder, trash_selected};
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

    // Capture the loop's result, then ALWAYS restore the terminal before
    // returning — so an IO error mid-session never leaves the shell in raw
    // mode / the alternate screen (mirrors tree.rs::run / event_loop).
    let outcome = list_loop(&mut terminal, ListState::new(rows, sort), &config);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    outcome
}

fn list_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut state: ListState,
    config: &ListConfig,
) -> anyhow::Result<TrashOutcome> {
    let mut confirming = false;
    let mut outcome = TrashOutcome::default();
    loop {
        terminal.draw(|f| render::draw(f, &state, &config.title, confirming))?;
        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if confirming {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let items: Vec<(PathBuf, u64)> = state
                        .selected()
                        .map(|r| (r.path.clone(), r.bytes))
                        .collect();
                    let round = trash_selected(&items, config.home.as_deref());
                    let trashed: HashSet<PathBuf> = items.iter().map(|(p, _)| p.clone()).collect();
                    let failed: HashSet<PathBuf> =
                        round.failures.iter().map(|(p, _)| p.clone()).collect();
                    state
                        .rows
                        .retain(|r| !trashed.contains(&r.path) || failed.contains(&r.path));
                    for r in &mut state.rows {
                        r.selected = false;
                    }
                    state.cursor = state.cursor.min(state.rows.len().saturating_sub(1));
                    outcome.freed_bytes += round.freed_bytes;
                    outcome.failures.extend(round.failures);
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
            KeyCode::Char('d') | KeyCode::Enter if state.selected_count() > 0 => {
                confirming = true;
            }
            _ => {}
        }
    }
    Ok(outcome)
}
