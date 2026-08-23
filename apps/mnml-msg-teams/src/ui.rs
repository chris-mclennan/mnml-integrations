//! ratatui rendering + the main event loop.

use crate::app::{App, DetailKind, Item, TabState, ThreadView, short_ts};
use crate::keys;
use crate::teams::Message;
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
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};
use std::collections::HashMap;
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
    // #1006 — the threads tab has its own two-column layout: left =
    // the reply list, right = the parent message. The generic
    // list/detail pair below only fits the tabs (teams / chats /
    // search) that keep a per-tab item list — threads renders from
    // App::focused_thread instead.
    if app.active().spec.kind == "threads" {
        draw_thread_list(f, body[0], app);
        draw_thread_detail(f, body[1], app);
    } else {
        draw_list(f, body[0], app.active());
        draw_detail(f, body[1], app);
    }
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
        .block(Block::default().borders(Borders::ALL).title(" teams "))
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
            .wrap(Wrap { trim: false })
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

    // Search-tab query line takes the top row when in search-mode.
    let mut top_lines: Vec<Line> = Vec::new();
    if tab.data.search_mode {
        top_lines.push(Line::from(vec![
            Span::styled(" search: ", Style::default().fg(Color::Yellow)),
            Span::raw(tab.data.search_query.clone()),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
        ]));
    }

    let body_rows = area.height.saturating_sub(2 + top_lines.len() as u16) as usize;
    let total = tab.data.items.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let mut lines: Vec<Line> = top_lines;
    for (i, item) in tab.data.items[start..].iter().take(body_rows).enumerate() {
        let abs = start + i;
        let cursor = if abs == selected { "▸ " } else { "  " };
        let primary = truncate(&item.primary_label(), 36);
        let secondary = truncate(&item.secondary_label(), 80);
        let line = if secondary.is_empty() {
            format!("{cursor}{}", pad_display(&primary, 36))
        } else {
            format!("{cursor}{}  {secondary}", pad_display(&primary, 36))
        };
        let style = if abs == selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            row_style(item)
        };
        lines.push(Line::from(Span::styled(line, style)));
    }

    let title = match tab.spec.kind.as_str() {
        "teams" => format!(" teams ({total}) "),
        "chats" => format!(" chats ({total}) "),
        "search" => format!(" search ({total}) "),
        "threads" => " threads ".to_string(),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn row_style(item: &Item) -> Style {
    match item {
        Item::Team { .. } => Style::default().fg(Color::White),
        Item::Channel { .. } => Style::default().fg(Color::Gray),
        Item::Chat(_) => Style::default().fg(Color::White),
        Item::Message(_) => Style::default().fg(Color::Gray),
        Item::SearchPrompt => Style::default().fg(crate::theme::remap(Color::DarkGray)),
        Item::Placeholder(_) => Style::default().fg(crate::theme::remap(Color::DarkGray)),
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();

    // If composing, show the post buffer.
    if let Some(mode) = &app.post_mode {
        let title = match mode {
            crate::app::PostMode::Channel { .. } => " post → channel ",
            crate::app::PostMode::Chat { .. } => " post → chat ",
            crate::app::PostMode::ChannelReply { .. } => " thread reply ",
        };
        let p = Paragraph::new(format!(
            "{}\n\n{}▏",
            instructions_for_post(),
            app.post_buffer
        ))
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }

    let title = match &tab.data.detail_kind {
        DetailKind::Channel(_, _) => " channel ",
        DetailKind::Chat(_) => " chat ",
        DetailKind::Message => " message ",
        DetailKind::None => " detail ",
    };

    if tab.data.detail_messages.is_empty() {
        let hint = match tab.spec.kind.as_str() {
            "teams" => "Enter to expand a team's channels · focus a channel for scrollback",
            "chats" => "focus a chat to load recent messages",
            "search" => "/ to search messages across Teams",
            "threads" => "v0.1: thread tab is a placeholder",
            _ => "(no detail)",
        };
        let p = Paragraph::new(hint)
            .style(Style::default().fg(crate::theme::remap(Color::DarkGray)))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }

    // Render last ~30 messages (or whatever the API returned).
    // Each message → `HH:MM · username · body`. System messages dim.
    let mentions = app.graph.mention_snapshot();
    let lines: Vec<Line> = render_messages(&tab.data.detail_messages, &mentions);

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn instructions_for_post() -> &'static str {
    "(Ctrl+S to send, Esc to cancel — single-line v0.1)"
}

fn render_messages(msgs: &[Message], mentions: &HashMap<String, String>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Graph returns newest-first; reverse for chronological scrollback.
    let ordered: Vec<&Message> = msgs.iter().rev().collect();
    for m in ordered {
        if m.is_system() {
            let body = strip_to_one_line(&m.body_text());
            lines.push(Line::from(Span::styled(
                format!(
                    " {} · (system) {}",
                    m.created_date_time
                        .as_deref()
                        .map(short_ts)
                        .unwrap_or_default(),
                    body
                ),
                Style::default()
                    .fg(crate::theme::remap(Color::DarkGray))
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }
        let ts = m
            .created_date_time
            .as_deref()
            .map(short_ts)
            .unwrap_or_default();
        let author = m.author();
        let body = m.body_text();
        let resolved = resolve_mentions(&body, mentions);

        let header = Line::from(vec![
            Span::styled(
                format!(" {ts} "),
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            ),
            Span::styled(
                "· ",
                Style::default().fg(crate::theme::remap(Color::DarkGray)),
            ),
            Span::styled(
                author,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        lines.push(header);

        for ln in resolved.lines().take(8) {
            lines.push(Line::from(Span::styled(
                format!("   {ln}"),
                Style::default().fg(Color::White),
            )));
        }

        // Reactions chips
        if !m.reactions.is_empty() {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for r in &m.reactions {
                if let Some(rt) = &r.reaction_type {
                    *counts.entry(rt.as_str()).or_insert(0) += 1;
                }
            }
            let mut chip_spans: Vec<Span<'static>> = vec![Span::raw("   ")];
            for (rt, n) in counts.iter() {
                let glyph = reaction_glyph(rt);
                chip_spans.push(Span::styled(
                    format!("[{glyph} {n}] "),
                    Style::default().fg(Color::Yellow),
                ));
            }
            lines.push(Line::from(chip_spans));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn reaction_glyph(rt: &str) -> &'static str {
    match rt {
        "like" => "👍",
        "heart" => "❤",
        "laugh" => "😂",
        "surprised" => "😮",
        "sad" => "😢",
        "angry" => "😡",
        _ => "•",
    }
}

fn strip_to_one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    truncate(first, 80)
}

/// Resolve `<at id="X">Display Name</at>` spans inline against the
/// mention cache. #1006. In practice, `strip_html` above has
/// already normalised Teams' HTML message bodies into plain text
/// (the inner "Display Name" survives, the tag is dropped) — so
/// this function's real job is to catch two edge cases:
///
/// * Bodies whose `contentType` is `"text"` — Teams occasionally
///   emits raw `<at id="X"></at>` (empty content) inside plain-text
///   bodies. Without cache lookup that would render as `""`.
/// * Bodies where the inner span was stripped by an upstream
///   sanitizer, leaving `@X` (the raw id).
///
/// Missing ids fall back to `@<id>` so the reader still sees a
/// mention shape rather than a silent blank.
fn resolve_mentions(body: &str, cache: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        // Fast path: look for the literal "<at " opener; anything
        // else passes through byte-for-byte. Teams' `<at>` spans
        // always carry an `id` attribute so this is the only prefix
        // we care about here.
        if bytes[i] == b'<'
            && bytes.get(i + 1..i + 4) == Some(b"at ")
            && let Some((consumed, id, inner)) = parse_at_tag(&body[i..])
        {
            let resolved = if !inner.is_empty() {
                // Prefer the inline display name — Teams typically
                // writes it into the tag body.
                inner
            } else if let Some(name) = cache.get(&id) {
                format!("@{name}")
            } else {
                format!("@{id}")
            };
            out.push_str(&resolved);
            i += consumed;
            continue;
        }
        // Not a mention we recognise — push one char and advance.
        let ch = body[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Parse a single `<at id="X">inner</at>` starting at `s[0]`.
/// Returns `(bytes_consumed, id, inner_text)` when a full tag pair
/// matches; None when the input doesn't look like one (unclosed tag,
/// malformed, or a bare `<at` that turned out to be something else).
///
/// v0.2 lookahead — no regex crate; the parser is intentionally
/// permissive on attribute ordering (`id="…"` may sit alongside
/// `mentionId="0"`, which Teams also emits).
fn parse_at_tag(s: &str) -> Option<(usize, String, String)> {
    let open_end = s.find('>')?;
    let attrs = &s[3..open_end];
    // Extract id="X" (or id='X') — Teams emits double-quoted.
    let id_start = attrs.find("id=")?;
    let after_eq = attrs.get(id_start + 3..)?;
    let (quote, quoted) = match after_eq.chars().next()? {
        '"' => ('"', &after_eq[1..]),
        '\'' => ('\'', &after_eq[1..]),
        _ => return None,
    };
    let id_end = quoted.find(quote)?;
    let id = quoted[..id_end].to_string();
    // Find `</at>` after the open tag.
    let rest = &s[open_end + 1..];
    let close_start = rest.find("</at>")?;
    let inner = rest[..close_start].to_string();
    let total = open_end + 1 + close_start + "</at>".len();
    Some((total, id, inner))
}

/// #1006 — threads-tab left column: parent snippet + list of
/// replies. Selection is decorative in v0.2 (the tab renders from
/// `App::focused_thread`, not from an item list), but keeps the
/// same visual grammar as the other tabs' left columns.
fn draw_thread_list(f: &mut Frame, area: Rect, app: &App) {
    let Some(thread) = &app.focused_thread else {
        // No focused thread — reuse the standard list path so the
        // configured placeholder hint renders.
        draw_list(f, area, app.active());
        return;
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" ▸ thread ({} repl.) ", thread.replies.len()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    // body_text() returns a String; keep it alive until truncate
    // finishes copying by binding it before slicing.
    let parent_body = thread.parent.body_text();
    let parent_snip = truncate(parent_body.lines().next().unwrap_or(""), 60);
    lines.push(Line::from(Span::styled(
        format!("   {parent_snip}"),
        Style::default().fg(Color::White),
    )));
    if app.thread_loading {
        lines.push(Line::from(Span::styled(
            "   (loading replies…)",
            Style::default().fg(crate::theme::remap(Color::DarkGray)),
        )));
    } else if thread.replies.is_empty() {
        lines.push(Line::from(Span::styled(
            "   (no replies yet — press p on the parent to reply)",
            Style::default().fg(crate::theme::remap(Color::DarkGray)),
        )));
    } else {
        // Newest-first from Graph; reverse for chronological order.
        for r in thread.replies.iter().rev() {
            let author = r.author();
            let ts = r
                .created_date_time
                .as_deref()
                .map(short_ts)
                .unwrap_or_default();
            let reply_body = r.body_text();
            let snip = truncate(reply_body.lines().next().unwrap_or(""), 44);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {ts} "),
                    Style::default().fg(crate::theme::remap(Color::DarkGray)),
                ),
                Span::styled(
                    format!("{author}: "),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(snip, Style::default().fg(Color::White)),
            ]));
        }
    }
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" thread "));
    f.render_widget(p, area);
}

/// #1006 — threads-tab right column: full parent message + all
/// replies rendered as scrollback, matching the channel-detail
/// layout. Mention resolution reuses the same cache-driven path
/// as the channel scrollback (`render_messages`).
fn draw_thread_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(thread) = &app.focused_thread else {
        // Same fallback as the list column — reuse the generic
        // detail hint.
        draw_detail(f, area, app);
        return;
    };
    let mentions = app.graph.mention_snapshot();
    // Show parent as the first block, then replies chronologically.
    let combined = build_thread_scrollback(thread);
    let lines = render_messages(&combined, &mentions);
    let title = " thread scrollback ";
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

/// Assemble [parent, ...replies-newest-first] so `render_messages`'
/// existing reverse-for-chronological pass presents them
/// oldest→newest with the parent at the top. Cheap — clones a few
/// `Message`s.
fn build_thread_scrollback(thread: &ThreadView) -> Vec<Message> {
    // render_messages reverses the slice. To get [parent, replies-oldest-first]
    // in the output, feed it [replies-oldest-first-reversed = replies-newest-first, parent]
    // then reverse → [parent, replies-oldest-first].
    let mut out = thread.replies.clone();
    // Graph gave newest-first; that's already what we want here so
    // the reverse in render_messages lands them oldest-first.
    out.push(thread.parent.clone());
    out
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter open · / search · p post · R react · T thread · v view thread · y permalink · r refresh · q quit ";
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

/// Right-pad `s` to exactly `cols` display cells with U+0020. If `s`
/// already wider than `cols`, returns it unchanged. Use INSTEAD of
/// `format!("{:<W}", s)` — Rust's left-pad format spec counts CHARS,
/// not display cells, so CJK / emoji rows misalign.
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
    fn reaction_glyphs_known() {
        assert_eq!(reaction_glyph("like"), "👍");
        assert_eq!(reaction_glyph("heart"), "❤");
        assert_eq!(reaction_glyph("unknown"), "•");
    }

    // #1006 — mention-cache-driven resolution.

    #[test]
    fn parse_at_tag_extracts_id_and_inner() {
        let (n, id, inner) = parse_at_tag(r#"<at id="42">Alice</at> hi"#).unwrap();
        assert_eq!(id, "42");
        assert_eq!(inner, "Alice");
        // consumed = `<at id="42">Alice</at>` = 22 chars.
        assert_eq!(n, r#"<at id="42">Alice</at>"#.len());
    }

    #[test]
    fn parse_at_tag_handles_extra_attrs() {
        // Teams sometimes writes `<at id="0" mentionId="0">Bob</at>`.
        let (_, id, inner) = parse_at_tag(r#"<at id="0" mentionId="0">Bob</at>"#).unwrap();
        assert_eq!(id, "0");
        assert_eq!(inner, "Bob");
    }

    #[test]
    fn parse_at_tag_rejects_malformed() {
        assert!(parse_at_tag(r#"<at>Anon</at>"#).is_none()); // no id
        assert!(parse_at_tag(r#"<at id="X">unclosed"#).is_none()); // no </at>
    }

    #[test]
    fn resolve_mentions_keeps_inline_display_name() {
        let out = resolve_mentions(r#"ping <at id="42">Alice</at> for lunch"#, &HashMap::new());
        assert_eq!(out, "ping Alice for lunch");
    }

    #[test]
    fn resolve_mentions_uses_cache_when_inner_empty() {
        let cache: HashMap<String, String> = [("u1".into(), "Carol".into())].into_iter().collect();
        let out = resolve_mentions(r#"hi <at id="u1"></at>"#, &cache);
        assert_eq!(out, "hi @Carol");
    }

    #[test]
    fn resolve_mentions_falls_back_to_raw_id_on_miss() {
        let out = resolve_mentions(r#"hi <at id="u1"></at>"#, &HashMap::new());
        assert_eq!(out, "hi @u1");
    }

    #[test]
    fn resolve_mentions_leaves_non_mention_text_intact() {
        let out = resolve_mentions("just a plain line & <b>tag</b>", &HashMap::new());
        assert_eq!(out, "just a plain line & <b>tag</b>");
    }
}
