//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
use crate::keys;
use crate::rds::Item;
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
            } else {
                format!(" ({})", t.data.items.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" rds "))
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
            .style(Style::default().fg(Color::DarkGray))
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
            let primary = truncate(item.primary_label(), 28);
            let secondary = item.secondary_label();
            let line = format!("{cursor}{:<28}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_color_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "instances" => format!(" db instances ({total}) "),
        "clusters" => format!(" db clusters ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    let status = match item {
        Item::Instance(i) => i.status.as_deref(),
        Item::Cluster(c) => c.status.as_deref(),
    };
    match status {
        Some("available") => Style::default().fg(Color::Gray),
        Some(s) if s.contains("ing") => Style::default().fg(Color::Yellow), // creating, modifying, etc.
        Some("stopped") | Some("inaccessible") => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::Gray),
    }
}

fn draw_detail(f: &mut Frame, area: Rect, item: Option<&Item>) {
    let title = " detail ";
    let Some(item) = item else {
        let p = Paragraph::new("(no item selected)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<16}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    match item {
        Item::Instance(i) => {
            lines.push(kv("Identifier", i.identifier.clone()));
            lines.push(kv(
                "Engine",
                match (&i.engine, &i.engine_version) {
                    (Some(e), Some(v)) => format!("{e} {v}"),
                    (Some(e), None) => e.clone(),
                    _ => "—".into(),
                },
            ));
            if let Some(class) = &i.instance_class {
                lines.push(kv("Class", class.clone()));
            }
            lines.push(kv("Status", i.status.clone().unwrap_or_else(|| "—".into())));
            if let Some(endpoint) = Item::Instance(i.clone()).endpoint() {
                lines.push(kv("Endpoint", endpoint));
            }
            if let Some(s) = i.allocated_storage {
                lines.push(kv(
                    "Storage",
                    format!(
                        "{} GB · {}",
                        s,
                        i.storage_type.clone().unwrap_or_else(|| "—".into())
                    ),
                ));
            }
            if let Some(multi_az) = i.multi_az {
                lines.push(kv("Multi-AZ", multi_az.to_string()));
            }
            if let Some(az) = &i.az {
                lines.push(kv("AZ", az.clone()));
            }
            if let Some(public) = i.publicly_accessible {
                lines.push(kv("Public", public.to_string()));
            }
            if let Some(user) = &i.master_username {
                lines.push(kv("Master user", user.clone()));
            }
            if let Some(cluster) = &i.cluster_identifier {
                lines.push(kv("Cluster", cluster.clone()));
            }
            if let Some(ct) = &i.create_time {
                lines.push(kv("Created", ct.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " ARN ",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", i.arn),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
        Item::Cluster(c) => {
            lines.push(kv("Identifier", c.identifier.clone()));
            lines.push(kv(
                "Engine",
                match (&c.engine, &c.engine_version) {
                    (Some(e), Some(v)) => format!("{e} {v}"),
                    (Some(e), None) => e.clone(),
                    _ => "—".into(),
                },
            ));
            if let Some(mode) = &c.engine_mode {
                lines.push(kv("Mode", mode.clone()));
            }
            lines.push(kv("Status", c.status.clone().unwrap_or_else(|| "—".into())));
            if let Some(endpoint) = Item::Cluster(c.clone()).endpoint() {
                lines.push(kv("Endpoint", endpoint));
            }
            if let Some(reader) = &c.reader_endpoint {
                let reader_str = match c.port {
                    Some(p) => format!("{reader}:{p}"),
                    None => reader.clone(),
                };
                lines.push(kv("Reader endpoint", reader_str));
            }
            if let Some(db) = &c.database_name {
                lines.push(kv("Database", db.clone()));
            }
            if let Some(multi_az) = c.multi_az {
                lines.push(kv("Multi-AZ", multi_az.to_string()));
            }
            if let Some(user) = &c.master_username {
                lines.push(kv("Master user", user.clone()));
            }
            if let Some(s) = c.allocated_storage {
                lines.push(kv("Allocated", format!("{} GB", s)));
            }
            if let Some(ct) = &c.create_time {
                lines.push(kv("Created", ct.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " ARN ",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", c.arn),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ARN · E endpoint · L logs · D db · r refresh · q quit ";
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

    #[test]
    fn truncate_long_strings_get_ellipsis() {
        let out = truncate("0123456789abcdef", 8);
        assert_eq!(out.chars().count(), 8);
        assert!(out.ends_with('…'));
    }
}
