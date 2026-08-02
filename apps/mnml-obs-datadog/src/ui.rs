//! ratatui rendering + the main event loop.

use crate::app::{App, Item, TabState};
use crate::keys;
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

pub fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        crate::theme::poll_refresh();
        terminal.draw(|f| draw(f, app))?;
        app.tick();
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && let Some(action) = keys::handle(key, app)
        {
            let quit = keys::apply(action, app);
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
    draw_detail(f, body[1], app.focused_item());
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
            } else if t.data.truncated {
                format!(" ({}+)", t.data.items.len())
            } else {
                format!(" ({})", t.data.items.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" datadog "))
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
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    if tab.data.items.is_empty() {
        let msg = if tab.data.loading {
            "(loading…)"
        } else {
            "(none)"
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = tab.data.items.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let lines: Vec<Line> = tab.data.items[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let primary = truncate(&item.primary_label(), 32);
            let secondary = item.secondary_label();
            let line = format!("{cursor}{:<32}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_color_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "monitors" => format!(" monitors ({total}) "),
        "dashboards" => format!(" dashboards ({total}) "),
        "logs" => format!(" logs ({total}) "),
        "incidents" => format!(" incidents ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    match item {
        Item::Monitor(m) => match m.overall_state.as_str() {
            "Alert" => Style::default().fg(Color::Red),
            "Warn" => Style::default().fg(Color::Yellow),
            "No Data" => Style::default().fg(Color::Yellow),
            "OK" => Style::default().fg(Color::Green),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Dashboard(_) => Style::default().fg(Color::Gray),
        Item::Log(l) => match l.attributes.status.as_deref() {
            Some("error") | Some("critical") | Some("emerg") => Style::default().fg(Color::Red),
            Some("warn") | Some("warning") => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Incident(i) => match i.attributes.severity.as_deref() {
            Some(s) if s.starts_with("SEV-1") => Style::default().fg(Color::Red),
            Some(s) if s.starts_with("SEV-2") => Style::default().fg(Color::Red),
            Some(s) if s.starts_with("SEV-3") => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Gray),
        },
    }
}

fn draw_detail(f: &mut Frame, area: Rect, item: Option<&Item>) {
    let title = " detail ";
    let Some(item) = item else {
        let p = Paragraph::new("(no item selected)")
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!(" {k:<18}"),
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            ),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    match item {
        Item::Monitor(m) => {
            lines.push(kv("Name", m.short_name().to_string()));
            lines.push(kv("Type", m.monitor_type.clone()));
            lines.push(kv("State", m.overall_state.clone()));
            lines.push(kv("ID", m.id.to_string()));
            if let Some(modified) = &m.modified {
                lines.push(kv("Modified", modified.clone()));
            }
            if !m.tags.is_empty() {
                lines.push(kv("Tags", m.tags.join(", ")));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " Query ",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            )]));
            for ln in m.query.lines().take(8) {
                lines.push(Line::from(Span::styled(
                    format!(" {ln}"),
                    Style::default().fg(Color::Gray),
                )));
            }
            if !m.message.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Message ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ln in m.message.lines().take(8) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                    )));
                }
            }
        }
        Item::Dashboard(d) => {
            lines.push(kv("Title", d.title.clone()));
            lines.push(kv("ID", d.id.clone()));
            if let Some(a) = &d.author_handle {
                lines.push(kv("Author", a.clone()));
            }
            if let Some(l) = &d.layout_type {
                lines.push(kv("Layout", l.clone()));
            }
            if let Some(m) = &d.modified_at {
                lines.push(kv("Modified", m.clone()));
            }
            if let Some(u) = &d.url {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Path ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {u}"),
                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                )));
            }
        }
        Item::Log(l) => {
            if let Some(t) = &l.attributes.timestamp {
                lines.push(kv("Timestamp", t.clone()));
            }
            if let Some(s) = &l.attributes.service {
                lines.push(kv("Service", s.clone()));
            }
            if let Some(s) = &l.attributes.status {
                lines.push(kv("Status", s.clone()));
            }
            if let Some(h) = &l.attributes.host {
                lines.push(kv("Host", h.clone()));
            }
            lines.push(kv("ID", l.id.clone()));
            if let Some(m) = &l.attributes.message {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Message ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ln in m.lines().take(20) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
        }
        Item::Incident(i) => {
            lines.push(kv("Title", i.attributes.title.clone()));
            if let Some(p) = i.attributes.public_id {
                lines.push(kv("Public ID", p.to_string()));
            }
            if let Some(s) = &i.attributes.state {
                lines.push(kv("State", s.clone()));
            }
            if let Some(s) = &i.attributes.severity {
                lines.push(kv("Severity", s.clone()));
            }
            if let Some(c) = &i.attributes.created {
                lines.push(kv("Created", c.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " UUID ",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", i.id),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y URL · L jump · r refresh · q quit ";
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(crate::theme::remap(Color::DarkGray))
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
