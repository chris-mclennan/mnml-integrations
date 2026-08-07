//! ratatui rendering + the main event loop. Run from `main.rs` with
//! a fully-initialized `App`.

use crate::app::App;
use crate::keys;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseButton, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs},
};
use std::io::Stdout;
use std::time::{Duration, Instant};

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    // 2026-07-26 — enable mouse capture so tree-tab clicks reach us
    // (tree activation: click a group header to toggle collapse,
    // click a ticket to expand PRs, click a PR to open in browser).
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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

/// 2026-07-26 — pull the fixVersion literal out of a resolved JQL
/// string like `project = TE AND fixVersion = "13.15.0" ORDER BY
/// rank`. Returns the value between the quotes; None when the
/// pattern doesn't appear (raw-JQL tabs, non-fix-version tabs).
fn extract_fix_version(jql: &str) -> Option<String> {
    // Match `fixVersion = "..."` — literal token then equals-sign
    // then double-quoted value. Case-insensitive on `fixVersion`
    // just in case a user hand-wrote `fixversion`.
    let needle = "fixversion";
    let lower = jql.to_ascii_lowercase();
    let start = lower.find(needle)?;
    let after = &jql[start + needle.len()..];
    let eq = after.find('=')?;
    let after_eq = &after[eq + 1..];
    let first_quote = after_eq.find('"')?;
    let rest = &after_eq[first_quote + 1..];
    let end_quote = rest.find('"')?;
    Some(rest[..end_quote].to_string())
}

/// 2026-07-26 — map an absolute screen row → visible-row index in
/// the table body. Accounts for the tab strip (3 rows when shown)
/// + one border row + one header row. Ignores columns — a click
/// anywhere on the row targets that row. Returns None when the
/// click landed above the table body.
fn table_row_at(row: u16, app: &App) -> Option<usize> {
    let show_strip = !app.hide_tab_strip && app.tabs.len() > 1;
    let area_y: u16 = if show_strip { 3 } else { 0 };
    // Body starts 2 rows below area_y: border top + header row.
    let body_top = area_y + 2;
    if row < body_top {
        return None;
    }
    Some((row - body_top) as usize)
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| draw(f, app))?;

        // Auto-refresh on interval.
        if app.cfg.refresh_interval_secs > 0
            && last_refresh.elapsed().as_secs() >= app.cfg.refresh_interval_secs
        {
            app.refresh_active().await;
            last_refresh = Instant::now();
        }

        // Poll for keys with a small timeout so the auto-refresh
        // can fire even when the user isn't typing.
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
                Event::Mouse(m) => match m.kind {
                    // 2026-07-26 — left-click on a row: focus it,
                    // then activate. Activation semantics come from
                    // App::tree_activate_focused: toggle group /
                    // expand ticket (fetches linked PRs on first
                    // expand) / open PR URL. On non-tree tabs, we
                    // just move the cursor to the clicked row.
                    MouseEventKind::Down(MouseButton::Left) => {
                        let Some(row_idx) = table_row_at(m.row, app) else {
                            continue;
                        };
                        let vis = if app.active().tree.is_some() {
                            app.active()
                                .tree_rows(&app.cfg.tabs[app.active_tab], &app.cfg)
                                .map(|r| r.len())
                                .unwrap_or(0)
                        } else {
                            app.visible_indices().len()
                        };
                        if vis == 0 || row_idx >= vis {
                            continue;
                        }
                        if app.active().tree.is_some() {
                            app.active_mut().selected = row_idx;
                            app.tree_activate_focused().await;
                        } else {
                            // Flat-tab click: map visible row →
                            // issue index and focus.
                            let visible = app.visible_indices();
                            if let Some(&idx) = visible.get(row_idx) {
                                app.active_mut().selected = idx;
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => app.move_selection(-3),
                    MouseEventKind::ScrollDown => app.move_selection(3),
                    _ => {}
                },
                Event::Resize(_, _) => { /* terminal handles re-draw */ }
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    // 2026-07-25 — hide the tab strip entirely when the caller
    // passed `--only` OR there's only one tab (the strip becomes
    // visual noise in either case). Layout collapses the top
    // 3-row block so the body gets those rows.
    let show_strip = !app.hide_tab_strip && app.tabs.len() > 1;
    let chunks = if show_strip {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // tab strip
                Constraint::Min(1),    // body (table + optional details)
                Constraint::Length(1), // status line
            ])
            .split(size)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(size)
    };

    let (body_area, status_area) = if show_strip {
        draw_tabs(f, chunks[0], app);
        (chunks[1], chunks[2])
    } else {
        (chunks[0], chunks[1])
    };
    if app.details_visible {
        // Horizontal split: 60% list, 40% detail.
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(body_area);
        draw_table(f, body[0], app);
        draw_details(f, body[1], app);
    } else {
        draw_table(f, body_area, app);
    }
    draw_status(f, status_area, app);
    // Modal overlays last so they sit on top of everything else.
    if app.transition_picker.is_some() {
        draw_transition_picker(f, size, app);
    }
    if app.field_picker.is_some() {
        draw_field_picker(f, size, app);
    }
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let n = t.issues.len();
            let label = if t.last_fetched.is_some() {
                format!("{}.{} ({n})", i + 1, t.name)
            } else {
                format!("{}.{}", i + 1, t.name)
            };
            Line::from(label)
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" tickets "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_table(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if tab.issues.is_empty() && tab.last_fetched.is_some() {
        let p = Paragraph::new("(no issues match this query)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if tab.issues.is_empty() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    // 2026-08-06 — Board tabs render as a 3-column kanban (To Do /
    // In Progress / Done). Fix-version + work tabs stay in the
    // status-grouped tree table below.
    if let Some(tab_cfg) = app.cfg.tabs.get(app.active_tab)
        && matches!(
            tab_cfg.kind,
            Some(crate::config::TabKind::BoardActiveSprint)
                | Some(crate::config::TabKind::BoardBacklog)
        )
    {
        draw_kanban_board(f, area, app);
        return;
    }
    // 2026-07-25 — FixVersionTree tabs render as a grouped tree:
    // status headers with counts, indented tickets, expandable
    // linked-PR sub-rows. Any other tab kind (Work, Boards, or
    // legacy no-kind) falls through to the flat-table path below.
    if tab.tree.is_some()
        && let Some(tab_cfg) = app.cfg.tabs.get(app.active_tab)
    {
        draw_tree_table(f, area, app, tab_cfg);
        return;
    }

    // Split off a 1-row filter strip above the table when there is
    // any filter at all (open or committed). Otherwise the table
    // gets the full body region.
    let (filter_area, table_area) = if app.filter.is_some() {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        (Some(parts[0]), parts[1])
    } else {
        (None, area)
    };
    if let Some(a) = filter_area {
        draw_filter_strip(f, a, app);
    }

    // Per-tab column override — falls back to the family default
    // (key, status, assignee, updated, summary). Resolved on every
    // draw so config reloads (future) would pick up changes.
    let columns: Vec<crate::config::Column> = app
        .cfg
        .tabs
        .get(app.active_tab)
        .and_then(|t| t.columns.clone())
        .unwrap_or_else(crate::config::Column::default_set);

    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(c.header()))
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let visible = app.visible_indices();
    let total = tab.issues.len();
    let rows: Vec<Row> = visible
        .iter()
        .map(|&idx| &tab.issues[idx])
        .map(|i| {
            let cells: Vec<Cell> = columns.iter().map(|c| cell_for_column(i, *c)).collect();
            let mut row = Row::new(cells);
            // Highlight rows whose key is in the bulk-selection set —
            // a magenta tint distinguishes "this is in the operation
            // basket" from the regular cursor's blue/cyan highlight.
            if app.selection.contains(&i.key) {
                row = row.style(
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                );
            }
            row
        })
        .collect();

    let widths: Vec<Constraint> = columns
        .iter()
        .map(|c| match c.width() {
            Some(w) => Constraint::Length(w),
            None => Constraint::Min(20),
        })
        .collect();

    let title = if app.filter.is_some() && visible.len() != total {
        format!(" {} ({}/{}) ", tab.name, visible.len(), total)
    } else {
        format!(" {} ", tab.name)
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    // Translate the raw `issues[]` index in `selected` into the
    // visible-rows index — TableState selects by row position.
    let visible_pos = visible.iter().position(|&i| i == tab.selected);
    let mut state = TableState::default();
    state.select(visible_pos);
    f.render_stateful_widget(table, table_area, &mut state);
}

/// One-row filter strip above the table. Three visual states:
///   editing       → `/<buffer>│`  (cursor block, cyan)
///   committed     → `filter: <buffer>   Esc clears`  (dimmed)
///   no filter     → not drawn (the caller skips when filter is None)
/// 2026-07-25 — Fix Versions grouped tree renderer.
///
/// Layout: one line per `VisibleRow` variant, styled distinctly so
/// the hierarchy reads at a glance:
///   ▼ Testing (4)                        (Cyan, BOLD — group header)
///     TE-14337  Claude  2026-07-24  fix Reporting Insights ▶
///     TE-14200  Chris   2026-07-23  fix …   ★                 ← bumped
///   ▶ In PR Review (7)                    (Cyan, BOLD, collapsed)
///   ▼ Done (12)
///
/// When a ticket is expanded:
///     ▼ TE-14337  Claude  …  fix Reporting Insights
///          → MERGED  #2023  main  approved by Chris     (LinkedPr)
///          → OPEN    #2024  develop                     (LinkedPr)
///
/// Cursor: `tab.selected` is a plain index into the row list. Up/
/// Down move by 1 through it; every row type is cursor-visitable
/// (headers, tickets, PR rows). Enter / Space / click actions
/// route by variant — implemented in Phase 4.
/// 2026-08-06 — Kanban view for board tabs. Three vertical columns
/// (To Do / In Progress / Done). Each column renders a Paragraph of
/// its tickets (key + summary), scrollable via the shared cursor.
/// Assignee shows on line 2 of each card if present.
///
/// Column bucketing: exact match on status name first (case-insensitive
/// against a stable synonym table); anything unrecognized falls into
/// the "In Progress" middle column so it's visible rather than hidden.
///
/// Cursor: the shared `tab.selected` still points at an issue index;
/// the column that contains it gets the highlight border. Left/Right
/// on a keyboard tick moves the cursor across columns (v1 uses the
/// existing MoveSelection actions — no per-column cursor yet).
fn draw_kanban_board(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    // 2026-08-06 — added Testing column (In PR Review + Testing + QA
    // statuses) between In Progress and Done. Tattle's status_order
    // default is "Testing, In PR Review, In Progress, To Do, Done" so
    // "in-flight-but-not-live" is a real bucket worth its own column.
    // Also added `team` config filter: `[[tabs]] team = "web"` narrows
    // to issues whose component name OR label contains the string
    // (case-insensitive substring — flexible for Tattle's
    // `component=web-team` and `label=team:web` conventions).
    let team_filter: Option<String> = app
        .cfg
        .tabs
        .get(app.active_tab)
        .and_then(|t| t.team.clone())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_ascii_lowercase());
    let team_matches = |issue: &crate::jira::Issue| -> bool {
        let Some(needle) = &team_filter else {
            return true;
        };
        let comp_hit = issue
            .fields
            .components
            .iter()
            .any(|c| c.name.to_ascii_lowercase().contains(needle));
        let label_hit = issue
            .fields
            .labels
            .iter()
            .any(|l| l.to_ascii_lowercase().contains(needle));
        // 2026-08-07 — also probe the "Tattle Team" custom field
        // (customfield_10056) which is where Tattle keeps HeliOS /
        // Atlas / etc. Value shape is `{"value":"HeliOS", ...}`.
        let custom_hit = crate::app::team_value_of(issue)
            .map(|v| v.to_ascii_lowercase().contains(needle))
            .unwrap_or(false);
        comp_hit || label_hit || custom_hit
    };
    if tab.issues.is_empty() {
        let p = Paragraph::new("(no issues in this sprint)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    // Bucket every filtered issue into one of four columns.
    #[derive(Copy, Clone)]
    enum Col {
        Todo,
        InProgress,
        Testing,
        Done,
    }
    fn bucket_of(status: Option<&str>) -> Col {
        let s = status.unwrap_or("").to_ascii_lowercase();
        match s.as_str() {
            "to do" | "backlog" | "open" | "reopened" | "selected for development" => Col::Todo,
            "done" | "closed" | "resolved" | "released" => Col::Done,
            "testing" | "in pr review" | "in review" | "qa" | "ready for qa" | "code review" => {
                Col::Testing
            }
            _ => Col::InProgress,
        }
    }
    let mut todo: Vec<usize> = Vec::new();
    let mut in_prog: Vec<usize> = Vec::new();
    let mut testing: Vec<usize> = Vec::new();
    let mut done: Vec<usize> = Vec::new();
    for (i, issue) in tab.issues.iter().enumerate() {
        if !team_matches(issue) {
            continue;
        }
        let status = issue.fields.status.as_ref().map(|s| s.name.as_str());
        match bucket_of(status) {
            Col::Todo => todo.push(i),
            Col::InProgress => in_prog.push(i),
            Col::Testing => testing.push(i),
            Col::Done => done.push(i),
        }
    }
    let selected_col = if todo.contains(&tab.selected) {
        Col::Todo
    } else if testing.contains(&tab.selected) {
        Col::Testing
    } else if done.contains(&tab.selected) {
        Col::Done
    } else {
        Col::InProgress
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
        ])
        .split(area);
    let title_todo = format!(" To Do ({}) ", todo.len());
    let title_prog = format!(" In Progress ({}) ", in_prog.len());
    let title_testing = format!(" Testing ({}) ", testing.len());
    let title_done = format!(" Done ({}) ", done.len());
    draw_kanban_column(
        f,
        cols[0],
        &title_todo,
        &todo,
        tab,
        app,
        matches!(selected_col, Col::Todo),
    );
    draw_kanban_column(
        f,
        cols[1],
        &title_prog,
        &in_prog,
        tab,
        app,
        matches!(selected_col, Col::InProgress),
    );
    draw_kanban_column(
        f,
        cols[2],
        &title_testing,
        &testing,
        tab,
        app,
        matches!(selected_col, Col::Testing),
    );
    draw_kanban_column(
        f,
        cols[3],
        &title_done,
        &done,
        tab,
        app,
        matches!(selected_col, Col::Done),
    );
}

/// One kanban column. Highlighted border when it contains the cursor.
fn draw_kanban_column(
    f: &mut Frame,
    area: Rect,
    title: &str,
    issue_indices: &[usize],
    tab: &crate::app::TabState,
    app: &App,
    is_active: bool,
) {
    use ratatui::text::{Line, Span};
    let border_color = if is_active {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(if is_active { Color::Cyan } else { Color::White })
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for &i in issue_indices {
        let issue = &tab.issues[i];
        let is_focused = i == tab.selected;
        let bulk_selected = app.selection.contains(&issue.key);
        let key_color = if bulk_selected {
            Color::Magenta
        } else if is_focused {
            Color::Cyan
        } else {
            Color::White
        };
        let key_style = if is_focused {
            Style::default()
                .fg(key_color)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
                .fg(key_color)
                .add_modifier(Modifier::BOLD)
        };
        let summary = issue.fields.summary.as_str();
        // Card = 2 lines: KEY + wrapped summary
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", issue.key), key_style),
        ]));
        // Wrap summary to inner width - 2 for padding.
        let wrap_w = (inner.width as usize).saturating_sub(2).max(10);
        let s: String = summary.chars().take(wrap_w * 2).collect();
        let mut chunk = String::new();
        for word in s.split_whitespace() {
            if chunk.len() + word.len() + 1 > wrap_w {
                lines.push(Line::from(Span::styled(
                    format!("  {chunk}"),
                    Style::default().fg(Color::Gray),
                )));
                chunk.clear();
            }
            if !chunk.is_empty() {
                chunk.push(' ');
            }
            chunk.push_str(word);
        }
        if !chunk.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {chunk}"),
                Style::default().fg(Color::Gray),
            )));
        }
        // Assignee line (dim).
        if let Some(a) = &issue.fields.assignee {
            lines.push(Line::from(Span::styled(
                format!("  · {}", a.display_name),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        // Blank separator line between cards.
        lines.push(Line::from(""));
    }
    let text = ratatui::text::Text::from(lines);
    f.render_widget(Paragraph::new(text).wrap(ratatui::widgets::Wrap { trim: false }), inner);
}

fn draw_tree_table(f: &mut Frame, area: Rect, app: &App, tab_cfg: &crate::config::Tab) {
    let tab = app.active();
    let rows = tab.tree_rows(tab_cfg, &app.cfg).unwrap_or_default();
    if rows.is_empty() {
        let p = Paragraph::new("(no issues)").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }

    // Header row — matches the flat-table headers for visual parity.
    let header = Row::new(vec![
        Cell::from("  KEY"),
        Cell::from("STATUS"),
        Cell::from("ASSIGNEE"),
        Cell::from("UPDATED"),
        Cell::from("SUMMARY"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let table_rows: Vec<Row> = rows.iter().map(|vr| tree_row_for(vr, tab, app)).collect();

    let widths = [
        Constraint::Length(18),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Min(20),
    ];

    // 2026-07-26 — surface the resolved fixVersion in the title so
    // it's obvious which version the tab landed on ("Current Release
    // · 13.15.0 · 98 tickets" beats "Current Release · 29 rows"
    // where the number of rows is meaningless and the version is
    // hidden). Extract the version name from the tab's JQL
    // (`fixVersion = "13.15.0"` → `13.15.0`); falls back to just
    // the tab name if the pattern doesn't match.
    let fixv = extract_fix_version(&tab.jql);
    let ticket_count = rows
        .iter()
        .filter(|r| matches!(r, crate::tree::VisibleRow::Ticket { .. }))
        .count();
    let title = if let Some(v) = fixv {
        format!(
            " {} · {} · {} ticket{} ",
            tab.name,
            v,
            ticket_count,
            if ticket_count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            " {} · {} ticket{} ",
            tab.name,
            ticket_count,
            if ticket_count == 1 { "" } else { "s" }
        )
    };
    let table = Table::new(table_rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut state = TableState::default();
    state.select(Some(tab.selected.min(rows.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
}

/// Build one ratatui Row for a single `VisibleRow`. Styling reflects
/// the row type — headers are BOLD cyan, tickets get the standard
/// column cells, PR sub-rows are indented and dim-styled.
fn tree_row_for<'a>(
    vr: &crate::tree::VisibleRow,
    tab: &'a crate::app::TabState,
    app: &'a App,
) -> Row<'a> {
    use crate::tree::VisibleRow;
    match vr {
        VisibleRow::GroupHeader {
            status,
            count,
            expanded,
        } => {
            let arrow = if *expanded { "▼" } else { "▶" };
            let label = format!(" {arrow} {status} ({count})");
            let style = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);
            Row::new(vec![
                Cell::from(label).style(style),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ])
        }
        VisibleRow::Ticket {
            issue_idx,
            was_bumped,
            ..
        } => {
            let issue = &tab.issues[*issue_idx];
            let expanded = tab
                .tree
                .as_ref()
                .is_some_and(|t| t.expanded_tickets.contains(&issue.key));
            let arrow = if expanded { "▼" } else { "▶" };
            let bump_mark = if *was_bumped { " ★" } else { "" };
            let key_cell = format!("    {arrow} {}{bump_mark}", issue.key);
            let status = issue
                .fields
                .status
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("");
            let assignee = issue
                .fields
                .assignee
                .as_ref()
                .map(|u| u.display_name.as_str())
                .unwrap_or("—");
            let updated = issue
                .fields
                .updated
                .as_deref()
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            let selected_style = if app.selection.contains(&issue.key) {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            // 2026-07-25 — trailing action buttons (bracketed
            // words) after the summary. 2026-07-26 — each button
            // rendered as a styled Span with its own color (green
            // Implement / red Fix / yellow Triage) so they read
            // as actionable chips rather than plain text.
            let buttons = crate::dispatch::buttons_for_ticket(issue);
            let summary_cell = if buttons.is_empty() {
                Cell::from(issue.fields.summary.clone())
            } else {
                use ratatui::text::{Line, Span};
                let mut spans: Vec<Span> =
                    vec![Span::raw(issue.fields.summary.clone()), Span::raw("   ")];
                for (i, b) in buttons.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw(" "));
                    }
                    let color = match b.color_slot() {
                        "green" => Color::Green,
                        "red" => Color::Red,
                        "yellow" => Color::Yellow,
                        _ => Color::Cyan,
                    };
                    spans.push(Span::styled(
                        b.label().to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ));
                }
                Cell::from(Line::from(spans))
            };
            Row::new(vec![
                Cell::from(key_cell).style(Style::default().fg(Color::Yellow)),
                Cell::from(status.to_string()),
                Cell::from(assignee.to_string()),
                Cell::from(updated),
                summary_cell,
            ])
            .style(selected_style)
        }
        VisibleRow::LinkedPr { issue_idx, pr_idx } => {
            let issue = &tab.issues[*issue_idx];
            // Safe: splice_ticket_sub_rows only emits LinkedPr when
            // the cache has that index. Defensive fallback = blank
            // row so we don't panic if state drifts.
            let pr = tab
                .tree
                .as_ref()
                .and_then(|t| t.pr_cache.get(&issue.key))
                .and_then(|prs| prs.get(*pr_idx));
            let Some(pr) = pr else {
                return Row::new(vec![Cell::from("        (missing pr)")]);
            };
            let dest = if pr.destination.branch.is_empty() {
                "?".to_string()
            } else {
                pr.destination.branch.clone()
            };
            let approvals = pr.reviewers.iter().filter(|r| r.approved).count();
            let approval_hint = if approvals > 0 {
                format!(" ({approvals}✓)")
            } else {
                String::new()
            };
            // Chevron on merged PRs signals the pipeline drill-down
            // is available; open/declined PRs are terminal rows
            // (no chevron so they don't look expandable). 2026-07-30.
            let is_merged = pr.status.eq_ignore_ascii_case("MERGED");
            let is_expanded = tab
                .tree
                .as_ref()
                .is_some_and(|t| t.expanded_prs.contains(&(issue.key.clone(), pr.id.clone())));
            let chevron = if is_merged {
                if is_expanded { "▼ " } else { "▶ " }
            } else {
                "  "
            };
            let label = format!(
                "        {chevron}{status:<7} {id}  {dest}{approval_hint}",
                status = pr.status,
                id = pr.id,
                dest = dest,
                approval_hint = approval_hint,
            );
            let status_color = match pr.status.to_ascii_uppercase().as_str() {
                "MERGED" => Color::Magenta,
                "OPEN" | "DRAFT" | "IN_REVIEW" => Color::Green,
                "DECLINED" | "SUPERSEDED" => Color::DarkGray,
                _ => Color::Gray,
            };
            // 2026-07-25 — [ Review ] button appears on OPEN /
            // DRAFT PRs (the states where a review is actionable).
            // 2026-07-26 — colored (cyan bold) so it reads as a
            // clickable chip, matching the ticket-row action
            // buttons above.
            let title_cell = if pr.is_open() {
                use ratatui::text::{Line, Span};
                Cell::from(Line::from(vec![
                    Span::styled(pr.name.clone(), Style::default().fg(Color::DarkGray)),
                    Span::raw("   "),
                    Span::styled(
                        "[ Review ]",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
            } else {
                Cell::from(pr.name.clone()).style(Style::default().fg(Color::DarkGray))
            };
            Row::new(vec![
                Cell::from(label).style(Style::default().fg(status_color)),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                title_cell,
            ])
        }
        VisibleRow::PrLoading { .. } => Row::new(vec![
            Cell::from("        → fetching linked PRs…")
                .style(Style::default().fg(Color::DarkGray)),
        ]),
        VisibleRow::PrEmpty { .. } => Row::new(vec![
            Cell::from("        → no linked PRs").style(Style::default().fg(Color::DarkGray)),
        ]),
        VisibleRow::PrPipelineLoading { .. } => Row::new(vec![
            Cell::from("            → fetching pipeline…")
                .style(Style::default().fg(Color::DarkGray)),
        ]),
        VisibleRow::PrPipelineEmpty { .. } => Row::new(vec![
            Cell::from("            → no pipeline ran on merge commit")
                .style(Style::default().fg(Color::DarkGray)),
        ]),
        VisibleRow::PrPipelineError {
            issue_idx, pr_idx, ..
        } => {
            let issue = &tab.issues[*issue_idx];
            // Look up the recorded error message so the UI shows
            // WHY the pipeline lookup failed (not just "failed").
            // Fallback text when the map lost the entry — shouldn't
            // happen given the splice logic, but be defensive.
            let msg = tab
                .tree
                .as_ref()
                .and_then(|t| {
                    t.pr_cache.get(&issue.key).and_then(|prs| {
                        prs.get(*pr_idx).and_then(|pr| {
                            t.pipeline_errors
                                .get(&(issue.key.clone(), pr.id.clone()))
                                .cloned()
                        })
                    })
                })
                .unwrap_or_else(|| "unknown error".to_string());
            Row::new(vec![
                Cell::from(format!("            → pipeline lookup failed: {msg}"))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        }
        VisibleRow::PrPipeline {
            issue_idx,
            pr_idx,
            pipeline_idx,
        } => {
            let issue = &tab.issues[*issue_idx];
            // Resolve the pipeline via (issue.key, pr.id) →
            // pipeline_cache. Defensive fallback = blank row.
            let pipeline = tab.tree.as_ref().and_then(|t| {
                t.pr_cache.get(&issue.key).and_then(|prs| {
                    prs.get(*pr_idx).and_then(|pr| {
                        t.pipeline_cache
                            .get(&(issue.key.clone(), pr.id.clone()))
                            .and_then(|pipelines| pipelines.get(*pipeline_idx))
                    })
                })
            });
            let Some(pipeline) = pipeline else {
                return Row::new(vec![
                    Cell::from("            → (missing pipeline)")
                        .style(Style::default().fg(Color::DarkGray)),
                ]);
            };
            let (glyph, color) = pipeline_glyph(pipeline.state_label());
            // Format the row Bitbucket-sibling-style — one span-heavy
            // Line so glyph, state, build, branch, date, duration
            // each get their own color / weight instead of one flat
            // string. Matches the visual language in
            // `render_pr_expand_title_cell` on the sibling.
            let state = pipeline.state_label().to_string();
            let build = pipeline.build_number;
            let branch = pipeline.branch_label().to_string();
            let when = pipeline.created_date();
            let dur = pipeline.duration_label();
            let line = Line::from(vec![
                Span::raw("            "),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{state:<11} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("#{build}"), Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled(format!("on {branch}"), Style::default().fg(Color::Gray)),
                Span::raw("  "),
                Span::styled(
                    format!("{when}  {dur}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            Row::new(vec![Cell::from(line)])
        }
    }
}

/// Map a Bitbucket pipeline state label to its display glyph + color.
/// Ported from `mnml-forge-bitbucket/src/ui.rs::pipeline_glyph` so
/// the visual language matches across siblings. 2026-07-30.
fn pipeline_glyph(label: &str) -> (&'static str, Color) {
    match label {
        "SUCCESSFUL" => ("✓", Color::Green),
        "FAILED" | "ERROR" => ("✗", Color::Red),
        "IN_PROGRESS" | "PENDING" => ("⏵", Color::Yellow),
        "STOPPED" | "HALTED" => ("⊘", Color::DarkGray),
        _ => ("?", Color::DarkGray),
    }
}

fn draw_filter_strip(f: &mut Frame, area: Rect, app: &App) {
    let Some(filter) = app.filter.as_ref() else {
        return;
    };
    let line = if filter.editing {
        // Render `/buffer│` — the `│` is the cursor block. Truncate
        // the buffer so the cursor stays on-screen on a narrow strip.
        let avail = area.width.saturating_sub(2) as usize;
        let chars: Vec<char> = filter.buffer.chars().collect();
        let cursor = filter.cursor.min(chars.len());
        let start = if cursor >= avail {
            cursor - avail + 1
        } else {
            0
        };
        let end = (start + avail).min(chars.len());
        let head: String = chars[start..cursor].iter().collect();
        let tail: String = chars[cursor..end].iter().collect();
        Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::styled(head, Style::default().fg(Color::White)),
            Span::styled("│", Style::default().fg(Color::Cyan)),
            Span::styled(
                tail,
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "filter: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(filter.buffer.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(
                "   Esc to clear · / to refine",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = if app.comment_editor.is_some() {
        " typing comment · Ctrl+S send · Esc cancel · Enter newline "
    } else if app.field_picker.is_some() {
        " type to filter · ↑↓ move · Enter commit · Esc cancel "
    } else if app.transition_picker.is_some() {
        " 1-9 jump · ↑↓/jk move · Enter commit · Esc cancel "
    } else if app.filter.as_ref().map(|f| f.editing).unwrap_or(false) {
        " type to filter · Enter commit · Esc cancel "
    } else if app.details_visible {
        " d close · c comment · a assignee · f version · t move · w watch · Space pick · / filter · q "
    } else if !app.selection.is_empty() {
        " ↑↓ · Space pick · t move · a assignee · f version · Esc clear · / filter · q "
    } else {
        // Board tabs get a kanban-specific hint that surfaces the
        // team-filter key (T) added 2026-08-06. Other tabs keep the
        // classic flat/tree table hint.
        let is_board = app
            .cfg
            .tabs
            .get(app.active_tab)
            .and_then(|t| t.kind)
            .is_some_and(|k| {
                matches!(
                    k,
                    crate::config::TabKind::BoardActiveSprint
                        | crate::config::TabKind::BoardBacklog
                )
            });
        if is_board {
            " ↑↓ · Space pick · t move · a assignee · f version · T team · w watch · d details · q "
        } else {
            " 1-9 · ↑↓ · / filter · Space pick · t move · a assignee · f version · w watch · d details · q "
        }
    };
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

/// Right-half pane: the focused ticket's summary + status / assignee /
/// fixVersion header, then description, then the last N comments.
/// Content is plain text — ADF formatting is stripped to a
/// single-style paragraph (see `jira::adf_to_text`).
fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    // Reserve a bottom strip for the comment editor when open.
    let (detail_area, editor_area) = if app.comment_editor.is_some() {
        let editor_h = 8u16.min(area.height.saturating_sub(4));
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(editor_h)])
            .split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };

    let tab = app.active();
    let Some(issue) = tab.issues.get(tab.selected) else {
        let p = Paragraph::new("(no ticket focused)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" detail "));
        f.render_widget(p, detail_area);
        if let Some(ea) = editor_area {
            draw_comment_editor(f, ea, app);
        }
        return;
    };
    let key = &issue.key;
    let summary = &issue.fields.summary;
    let status = issue
        .fields
        .status
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "?".to_string());
    let assignee = issue
        .fields
        .assignee
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| "—".to_string());
    let reporter = issue
        .fields
        .reporter
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| "—".to_string());
    let priority = issue
        .fields
        .priority
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let issuetype = issue
        .fields
        .issuetype
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let fix = if issue.fields.fix_versions.is_empty() {
        "—".to_string()
    } else {
        issue
            .fields
            .fix_versions
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                key.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(summary.clone(), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        meta_line("type", &issuetype),
        meta_line("status", &status),
        meta_line("priority", &priority),
        meta_line("assignee", &assignee),
        meta_line("reporter", &reporter),
        meta_line("fixVersion", &fix),
        Line::from(""),
    ];

    // Watcher chip — surfaces alongside the meta lines once the
    // detail is loaded. `★` = watching, `☆` = not.
    if let Some(detail) = app.focused_detail() {
        let glyph = if detail.watching { "★" } else { "☆" };
        let label = if detail.watching {
            format!("watching ({} total)", detail.watch_count)
        } else if detail.watch_count == 0 {
            "no watchers".to_string()
        } else {
            format!("{} watcher(s)", detail.watch_count)
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  watcher: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                format!("{glyph} "),
                if detail.watching {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(label),
        ]));
        lines.push(Line::from(""));
    }

    // Body — description + comments, lazy-loaded.
    if let Some(detail) = app.focused_detail() {
        lines.push(section_header("description"));
        match detail.description.as_deref() {
            Some(d) if !d.trim().is_empty() => {
                for raw in d.lines() {
                    lines.push(Line::from(raw.to_string()));
                }
            }
            _ => lines.push(Line::from(Span::styled(
                "(no description)",
                Style::default().fg(Color::DarkGray),
            ))),
        }
        lines.push(Line::from(""));
        lines.push(section_header(&format!(
            "comments ({})",
            detail.comments.len()
        )));
        if detail.comments.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no comments)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // Show the most-recent N — Jira returns comments
            // chronologically (oldest first), so reverse + take.
            let take = 10.min(detail.comments.len());
            for c in detail.comments.iter().rev().take(take) {
                let author = c.author.as_deref().unwrap_or("?");
                let when = c
                    .created
                    .as_deref()
                    .and_then(|s| s.split('T').next())
                    .unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(author.to_string(), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("  {when}"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                for raw in c.body.lines() {
                    lines.push(Line::from(format!("  {raw}")));
                }
                lines.push(Line::from(""));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "loading detail…",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let scroll = app.details_scroll;
    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" detail "))
        .scroll((scroll, 0));
    f.render_widget(p, detail_area);
    if let Some(ea) = editor_area {
        draw_comment_editor(f, ea, app);
    }
}

/// Inline comment editor docked at the bottom of the detail panel.
/// Shows the buffer text, a cursor block, an error line if posting
/// failed, and a hint row.
fn draw_comment_editor(f: &mut Frame, area: Rect, app: &App) {
    let Some(editor) = app.comment_editor.as_ref() else {
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    // Render the buffer line by line. The cursor block sits at the
    // (row, col) corresponding to `editor.cursor` chars into the buffer.
    let chars: Vec<char> = editor.buffer.chars().collect();
    let cursor = editor.cursor.min(chars.len());
    let mut row = 0usize;
    let mut col = 0usize;
    let mut row_buf = String::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if i == cursor {
            cursor_row = row;
            cursor_col = col;
        }
        if c == '\n' {
            lines.push(Line::from(row_buf.clone()));
            row_buf.clear();
            row += 1;
            col = 0;
        } else {
            row_buf.push(c);
            col += 1;
        }
    }
    if cursor == chars.len() {
        cursor_row = row;
        cursor_col = col;
    }
    // Add the trailing line (with the cursor block appended if it's
    // at end-of-row).
    if cursor_row == row {
        row_buf.insert(cursor_col, '│');
        lines.push(Line::from(row_buf));
    } else {
        lines.push(Line::from(row_buf));
    }
    // If cursor is on an earlier row, re-render that row with the
    // cursor block injected. (We've already pushed it; replace.)
    if cursor_row < row && cursor_row < lines.len() {
        let mut s: String = chars
            .iter()
            .take(cursor)
            .rev()
            .take_while(|&&c| c != '\n')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let tail: String = chars
            .iter()
            .skip(cursor)
            .take_while(|&&c| c != '\n')
            .collect();
        s.push('│');
        s.push_str(&tail);
        lines[cursor_row] = Line::from(s);
    }
    if let Some(err) = editor.error.as_ref() {
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    let hint = if editor.posting {
        "posting…"
    } else if editor.buffer.trim().is_empty() {
        "type a comment · Esc cancel"
    } else {
        "Ctrl+S send · Esc cancel · Enter newline"
    };
    let title = format!(" comment on {} ", editor.key);
    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(Line::from(Span::styled(
                hint,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))),
    );
    f.render_widget(p, area);
}

/// Modal overlay listing the focused ticket's available workflow
/// transitions. Centered ~50% × ~50% in the screen, opaque (Clear
/// widget below) so the table underneath doesn't bleed through.
fn draw_transition_picker(f: &mut Frame, screen: Rect, app: &App) {
    let Some(picker) = app.transition_picker.as_ref() else {
        return;
    };
    // Center a 60-cell × 14-row box (clamped to screen) — wide enough
    // for "Start review → In Review", short enough to feel modal.
    let w = 60.min(screen.width.saturating_sub(4));
    let h = 14.min(screen.height.saturating_sub(4));
    let x = (screen.width.saturating_sub(w)) / 2;
    let y = (screen.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);

    let title = if app.selection.is_empty() {
        format!(" transition {} ", picker.key)
    } else {
        format!(" transition × {} ticket(s) ", app.selection.len())
    };
    let body: Vec<Line> = match picker.transitions.as_ref() {
        None => vec![Line::from(Span::styled(
            "  loading…",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(list) if list.is_empty() => {
            let msg = if let Some(err) = picker.error.as_ref() {
                format!("  error: {err}")
            } else {
                "  (no transitions available — terminal state or no permission)".to_string()
            };
            vec![
                Line::from(Span::styled(msg, Style::default().fg(Color::Red))),
                Line::from(""),
                Line::from(Span::styled(
                    "  Esc to close",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )),
            ]
        }
        Some(list) => {
            let mut lines: Vec<Line> = list
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let arrow = match t.to_name.as_deref() {
                        Some(dest) => format!("  {arrow} {dest}", arrow = "→"),
                        None => String::new(),
                    };
                    let prefix = if i == picker.selected { "▸ " } else { "  " };
                    // Number-key hint for the first 9 (1-9 jumps).
                    let num = if i < 9 {
                        format!("{}. ", i + 1)
                    } else {
                        "   ".to_string()
                    };
                    let style = if i == picker.selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    Line::from(vec![Span::styled(
                        format!("{prefix}{num}{name}{arrow}", name = t.name),
                        style,
                    )])
                })
                .collect();
            if let Some(err) = picker.error.as_ref() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  error: {err}"),
                    Style::default().fg(Color::Red),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  1-9 jump · ↑↓/jk move · Enter commit · Esc cancel",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            lines
        }
    };

    // Clear the cells underneath so the table doesn't bleed through.
    f.render_widget(Clear, area);
    let p = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black))
            .title(title),
    );
    f.render_widget(p, area);
}

/// Modal overlay listing assignable users (`a`) or fixVersions (`f`).
/// Type-to-filter editor at the top, filtered list below, hint row at
/// the bottom. Centered on the screen, opaque.
fn draw_field_picker(f: &mut Frame, screen: Rect, app: &App) {
    let Some(picker) = app.field_picker.as_ref() else {
        return;
    };
    let w = 60.min(screen.width.saturating_sub(4));
    let h = 18.min(screen.height.saturating_sub(4));
    let x = (screen.width.saturating_sub(w)) / 2;
    let y = (screen.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);

    let field_label = match picker.kind {
        crate::app::FieldKind::Assignee => "assignee",
        crate::app::FieldKind::FixVersion => "fixVersion",
        crate::app::FieldKind::Team => "team",
        crate::app::FieldKind::TabFixVersion => "tab fixVersion",
    };
    let target_count = if app.selection.is_empty() {
        1
    } else {
        app.selection.len()
    };
    let title = if matches!(picker.kind, crate::app::FieldKind::Team) {
        " filter kanban by team ".to_string()
    } else if matches!(picker.kind, crate::app::FieldKind::TabFixVersion) {
        " switch tab view to fixVersion ".to_string()
    } else if target_count == 1 {
        format!(" set {field_label} ")
    } else {
        format!(" set {field_label} × {target_count} ")
    };

    let mut body: Vec<Line> = Vec::new();
    // Filter line — `/<buffer>│`.
    let chars: Vec<char> = picker.filter.chars().collect();
    let cursor = picker.cursor.min(chars.len());
    let head: String = chars[..cursor].iter().collect();
    let tail: String = chars[cursor..].iter().collect();
    body.push(Line::from(vec![
        Span::styled("  /", Style::default().fg(Color::Cyan)),
        Span::styled(head, Style::default().fg(Color::White)),
        Span::styled("│", Style::default().fg(Color::Cyan)),
        Span::styled(
            tail,
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
        ),
    ]));
    body.push(Line::from(""));

    if !picker.loaded {
        body.push(Line::from(Span::styled(
            "  loading…",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible = picker.visible_indices();
        if visible.is_empty() {
            body.push(Line::from(Span::styled(
                "  (no matches)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // Window the visible items around the selection — paint
            // up to (area_rows - 5) rows so the hint + filter fit.
            let row_cap = h.saturating_sub(5) as usize;
            let sel_pos = visible
                .iter()
                .position(|&i| i == picker.selected)
                .unwrap_or(0);
            let start = sel_pos.saturating_sub(row_cap / 2);
            let end = (start + row_cap).min(visible.len());
            for &idx in &visible[start..end] {
                let (_, label) = &picker.items[idx];
                let style = if idx == picker.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if idx == picker.selected {
                    "  ▸ "
                } else {
                    "    "
                };
                body.push(Line::from(Span::styled(format!("{prefix}{label}"), style)));
            }
        }
    }

    if let Some(err) = picker.error.as_ref() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            format!("  error: {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "  type to filter · ↑↓ move · Enter commit · Esc cancel",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )));

    f.render_widget(Clear, area);
    let p = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black))
            .title(title),
    );
    f.render_widget(p, area);
}

fn meta_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:>10}: "),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn section_header(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {label} "),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

/// `2026-01-15T12:34:56.789+0000` → `2026-01-15`.
fn format_updated(s: &str) -> String {
    s.split('T').next().unwrap_or(s).to_string()
}

/// Build a single styled table cell for `issue` × `column`. Handles
/// the per-column missing-data fallback (`—`) and the per-column
/// color theme (yellow KEY, status-themed STATUS, plain for others).
fn cell_for_column(issue: &crate::jira::Issue, column: crate::config::Column) -> Cell<'static> {
    use crate::config::Column;
    let f = &issue.fields;
    match column {
        Column::Key => Cell::from(issue.key.clone()).style(Style::default().fg(Color::Yellow)),
        Column::Status => {
            let s = f.status.as_ref().map(|x| x.name.as_str()).unwrap_or("?");
            Cell::from(s.to_string()).style(status_color(s))
        }
        Column::Assignee => {
            let s = f
                .assignee
                .as_ref()
                .map(|x| x.display_name.as_str())
                .unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Reporter => {
            let s = f
                .reporter
                .as_ref()
                .map(|x| x.display_name.as_str())
                .unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Priority => {
            let s = f.priority.as_ref().map(|x| x.name.as_str()).unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Type => {
            let s = f.issuetype.as_ref().map(|x| x.name.as_str()).unwrap_or("—");
            Cell::from(s.to_string())
        }
        Column::Updated => {
            let s = f
                .updated
                .as_deref()
                .map(format_updated)
                .unwrap_or_else(|| "—".to_string());
            Cell::from(s)
        }
        Column::FixVersion => {
            let s = if f.fix_versions.is_empty() {
                "—".to_string()
            } else {
                f.fix_versions
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            Cell::from(s)
        }
        Column::Summary => Cell::from(f.summary.clone()),
    }
}

fn status_color(name: &str) -> Style {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "done" | "closed" | "resolved" => Style::default().fg(Color::Green),
        "in progress" | "in review" | "in development" => Style::default().fg(Color::Cyan),
        "testing" | "qa" => Style::default().fg(Color::Magenta),
        "to do" | "open" | "backlog" => Style::default().fg(Color::White),
        "blocked" | "blocker" => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::Gray),
    }
}
