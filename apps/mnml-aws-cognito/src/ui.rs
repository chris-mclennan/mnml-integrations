//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
use crate::cognito::{Item, fmt_epoch};
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
    // Reserve a row for the search bar when the user is editing or
    // when a filter is active.
    let show_search_bar = app.query_editing.is_some() || app.active_filter.is_some();
    let constraints: Vec<Constraint> = if show_search_bar {
        vec![
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(size);
    draw_tabs(f, chunks[0], app);
    let body_idx = if show_search_bar {
        draw_search_bar(f, chunks[1], app);
        2
    } else {
        1
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[body_idx]);
    draw_list(f, body[0], app.active());
    draw_detail(f, body[1], app.focused_item());
    draw_status(f, chunks[body_idx + 1], app);
}

fn draw_search_bar(f: &mut Frame, area: Rect, app: &App) {
    let t = ratatui::style::Color::Cyan;
    let line = if let Some(q) = &app.query_editing {
        Line::from(vec![
            Span::styled(" / ", Style::default().fg(t).add_modifier(Modifier::BOLD)),
            Span::styled(q.clone(), Style::default().fg(Color::White)),
            Span::styled(" █", Style::default().fg(t).add_modifier(Modifier::DIM)),
            Span::styled(
                "   (Enter to commit · Esc to cancel)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ])
    } else if let Some(active) = &app.active_filter {
        Line::from(vec![
            Span::styled(
                " filter: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(active.clone(), Style::default().fg(t)),
            Span::styled(
                "   (Esc to clear)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ])
    } else {
        return;
    };
    f.render_widget(Paragraph::new(line), area);
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
        .block(Block::default().borders(Borders::ALL).title(" cognito "))
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
        "pools" => format!(" pools ({total}) "),
        "users" => format!(
            " users · {} ({total}) ",
            tab.spec.user_pool_id.as_deref().unwrap_or("?")
        ),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    match item {
        Item::Pool(_) => Style::default().fg(Color::Gray),
        Item::User(u) => {
            if u.enabled == Some(false) {
                return Style::default().fg(Color::DarkGray);
            }
            match u.status.as_deref() {
                Some("CONFIRMED") => Style::default().fg(Color::Gray),
                Some("FORCE_CHANGE_PASSWORD") | Some("RESET_REQUIRED") => {
                    Style::default().fg(Color::Yellow)
                }
                Some("UNCONFIRMED") | Some("UNKNOWN") => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::Gray),
            }
        }
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
        Item::Pool(p) => {
            lines.push(kv("Name", p.name.clone()));
            lines.push(kv("ID", p.id.clone()));
            lines.push(kv("Status", p.status.clone().unwrap_or_else(|| "—".into())));
            if let Some(d) = p.creation_date {
                lines.push(kv("Created", fmt_epoch(d)));
            }
            if let Some(d) = p.last_modified_date {
                lines.push(kv("Last modified", fmt_epoch(d)));
            }
            if let Some(lambdas) = &p.lambda_config
                && let Some(obj) = lambdas.as_object()
                && !obj.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    format!(" Lambda triggers ({}) ", obj.len()),
                    Style::default().fg(Color::DarkGray),
                )]));
                for (trigger, arn) in obj.iter().take(10) {
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {trigger:<24}"), Style::default().fg(Color::Cyan)),
                        Span::styled(
                            short_arn_tail(arn.as_str().unwrap_or("")),
                            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
            }
        }
        Item::User(u) => {
            lines.push(kv("Username", u.username.clone()));
            lines.push(kv("Status", u.status.clone().unwrap_or_else(|| "—".into())));
            lines.push(kv(
                "Enabled",
                match u.enabled {
                    Some(true) => "true".into(),
                    Some(false) => "FALSE".into(),
                    None => "—".into(),
                },
            ));
            if let Some(d) = u.create_date {
                lines.push(kv("Created", fmt_epoch(d)));
            }
            if let Some(d) = u.last_modified_date {
                lines.push(kv("Modified", fmt_epoch(d)));
            }
            // Attributes section.
            if !u.attributes.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    format!(" Attributes ({}) ", u.attributes.len()),
                    Style::default().fg(Color::DarkGray),
                )]));
                for a in u.attributes.iter().take(15) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {:<22}", truncate(&a.name, 22)),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(
                            a.value.clone().unwrap_or_default(),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));
                }
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ID · / search · r refresh · q quit ";
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

/// Last segment after `:` from a Lambda ARN, for the trigger list.
fn short_arn_tail(arn: &str) -> String {
    arn.rsplit(':').next().unwrap_or(arn).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn short_arn_tail_extracts_function_name() {
        assert_eq!(
            short_arn_tail("arn:aws:lambda:us-east-1:1:function:pre-signup"),
            "pre-signup"
        );
    }
}
