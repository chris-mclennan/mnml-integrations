//! ratatui render — one draw pass per frame.

use crate::app::{App, View};
use crate::gcal::Event;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tabs
            Constraint::Min(0),    // body
            Constraint::Length(1), // status
        ])
        .split(area);

    draw_tabs(frame, app, chunks[0]);
    draw_body(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, v) in [View::Today, View::Week, View::Upcoming].iter().enumerate() {
        let label = format!(" {}.{} ", i + 1, view_label(*v));
        let active = *v == app.view;
        let style = if active {
            Style::default()
                .fg(app.theme.bg)
                .bg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.fg).bg(app.theme.bg)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(app.theme.comment));
    let para = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(para, area);
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    draw_event_list(frame, app, cols[0]);
    draw_detail(frame, app, cols[1]);
}

fn draw_event_list(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.comment))
        .title(format!(" {} ({}) ", view_label(app.view), app.events.len()));
    if app.events.is_empty() {
        let msg = if app.loading {
            "loading…"
        } else if let Some(err) = &app.last_error {
            err.as_str()
        } else {
            "no events"
        };
        let para = Paragraph::new(msg).block(block).wrap(Wrap { trim: true });
        frame.render_widget(para, area);
        return;
    }
    let items: Vec<ListItem> = app
        .events
        .iter()
        .map(|e| ListItem::new(Line::from(event_row(e))))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(app.theme.bg)
                .bg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.comment))
        .title(" detail ");
    let Some(evt) = app.events.get(app.selected) else {
        frame.render_widget(block, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Summary   ", Style::default().fg(app.theme.comment)),
        Span::styled(
            evt.summary.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("When      ", Style::default().fg(app.theme.comment)),
        Span::raw(event_time_display(evt)),
    ]));
    if let Some(loc) = &evt.location {
        lines.push(Line::from(vec![
            Span::styled("Where     ", Style::default().fg(app.theme.comment)),
            Span::raw(loc.clone()),
        ]));
    }
    if let Some(link) = &evt.hangout_link {
        lines.push(Line::from(vec![
            Span::styled("Meet      ", Style::default().fg(app.theme.accent)),
            Span::raw(link.clone()),
        ]));
    }
    if let Some(url) = &evt.html_link {
        lines.push(Line::from(vec![
            Span::styled("URL       ", Style::default().fg(app.theme.comment)),
            Span::raw(url.clone()),
        ]));
    }
    if !evt.attendees.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Attendees ({})", evt.attendees.len()),
            Style::default().fg(app.theme.comment),
        )));
        for a in evt.attendees.iter().take(15) {
            let name = a.display_name.clone().unwrap_or_else(|| a.email.clone());
            let status = a.response_status.as_deref().unwrap_or("—");
            let mark = match status {
                "accepted" => "✓ ",
                "declined" => "✗ ",
                "tentative" => "? ",
                _ => "· ",
            };
            lines.push(Line::from(format!("  {mark}{name}  [{status}]")));
        }
    }
    if let Some(desc) = &evt.description
        && !desc.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Description",
            Style::default().fg(app.theme.comment),
        )));
        for line in desc.lines().take(20) {
            lines.push(Line::from(format!("  {line}")));
        }
    }
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let msg = if app.loading {
        "loading…".to_string()
    } else if let Some(err) = &app.last_error {
        format!("error: {err}")
    } else {
        "  1/2/3 tab · j/k move · Enter open · y yank URL · r refresh · q quit".to_string()
    };
    let para = Paragraph::new(msg).style(Style::default().fg(app.theme.comment));
    frame.render_widget(para, area);
}

fn view_label(view: View) -> &'static str {
    match view {
        View::Today => "Today",
        View::Week => "Week",
        View::Upcoming => "Upcoming",
    }
}

fn event_row(e: &Event) -> Vec<Span<'static>> {
    let when = event_time_display_short(e);
    let title = e.summary.clone();
    vec![
        Span::raw(when),
        Span::raw("  "),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]
}

fn event_time_display(e: &Event) -> String {
    if let Some(dt) = &e.start.date_time {
        return format!("{dt} → {}", e.end.date_time.as_deref().unwrap_or(""));
    }
    if let Some(d) = &e.start.date {
        return format!("(all-day) {d}");
    }
    "(no time)".to_string()
}

fn event_time_display_short(e: &Event) -> String {
    if let Some(dt) = &e.start.date_time {
        // ISO-8601 like "2026-07-03T14:00:00-04:00" — take the
        // 11..16 slice which is "HH:MM".
        if dt.len() >= 16 {
            return dt[11..16].to_string();
        }
        return dt.clone();
    }
    if let Some(d) = &e.start.date {
        if d.len() >= 10 {
            return d[5..10].to_string(); // MM-DD
        }
        return d.clone();
    }
    "  ??  ".to_string()
}
