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
        .block(Block::default().borders(Borders::ALL).title(" mandrill "))
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
            let line = format!("{cursor}{}  {secondary}", pad_display(&primary, 32));
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                state_color_for(item)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "messages" => format!(" messages ({total}) "),
        "templates" => format!(" templates ({total}) "),
        "tags" => format!(" tags ({total}) "),
        "webhooks" => format!(" webhooks ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    match item {
        Item::Message(m) => match m.state.as_str() {
            "delivered" => Style::default().fg(Color::Green),
            "queued" | "scheduled" | "deferred" => Style::default().fg(Color::Yellow),
            "bounced" | "soft-bounced" | "rejected" | "spam" | "unsub" | "invalid" => {
                Style::default().fg(Color::Red)
            }
            "sent" => Style::default().fg(Color::Gray),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Template(t) => match t.publish_state() {
            "published" => Style::default().fg(Color::Green),
            _ => Style::default().fg(Color::Gray),
        },
        Item::Tag(t) => {
            let br = t.bounce_rate();
            if br >= 0.05 {
                Style::default().fg(Color::Red)
            } else if br >= 0.02 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            }
        }
        Item::Webhook(w) => {
            if w.last_error.as_deref().is_some_and(|s| !s.is_empty()) {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Gray)
            }
        }
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
        Item::Message(m) => {
            lines.push(kv("Subject", m.short_subject().to_string()));
            lines.push(kv("ID", m.id.clone()));
            lines.push(kv("State", m.state.clone()));
            lines.push(kv("To", m.email.clone()));
            lines.push(kv("From", m.sender.clone()));
            lines.push(kv("Sent", m.ts.to_string()));
            lines.push(kv("Opens", m.opens.to_string()));
            lines.push(kv("Clicks", m.clicks.to_string()));
            if !m.tags.is_empty() {
                lines.push(kv("Tags", m.tags.join(", ")));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " Press L for full event log ",
                Style::default()
                    .fg(crate::theme::remap(Color::DarkGray))
                    .add_modifier(Modifier::DIM),
            )]));
        }
        Item::Template(t) => {
            lines.push(kv("Name", t.name.clone()));
            lines.push(kv("Slug", t.slug.clone()));
            lines.push(kv("Subject", t.display_subject().to_string()));
            lines.push(kv("State", t.publish_state().to_string()));
            if let Some(ts) = &t.published_at {
                lines.push(kv("Published", ts.clone()));
            }
            if let Some(ts) = &t.created_at {
                lines.push(kv("Created", ts.clone()));
            }
            if let Some(ts) = &t.updated_at {
                lines.push(kv("Updated", ts.clone()));
            }
            if let Some(from) = &t.from_email {
                lines.push(kv("From email", from.clone()));
            }
            if !t.labels.is_empty() {
                lines.push(kv("Labels", t.labels.join(", ")));
            }
            let preview = t
                .publish_code
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(t.code.as_deref())
                .unwrap_or("");
            if !preview.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " HTML preview (first 8 lines) ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ln in preview.lines().take(8) {
                    lines.push(Line::from(Span::styled(
                        format!(" {}", truncate(ln, 80)),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                    )));
                }
            }
        }
        Item::Tag(t) => {
            lines.push(kv("Tag", t.tag.clone()));
            lines.push(kv("Sent", t.sent.to_string()));
            lines.push(kv("Hard bounces", t.hard_bounces.to_string()));
            lines.push(kv("Soft bounces", t.soft_bounces.to_string()));
            lines.push(kv("Rejects", t.rejects.to_string()));
            lines.push(kv("Complaints", t.complaints.to_string()));
            lines.push(kv("Unsubs", t.unsubs.to_string()));
            lines.push(kv("Opens", t.opens.to_string()));
            lines.push(kv("Unique opens", t.unique_opens.to_string()));
            lines.push(kv("Clicks", t.clicks.to_string()));
            lines.push(kv("Unique clicks", t.unique_clicks.to_string()));
            lines.push(kv("Reputation", t.reputation.to_string()));
            lines.push(kv(
                "Bounce rate",
                format!("{:.2}%", t.bounce_rate() * 100.0),
            ));
        }
        Item::Webhook(w) => {
            lines.push(kv("ID", w.id.to_string()));
            lines.push(kv("URL", w.url.clone()));
            if let Some(d) = &w.description {
                lines.push(kv("Description", d.clone()));
            }
            lines.push(kv("Auth key", w.auth_key_trailing()));
            lines.push(kv("Events", w.events.join(", ")));
            lines.push(kv("Batches sent", w.batches_sent.to_string()));
            lines.push(kv("Events sent", w.events_sent.to_string()));
            if let Some(ts) = &w.created_at {
                lines.push(kv("Created", ts.clone()));
            }
            if let Some(ts) = &w.last_sent_at {
                lines.push(kv("Last sent", ts.clone()));
            }
            if let Some(err) = &w.last_error
                && !err.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Last error ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {err}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o web · y ID · L jump · r refresh · q quit ";
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

/// Truncate to `max` DISPLAY CELLS (not chars), inserting `…` when
/// shortened. CJK / emoji glyphs are 2 cells; the prior char-based
/// truncate would over-shoot the column on those scripts.
fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max {
        return s.to_string();
    }
    let limit = max.saturating_sub(1);
    let mut out = String::new();
    let mut cells = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if cells + w > limit {
            break;
        }
        out.push(c);
        cells += w;
    }
    out.push('…');
    out
}

/// Right-pad `s` to exactly `cols` display cells with U+0020. Use
/// instead of `format!("{:<W}", s)` for column alignment.
fn pad_display(s: &str, cols: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = s.width();
    if w >= cols {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + (cols - w));
    out.push_str(s);
    for _ in 0..(cols - w) {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }
}
