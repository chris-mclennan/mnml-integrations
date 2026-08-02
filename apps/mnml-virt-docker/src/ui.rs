//! ratatui rendering + the main event loop.

use crate::app::{App, Item, TabState};
use crate::docker::DaemonState;
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
use std::process::Command;
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
        crate::theme::poll_refresh();
        terminal.draw(|f| draw(f, app))?;
        app.tick();
        drain_pending_spawns(app, terminal).await?;
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

/// Run any queued `pending_spawns` — e.g. `docker logs -f <id>` or
/// `docker exec -it <id> /bin/sh`. We hand the controlling terminal
/// over for the duration (leave the alt-screen, run the command,
/// re-enter on return). This is the pty-ish path; v0.2 will hand
/// off through tmnl / pane_host for the real pty experience.
async fn drain_pending_spawns(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    if app.pending_spawns.is_empty() {
        return Ok(());
    }
    let spawns = std::mem::take(&mut app.pending_spawns);
    for argv in spawns {
        if argv.is_empty() {
            continue;
        }
        // Save terminal state, hand over to the child.
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        let _ = Command::new(&argv[0]).args(&argv[1..]).status();
        // Restore.
        enable_raw_mode()?;
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.clear()?;
        // Refresh the list so post-action state shows up.
        app.refresh_active();
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let show_rm = app.rm_pending.is_some();
    let constraints: Vec<Constraint> = if show_rm {
        vec![
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
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

    if !app.daemon_online() {
        draw_daemon_offline(f, chunks[1], app);
    } else {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);
        draw_list(f, body[0], app.active());
        draw_detail(f, body[1], app);
    }

    if show_rm {
        draw_rm_bar(f, chunks[2], app);
        draw_status(f, chunks[3], app);
    } else {
        draw_status(f, chunks[2], app);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let badge = if !app.daemon_online() {
                String::new()
            } else if t.data.loading {
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
        .block(Block::default().borders(Borders::ALL).title(" docker "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_daemon_offline(f: &mut Frame, area: Rect, app: &App) {
    let msg = match &app.daemon {
        DaemonState::Offline => {
            "Docker daemon not running.\n\nStart Docker Desktop, then press `r` to retry."
                .to_string()
        }
        DaemonState::CliMissing(e) => {
            format!(
                "`docker` CLI not found on PATH.\n\n{e}\n\nInstall Docker Desktop or the docker CLI, then press `r` to retry."
            )
        }
        DaemonState::Error(e) => format!("docker error:\n\n{e}\n\nPress `r` to retry."),
        DaemonState::Ok(_) => String::new(),
    };
    let p = Paragraph::new(msg)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title(" ⚠ daemon "));
    f.render_widget(p, area);
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
            let badge = state_badge(item);
            let primary = truncate(&item.primary_label(), 24);
            let secondary = item.secondary_label();
            let line = format!("{cursor}{badge} {:<24}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_color_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = format!(" {} ({}) ", tab.spec.kind.as_str(), total);
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

/// Short status chip — leading badge in each list row.
fn state_badge(item: &Item) -> &'static str {
    match item.state() {
        "running" => "●",
        "exited" | "dead" => "○",
        "restarting" => "↺",
        "paused" => "‖",
        "created" => "·",
        _ => " ",
    }
}

fn state_color_for(item: &Item) -> Style {
    match item.state() {
        "running" => Style::default().fg(Color::Green),
        "exited" => Style::default().fg(crate::theme::remap(Color::DarkGray)),
        "dead" => Style::default().fg(Color::Red),
        "restarting" => Style::default().fg(Color::Yellow),
        "paused" => Style::default().fg(Color::Yellow),
        "created" => Style::default().fg(Color::Gray),
        _ => Style::default().fg(Color::Gray),
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let title = " inspect ";
    let item = app.focused_item();
    let Some(item) = item else {
        let p = Paragraph::new("(no item selected)")
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let detail_opt = &app.active().data.focused_detail;
    let header_lines: Vec<Line> = header_for(item);
    let mut lines = header_lines;
    lines.push(Line::from(""));
    if let Some(detail) = detail_opt {
        lines.push(Line::from(Span::styled(
            " inspect ",
            Style::default().fg(crate::theme::remap(Color::DarkGray)),
        )));
        for ln in detail.lines() {
            lines.push(Line::from(Span::styled(
                format!(" {ln}"),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            " (loading inspect…) ",
            Style::default()
                .fg(crate::theme::remap(Color::DarkGray))
                .add_modifier(Modifier::DIM),
        )));
    }
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn header_for(item: &Item) -> Vec<Line<'static>> {
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!(" {k:<14}"),
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            ),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };
    let mut out = Vec::new();
    match item {
        Item::Container(c) => {
            out.push(kv("Name", c.names.clone()));
            out.push(kv("ID", c.short_id().to_string()));
            out.push(kv("Image", c.image.clone()));
            out.push(kv("State", c.state.clone()));
            out.push(kv("Status", c.status.clone()));
            if !c.ports.is_empty() {
                out.push(kv("Ports", c.ports.clone()));
            }
            out.push(kv("Running for", c.running_for.clone()));
        }
        Item::Image(i) => {
            out.push(kv("Repo:Tag", i.repo_tag()));
            out.push(kv("ID", i.short_id().to_string()));
            out.push(kv("Size", i.size.clone()));
            out.push(kv("Created", i.created_since.clone()));
        }
        Item::Volume(v) => {
            out.push(kv("Name", v.name.clone()));
            out.push(kv("Driver", v.driver.clone()));
            out.push(kv("Mountpoint", v.mountpoint.clone()));
            out.push(kv("Scope", v.scope.clone()));
        }
        Item::Network(n) => {
            out.push(kv("Name", n.name.clone()));
            out.push(kv("ID", n.short_id().to_string()));
            out.push(kv("Driver", n.driver.clone()));
            out.push(kv("Scope", n.scope.clone()));
        }
        Item::ComposeService(s) => {
            out.push(kv("Service", s.service.clone()));
            out.push(kv("Container", s.name.clone()));
            out.push(kv("State", s.state.clone()));
            out.push(kv("Status", s.status.clone()));
            if !s.image.is_empty() {
                out.push(kv("Image", s.image.clone()));
            }
            if !s.project.is_empty() {
                out.push(kv("Project", s.project.clone()));
            }
        }
    }
    out
}

fn draw_rm_bar(f: &mut Frame, area: Rect, app: &App) {
    let Some(pending) = &app.rm_pending else {
        return;
    };
    let line = Line::from(vec![
        Span::styled(
            " R ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", pending.description()),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            " [y] confirm · [n / Esc] cancel ",
            Style::default()
                .fg(crate::theme::remap(Color::DarkGray))
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o desktop · y ID · l logs · e exec · s/S stop/start · R rm · L ECR · r refresh · q quit ";
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

    #[test]
    fn truncate_long_strings_ellipsised() {
        let s = truncate("a_very_long_image_name", 8);
        assert_eq!(s.chars().count(), 8);
        assert!(s.ends_with('…'));
    }
}
