//! Top strip: connection label + engine chip + driver describe().

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;
use crate::ui::themed;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let (label, engine, describe, err) = match app.active_conn() {
        Some(c) => (
            c.spec.display_label().to_string(),
            c.spec.engine.clone(),
            c.describe
                .clone()
                .unwrap_or_else(|| "(not connected)".into()),
            c.last_error.clone(),
        ),
        None => ("(no connection)".into(), "-".into(), "-".into(), None),
    };

    let line1 = Line::from(vec![
        Span::styled(
            " mnml-db ",
            Style::default()
                .fg(themed(Color::Black))
                .bg(themed(Color::Cyan)),
        ),
        Span::raw("  "),
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            format!("[{engine}]"),
            Style::default().fg(themed(Color::Cyan)),
        ),
        Span::raw("  "),
        Span::styled(describe, Style::default().fg(themed(Color::DarkGray))),
    ]);
    let line2 = if let Some(e) = err {
        Line::from(vec![
            Span::styled("  error: ", Style::default().fg(themed(Color::Red))),
            Span::raw(e),
        ])
    } else {
        Line::from(Span::styled(
            "  Ctrl+K switch conn · Ctrl+H history · Ctrl+P objects · Ctrl+Enter run · Tab focus",
            Style::default().fg(themed(Color::DarkGray)),
        ))
    };

    let p = Paragraph::new(vec![line1, line2]);
    f.render_widget(p, area);
}
