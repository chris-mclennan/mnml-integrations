//! ratatui rendering + the main event loop.

use crate::app::{App, Item, TabState};
use crate::keys;
use crate::sns::topic_name_from_arn;
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
    let show_publish_bar = app.publish_editing.is_some();
    let constraints: Vec<Constraint> = if show_publish_bar {
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
    let body_idx = if show_publish_bar {
        draw_publish_bar(f, chunks[1], app);
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

fn draw_publish_bar(f: &mut Frame, area: Rect, app: &App) {
    let Some(buf) = &app.publish_editing else {
        return;
    };
    let line = Line::from(vec![
        Span::styled(
            " P ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(buf.clone(), Style::default().fg(Color::White)),
        Span::styled(
            " █",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        ),
        Span::styled(
            "   (Enter to publish · Esc to cancel)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
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
        .block(Block::default().borders(Borders::ALL).title(" sns "))
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
            let primary = truncate(&item.primary_label(), 28);
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
        "topics" => format!(" topics ({total}) "),
        "subscriptions" => format!(
            " subs · {} ({total}) ",
            tab.spec
                .topic_arn
                .as_deref()
                .map(topic_name_from_arn)
                .unwrap_or("?")
        ),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn state_color_for(item: &Item) -> Style {
    match item {
        Item::Topic(t) => match &t.attributes {
            None => Style::default().fg(Color::DarkGray),
            Some(attrs) => {
                let pending = attrs.subscriptions_pending().unwrap_or(0);
                if pending > 0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Gray)
                }
            }
        },
        Item::Subscription(s) => {
            if s.is_pending_confirmation() {
                Style::default().fg(Color::Yellow)
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
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<18}"), Style::default().fg(Color::DarkGray)),
            Span::styled(v, Style::default().fg(Color::White)),
        ])
    };

    match item {
        Item::Topic(t) => {
            lines.push(kv("Name", t.name().to_string()));
            lines.push(kv(
                "Type",
                if t.is_fifo() {
                    "FIFO".into()
                } else {
                    "Standard".into()
                },
            ));
            let Some(attrs) = &t.attributes else {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "(loading attributes…)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )));
                let p = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(title));
                f.render_widget(p, area);
                return;
            };
            if let Some(d) = attrs.display_name() {
                lines.push(kv("Display name", d.to_string()));
            }
            if let Some(o) = attrs.owner() {
                lines.push(kv("Owner", o.to_string()));
            }
            lines.push(kv(
                "Confirmed subs",
                attrs
                    .subscriptions_confirmed()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
            ));
            if let Some(p) = attrs.subscriptions_pending()
                && p > 0
            {
                lines.push(kv("Pending subs", p.to_string()));
            }
            if let Some(d) = attrs.subscriptions_deleted()
                && d > 0
            {
                lines.push(kv("Deleted subs", d.to_string()));
            }
            if let Some(k) = attrs.kms_master_key_id() {
                lines.push(kv("KMS key", k.to_string()));
            }
            if let Some(v) = attrs.signature_version() {
                lines.push(kv("Signature ver", v.to_string()));
            }
            if let Some(dp) = attrs.delivery_policy() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Delivery policy ",
                    Style::default().fg(Color::DarkGray),
                )]));
                for ln in dp.lines().take(8) {
                    lines.push(Line::from(Span::styled(
                        format!(" {ln}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " ARN ",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(Span::styled(
                format!(" {}", t.arn),
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )));
        }
        Item::Subscription(s) => {
            lines.push(kv(
                "Protocol",
                s.protocol.clone().unwrap_or_else(|| "—".into()),
            ));
            lines.push(kv(
                "Endpoint",
                s.endpoint.clone().unwrap_or_else(|| "—".into()),
            ));
            if s.is_pending_confirmation() {
                lines.push(kv("Status", "Pending confirmation".to_string()));
            } else {
                lines.push(kv("Status", "Confirmed".to_string()));
            }
            if let Some(owner) = &s.owner {
                lines.push(kv("Owner", owner.clone()));
            }
            if let Some(topic) = &s.topic_arn {
                lines.push(kv("Topic", topic_name_from_arn(topic).to_string()));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Topic ARN ",
                    Style::default().fg(Color::DarkGray),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {topic}"),
                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                )));
            }
            if !s.arn.is_empty() && !s.is_pending_confirmation() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled(
                    " Subscription ARN ",
                    Style::default().fg(Color::DarkGray),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {}", s.arn),
                    Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · o console · y ARN · Y endpoint · L jump (sqs/lambda) · P publish · r refresh · q quit ";
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
