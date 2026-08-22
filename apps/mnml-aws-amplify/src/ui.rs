//! ratatui rendering + main event loop.

use crate::app::{App, TabData};
use crate::keys;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseButton, MouseEventKind},
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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::io::Stdout;
use std::time::{Duration, Instant};

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    // OSC 0 window/tab title — mnml's Pty tab strip captures this to
    // label the pane. Without it the tab falls back to the process
    // name / chip tooltip, which is longer and truncates awkwardly.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetTitle("Amplify Deployments")
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let res = event_loop(&mut terminal, app).await;
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.drain();
        if app.cfg.refresh_interval_secs > 0
            && last_refresh.elapsed().as_secs() >= app.cfg.refresh_interval_secs
        {
            app.refresh_active();
            last_refresh = Instant::now();
        }
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    if let Some(action) = keys::handle(key, app) {
                        let quit = keys::apply(action, app).await;
                        if quit {
                            break;
                        }
                        last_refresh = Instant::now();
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse(mouse, app);
                    last_refresh = Instant::now();
                }
                Event::Resize(_, _) => {
                    last_refresh = Instant::now();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_mouse(m: crossterm::event::MouseEvent, app: &mut App) {
    if app.logs_view.is_some() {
        // Overlay owns the mouse — scroll only for now.
        match m.kind {
            MouseEventKind::ScrollUp => app.logs_scroll(-3),
            MouseEventKind::ScrollDown => app.logs_scroll(3),
            _ => {}
        }
        return;
    }
    // #988 (2026-08-20) — the deployment-history drill-in pane owns
    // the mouse the same way logs_view does. Without this guard,
    // a click while it's open falls through to the unified-view row-
    // hit logic below and silently toggles expand on the (hidden)
    // Apps view. Scroll-only for now; row-select in the history
    // pane stays keyboard-driven until the sub-view grows its own
    // row-hit math.
    if app.deployment_history.is_some() {
        match m.kind {
            MouseEventKind::ScrollUp => app.deployment_history_move(-3),
            MouseEventKind::ScrollDown => app.deployment_history_move(3),
            _ => {}
        }
        return;
    }
    // Recompute the layout chunks the same way `draw()` does so
    // we can translate the mouse row to a visible-row index.
    // Terminal size isn't cached on App; use crossterm's size().
    let (cols, rows) = match crossterm::terminal::size() {
        Ok(sz) => sz,
        Err(_) => return,
    };
    let full = Rect {
        x: 0,
        y: 0,
        width: cols,
        height: rows,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(full);
    let body = chunks[0];
    if m.column < body.x
        || m.row < body.y
        || m.column >= body.x + body.width
        || m.row >= body.y + body.height
    {
        return;
    }
    // Body has a 1-row border on top when standalone; the border
    // block is skipped when launched inside a mnml Pty pane (see
    // `inside_mnml` in draw_unified_view), so content starts at
    // row 0 in that case. 2026-08-01 — user: "every click I make
    // clicks 1 cell above" — this was the off-by-one.
    let inside_mnml = std::env::var_os("MNML_PANE").is_some();
    let border_offset: u16 = if inside_mnml { 0 } else { 1 };
    let row_in_body = m.row.saturating_sub(body.y + border_offset);
    // 2026-08-01 — line→row translation. Compute per-row line
    // starts (1 line for header rows + branch rows; +1-2 more
    // when a branch is expanded). Walk to find which visible
    // row contains the clicked line. Mirrors the math
    // draw_unified_view uses for the selection highlight.
    let rows = app.visible_rows();
    let row_line_starts = visible_row_line_starts(app, &rows);
    let sel_row = {
        let TabData::Apps(a) = &app.active().data else {
            return;
        };
        a.selected
    };
    let sel_line = row_line_starts.get(sel_row).copied().unwrap_or(0) as u16;
    let scroll_offset = sel_line.saturating_sub(body.height.saturating_sub(4));
    let clicked_line = (row_in_body + scroll_offset) as usize;
    // Find the visible-row whose line-span contains this line.
    let mut idx: Option<usize> = None;
    for (i, start) in row_line_starts.iter().enumerate() {
        let end = row_line_starts.get(i + 1).copied().unwrap_or(usize::MAX);
        if clicked_line >= *start && clicked_line < end {
            idx = Some(i);
            break;
        }
    }
    let Some(idx) = idx else {
        return;
    };
    let clicked_row = rows.get(idx).cloned();
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let TabData::Apps(a) = &mut app.active_mut().data {
                a.selected = idx;
            }
            // Click-target zones:
            //   AppHeader row → toggle expand (whole row)
            //   Branch row chevron (cols 4-5) → toggle inline expand
            //   Branch row elsewhere → open deployment history
            //   Deployment row (rendered as its own selectable row
            //                   now) → drill into that job's logs
            let col_in_body = m.column.saturating_sub(body.x);
            match clicked_row {
                Some(crate::app::VisibleRow::AppHeader { .. }) => {
                    app.enter_focused();
                }
                Some(crate::app::VisibleRow::Branch { .. }) if (4..=5).contains(&col_in_body) => {
                    app.toggle_branch_expand_selected();
                }
                Some(crate::app::VisibleRow::Branch { .. })
                | Some(crate::app::VisibleRow::Deployment { .. }) => {
                    app.enter_focused();
                }
                _ => {}
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            if let TabData::Apps(a) = &mut app.active_mut().data {
                a.selected = idx;
            }
            // Right-click semantics:
            //   - AppHeader row  → toggle hide/unhide (same as `x`)
            //   - Branch row     → open in-app logs viewer
            //                      (same as Enter). No popup menu
            //                      yet; use `L` for CloudWatch or
            //                      `o` for the AWS console.
            match clicked_row {
                Some(crate::app::VisibleRow::AppHeader { .. }) => {
                    app.toggle_hide_selected();
                }
                Some(crate::app::VisibleRow::Branch { .. })
                | Some(crate::app::VisibleRow::Deployment { .. }) => {
                    app.enter_focused();
                }
                None => {}
            }
        }
        MouseEventKind::ScrollUp => app.move_selection(-3),
        MouseEventKind::ScrollDown => app.move_selection(3),
        _ => {}
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(size);
    if app.logs_view.is_some() {
        draw_logs_view(f, chunks[0], app);
    } else if app.deployment_history.is_some() {
        draw_deployment_history(f, chunks[0], app);
    } else {
        draw_unified_view(f, chunks[0], app);
    }
    draw_status(f, chunks[1], app);
}

fn draw_deployment_history(f: &mut Frame, area: Rect, app: &App) {
    let Some(dh) = &app.deployment_history else {
        return;
    };
    // Split into summary card (top) + table (bottom).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(1)])
        .split(area);

    // ── Summary card — latest deployment's key facts. Mirrors the
    //    top row of the AWS console's Deployments panel. ─────────
    let latest = dh.jobs.first();
    let mut summary_lines: Vec<Line> = Vec::new();
    if dh.loading && dh.jobs.is_empty() {
        summary_lines.push(Line::from(Span::styled(
            "loading deployments…",
            Style::default().fg(Color::DarkGray),
        )));
    } else if let Some(err) = &dh.error {
        summary_lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
    } else if let Some(j) = latest {
        let status_color = match j.status.as_str() {
            "SUCCEED" => Color::Green,
            "FAILED" | "CANCELLED" => Color::Red,
            "RUNNING" | "PENDING" | "PROVISIONING" => Color::Cyan,
            _ => Color::Gray,
        };
        summary_lines.push(Line::from(vec![
            Span::styled(
                format!("Deployment #{}", j.job_id),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(j.status.clone(), Style::default().fg(status_color)),
        ]));
        summary_lines.push(Line::from(""));
        let duration = job_duration_str(j);
        summary_lines.push(Line::from(vec![
            Span::styled("Started    ", Style::default().fg(Color::DarkGray)),
            Span::raw(j.start_time.clone().unwrap_or_default()),
        ]));
        summary_lines.push(Line::from(vec![
            Span::styled("Duration   ", Style::default().fg(Color::DarkGray)),
            Span::raw(duration),
        ]));
        summary_lines.push(Line::from(vec![
            Span::styled("Commit     ", Style::default().fg(Color::DarkGray)),
            Span::raw(
                j.commit_message
                    .as_deref()
                    .unwrap_or("(no message)")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string(),
            ),
        ]));
        if let Some(cid) = &j.commit_id {
            summary_lines.push(Line::from(vec![
                Span::styled("SHA        ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    cid.chars().take(12).collect::<String>(),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
    } else {
        summary_lines.push(Line::from(Span::styled(
            "(no deployments)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let summary =
        Paragraph::new(summary_lines).block(Block::default().borders(Borders::ALL).title(format!(
            " {} · {}  latest deployment ",
            dh.app_name, dh.branch_name
        )));
    f.render_widget(summary, chunks[0]);

    // ── History table ─────────────────────────────────────────────
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATUS"),
        Cell::from("DURATION"),
        Cell::from("COMMIT"),
        Cell::from("STARTED"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = dh
        .jobs
        .iter()
        .map(|j| {
            let color = match j.status.as_str() {
                "SUCCEED" => Color::Green,
                "FAILED" | "CANCELLED" => Color::Red,
                "RUNNING" | "PENDING" | "PROVISIONING" => Color::Cyan,
                _ => Color::Gray,
            };
            let started = j
                .start_time
                .as_deref()
                .unwrap_or("")
                .split('T')
                .next()
                .unwrap_or("")
                .to_string();
            let commit_line = j
                .commit_message
                .as_deref()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>();
            Row::new(vec![
                Cell::from(format!("#{}", j.job_id)).style(Style::default().fg(Color::Yellow)),
                Cell::from(j.status.clone()).style(Style::default().fg(color)),
                Cell::from(job_duration_str(j)),
                Cell::from(commit_line),
                Cell::from(started).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Min(30),
        Constraint::Length(12),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " deployment history ({}) · Enter drills into logs · Esc back ",
            dh.jobs.len()
        )))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    state.select(Some(dh.selected));
    f.render_stateful_widget(table, chunks[1], &mut state);
}

/// 2026-08-01 — how many rendered lines each visible row occupies,
/// as an absolute-line-index-of-each-row vector. Line 0 is the
/// column-header row (always emitted), so `row_line_starts[0] = 1`.
/// AppHeader + collapsed Branch = 1 line; expanded Branch with a
/// cached job = 3 lines (main + status/commit + started-at);
/// expanded Branch loading / no-deploys = 2 lines. Both
/// `draw_unified_view` (for highlight + scroll) and `handle_mouse`
/// (for line→row translation) share this — MUST match the draw
/// loop's actual push count.
fn visible_row_line_starts(app: &App, rows: &[crate::app::VisibleRow]) -> Vec<usize> {
    let mut out = Vec::with_capacity(rows.len());
    let mut cursor = 1usize; // account for column-header line 0
    for row in rows {
        out.push(cursor);
        match row {
            crate::app::VisibleRow::AppHeader { .. } => cursor += 1,
            crate::app::VisibleRow::Branch {
                app_id,
                branch_name,
            } => {
                let is_expanded = app
                    .expanded_branches
                    .contains(&(app_id.clone(), branch_name.clone()));
                if !is_expanded {
                    cursor += 1;
                } else {
                    // Expanded branch layout:
                    //   - jobs present: Branch row = 1 line, followed
                    //     by N `Deployment` VisibleRow entries (each
                    //     1 line, accounted for below in their own
                    //     match arm).
                    //   - no jobs (loading / empty / error): Branch
                    //     row = 2 lines (main + placeholder). No
                    //     Deployment rows emitted.
                    let has_jobs = if let TabData::Apps(a) = &app.active().data {
                        a.jobs_by_key
                            .get(&(app_id.clone(), branch_name.clone()))
                            .is_some_and(|js| !js.is_empty())
                    } else {
                        false
                    };
                    cursor += if has_jobs { 1 } else { 2 };
                }
            }
            crate::app::VisibleRow::Deployment { .. } => cursor += 1,
        }
    }
    out
}

/// 2026-08-01 — status glyph + accent color for a deploy job.
/// Used by the branch-row expansion detail block. Kept in sync
/// with the color rules that draw_deployment_history uses.
fn deploy_status_glyph(status: &str) -> (&'static str, Color) {
    match status {
        "SUCCEED" => ("✓", Color::Green),
        "FAILED" => ("✗", Color::Red),
        "CANCELLED" => ("⊘", Color::Red),
        "RUNNING" => ("●", Color::Cyan),
        "PENDING" => ("○", Color::Cyan),
        "PROVISIONING" => ("◐", Color::Cyan),
        _ => ("·", Color::Gray),
    }
}

/// 2026-08-01 — format an ISO-8601 timestamp for the expanded
/// branch detail block's "Started …" line. Falls back to the
/// raw string on parse failure so the user still sees SOMETHING.
fn format_iso_ts(iso: &str) -> String {
    use chrono::{DateTime, Local};
    match DateTime::parse_from_rfc3339(iso) {
        Ok(dt) => dt
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string(),
        Err(_) => iso.to_string(),
    }
}

/// Human-readable duration between start and end times. Amplify
/// timestamps come back as ISO 8601 strings; parse via `chrono`.
fn job_duration_str(j: &crate::amplify::AmplifyJob) -> String {
    use chrono::DateTime;
    let start = j
        .start_time
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    let end = j
        .end_time
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    match (start, end) {
        (Some(s), Some(e)) => {
            let secs = (e - s).num_seconds().max(0);
            format!("{}m {}s", secs / 60, secs % 60)
        }
        _ => "—".to_string(),
    }
}

fn draw_unified_view(f: &mut Frame, area: Rect, app: &App) {
    let TabData::Apps(a) = &app.active().data else {
        return;
    };
    if let Some(err) = &a.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if a.loading && a.items.is_empty() {
        let p = Paragraph::new("loading apps…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if a.items.is_empty() {
        let p = Paragraph::new("(no Amplify apps in this region)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    let hidden = &app.cfg.hidden_app_ids;
    let rows = app.visible_rows();
    let mut lines: Vec<Line> = Vec::new();
    // 2026-08-01 — precomputed line-index-of-each-row (accounts
    // for expanded branches spanning 2-3 lines). Selection
    // highlight + scroll offset use this instead of assuming
    // 1 line per row.
    let row_line_starts = visible_row_line_starts(app, &rows);
    // 2026-07-20 — column headers, matching the mnml-forge-bitbucket
    // pipelines pane. Layout: `<chev+name> <app_id> <platform>
    // <repo>` for app-header rows, and `  <branch> <stage>
    // <current> <last-deploy>` for expanded branch children. One
    // header row summarizes both shapes.
    let header_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    // 2026-07-20 — each header sits above its column's data.
    // Branch rows use fixed widths: 5-col indent + 28-col name +
    // 14-col stage + 28-col current + rest. App-header rows use
    // the same widths (see the row builders below). Headers here
    // reflect those exact positions, padded to the column widths
    // with format! left-align. User asked to bump APP / BRANCH
    // right ~1 cell and the others further to line up with data.
    // 2026-07-20 — dropped ID + REPO columns per user asks. IDs
    // (2026-07-20 first ask) and repo names (this ask) are both
    // reachable via drill-in (Enter → history) or `o` (open on
    // web console), so they don't need to burn tree-column real
    // estate. Column widths still match the branch-row layout
    // (5 indent + 28 name + 14 stage + 28 current + last-deploy),
    // so branches slot cleanly under their headers.
    // 2026-07-20 — column set is now (indent · APP/BRANCH ·
    // STAGE · LAST DEPLOY). Dropped CURRENT/PLATFORM per user
    // ask "also can we hide the current/platform col". Row
    // widths match this header: 5-col app-indent (or 9-col
    // branch-indent) + 28-col name (or 24 for branches) +
    // 14-col stage + rest for last-deploy.
    // 2026-07-20 — every header starts on the first letter of its
    // column's data. Layout:
    //   cols 0-2  " ▶ " chev gutter (app rows) / 6-space indent
    //             (branch rows — see below where we compensate)
    //   cols 3-30 APP / BRANCH  (28 wide)
    //   cols 31-44 STAGE        (14 wide)
    //   cols 45+   LAST DEPLOY
    // 2026-08-01 — LAST DEPLOY split into 4 sub-columns so the
    // extra fields (duration, commit) added to branch rows are
    // labelled. Widths match the branch-row format below:
    //   7-col  #JOB     (e.g. "#12345 ")
    //   13-col STATUS   (PROVISIONING = 12)
    //   11-col DATE     (YYYY-MM-DD)
    //   9-col  DURATION (e.g. "12m34s")
    //   rest   COMMIT
    lines.push(Line::from(vec![
        Span::styled(format!("{:<3}", ""), header_style),
        Span::styled(format!("{:<28}", "APP / BRANCH"), header_style),
        Span::styled(format!("{:<14}", "STAGE"), header_style),
        Span::styled(format!("{:<7}", "#JOB"), header_style),
        Span::styled(format!("{:<13}", "STATUS"), header_style),
        Span::styled(format!("{:<11}", "DATE"), header_style),
        Span::styled(format!("{:<9}", "DURATION"), header_style),
        Span::styled("COMMIT", header_style),
    ]));
    for row in &rows {
        match row {
            crate::app::VisibleRow::AppHeader {
                items_index,
                expanded,
            } => {
                let Some(app_row) = a.items.get(*items_index) else {
                    continue;
                };
                let is_hidden = hidden.iter().any(|h| h == &app_row.app_id);
                let chev = if *expanded { "▼" } else { "▶" };
                // 2026-07-03 — hidden rows in show-hidden mode used
                // DarkGray+DIM which was effectively invisible on
                // dark terminal themes. Switch to Gray (readable)
                // with italic — obviously secondary, but the name
                // stays legible.
                // 2026-07-20 — cyan+bold to match
                // mnml-forge-bitbucket's repo-row color. User asked
                // for uniformity across the two panes.
                let name_style = if is_hidden {
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC)
                } else {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                };
                let name_text = if is_hidden {
                    format!("{}  · hidden (x to unhide)", app_row.name)
                } else {
                    app_row.name.clone()
                };
                // 2026-07-20 — align app-header rows to the same
                // fixed columns as branch rows (see row builder
                // below): 5-col indent + 28-col primary + 14-col
                // secondary + 28-col tertiary + rest. Chevron
                // lives inside the 5-col indent block so the tree
                // gutter and the name column both line up under
                // their respective headers.
                let name_trunc = truncate_to(&name_text, 27);
                // 2026-07-20 — only the app name renders now.
                // STAGE / LAST DEPLOY are branch-only fields;
                // PLATFORM + REPO + ID all live behind Enter (deploy
                // history) or `o` (AWS console).
                // 2026-07-20 — match bitbucket layout: 1-cell
                // pad + chev + space + name (chev at col 2, name
                // at col 4). Branch rows below use a 6-space
                // leading pad → branch text at col 6 for a
                // 2-column offset from the name, matching
                // bitbucket's tree feel.
                let mut spans = vec![
                    Span::styled(format!(" {chev} "), Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{name_trunc:<28}"), name_style),
                ];
                // Loading indicator for expanded apps whose
                // branches haven't landed yet.
                if *expanded
                    && !a.branches_by_app.contains_key(&app_row.app_id)
                    && a.pending_branches.contains_key(&app_row.app_id)
                {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        "loading branches…".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines.push(Line::from(spans));
            }
            crate::app::VisibleRow::Branch {
                app_id,
                branch_name,
            } => {
                // Look up the branch's stage + latest job for
                // inline display.
                let br = a
                    .branches_by_app
                    .get(app_id)
                    .and_then(|brs| brs.iter().find(|b| &b.branch_name == branch_name));
                let stage = br.and_then(|b| b.stage.clone()).unwrap_or_default();
                let stage_style = match stage.as_str() {
                    "PRODUCTION" => Style::default().fg(Color::Green),
                    "BETA" => Style::default().fg(Color::Yellow),
                    "DEVELOPMENT" => Style::default().fg(Color::Cyan),
                    _ => Style::default(),
                };
                let key = (app_id.clone(), branch_name.clone());
                let jobs = a.jobs_by_key.get(&key);
                let in_flight = a
                    .pending_jobs
                    .iter()
                    .any(|(a_id, b_name, _)| a_id == &key.0 && b_name == &key.1);
                let job_err = a.jobs_error_by_key.get(&key);
                let (last_text, last_style) = match jobs {
                    None if job_err.is_some() => {
                        // 2026-08-22 (#1123) — was a fixed
                        // `err (see expand)` filler. Show the
                        // classified reason ("throttled" /
                        // "no access" / "not found" / "err: …")
                        // inline so the user can see at a glance
                        // what's wrong without opening every row.
                        let reason = crate::amplify::short_error_reason(
                            job_err.map(String::as_str).unwrap_or(""),
                        );
                        (reason, Style::default().fg(Color::Red))
                    }
                    None if in_flight => {
                        ("fetching…".to_string(), Style::default().fg(Color::Cyan))
                    }
                    None => ("queued".to_string(), Style::default().fg(Color::DarkGray)),
                    Some(js) if js.is_empty() => (
                        "(no deploys)".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Some(js) => {
                        let in_flight =
                            |s: &str| matches!(s, "RUNNING" | "PENDING" | "PROVISIONING");
                        let last_j = if in_flight(&js[0].status) {
                            js.get(1)
                        } else {
                            Some(&js[0])
                        };
                        match last_j {
                            Some(j) => {
                                let color = match j.status.as_str() {
                                    "SUCCEED" => Color::Green,
                                    "FAILED" | "CANCELLED" => Color::Red,
                                    _ => Color::Gray,
                                };
                                let when = j.end_time.as_deref().unwrap_or("");
                                let short_when = when
                                    .split('T')
                                    .next()
                                    .map(str::to_string)
                                    .unwrap_or_default();
                                // 2026-08-01 — user: "just add duration
                                // and commit to what we already have."
                                // Duration slots between status and
                                // date; commit line goes on the end.
                                // Commit is truncated (30 cells) so
                                // long messages don't push the row
                                // past terminal width; full text lands
                                // in the expand.
                                let duration = job_duration_str(j);
                                let commit_first = j
                                    .commit_message
                                    .as_deref()
                                    .unwrap_or("")
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .to_string();
                                let commit_trunc = truncate_to(&commit_first, 30);
                                // 2026-08-01 — fixed widths so the
                                // #JOB / STATUS / DATE / DURATION /
                                // COMMIT column headers above line
                                // up with the data below.
                                (
                                    format!(
                                        "#{:<6}{:<13}{:<11}{:<9}{}",
                                        j.job_id, j.status, short_when, duration, commit_trunc
                                    ),
                                    Style::default().fg(color),
                                )
                            }
                            None => (
                                "(no deploys)".to_string(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        }
                    }
                };
                // 2026-08-01 — chevron indicator so it's discoverable
                // that Enter/→ expands the row. 4 spaces + chev + space
                // = 6 cols total, matching the previous plain 6-space
                // indent so STAGE still lands at col 31 (under its
                // header).
                let is_expanded = app
                    .expanded_branches
                    .contains(&(app_id.clone(), branch_name.clone()));
                let chev = if is_expanded { "▼" } else { "▶" };
                let branch_trunc = truncate_to(branch_name, 24);
                let stage_trunc = truncate_to(&stage, 13);
                // 2026-08-01 — chevron was `DarkGray` which reads
                // as invisible on the selected-row highlight bg
                // (user: "I can't see the expand collapse arrow
                // for main here"). Bump to `Gray` — still secondary
                // vs the branch name, but stays legible on both
                // the highlight AND the plain bg.
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("{chev} "), Style::default().fg(Color::Gray)),
                    Span::styled(
                        format!("{branch_trunc:<25}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{stage_trunc:<14}"), stage_style),
                    Span::styled(last_text, last_style),
                ]));
                // 2026-08-01 — inline detail block for the expanded
                // branch. Two extra lines below the branch row:
                //   line 1: <glyph> <STATUS>  #<job>  <duration>  <commit>
                //   line 2: Started <iso start time>
                // On jobs-not-loaded we show "(loading…)"; on jobs-
                // loaded-but-empty we show "(no deployments yet)".
                // The `refresh_active` fan-out populates jobs_by_key
                // for every known branch, so the "(loading…)" state
                // is short-lived.
                if is_expanded {
                    // Placeholder line only when there are no jobs
                    // to render as their own Deployment rows below.
                    // Prefer showing any recorded list-jobs error so
                    // "(loading…)" isn't stuck forever on throttle /
                    // permissions.
                    let job_err = a
                        .jobs_error_by_key
                        .get(&(app_id.clone(), branch_name.clone()))
                        .cloned();
                    let no_cached_data = jobs.is_none_or(|v| v.is_empty());
                    if no_cached_data {
                        let (text, color) = if let Some(e) = job_err.as_deref() {
                            let short = e
                                .lines()
                                .next()
                                .unwrap_or(e)
                                .chars()
                                .take(120)
                                .collect::<String>();
                            (format!("error: {short}"), Color::Red)
                        } else if jobs.is_none() {
                            ("(loading…)".to_string(), Color::DarkGray)
                        } else {
                            ("(no deployments yet)".to_string(), Color::DarkGray)
                        };
                        lines.push(Line::from(vec![
                            Span::raw("      "),
                            Span::styled(text, Style::default().fg(color)),
                        ]));
                    }
                }
            }
            crate::app::VisibleRow::Deployment {
                app_id,
                branch_name,
                job_id,
            } => {
                let Some(j) = a
                    .jobs_by_key
                    .get(&(app_id.clone(), branch_name.clone()))
                    .and_then(|js| js.iter().find(|j| &j.job_id == job_id))
                else {
                    // Job vanished between visible_rows() + draw —
                    // emit an empty line so line-count math stays in
                    // sync with visible_row_line_starts.
                    lines.push(Line::from(""));
                    continue;
                };
                let (glyph, color) = deploy_status_glyph(&j.status);
                let duration = job_duration_str(j);
                let commit_line = j
                    .commit_message
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let started = j
                    .start_time
                    .as_deref()
                    .map(format_iso_ts)
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(
                        format!("{glyph} {:<10}", j.status),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("#{:<6}", j.job_id),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{duration:<9}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{started:<20} "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(commit_line),
                ]));
            }
        }
    }
    // Selection highlight via row background only — the ▶/▼
    // chevron on each row is the only arrow the user should see.
    // 2026-08-01 — was `i - HEADER_OFFSET == a.selected` which
    // assumed 1 line per row. With expanded branches spanning
    // multiple lines, use `row_line_starts[a.selected]` to find
    // the highlight target directly. Detail lines below the
    // branch row are intentionally NOT highlighted — the branch
    // row itself is the "selected" line.
    let selected_line_idx: Option<usize> = row_line_starts.get(a.selected).copied();
    let mut styled_lines: Vec<Line> = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if Some(i) == selected_line_idx {
            styled_lines.push(line.style(Style::default().bg(Color::DarkGray)));
        } else {
            styled_lines.push(line);
        }
    }
    // Vertical scroll so the selected row is always in view.
    // 2026-08-01 — was `(a.selected as u16)` which assumed 1
    // line per row. Use the actual line index of the selected
    // row so expanded-branch detail lines above don't push the
    // cursor off-screen.
    let selected_line = selected_line_idx.unwrap_or(a.selected) as u16;
    let scroll = selected_line.saturating_sub(area.height.saturating_sub(4));
    // 2026-07-20 — in-mnml chrome trim. When launched inside a
    // mnml Pty pane (env `MNML_PANE=1`), skip the border block —
    // mnml already draws pane borders and the title is redundant
    // with the tab label. Standalone runs still get the block +
    // "Amplify · N apps" title.
    let inside_mnml = std::env::var_os("MNML_PANE").is_some();
    let title = if a.show_hidden {
        format!(" Amplify · {} apps · show-hidden ", a.items.len())
    } else {
        format!(" Amplify · {} apps ", a.items.len())
    };
    let mut para = Paragraph::new(styled_lines).scroll((scroll, 0));
    if !inside_mnml {
        para = para.block(Block::default().borders(Borders::ALL).title(title));
    }
    f.render_widget(para, area);
}

fn draw_logs_view(f: &mut Frame, area: Rect, app: &App) {
    let Some(lv) = &app.logs_view else { return };
    let mut lines: Vec<Line> = Vec::new();
    // Header — branch + job + commit message. Cyan title,
    // yellow job id, dark-gray body.
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", lv.branch_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("· "),
        Span::styled(
            lv.job_id.as_deref().unwrap_or("?").to_string(),
            Style::default().fg(Color::Yellow),
        ),
    ]));
    if let Some(msg) = &lv.commit_message {
        let first = msg.lines().next().unwrap_or("").to_string();
        lines.push(Line::from(Span::styled(
            format!(" {first}"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    if lv.loading_detail {
        lines.push(Line::from(Span::styled(
            "loading job detail…",
            Style::default().fg(Color::DarkGray),
        )));
    } else if let Some(err) = &lv.error {
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
    } else if lv.steps.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no steps in this job)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (step_name, text) in &lv.steps {
            // Step separator.
            lines.push(Line::from(Span::styled(
                format!("── {step_name} "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            if text.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  loading…",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for line in text.lines() {
                    lines.push(Line::from(line.to_string()));
                }
            }
            lines.push(Line::from(""));
        }
    }
    let title = format!(
        " logs — {} · {} (Esc close · j/k scroll · g/G top/bottom) ",
        lv.branch_name,
        lv.job_id.as_deref().unwrap_or("?")
    );
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((lv.scroll, 0));
    f.render_widget(para, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " ↑↓ move · →← expand/collapse · E/C expand-all/collapse-all · Enter logs · o open on web · L cw-logs · Alt-↑↓ reorder · r refresh ";
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

/// Truncate `s` to `max` chars, replacing the tail with `…` when it
/// overflows. Preserves fixed-column alignment on branch tables
/// where the raw string can be arbitrarily long (e.g. a jira ticket
/// slug like `fix/TE-13442-brink-stack-id-required`).
fn truncate_to(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let head: String = chars.iter().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}
