//! ratatui rendering + the main event loop.

use crate::app::{App, ConfirmState, Item, TabState};
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
        .block(Block::default().borders(Borders::ALL).title(" cloudflare "))
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
        "zones" => format!(" zones ({total}) "),
        "dns" => format!(" dns ({total}) "),
        "workers" => format!(" workers ({total}) "),
        "pages" => format!(" pages ({total}) "),
        "security_events" => format!(" security events ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    match item {
        Item::Zone(z) => match z.status.as_str() {
            "active" => Style::default().fg(Color::Green),
            "pending" | "initializing" => Style::default().fg(Color::Yellow),
            "deactivated" | "deleted" | "moved" => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Dns(d) => {
            let base = match d.record_type.as_str() {
                "A" | "AAAA" => Color::Cyan,
                "CNAME" => Color::Blue,
                "MX" => Color::Yellow,
                "TXT" => Color::Gray,
                _ => Color::White,
            };
            let mut s = Style::default().fg(base);
            if d.proxied {
                // orange-ish via DIM + Yellow isn't right; ratatui's
                // 16-color palette doesn't have orange, so we lean
                // on bold+yellow as the proxied accent.
                s = Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD);
            }
            s
        }
        Item::Worker(_) => Style::default().fg(Color::Magenta),
        Item::Pages(p) => match p.last_deploy_status() {
            "success" => Style::default().fg(Color::Green),
            "failure" | "canceled" => Style::default().fg(Color::Red),
            "active" | "idle" => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Security(e) => match e.action.as_str() {
            "block" | "connectionClose" => Style::default().fg(Color::Red),
            "challenge" | "jschallenge" | "managed_challenge" => Style::default().fg(Color::Yellow),
            "allow" => Style::default().fg(Color::Green),
            "log" => Style::default().fg(Color::Gray),
            _ => Style::default().fg(Color::White),
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
        Item::Zone(z) => {
            lines.push(kv("Name", z.name.clone()));
            lines.push(kv("ID", z.id.clone()));
            lines.push(kv("Status", z.status.clone()));
            lines.push(kv("Plan", z.plan_name().to_string()));
            lines.push(kv("Paused", z.paused.to_string()));
            let dev_mode = z.development_mode.unwrap_or(0);
            let dev_label = if dev_mode > 0 {
                format!("on ({dev_mode}s remaining)")
            } else {
                "off".to_string()
            };
            lines.push(kv("Dev mode", dev_label));
            if let Some(m) = &z.modified_on {
                lines.push(kv("Modified", m.clone()));
            }
            if !z.name_servers.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Name servers ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ns in &z.name_servers {
                    lines.push(Line::from(Span::styled(
                        format!(" {ns}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            if !z.original_name_servers.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Original name servers ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ns in &z.original_name_servers {
                    lines.push(Line::from(Span::styled(
                        format!(" {ns}"),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                    )));
                }
            }
        }
        Item::Dns(d) => {
            lines.push(kv("Name", d.name.clone()));
            lines.push(kv("Type", d.record_type.clone()));
            lines.push(kv("ID", d.id.clone()));
            lines.push(kv("Proxied", d.proxied.to_string()));
            let ttl = if d.ttl == 1 {
                "auto".to_string()
            } else {
                format!("{}s", d.ttl)
            };
            lines.push(kv("TTL", ttl));
            if !d.zone_name.is_empty() {
                lines.push(kv("Zone", d.zone_name.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " Content ",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            )]));
            for ln in d.content.lines().take(8) {
                lines.push(Line::from(Span::styled(
                    format!(" {ln}"),
                    Style::default().fg(Color::Gray),
                )));
            }
        }
        Item::Worker(w) => {
            lines.push(kv("Name", w.id.clone()));
            if let Some(c) = &w.created_on {
                lines.push(kv("Created", c.clone()));
            }
            if let Some(m) = &w.modified_on {
                lines.push(kv("Modified", m.clone()));
            }
            if let Some(u) = &w.usage_model {
                lines.push(kv("Usage model", u.clone()));
            }
            // Routes would be fetched here in a follow-up — v0.1
            // includes the helper but doesn't auto-fetch per focus
            // change (would block render thread on every cursor move).
        }
        Item::Pages(p) => {
            lines.push(kv("Name", p.name.clone()));
            lines.push(kv("ID", p.id.clone()));
            lines.push(kv("Domain", p.primary_domain()));
            if let Some(b) = &p.production_branch {
                lines.push(kv("Prod branch", b.clone()));
            }
            if let Some(c) = &p.created_on {
                lines.push(kv("Created", c.clone()));
            }
            if !p.domains.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Domains ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for d in &p.domains {
                    lines.push(Line::from(Span::styled(
                        format!(" {d}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            if let Some(dep) = &p.latest_deployment {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Latest deployment ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                lines.push(kv("Deploy ID", dep.id.clone()));
                if let Some(env) = &dep.environment {
                    lines.push(kv("Environment", env.clone()));
                }
                if let Some(c) = &dep.created_on {
                    lines.push(kv("Created", c.clone()));
                }
                if let Some(s) = &dep.latest_stage {
                    lines.push(kv("Stage", s.name.clone()));
                    lines.push(kv("Status", s.status.clone()));
                    if let Some(e) = &s.ended_on {
                        lines.push(kv("Ended", e.clone()));
                    }
                }
                if let Some(u) = &dep.url {
                    lines.push(kv("Preview URL", u.clone()));
                }
            }
        }
        Item::Security(e) => {
            if let Some(t) = &e.occurred_at {
                lines.push(kv("Occurred", t.clone()));
            }
            lines.push(kv("Action", e.action.clone()));
            lines.push(kv("Source", e.source.clone()));
            if let Some(r) = &e.rule_id {
                lines.push(kv("Rule", r.clone()));
            }
            if let Some(ip) = &e.client_ip {
                lines.push(kv("Client IP", ip.clone()));
            }
            if let Some(c) = &e.client_country {
                lines.push(kv("Country", c.clone()));
            }
            if let Some(h) = &e.host {
                lines.push(kv("Host", h.clone()));
            }
            if let Some(r) = &e.ray_id {
                lines.push(kv("Ray ID", r.clone()));
            }
            if let Some(ua) = &e.user_agent {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " User agent ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {ua}"),
                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = match app.confirm {
        ConfirmState::None => {
            " 1-9 tab · ↑↓/jk move · o dashboard · y ID · X purge · D dev-mode · r refresh · q quit "
        }
        ConfirmState::PurgeCache { .. } => " [y/n] confirm purge ",
    };
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
