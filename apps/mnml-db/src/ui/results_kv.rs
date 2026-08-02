//! Key/value result renderer (`QueryResult::KeyValue`) — used by
//! the Redis driver.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Cell, Row, Table, TableState},
};

use crate::app::{App, Focus};
use crate::driver::{KeyValueType, QueryResult};
use crate::ui::themed;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Results;
    let Some(QueryResult::KeyValue {
        entries,
        elapsed_ms,
        truncated,
        server_row_count,
    }) = &app.result
    else {
        return;
    };

    let filter = app.result_filter.to_ascii_lowercase();
    let filtered: Vec<&crate::driver::KeyValueEntry> = if filter.is_empty() {
        entries.iter().collect()
    } else {
        entries
            .iter()
            .filter(|e| {
                e.key.to_ascii_lowercase().contains(&filter)
                    || e.value.to_ascii_lowercase().contains(&filter)
            })
            .collect()
    };

    let mut title = if *truncated {
        format!(
            " result ({}/{} · {}ms · truncated) ",
            filtered.len(),
            server_row_count,
            elapsed_ms
        )
    } else {
        format!(" result ({} · {}ms) ", filtered.len(), elapsed_ms)
    };
    if !filter.is_empty() {
        title.push_str(&format!(" /{filter}"));
    }
    let title_style = if focused {
        Style::default()
            .fg(themed(Color::Cyan))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(themed(Color::DarkGray))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, title_style));

    let header = Row::new(vec!["key", "value", "type"]).style(
        Style::default()
            .fg(themed(Color::DarkGray))
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = filtered
        .iter()
        .map(|e| {
            let type_chip = match e.type_hint {
                KeyValueType::Nil => "nil",
                KeyValueType::Str => "str",
                KeyValueType::Int => "int",
                KeyValueType::Bytes => "bytes",
            };
            Row::new(vec![
                Cell::from(e.key.clone()),
                Cell::from(truncate(&e.value, 60)),
                Cell::from(type_chip),
            ])
        })
        .collect();

    let widths = vec![
        Constraint::Percentage(35),
        Constraint::Percentage(55),
        Constraint::Percentage(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(themed(Color::DarkGray))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    state.select(Some(app.result_row.min(filtered.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}
