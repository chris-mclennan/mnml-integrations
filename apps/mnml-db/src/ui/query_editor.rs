//! Top-right pane — the multi-line query / command editor.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, Focus};
use crate::ui::themed;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Editor;
    let engine = app
        .active_conn()
        .map(|c| c.spec.engine.as_str())
        .unwrap_or("-");
    let title = format!(" query [{engine}] · Ctrl+Enter run · Ctrl+U clear ");
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

    // Split text into visible lines. Render the caret as a vertical
    // bar in the character stream (no cursor-sh emit in v0.1 to
    // keep hosted-in-mnml correct).
    let text = &app.editor.text;
    let cursor = app.editor.cursor.min(text.chars().count());

    // Walk chars and build lines with a caret marker inserted.
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur_line: Vec<Span<'static>> = Vec::new();
    let mut cur_text = String::new();

    let caret_span = || {
        Span::styled(
            "│".to_string(),
            Style::default()
                .fg(themed(Color::Cyan))
                .add_modifier(Modifier::BOLD),
        )
    };

    for (char_i, c) in text.chars().enumerate() {
        if char_i == cursor && focused {
            if !cur_text.is_empty() {
                cur_line.push(Span::raw(std::mem::take(&mut cur_text)));
            }
            cur_line.push(caret_span());
        }
        if c == '\n' {
            if !cur_text.is_empty() {
                cur_line.push(Span::raw(std::mem::take(&mut cur_text)));
            }
            lines.push(Line::from(std::mem::take(&mut cur_line)));
        } else {
            cur_text.push(c);
        }
    }
    if cursor == text.chars().count() && focused {
        if !cur_text.is_empty() {
            cur_line.push(Span::raw(std::mem::take(&mut cur_text)));
        }
        cur_line.push(caret_span());
    }
    if !cur_text.is_empty() {
        cur_line.push(Span::raw(std::mem::take(&mut cur_text)));
    }
    lines.push(Line::from(std::mem::take(&mut cur_line)));

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(type a query — Ctrl+Enter to run · multi-line supported)",
            Style::default().fg(themed(Color::DarkGray)),
        )));
    }

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}
