//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
use crate::keys;
use crate::sqs::{Queue, fmt_duration_secs, fmt_epoch_secs};
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
};
use std::io::Stdout;
use std::time::Duration;

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.tick();
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && let Some(action) = keys::handle(key, app)
        {
            let quit = keys::apply(action, app).await;
            if quit {
                break;
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);
    draw_tabs(f, chunks[0], app);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);
    draw_list(f, body[0], app.active());
    draw_detail(f, body[1], app.focused_queue());
    draw_status(f, chunks[2], app);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let badge = if t.data.loading {
                " (…)".to_string()
            } else if t.data.last_error.is_some() {
                " (err)".to_string()
            } else {
                format!(" ({})", t.data.queues.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" sqs "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_list(f: &mut Frame, area: Rect, tab: &TabState) {
    if let Some(err) = &tab.data.last_error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title(" queues "));
        f.render_widget(p, area);
        return;
    }
    if tab.data.queues.is_empty() {
        let msg = if tab.data.loading {
            "(loading…)"
        } else {
            "(none)"
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" queues "));
        f.render_widget(p, area);
        return;
    }
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = tab.data.queues.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let lines: Vec<Line> = tab.data.queues[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, q)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let primary = truncate(&q.primary_label(), 28);
            let secondary = q.secondary_label();
            let line = format!("{cursor}{:<28}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_color_for(q)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = format!(" queues ({total}) ");
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(q: &Queue) -> Style {
    let Some(attrs) = &q.attributes else {
        return Style::default().fg(Color::DarkGray);
    };
    let visible = attrs.approximate_messages().unwrap_or(0);
    let in_flight = attrs.approximate_messages_not_visible().unwrap_or(0);
    // Backlog hint: lots of messages waiting → yellow; mostly drained → gray.
    if visible > 1000 || in_flight > 100 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn draw_detail(f: &mut Frame, area: Rect, queue: Option<&Queue>) {
    let title = " detail ";
    let Some(q) = queue else {
        let p = Paragraph::new("(no queue selected)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<22}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    lines.push(kv("Name", q.name().to_string()));
    lines.push(kv(
        "Type",
        if q.is_fifo() {
            "FIFO".into()
        } else {
            "Standard".into()
        },
    ));

    let Some(attrs) = &q.attributes else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(loading attributes…)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
        let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };

    // Backlog section
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " Backlog ",
        Style::default().fg(Color::DarkGray),
    )]));
    lines.push(kv(
        "ApproxNumMessages",
        attrs
            .approximate_messages()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
    ));
    lines.push(kv(
        "ApproxNotVisible",
        attrs
            .approximate_messages_not_visible()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into()),
    ));
    if let Some(d) = attrs.approximate_messages_delayed()
        && d > 0
    {
        lines.push(kv("ApproxDelayed", d.to_string()));
    }

    // Config section
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " Config ",
        Style::default().fg(Color::DarkGray),
    )]));
    if let Some(v) = attrs.visibility_timeout() {
        lines.push(kv("VisibilityTimeout", fmt_duration_secs(v)));
    }
    if let Some(r) = attrs.message_retention_period() {
        lines.push(kv("MessageRetention", fmt_duration_secs(r)));
    }
    if let Some(d) = attrs.delay_seconds() {
        lines.push(kv("DelaySeconds", fmt_duration_secs(d)));
    }
    if let Some(w) = attrs.receive_message_wait_time_seconds() {
        lines.push(kv("ReceiveWait", fmt_duration_secs(w)));
    }
    if let Some(m) = attrs.maximum_message_size() {
        let kb = m / 1024;
        lines.push(kv("MaxMessageSize", format!("{kb} KB")));
    }
    if let Some(created) = attrs.get("CreatedTimestamp").and_then(fmt_epoch_secs) {
        lines.push(kv("Created", created));
    }
    if let Some(modified) = attrs.get("LastModifiedTimestamp").and_then(fmt_epoch_secs) {
        lines.push(kv("LastModified", modified));
    }

    // Redrive policy (DLQ) — this queue *has* a DLQ.
    if let Some(rp) = attrs.redrive_policy() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            " ↓ Redrive policy (this queue's DLQ) ",
            Style::default().fg(Color::DarkGray),
        )]));
        for ln in rp.lines().take(6) {
            lines.push(Line::from(Span::styled(
                format!(" {ln}"),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    // Redrive sources — this queue *is* a DLQ for N others. Empty
    // until the user runs `A` (load all + correlate).
    if !q.redrive_sources.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            format!(" ↑ DLQ for ({}) ", q.redrive_sources.len()),
            Style::default().fg(Color::DarkGray),
        )]));
        for src in &q.redrive_sources {
            lines.push(Line::from(Span::styled(
                format!(" {src}"),
                Style::default().fg(Color::Cyan),
            )));
        }
    }

    // ARN
    if let Some(arn) = attrs.arn() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            " ARN ",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(Span::styled(
            format!(" {arn}"),
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
        )));
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint =
        " 1-9 tab · ↑↓/jk move · o console · y URL · Y ARN · A all+DLQ · r refresh · q quit ";
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }
}
