//! ratatui drawing for the shared sized-list and its confirm modal. Pure view
//! over `ListState`; all state transitions live in `widget.rs`/`mod.rs`.

use deepscan_core::human;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState as UiListState, Paragraph,
};
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
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}
