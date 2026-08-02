//! ratatui rendering + main event loop for the CodeBuild browser.
//!
//! Single full-width list of projects with expandable rows —
//! same shape as `mnml-aws-eventbridge`'s Schedules browser.

use crate::app::App;
use crate::codebuild::{BuildStatus, CodeBuildRecord};
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
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => crate::keys::handle(k, app),
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
            .title(" AWS CodeBuild ");
        let inner = block.inner(size);
        f.render_widget(block, size);
        inner
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(outer_area);
    draw_list(f, chunks[0], app);
    draw_status(f, chunks[1], app);
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App) {
    // Header.
    let header_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let header = Paragraph::new(Line::from(vec![Span::styled(
        format!("  {:<40} {:<12} {:<40}", "PROJECT", "STATUS", "LATEST"),
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

    let mut lines: Vec<Line> = Vec::new();
    let mut hits: Vec<(usize, u16, u16)> = Vec::new();
    let mut cursor_row: u16 = 0;
    let selected = app.selected;
    for (i, name) in app.projects.iter().enumerate() {
        let is_sel = i == selected;
        let is_expanded = app.expanded.contains(name);
        let cached_detail = app.detail_cache.get(name);
        let cached_builds = app.builds_cache.get(name);

        let latest = cached_builds.and_then(|b| b.first());
        // Three states for the STATUS + LATEST columns:
        //   - fetch pending → dim `…`
        //   - fetch landed, project has NO builds ever → dim `(no builds)`
        //   - fetch landed with a build → the status pill + summary
        let has_fetched = cached_builds.is_some();
        let (status_glyph, status_label, status_style) = match latest {
            Some(b) => (
                b.status.glyph(),
                b.status.label().to_string(),
                status_color(b.status),
            ),
            None => (
                if has_fetched { "-" } else { "…" },
                String::new(),
                Style::default().fg(Color::DarkGray),
            ),
        };
        let latest_summary = if let Some(b) = latest {
            format_latest(b)
        } else if has_fetched {
            "(no builds)".to_string()
        } else {
            "…".to_string()
        };

        // Match mnml's tree-view convention (`▶` collapsed, `▼`
        // expanded) so the sibling reads like the rest of the app.
        let indicator = if is_expanded { "▼ " } else { "▶ " };
        let name_trunc: String = name.chars().take(39).collect();
        let name_padded = format!("{name_trunc:<40}");
        let name_style = if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let row_y_start = cursor_row;
        lines.push(Line::from(vec![
            Span::styled(
                indicator,
                Style::default().fg(if is_sel { Color::Cyan } else { Color::DarkGray }),
            ),
            Span::styled(name_padded, name_style),
            Span::styled(format!("{status_glyph} "), status_style),
            Span::styled(format!("{status_label:<11}"), status_style),
            Span::styled(latest_summary, Style::default().fg(Color::Gray)),
        ]));
        cursor_row += 1;

        if is_expanded {
            match (cached_detail, cached_builds) {
                (Some(d), builds_opt) => {
                    let ind = "      ";
                    lines.push(Line::from(vec![
                        Span::styled(
                            "    Source  ",
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            d.source_type.clone().unwrap_or_default(),
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            d.source_location.clone().unwrap_or_default(),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));
                    cursor_row += 1;
                    if let Some(bs) = &d.buildspec {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "    Buildspec  ",
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(bs.clone(), Style::default().fg(Color::White)),
                        ]));
                        cursor_row += 1;
                    }
                    if let Some(img) = &d.environment_image {
                        let ct = d.compute_type.clone().unwrap_or_default();
                        lines.push(Line::from(vec![
                            Span::styled(
                                "    Environment  ",
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(img.clone(), Style::default().fg(Color::White)),
                            Span::raw("  "),
                            Span::styled(ct, Style::default().fg(Color::DarkGray)),
                        ]));
                        cursor_row += 1;
                    }
                    if let Some(lg) = &d.log_group {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "    Log group  ",
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(lg.clone(), Style::default().fg(Color::White)),
                        ]));
                        cursor_row += 1;
                    }
                    if let Some(role) = &d.service_role {
                        lines.push(Line::from(vec![
                            Span::styled(
                                "    Service role  ",
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(role.clone(), Style::default().fg(Color::DarkGray)),
                        ]));
                        cursor_row += 1;
                    }
                    lines.push(Line::from(""));
                    cursor_row += 1;

                    lines.push(Line::from(vec![Span::styled(
                        "    Recent builds",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )]));
                    cursor_row += 1;
                    match builds_opt {
                        Some(builds) if !builds.is_empty() => {
                            for b in builds {
                                let s = status_color(b.status);
                                lines.push(Line::from(vec![
                                    Span::raw(ind),
                                    Span::styled(format!("{} ", b.status.glyph()), s),
                                    Span::styled(
                                        format!("#{:<6} ", b.build_number),
                                        Style::default().fg(Color::White),
                                    ),
                                    Span::styled(
                                        format_duration(b.duration_ms),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::raw("  "),
                                    Span::styled(
                                        format_when(b.started_at_ms),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::raw("  "),
                                    Span::styled(
                                        b.source_version
                                            .clone()
                                            .map(|s| s.chars().take(12).collect::<String>())
                                            .unwrap_or_default(),
                                        Style::default().fg(Color::Yellow),
                                    ),
                                ]));
                                cursor_row += 1;
                            }
                        }
                        Some(_) => {
                            lines.push(Line::from(vec![
                                Span::raw(ind),
                                Span::styled(
                                    "(no builds yet)",
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]));
                            cursor_row += 1;
                        }
                        None => {
                            lines.push(Line::from(vec![
                                Span::raw(ind),
                                Span::styled(
                                    "loading builds…",
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]));
                            cursor_row += 1;
                        }
                    }
                    lines.push(Line::from(""));
                    cursor_row += 1;
                }
                (None, _) => {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled("loading detail…", Style::default().fg(Color::DarkGray)),
                    ]));
                    cursor_row += 1;
                    lines.push(Line::from(""));
                    cursor_row += 1;
                }
            }
        }
        let row_y_end = cursor_row;
        hits.push((i, row_y_start, row_y_end));
    }
    if app.projects.is_empty() && app.pending_projects.is_none() {
        lines.push(Line::from(Span::styled(
            "  (no projects in this region)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Clamp scroll and translate hits into terminal coords.
    let total = cursor_row;
    let max_scroll = total.saturating_sub(body_area.height);
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

fn status_color(s: BuildStatus) -> Style {
    match s {
        BuildStatus::Succeeded => Style::default().fg(Color::Green),
        BuildStatus::Failed | BuildStatus::Fault | BuildStatus::TimedOut => {
            Style::default().fg(Color::Red)
        }
        BuildStatus::InProgress => Style::default().fg(Color::Yellow),
        BuildStatus::Stopped => Style::default().fg(Color::DarkGray),
        BuildStatus::Unknown => Style::default().fg(Color::DarkGray),
    }
}

fn format_latest(b: &CodeBuildRecord) -> String {
    let dur = format_duration(b.duration_ms);
    let when = format_when(b.started_at_ms);
    format!("#{:<6} {when}  {dur}", b.build_number)
}

fn format_duration(ms: Option<u64>) -> String {
    match ms {
        None => "—".to_string(),
        Some(ms) => {
            let s = ms / 1000;
            if s < 60 {
                format!("{s}s")
            } else if s < 3600 {
                format!("{}m {}s", s / 60, s % 60)
            } else {
                format!("{}h {}m", s / 3600, (s % 3600) / 60)
            }
        }
    }
}

/// Coarse "when it started" — no chrono; we compare epoch-ms against
/// the current wall-clock via `SystemTime::now()`.
fn format_when(ms: Option<i64>) -> String {
    let Some(started) = ms else {
        return "—".to_string();
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let delta = (now_ms - started).max(0);
    let s = delta / 1000;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " ↑↓/jk move · Enter/Space/click expand · s start build · r refresh · q quit ";
    // Region indicator right-aligned.
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
