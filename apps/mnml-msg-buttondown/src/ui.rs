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
        // Follow mnml's family theme: cheap mtime poll, full reparse only on a
        // theme switch.
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
        .block(Block::default().borders(Borders::ALL).title(" buttondown "))
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
    let kind = tab.spec.kind.as_str();

    let lines: Vec<Line> = tab.data.items[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let primary = truncate(&item.primary_label(), 38);
            let secondary = item.secondary_label(kind);
            let line = format!("{cursor}{}  {secondary}", pad_display(&primary, 38));
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                row_color_for(item, kind)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match kind {
        "drafts" => format!(" drafts ({total}) "),
        "sent" => format!(" sent ({total}) "),
        "scheduled" => format!(" scheduled ({total}) "),
        "subscribers" => format!(" subscribers ({total}) "),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn row_color_for(item: &Item, _kind: &str) -> Style {
    match item {
        Item::Email(_) => Style::default().fg(Color::White),
        Item::Subscriber(s) => match s.sub_type.as_deref() {
            Some("premium") => Style::default().fg(Color::Yellow),
            Some("regular") => Style::default().fg(Color::Green),
            Some("unactivated") => Style::default().fg(Color::Gray),
            Some("unsubscribed") | Some("removed") => Style::default().fg(Color::Red),
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
        Item::Email(e) => {
            lines.push(kv(
                "Subject",
                if e.subject.is_empty() {
                    "(none)".into()
                } else {
                    e.subject.clone()
                },
            ));
            lines.push(kv("ID", e.id.clone()));
            if let Some(s) = &e.status {
                lines.push(kv("Status", s.clone()));
            }
            if let Some(t) = &e.email_type {
                lines.push(kv("Type", t.clone()));
            }
            if let Some(c) = &e.creation_date {
                lines.push(kv("Created", c.clone()));
            }
            if let Some(p) = &e.publish_date {
                lines.push(kv("Publish", p.clone()));
            }
            if let Some(m) = &e.modification_date {
                lines.push(kv("Modified", m.clone()));
            }
            if let Some(wc) = e.word_count {
                lines.push(kv("Word count", wc.to_string()));
            }
            if let Some(a) = &e.analytics {
                if let Some(r) = a.recipients {
                    lines.push(kv("Recipients", r.to_string()));
                }
                if let Some(o) = a.opens {
                    lines.push(kv("Opens", o.to_string()));
                }
                if let Some(c) = a.clicks {
                    lines.push(kv("Clicks", c.to_string()));
                }
            }
            if !e.body.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Body ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ln in e.body.lines().take(40) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
        }
        Item::Subscriber(s) => {
            lines.push(kv("Email", s.email_address.clone()));
            lines.push(kv("ID", s.id.clone()));
            if let Some(t) = &s.sub_type {
                lines.push(kv("Type", t.clone()));
            }
            if let Some(c) = &s.creation_date {
                lines.push(kv("Created", c.clone()));
            }
            if let Some(src) = &s.source {
                lines.push(kv("Source", src.clone()));
            }
            if let Some(notes) = &s.notes
                && !notes.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Notes ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                for ln in notes.lines().take(10) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            if let Some(meta) = &s.metadata
                && !meta.is_null()
            {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Metadata ",
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                )]));
                let pretty = serde_json::to_string_pretty(meta).unwrap_or_default();
                for ln in pretty.lines().take(20) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                    )));
                }
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint =
        " 1-9 tab · ↑↓/jk move · o web · y ID · p publish · X unsubscribe · r refresh · q quit ";
    let status_style = if app.confirm.is_some() {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", app.status), status_style),
        Span::styled(
            hint,
            Style::default()
                .fg(crate::theme::remap(Color::DarkGray))
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Truncate to `max` DISPLAY CELLS, inserting `…` when shortened.
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

/// Right-pad `s` to exactly `cols` display cells (cell-aware so CJK /
/// emoji rows align with ASCII rows).
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

    #[test]
    fn truncate_long_strings_get_ellipsis() {
        let s = truncate("this is a fairly long subject line", 10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
    }
}
