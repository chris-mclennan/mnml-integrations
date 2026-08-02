//! ratatui rendering + the main event loop.
//!
//! Layout (vertical): tab strip (3) · split body (min) · status (1).
//! Body splits horizontally 45 / 55 — function list left, focused
//! detail right.

use crate::app::{App, TabState};
use crate::keys;
use crate::lambda::{Function, fmt_bytes};
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
    draw_detail(f, body[1], app.focused_function());
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
        .block(Block::default().borders(Borders::ALL).title(" lambda "))
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
            .block(Block::default().borders(Borders::ALL).title(" functions "));
        f.render_widget(p, area);
        return;
    }
    if tab.data.items.is_empty() {
        let msg = if tab.data.loading {
            "(loading…)"
        } else {
            "(no functions)"
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" functions "));
        f.render_widget(p, area);
        return;
    }
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = tab.data.items.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        // Keep selection in the middle third when possible.
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let lines: Vec<Line> = tab.data.items[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, fun)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let runtime = fun.runtime.as_deref().unwrap_or("(image)");
            let line = format!(
                "{cursor}{:<32}  {runtime}",
                truncate(&fun.function_name, 32)
            );
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = format!(" functions ({total}) ");
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_detail(f: &mut Frame, area: Rect, fun: Option<&Function>) {
    let title = " detail ";
    let Some(fun) = fun else {
        let p = Paragraph::new("(no function selected)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<13}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    lines.push(kv("Name", fun.function_name.clone()));
    lines.push(kv(
        "Runtime",
        fun.runtime.clone().unwrap_or_else(|| "(image)".into()),
    ));
    lines.push(kv(
        "Handler",
        fun.handler.clone().unwrap_or_else(|| "—".into()),
    ));
    lines.push(kv(
        "Memory",
        match fun.memory_size {
            Some(m) => format!("{m} MB"),
            None => "—".into(),
        },
    ));
    lines.push(kv(
        "Timeout",
        match fun.timeout {
            Some(t) => format!("{t}s"),
            None => "—".into(),
        },
    ));
    lines.push(kv(
        "Code size",
        match fun.code_size {
            Some(n) => fmt_bytes(n),
            None => "—".into(),
        },
    ));
    if !fun.architectures.is_empty() {
        lines.push(kv("Arch", fun.architectures.join(", ")));
    }
    if let Some(pkg) = &fun.package_type {
        lines.push(kv("Package", pkg.clone()));
    }
    if let Some(modified) = &fun.last_modified {
        lines.push(kv("Modified", modified.clone()));
    }
    if !fun.role.is_empty() {
        lines.push(kv("Role", short_arn(&fun.role)));
    }
    // Env vars count (we don't render the values — typically secrets).
    if let Some(env) = &fun.environment {
        let n = env.var_count();
        if n > 0 {
            lines.push(kv("Env vars", format!("{n}")));
        }
        if let Some(err) = &env.error
            && err.error_code.is_some()
        {
            lines.push(kv(
                "Env error",
                err.message
                    .clone()
                    .or_else(|| err.error_code.clone())
                    .unwrap_or_else(|| "unknown".into()),
            ));
        }
    }
    // Reserved concurrent executions — capped throttle. Sets an
    // explicit pool size; `None` means unreserved (shared account
    // pool); `Some(0)` means "throttle everything" (a kill switch).
    if let Some(r) = fun.reserved_concurrent_executions {
        let label = if r == 0 {
            "0 (throttled)".to_string()
        } else {
            r.to_string()
        };
        lines.push(kv("Reserved concur", label));
    }
    if let Some(tracing) = &fun.tracing_config
        && let Some(mode) = &tracing.mode
    {
        lines.push(kv("Tracing", mode.clone()));
    }
    if let Some(dlc) = &fun.dead_letter_config
        && let Some(target) = &dlc.target_arn
        && !target.is_empty()
    {
        lines.push(kv("DLQ", short_arn(target)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " ARN ",
        Style::default().fg(Color::DarkGray),
    )]));
    lines.push(Line::from(Span::styled(
        format!(" {}", fun.function_arn),
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )));
    if let Some(desc) = &fun.description
        && !desc.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            " Description ",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(Span::styled(
            format!(" {desc}"),
            Style::default().fg(Color::Gray),
        )));
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ARN · l logs · L DLQ · r refresh · q quit ";
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

/// Shorten an IAM role ARN to just the role name (last `/` segment).
fn short_arn(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
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
    fn short_arn_extracts_role_name() {
        let arn = "arn:aws:iam::123456789012:role/my-role";
        assert_eq!(short_arn(arn), "my-role");
    }
}
