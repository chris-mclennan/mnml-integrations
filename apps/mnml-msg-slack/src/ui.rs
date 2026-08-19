//! ratatui rendering + the main event loop.

use crate::app::{App, ChannelDetail, InputBar, InputMode, Item, ReactionPicker, TabState};
use crate::keys;
use crate::slack::{Channel, Message, QUICK_REACTIONS, SearchMatch, ts_to_hms};
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Tabs},
};
use std::io::Stdout;
use std::time::Duration;

pub fn run(app: &mut App, title: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    // OSC 0/2 — sets the terminal window/tab title. mnml's bufferline (and
    // most terminals — Apple Terminal, iTerm2, Ghostty, Kitty, WezTerm, …)
    // read this and display it in the tab strip. Falls back silently on
    // terminals that don't honor OSC sequences.
    execute!(stdout, EnterAlternateScreen, SetTitle(title))?;
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
    draw_list(f, body[0], app.active(), app);
    draw_detail(f, body[1], app);
    draw_status(f, chunks[2], app);

    if let Some(bar) = &app.input {
        draw_input_overlay(f, size, bar);
    }
    if let Some(picker) = &app.reaction_picker {
        draw_reaction_overlay(f, size, picker);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let badge = if t.loading {
                " (…)".to_string()
            } else if t.last_error.is_some() {
                " (err)".to_string()
            } else if t.spec.kind == "threads" {
                String::new()
            } else if t.spec.kind == "channels" {
                // Show `visible/total` when the `[channels]`
                // filter is trimming the list, so the user can
                // see they've hidden things and by how much.
                let visible = t.items.len();
                let total = app
                    .channel_cache
                    .get("public_channel,private_channel")
                    .map(|(_, v)| v.len())
                    .unwrap_or(visible);
                if total > visible {
                    format!(" ({visible}/{total})")
                } else {
                    format!(" ({visible})")
                }
            } else {
                format!(" ({})", t.items.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let title = if app.team_name.is_empty() {
        " slack ".to_string()
    } else {
        format!(" slack — {} ", app.team_name)
    };
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(title))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_list(f: &mut Frame, area: Rect, tab: &TabState, app: &App) {
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    if tab.items.is_empty() {
        let msg = if tab.loading {
            "(loading…)"
        } else if tab.spec.kind == "search" && tab.search_query.is_empty() {
            "(press / to search)"
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
    let total = tab.items.len();
    let selected = tab.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let lines: Vec<Line> = tab.items[start..]
        .iter()
        .take(body_rows)
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let cursor = if abs == selected { "▸ " } else { "  " };
            let (primary, secondary, style) = item_row(item);
            // Prefix pinned channels with a small pin glyph so the
            // ordering doesn't look mysterious. Filter comes from
            // `app.cfg.channels`; only the channels tab uses it.
            let pin_prefix = match item {
                Item::Channel(c) if tab.spec.kind == "channels" && app.cfg.channels.is_pinned(&c.name) => "\u{f04a6} ",
                _ => "",
            };
            let primary_field = format!("{pin_prefix}{primary}");
            let primary = truncate(&primary_field, 28);
            let line = format!("{cursor}{:<28}  {secondary}", primary);
            let style = if abs == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                style
            };
            Line::from(Span::styled(line, style))
        })
        .collect();

    let title = match tab.spec.kind.as_str() {
        "channels" => format!(" channels ({total}) "),
        "dms" => format!(" dms ({total}) "),
        "search" => {
            if tab.search_query.is_empty() {
                format!(" search ({total}) ")
            } else {
                format!(" search [{}] ({total}) ", truncate(&tab.search_query, 24))
            }
        }
        "threads" => " threads ".to_string(),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn item_row(item: &Item) -> (String, String, Style) {
    match item {
        Item::Channel(c) => channel_row(c),
        Item::SearchHit(hit) => search_row(hit),
        Item::Canvas(c) => canvas_row(c),
        Item::ThreadPlaceholder => (
            "(coming soon)".to_string(),
            "needs scan across recent channels".to_string(),
            Style::default().fg(crate::theme::remap(Color::DarkGray)),
        ),
    }
}

/// #1005 (2026-08-19) — Canvas file row. Layout:
///   `<title>` (primary, white)
///   `<n>h ago  <permalink-suffix>` (secondary, dim)
/// Timestamp uses the same relative-time formatter the messages
/// use; permalink suffix is the trailing `/canvas/<id>` slug so
/// the row hints at the URL Enter will open without eating the
/// whole width.
fn canvas_row(c: &crate::slack::Canvas) -> (String, String, Style) {
    let name = c.display_title();
    let ts = c.timestamp();
    let ago = if ts == 0 {
        "—".to_string()
    } else {
        relative_time_short(ts)
    };
    let slug = c
        .permalink
        .rsplit('/')
        .next()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let secondary = if slug.is_empty() {
        ago
    } else {
        format!("{ago}  /{slug}")
    };
    (
        name,
        secondary,
        Style::default().fg(crate::theme::remap(Color::White)),
    )
}

/// Very compact relative-time formatter. Buckets:
///   <1m → "<1m"
///   <60m → "<n>m"
///   <24h → "<n>h"
///   <30d → "<n>d"
///   otherwise → absolute yyyy-mm-dd (via chrono, if present) or
///   a raw epoch fallback.
fn relative_time_short(ts_unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(ts_unix_secs);
    if delta < 60 {
        "<1m".into()
    } else if delta < 3600 {
        format!("{}m", delta / 60)
    } else if delta < 86_400 {
        format!("{}h", delta / 3600)
    } else if delta < 30 * 86_400 {
        format!("{}d", delta / 86_400)
    } else {
        format!("{}mo", delta / (30 * 86_400))
    }
}


fn channel_row(c: &Channel) -> (String, String, Style) {
    let name = c.display_name();
    let members = c
        .num_members
        .map(|n| format!("({n})"))
        .unwrap_or_else(|| "—".into());
    let topic = c.topic_text();
    let topic = truncate(&topic, 60);
    let secondary = format!("{members}  {topic}");
    // Member channels are highlighted; non-member channels dimmed.
    let style = if c.is_member {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Gray)
    };
    (name, secondary, style)
}

fn search_row(hit: &SearchMatch) -> (String, String, Style) {
    let chan = hit
        .channel
        .as_ref()
        .map(|c| {
            if c.name.is_empty() {
                c.id.clone()
            } else {
                format!("#{}", c.name)
            }
        })
        .unwrap_or_else(|| "—".into());
    let user = hit
        .username
        .clone()
        .or_else(|| hit.user.clone())
        .unwrap_or_else(|| "—".into());
    let ts = ts_to_hms(&hit.ts);
    let snippet = hit.text.lines().next().unwrap_or(&hit.text);
    let snippet = truncate(snippet, 60);
    let primary = format!("{ts} {chan}");
    let secondary = format!("{user}: {snippet}");
    (primary, secondary, Style::default().fg(Color::Gray))
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let title = " detail ";
    match app.focused_item() {
        Some(Item::Channel(c)) => {
            draw_channel_detail(f, area, c, app);
        }
        Some(Item::SearchHit(hit)) => {
            let mut lines: Vec<Line> = Vec::new();
            let kv = |k: &str, v: String| -> Line<'static> {
                Line::from(vec![
                    Span::styled(
                        format!(" {k:<14}"),
                        Style::default().fg(crate::theme::remap(Color::DarkGray)),
                    ),
                    Span::styled(v, Style::default().fg(Color::White)),
                ])
            };
            lines.push(kv("Timestamp", ts_to_hms(&hit.ts)));
            if let Some(c) = &hit.channel {
                let label = if c.name.is_empty() {
                    c.id.clone()
                } else {
                    format!("#{}", c.name)
                };
                lines.push(kv("Channel", label));
            }
            if let Some(u) = &hit.username {
                lines.push(kv("User", u.clone()));
            } else if let Some(u) = &hit.user {
                lines.push(kv("User", u.clone()));
            }
            if let Some(p) = &hit.permalink {
                lines.push(kv("Permalink", p.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                " Text ",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            )]));
            for ln in hit.text.lines().take(20) {
                lines.push(Line::from(Span::styled(
                    format!(" {ln}"),
                    Style::default().fg(Color::Gray),
                )));
            }
            let p =
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(p, area);
        }
        Some(Item::Canvas(c)) => {
            // #1005 (2026-08-19). Canvas detail: title + updated /
            // owner / permalink / a hint about `Enter → open in
            // browser`. No block/body rendering — canvas block model
            // is its own project.
            let kv = |k: &str, v: String| -> Line<'static> {
                Line::from(vec![
                    Span::styled(
                        format!(" {k:<14}"),
                        Style::default().fg(crate::theme::remap(Color::DarkGray)),
                    ),
                    Span::styled(v, Style::default().fg(Color::White)),
                ])
            };
            let mut lines: Vec<Line> = Vec::new();
            lines.push(kv("Title", c.display_title()));
            let ts = c.timestamp();
            if ts > 0 {
                lines.push(kv("Updated", relative_time_short(ts)));
            }
            if !c.user.is_empty() {
                lines.push(kv("Owner", c.user.clone()));
            }
            if !c.channels.is_empty() {
                lines.push(kv("Channels", c.channels.join(", ")));
            }
            if !c.permalink.is_empty() {
                lines.push(kv("Permalink", c.permalink.clone()));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Enter → open in browser",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            )));
            let p = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(p, area);
        }
        Some(Item::ThreadPlaceholder) | None => {
            let msg = match app.active().spec.kind.as_str() {
                "threads" => "Threads aggregation across recent channels — coming soon.",
                _ => "(no item selected)",
            };
            let p = Paragraph::new(msg)
                .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
                .block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(p, area);
        }
    }
}

fn draw_channel_detail(f: &mut Frame, area: Rect, c: &Channel, app: &App) {
    let title = format!(" {} ", c.display_name());
    let Some(detail) = &app.detail else {
        let p = Paragraph::new("(loading history…)")
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    };
    if detail.channel_id != c.id {
        let p = Paragraph::new("(loading history…)")
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }
    let lines: Vec<Line> = render_messages(detail, app);
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn render_messages(detail: &ChannelDetail, app: &App) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for m in &detail.messages {
        out.push(format_message(m, app));
    }
    out
}

fn format_message(m: &Message, app: &App) -> Line<'static> {
    let author = m
        .author_id()
        .map(|a| app.resolve_user(a))
        .unwrap_or_else(|| "(system)".to_string());
    let ts = ts_to_hms(&m.ts);
    let reply_hint = m
        .reply_count
        .filter(|n| *n > 0)
        .map(|n| format!(" ↳{n}"))
        .unwrap_or_default();
    let body = first_line_truncated(&m.text, 60);
    Line::from(vec![
        Span::styled(
            format!(" {ts} "),
            Style::default().fg(crate::theme::remap(Color::DarkGray)),
        ),
        Span::styled(format!("{author:<14}"), Style::default().fg(Color::Cyan)),
        Span::styled(body, Style::default().fg(Color::White)),
        Span::styled(reply_hint, Style::default().fg(Color::Yellow)),
    ])
}

fn first_line_truncated(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or(s);
    truncate(first, max)
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter open · / search · p post · R react · T thread · y permalink · r refresh · q quit ";
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

fn draw_input_overlay(f: &mut Frame, area: Rect, bar: &InputBar) {
    let row_y = area.height.saturating_sub(4);
    let h = 3;
    let rect = Rect {
        x: area.x,
        y: row_y,
        width: area.width,
        height: h,
    };
    let label = match bar.mode {
        InputMode::Search => " search > ",
        InputMode::Post => " post > ",
        InputMode::ThreadReply => " thread reply > ",
    };
    let body = format!("{label}{}_", bar.buffer);
    let p = Paragraph::new(body)
        .style(
            Style::default()
                .fg(Color::White)
                .bg(crate::theme::remap(Color::DarkGray)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Enter to submit · Esc cancel ")
                .style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(Clear, rect);
    f.render_widget(p, rect);
}

fn draw_reaction_overlay(f: &mut Frame, area: Rect, picker: &ReactionPicker) {
    // Centered ~50% width, 8 rows.
    let w = (area.width * 50 / 100).clamp(30, 60);
    let h = 8u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    let mut lines: Vec<Line> = Vec::new();
    // 12 emojis in 2 rows of 6.
    for chunk in QUICK_REACTIONS.chunks(6) {
        let spans: Vec<Span> = chunk
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let absolute = match QUICK_REACTIONS.iter().position(|n| n == name) {
                    Some(idx) => idx,
                    None => i,
                };
                let display = format!(" :{name}: ");
                let style = if absolute == picker.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Span::styled(display, style)
            })
            .collect();
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " ←→/hjkl move · Enter react · Esc cancel ",
        Style::default().fg(crate::theme::remap(Color::DarkGray)),
    )));
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" react ")
            .style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(Clear, rect);
    f.render_widget(p, rect);
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
        let s = truncate("abcdefghij", 5);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 5);
    }

    #[test]
    fn first_line_clips_to_first_line() {
        let body = "hello\nworld";
        assert_eq!(first_line_truncated(body, 100), "hello");
    }
}
