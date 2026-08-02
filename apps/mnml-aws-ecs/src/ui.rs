//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
use crate::ecs::{Item, task_definition_short};
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
        .block(Block::default().borders(Borders::ALL).title(" ecs "))
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
        "clusters" => format!(" clusters ({total}) "),
        "services" => format!(
            " services · {} ({total}) ",
            tab.spec.cluster.as_deref().unwrap_or("?")
        ),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    let (status, rollout) = match item {
        Item::Cluster(c) => (c.status.as_deref(), None),
        Item::Service(s) => {
            let rollout = s
                .deployments
                .first()
                .and_then(|d| d.rollout_state.as_deref());
            (s.status.as_deref(), rollout)
        }
    };
    // Failed rollout always wins — it's the urgent state to surface.
    if matches!(rollout, Some("FAILED")) {
        return Style::default().fg(Color::Red);
    }
    if matches!(rollout, Some("IN_PROGRESS")) {
        return Style::default().fg(Color::Yellow);
    }
    match status {
        Some("ACTIVE") => Style::default().fg(Color::Gray),
        Some("DRAINING") => Style::default().fg(Color::Yellow),
        Some("INACTIVE") => Style::default().fg(Color::DarkGray),
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
            Span::styled(format!(" {k:<14}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    match item {
        Item::Cluster(c) => {
            lines.push(kv("Name", c.name.clone()));
            lines.push(kv("Status", c.status.clone().unwrap_or_else(|| "—".into())));
            lines.push(kv(
                "Services",
                c.active_services
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ));
            lines.push(kv(
                "Running tasks",
                c.running_tasks
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ));
            if c.pending_tasks.unwrap_or(0) > 0 {
                lines.push(kv("Pending tasks", c.pending_tasks.unwrap().to_string()));
            }
            if c.container_instances.unwrap_or(0) > 0 {
                lines.push(kv(
                    "EC2 instances",
                    c.container_instances.unwrap().to_string(),
                ));
            }
            if !c.capacity_providers.is_empty() {
                lines.push(kv("Providers", c.capacity_providers.join(", ")));
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
        Item::Service(s) => {
            lines.push(kv("Name", s.name.clone()));
            lines.push(kv("Status", s.status.clone().unwrap_or_else(|| "—".into())));
            let desired = s.desired_count.unwrap_or(0);
            let running = s.running_count.unwrap_or(0);
            let pending = s.pending_count.unwrap_or(0);
            let tasks_line = if pending > 0 {
                format!("{running}/{desired} (pending {pending})")
            } else {
                format!("{running}/{desired}")
            };
            lines.push(kv("Tasks", tasks_line));
            if let Some(td) = &s.task_definition {
                lines.push(kv("Task def", task_definition_short(td)));
            }
            if let Some(launch) = &s.launch_type {
                lines.push(kv("Launch type", launch.clone()));
            }
            if let Some(pv) = &s.platform_version {
                lines.push(kv("Platform", pv.clone()));
            }
            if let Some(ct) = &s.created_at {
                lines.push(kv("Created", ct.clone()));
            }
            // Deployments — PRIMARY rollout state is the actionable bit.
            if !s.deployments.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    format!(" Deployments ({}) ", s.deployments.len()),
                    Style::default().fg(Color::DarkGray),
                )]));
                for d in s.deployments.iter().take(3) {
                    let status = d.status.as_deref().unwrap_or("?");
                    let rollout = d.rollout_state.as_deref().unwrap_or("");
                    let td = d
                        .task_definition
                        .as_deref()
                        .map(task_definition_short)
                        .unwrap_or_else(|| "—".into());
                    let counts = format!(
                        "{}/{}",
                        d.running_count.unwrap_or(0),
                        d.desired_count.unwrap_or(0)
                    );
                    let style = match rollout {
                        "FAILED" => Style::default().fg(Color::Red),
                        "IN_PROGRESS" => Style::default().fg(Color::Yellow),
                        "COMPLETED" => Style::default().fg(Color::Green),
                        _ => Style::default().fg(Color::Gray),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {status:<10} "), style),
                        Span::styled(format!("{td:<24} "), Style::default().fg(Color::White)),
                        Span::styled(counts, Style::default().fg(Color::Gray)),
                    ]));
                    if !rollout.is_empty() && rollout != "COMPLETED" {
                        lines.push(Line::from(Span::styled(
                            format!("     rollout: {rollout}"),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
            // Recent events — last 5, newest first (AWS already orders them).
            if !s.events.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Recent events ",
                    Style::default().fg(Color::DarkGray),
                )]));
                for e in s.events.iter().take(5) {
                    let ts = e
                        .created_at
                        .as_deref()
                        .map(short_timestamp)
                        .unwrap_or_else(|| "".to_string());
                    let msg = e.message.as_deref().unwrap_or("");
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {ts:<19} "),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        ),
                        Span::styled(truncate(msg, 64), Style::default().fg(Color::Gray)),
                    ]));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " ARN ",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", s.arn),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ARN · L logs · R ECR · r refresh · q quit ";
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

/// Trim a `2026-06-06T18:30:00.123Z` timestamp down to `2026-06-06 18:30:00`
/// for the event log line. Falls back to the raw string.
fn short_timestamp(ts: &str) -> String {
    if ts.len() >= 19 {
        let head = &ts[..19];
        head.replace('T', " ")
    } else {
        ts.to_string()
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

    #[test]
    fn short_timestamp_trims_iso8601() {
        assert_eq!(
            short_timestamp("2026-06-06T18:30:00.123Z"),
            "2026-06-06 18:30:00"
        );
        assert_eq!(short_timestamp("nope"), "nope");
    }
}
