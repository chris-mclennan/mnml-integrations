//! Tabular result renderer (`QueryResult::Rows`).

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
};

use crate::app::{App, Focus};
use crate::driver::QueryResult;
use crate::ui::themed;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Results;
    let Some(QueryResult::Rows {
        columns,
        rows,
        elapsed_ms,
        truncated,
        server_row_count,
    }) = &app.result
    else {
        return;
    };

    let filter = app.result_filter.to_ascii_lowercase();
    let filtered: Vec<&crate::driver::Row> = if filter.is_empty() {
        rows.iter().collect()
    } else {
        rows.iter()
            .filter(|r| {
                r.0.iter()
                    .any(|c| c.as_display().to_ascii_lowercase().contains(&filter))
            })
            .collect()
    };

    let mut title = if *truncated {
        format!(
            " results ({}/{} · {}ms · truncated) ",
            filtered.len(),
            server_row_count,
            elapsed_ms
        )
    } else {
        format!(" results ({} · {}ms) ", filtered.len(), elapsed_ms)
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

    if columns.is_empty() {
        f.render_widget(block, area);
        return;
    }

    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(c.name.clone()))
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(themed(Color::DarkGray))
            .add_modifier(Modifier::BOLD),
    );

    let rendered: Vec<Row> = filtered
        .iter()
        .map(|r| {
            Row::new(
                r.0.iter()
                    .map(|c| {
                        let s = c.as_display();
                        // Truncate wide cells inline so the table
                        // doesn't blow through the pane.
                        Cell::from(truncate(&s, 40))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    let n = columns.len().max(1);
    let widths: Vec<Constraint> = (0..n).map(|_| Constraint::Min(8)).collect();

    let table = Table::new(rendered, widths)
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

    // Empty-but-has-columns hint.
    if rows.is_empty() {
        let hint = Line::from(vec![Span::styled(
            "(query returned no rows)",
            Style::default().fg(themed(Color::DarkGray)),
        )]);
        let x = area.x + 2;
        let y = area.y + 2;
        f.buffer_mut().set_line(x, y, &hint, area.width - 2);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_strings_untouched() {
        assert_eq!(truncate("hi", 40), "hi");
    }

    #[test]
    fn truncate_long_strings_get_ellipsis() {
        let t = truncate(&"a".repeat(50), 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }
}
