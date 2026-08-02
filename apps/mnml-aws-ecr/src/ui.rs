//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
use crate::ecr::{Item, fmt_bytes, short_digest, short_timestamp};
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
        .block(Block::default().borders(Borders::ALL).title(" ecr "))
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
            let primary = truncate(item.primary_label(), 24);
            let secondary = item.secondary_label();
            let line = format!("{cursor}{:<24}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                vuln_color_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "repositories" => format!(" repositories ({total}) "),
        "images" => format!(
            " images · {} ({total}) ",
            tab.spec.repository.as_deref().unwrap_or("?")
        ),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

/// Image rows get a color cue from their scan findings:
/// - any CRITICAL → red
/// - any HIGH (and no CRITICAL) → yellow
/// - clean (scan ran, zero findings) → green
/// - no scan summary → gray (default)
fn vuln_color_for(item: &Item) -> Style {
    let Item::Image(i) = item else {
        return Style::default().fg(Color::Gray);
    };
    let Some(scan) = &i.scan_summary else {
        return Style::default().fg(Color::Gray);
    };
    if scan.critical() > 0 {
        Style::default().fg(Color::Red)
    } else if scan.high() > 0 {
        Style::default().fg(Color::Yellow)
    } else if scan.total() == 0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Gray)
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
        Item::Repository(r) => {
            lines.push(kv("Name", r.name.clone()));
            if let Some(uri) = &r.uri {
                lines.push(kv("URI", uri.clone()));
            }
            if let Some(registry) = &r.registry_id {
                lines.push(kv("Registry", registry.clone()));
            }
            if let Some(mutability) = &r.tag_mutability {
                lines.push(kv("Tag mutability", mutability.clone()));
            }
            if let Some(scan) = r.scanning.as_ref().and_then(|s| s.scan_on_push) {
                lines.push(kv(
                    "Scan on push",
                    if scan { "true" } else { "false" }.into(),
                ));
            }
            if let Some(created) = &r.created_at {
                lines.push(kv("Created", created.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " ARN ",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", r.arn),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
        Item::Image(i) => {
            lines.push(kv("Repository", i.repository_name.clone()));
            if !i.tags.is_empty() {
                lines.push(kv("Tags", i.tags.join(", ")));
            } else {
                lines.push(kv("Tags", "(untagged)".into()));
            }
            if let Some(d) = &i.digest {
                lines.push(kv("Digest", format!("{}…", short_digest(d))));
            }
            if let Some(s) = i.size_bytes {
                lines.push(kv("Size", fmt_bytes(s)));
            }
            if let Some(pushed) = &i.pushed_at {
                lines.push(kv("Pushed", short_timestamp(pushed)));
            }
            if let Some(media) = &i.manifest_media_type {
                lines.push(kv("Manifest", media.clone()));
            }
            if let Some(artifact) = &i.artifact_media_type {
                lines.push(kv("Artifact", artifact.clone()));
            }
            // Scan findings — per-severity vulnerability count breakdown.
            // We render this section even when all counts are zero,
            // because "zero criticals" is meaningful context (the
            // scan ran and found nothing). When scan_summary is None
            // entirely, the scan hasn't run.
            if let Some(scan) = &i.scan_summary {
                lines.push(Line::from(""));
                let total = scan.total();
                lines.push(Line::from(vec![Span::styled(
                    format!(" Vulnerability scan ({total} findings) "),
                    Style::default().fg(Color::DarkGray),
                )]));
                let severities = [
                    ("CRITICAL", scan.critical(), Color::Red),
                    ("HIGH", scan.high(), Color::Yellow),
                    ("MEDIUM", scan.medium(), Color::Cyan),
                    ("LOW", scan.low(), Color::Gray),
                    ("INFO", scan.informational(), Color::DarkGray),
                ];
                for (label, count, color) in severities {
                    if count == 0 {
                        continue;
                    }
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {label:<10}"), Style::default().fg(color)),
                        Span::styled(count.to_string(), Style::default().fg(Color::White)),
                    ]));
                }
                if total == 0 {
                    lines.push(Line::from(Span::styled(
                        " (clean — no findings)",
                        Style::default().fg(Color::Green),
                    )));
                }
                if let Some(completed) = &scan.completed_at {
                    lines.push(Line::from(Span::styled(
                        format!(" scanned: {}", short_timestamp(completed)),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    )));
                }
            } else if let Some(status) = &i.scan_status
                && let Some(s) = &status.status
            {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" Scan: {s}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " Full digest ",
                Style::default().fg(Color::DarkGray),
            )]));
            if let Some(d) = &i.digest {
                lines.push(Line::from(Span::styled(
                    format!(" {d}"),
                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ARN/URI · r refresh · q quit ";
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
