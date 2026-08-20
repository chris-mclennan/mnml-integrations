//! ratatui rendering + main event loop.
//!
//! One full-width list of schedules; each row collapses by default
//! (name + state + humanized cron) and expands inline on Enter /
//! Right / Space / click to show group, TZ, cron expression, and
//! Target Input JSON. `e` on the selected row enters an edit
//! overlay for the schedule expression + Target Input.

use crate::app::{App, EditFocus, Mode};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::io::Stdout;
use std::time::Duration;

/// `MNML_PANE=1` is stamped by mnml on Pty children so siblings can
/// drop their outer border when hosted in a mnml pane.
fn inside_mnml() -> bool {
    std::env::var_os("MNML_PANE").is_some()
}

pub fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let res = event_loop(&mut terminal, app);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(150))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    crate::keys::handle(key, app);
                }
                Event::Mouse(m) => handle_mouse(m, app),
                _ => {}
            }
        }
        app.poll_background();
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_mouse(m: MouseEvent, app: &mut App) {
    // Edit mode owns the cursor — ignore mouse there.
    if matches!(app.mode, Mode::Edit | Mode::Saving) {
        return;
    }
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let hit = app
                .row_hits
                .iter()
                .find(|(_, y0, y1)| m.row >= *y0 && m.row < *y1)
                .map(|(i, _, _)| *i);
            if let Some(idx) = hit {
                if app.selected == idx {
                    app.toggle_expand_at(idx);
                } else {
                    app.selected = idx;
                    app.detail = None;
                    app.refresh_detail();
                }
            }
        }
        MouseEventKind::ScrollDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(3);
        }
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
        }
        _ => {}
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    let outer_area = if inside_mnml() {
        size
    } else {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" EventBridge Schedules ");
        let inner = block.inner(size);
        f.render_widget(block, size);
        inner
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(outer_area);
    let body = chunks[0];
    let status = chunks[1];

    if matches!(app.mode, Mode::Edit | Mode::Saving) {
        draw_edit_overlay(f, body, app);
    } else {
        draw_list(f, body, app);
    }
    draw_status(f, status, app);
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    // Header row.
    let header_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    // 2026-08-01 — ENV / BRANCH columns extracted from each
    // schedule's target Input JSON per `config.env_path` /
    // `config.branch_path`. Header shows the columns
    // unconditionally so users know they exist even when the
    // paths are unset (dash for every row in that case).
    let header = Paragraph::new(Line::from(vec![Span::styled(
        format!(
            "  {:<32} {:<10} {:<8} {:<20} {:<40}",
            "NAME", "STATE", "ENV", "BRANCH", "SCHEDULE"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(header, header_area);

    let body_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    // Build lines + capture hit rects (in list-local coords).
    let mut lines: Vec<Line> = Vec::new();
    let mut hits: Vec<(usize, u16, u16)> = Vec::new();
    let mut cursor_row: u16 = 0;
    let selected = app.selected;
    for (i, s) in app.schedules.iter().enumerate() {
        let is_sel = i == selected;
        let key = (s.name.clone(), s.group_name.clone());
        let is_expanded = app.expanded.contains(&key);
        let cached = app.detail_cache.get(&key);

        // Match mnml's tree-view convention (`▶` collapsed, `▼`
        // expanded) so the sibling reads like the rest of the app.
        let indicator = if is_expanded { "▼ " } else { "▶ " };
        let state_style = match s.state.as_str() {
            "ENABLED" => Style::default().fg(Color::Green),
            "DISABLED" => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::Yellow),
        };
        let name_trunc: String = s.name.chars().take(31).collect();
        let name_padded = format!("{name_trunc:<32}");
        let name_style = if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let human = match cached {
            Some(d) => crate::schedule_expr::humanize(
                &d.schedule_expression,
                &d.schedule_expression_timezone,
            ),
            None => "…".to_string(),
        };
        // 2026-08-01 — ENV / BRANCH from the cached target Input.
        // Uncached rows show "…" (matches the SCHEDULE column's
        // loading state); rows where the path misses show "-".
        // Config keys unset = every row shows "-" in that column.
        let (env_text, branch_text) =
            extract_env_branch(cached, &app.cfg.env_path, &app.cfg.branch_path);
        let row_y_start = cursor_row;
        lines.push(Line::from(vec![
            Span::styled(
                indicator,
                Style::default().fg(if is_sel { Color::Cyan } else { Color::DarkGray }),
            ),
            Span::styled(name_padded, name_style),
            Span::styled(format!("{:<10} ", s.state), state_style),
            Span::styled(
                format!("{env_text:<8} "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{branch_text:<20} "),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(human, Style::default().fg(Color::Gray)),
        ]));
        cursor_row += 1;

        if is_expanded {
            if let Some(d) = cached {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("Group  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(d.group_name.clone(), Style::default().fg(Color::Yellow)),
                    Span::raw("    "),
                    Span::styled("TZ  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(d.schedule_expression_timezone.clone(), Style::default()),
                ]));
                cursor_row += 1;
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        "Schedule expression  ",
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        d.schedule_expression.clone(),
                        Style::default().fg(Color::White),
                    ),
                ]));
                cursor_row += 1;
                lines.push(Line::from(vec![Span::styled(
                    "    Target Input (JSON)",
                    Style::default().fg(Color::DarkGray),
                )]));
                cursor_row += 1;
                let input = d.target.input.clone().unwrap_or_default();
                if input.is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled("(none)", Style::default().fg(Color::DarkGray)),
                    ]));
                    cursor_row += 1;
                } else {
                    for l in input.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("      "),
                            Span::styled(l.to_string(), Style::default().fg(Color::Gray)),
                        ]));
                        cursor_row += 1;
                    }
                }
                lines.push(Line::from(""));
                cursor_row += 1;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("loading detail…", Style::default().fg(Color::DarkGray)),
                ]));
                cursor_row += 1;
                lines.push(Line::from(""));
                cursor_row += 1;
            }
        }
        let row_y_end = cursor_row;
        hits.push((i, row_y_start, row_y_end));
    }
    if app.schedules.is_empty() && app.pending_list.is_none() {
        lines.push(Line::from(Span::styled(
            "  (no schedules in this region)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Clamp scroll and translate hit rects into terminal coords.
    let total_rows = cursor_row;
    let max_scroll = total_rows.saturating_sub(body_area.height);
    if app.scroll_offset > max_scroll {
        app.scroll_offset = max_scroll;
    }
    let scroll = app.scroll_offset;
    app.row_hits = hits
        .into_iter()
        .filter_map(|(i, y0, y1)| {
            if y1 <= scroll || y0 >= scroll + body_area.height {
                return None;
            }
            let a0 = body_area.y + y0.saturating_sub(scroll);
            let a1 = body_area.y + y1.saturating_sub(scroll);
            let clamped_end = a1.min(body_area.y + body_area.height);
            Some((i, a0, clamped_end))
        })
        .collect();

    let widget = Paragraph::new(lines).scroll((scroll, 0));
    f.render_widget(widget, body_area);
}

/// 2026-08-01 — pull the ENV / BRANCH column values for a row.
///   - No cached detail → both "…" (loading; prefetch is in
///     flight and will fill this in on the next frame).
///   - No target input → both "-" (schedule has no Input JSON).
///   - Path missing / config unset → that column's "-".
///
/// Truncated to fit the fixed column widths (8 / 20).
fn extract_env_branch(
    cached: Option<&crate::eventbridge::ScheduleDetail>,
    env_path: &str,
    branch_path: &str,
) -> (String, String) {
    let Some(d) = cached else {
        return ("…".into(), "…".into());
    };
    let Some(input) = d.target.input.as_deref() else {
        return ("-".into(), "-".into());
    };
    let env = crate::config::extract_json_path(input, env_path).unwrap_or_else(|| "-".into());
    let branch = crate::config::extract_json_path(input, branch_path).unwrap_or_else(|| "-".into());
    (truncate(&env, 8), truncate(&branch, 20))
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        // Reserve 1 char for the ellipsis to keep the column
        // width honest.
        let head: String = chars.iter().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn draw_edit_overlay(f: &mut Frame, area: Rect, app: &App) {
    let name = app.selected_name();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" Edit — {name} "));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_at: Option<(u16, u16)> = None;
    let expr_focused = app.edit.focus == EditFocus::Expression;
    let input_focused = app.edit.focus == EditFocus::Input;

    lines.push(Line::from(vec![Span::styled(
        if expr_focused {
            "▸ Schedule expression"
        } else {
            "  Schedule expression"
        },
        Style::default()
            .fg(if expr_focused {
                Color::Cyan
            } else {
                Color::DarkGray
            })
            .add_modifier(Modifier::BOLD),
    )]));
    if expr_focused {
        let char_pos = app.edit.expression[..app.edit.expression_cursor]
            .chars()
            .count() as u16;
        cursor_at = Some((4 + char_pos, lines.len() as u16));
    }
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            app.edit.expression.clone(),
            Style::default().fg(Color::White),
        ),
    ]));
    let tz = app
        .detail
        .as_ref()
        .map(|d| d.schedule_expression_timezone.clone())
        .unwrap_or_default();
    let human = crate::schedule_expr::humanize(&app.edit.expression, &tz);
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(human, Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![Span::styled(
        if input_focused {
            "▸ Target Input (JSON)"
        } else {
            "  Target Input (JSON)"
        },
        Style::default()
            .fg(if input_focused {
                Color::Cyan
            } else {
                Color::DarkGray
            })
            .add_modifier(Modifier::BOLD),
    )]));
    let input_body_row = lines.len() as u16;
    let mut line_count = 0u16;
    for l in app.edit.input.split('\n') {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(l.to_string(), Style::default().fg(Color::White)),
        ]));
        line_count += 1;
    }
    if line_count == 0 {
        lines.push(Line::from(Span::raw("    ")));
    }
    if input_focused {
        let before = &app.edit.input[..app.edit.input_cursor];
        let line_idx = before.matches('\n').count() as u16;
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = app.edit.input[line_start..app.edit.input_cursor]
            .chars()
            .count() as u16;
        cursor_at = Some((4 + col, input_body_row + line_idx));
    }

    let widget = Paragraph::new(lines);
    f.render_widget(widget, inner);
    if let Some((cx, cy)) = cursor_at {
        let x = inner
            .x
            .saturating_add(cx)
            .min(inner.x + inner.width.saturating_sub(1));
        let y = inner
            .y
            .saturating_add(cy)
            .min(inner.y + inner.height.saturating_sub(1));
        f.set_cursor_position((x, y));
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = match app.mode {
        Mode::Browse => {
            " ↑↓/jk move · Enter/Space/click expand · e edit · t enable/disable · r refresh · q quit "
        }
        Mode::Edit => " Tab focus · Ctrl+S save · Esc cancel ",
        Mode::Saving => " saving… ",
    };
    // Region indicator (right side) so the user always knows which
    // AWS environment they're viewing / editing. Falls back to
    // "(aws default)" when the config lets the aws CLI resolve.
    let region = app
        .cfg
        .region
        .clone()
        .unwrap_or_else(|| "(aws default)".to_string());
    let region_text = format!(" {region} ");
    let region_w = region_text.chars().count() as u16;
    let left_w = area.width.saturating_sub(region_w);
    let left_area = Rect {
        x: area.x,
        y: area.y,
        width: left_w,
        height: 1,
    };
    let right_area = Rect {
        x: area.x + left_w,
        y: area.y,
        width: region_w,
        height: 1,
    };
    let left_line = Line::from(vec![
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
    f.render_widget(Paragraph::new(left_line), left_area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            region_text,
            Style::default().fg(Color::Yellow),
        ))),
        right_area,
    );
}
