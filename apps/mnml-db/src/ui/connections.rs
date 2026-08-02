//! Standalone rendering helper for the connection strip. Not used
//! by the current header layout (the header renders the label + a
//! chip inline), but kept for reuse from `Ctrl+K` overlays and any
//! future top-tabs shape.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::ui::themed;

#[allow(dead_code)]
pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, c) in app.connections.iter().enumerate() {
        let is_active = app.active == Some(i);
        let dot = if c.worker.is_some() { "●" } else { "○" };
        let label = format!(" {dot} Alt+{} {} ", i + 1, c.spec.display_label());
        let style = if is_active {
            Style::default()
                .fg(themed(Color::Black))
                .bg(themed(Color::Cyan))
                .add_modifier(Modifier::BOLD)
        } else if c.last_error.is_some() {
            Style::default().fg(themed(Color::Red))
        } else {
            Style::default().fg(themed(Color::Gray))
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    let p = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" connections "),
    );
    f.render_widget(p, area);
}
