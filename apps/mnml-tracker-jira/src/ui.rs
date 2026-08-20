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
        // 2026-08-07 — pre-draw hook: fetch friendly board names for
        // the active kanban tab so the toolbar chip shows
        // `[Board: HeliOS]` on the very first frame instead of
        // `[Board:200]` until the user does something.
        app.ensure_board_names_for_active().await;
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
                    MouseEventKind::Down(MouseButton::Left) => {
                        // #1094 (2026-08-20) — field-picker overlay
                        // wins over everything (except the ticket
                        // detail modal which handles its own dismiss).
                        // Row hit → set selection + commit via the
                        // same path Enter uses. Click-outside → cancel
                        // (matches Esc). Without this the click fell
                        // through to the table-row model beneath.
                        if app.field_picker.is_some() {
                            if let Some(idx) = app
                                .rects
                                .picker_rows
                                .iter()
                                .find(|(r, _)| rect_hit(*r, m.column, m.row))
                                .map(|(_, i)| *i)
                            {
                                if let Some(picker) = app.field_picker.as_mut() {
                                    picker.selected = idx;
                                }
                                app.commit_field_picker().await;
                            } else if let Some(body) = app.rects.picker_body {
                                if !rect_hit(body, m.column, m.row) {
                                    app.field_picker = None;
                                    app.rects.picker_rows.clear();
                                    app.rects.picker_body = None;
                                }
                            }
                            continue;
                        }
                        // 2026-08-07 — modal + kanban rects take
                        // priority over the flat-table row model.
                        // Modal wins over everything.
                        if app.detail_modal.is_some() {
                            if let Some(r) = app.rects.modal_close
                                && rect_hit(r, m.column, m.row)
                            {
                                app.close_detail_modal();
                            }
                            // Any click outside the modal panel
                            // dismisses too — click-away UX.
                            // (Modal rect itself is registered as
                            //  the close area's parent; clicks INSIDE
                            //  the modal body are ignored so users
                            //  don't accidentally close it while
                            //  clicking a link inside.)
                            continue;
                        }
                        if app.active_is_kanban() {
                            // Chevron first — it's a 1-cell target
                            // inside the card, so precedence matters.
                            if let Some(key) = app
                                .rects
                                .kanban_chevrons
                                .iter()
                                .find(|(r, _)| rect_hit(*r, m.column, m.row))
                                .map(|(_, k)| k.clone())
                            {
                                app.toggle_kanban_expanded(&key);
                                continue;
                            }
                            // Toolbar chips.
                            if let Some(kind) = app
                                .rects
                                .kanban_chips
                                .iter()
                                .find(|(r, _)| rect_hit(*r, m.column, m.row))
                                .map(|(_, k)| *k)
                            {
                                handle_chip_click(app, kind).await;
                                continue;
                            }
                            // Card body → focus + open detail modal.
                            let hit_idx = app
                                .rects
                                .kanban_cards
                                .iter()
                                .find(|(r, _)| rect_hit(*r, m.column, m.row))
                                .map(|(_, i)| *i);
                            if let Some(idx) = hit_idx {
                                app.active_mut().selected = idx;
                                let key = app.active().issues[idx].key.clone();
                                app.open_detail_modal(key).await;
                            }
                            continue;
                        }
                        // 2026-08-18 (#991) — version chip in the
                        // tree-table title bar. Click opens the tab-
                        // view fix-version picker (same as `f` on
                        // fix_version_tree tabs).
                        if let Some(r) = app.rects.version_chip
                            && rect_hit(r, m.column, m.row)
                        {
                            app.open_tab_fix_version_picker().await;
                            continue;
                        }
                        // 2026-08-19 (#1053) — refresh chip in the
                        // tree-table title bar. Click fires
                        // `refresh_active` (mirror of `r`).
                        if let Some(r) = app.rects.refresh_chip
                            && rect_hit(r, m.column, m.row)
                        {
                            app.refresh_active().await;
                            last_refresh = Instant::now();
                            continue;
                        }
                        // Non-kanban tabs: original flat/tree row model.
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
                        // Show-more PR row click.
                        if let Some(key) = app
                            .rects
                            .pr_show_more
                            .iter()
                            .find(|(r, _)| rect_hit(*r, m.column, m.row))
                            .map(|(_, k)| k.clone())
                        {
                            app.pr_show_more(&key);
                            continue;
                        }
                        if app.active().tree.is_some() {
                            app.active_mut().selected = row_idx;
                            app.tree_activate_focused().await;
                        } else {
                            let visible = app.visible_indices();
                            if let Some(&idx) = visible.get(row_idx) {
                                app.active_mut().selected = idx;
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        // Modal: scroll description pane.
                        if app.detail_modal.is_some() {
                            app.detail_modal_scroll(-3);
                            continue;
                        }
                        // Kanban: scroll the column under the cursor.
                        if app.active_is_kanban() {
                            for (i, r) in app.rects.kanban_cols.iter().enumerate() {
                                if let Some(r) = r
                                    && rect_hit(*r, m.column, m.row)
                                {
                                    app.kanban_scroll_col(i, -3);
                                    break;
                                }
                            }
                            continue;
                        }
                        app.move_selection(-3);
                    }
                    MouseEventKind::ScrollDown => {
                        if app.detail_modal.is_some() {
                            app.detail_modal_scroll(3);
                            continue;
                        }
                        if app.active_is_kanban() {
                            for (i, r) in app.rects.kanban_cols.iter().enumerate() {
                                if let Some(r) = r
                                    && rect_hit(*r, m.column, m.row)
                                {
                                    app.kanban_scroll_col(i, 3);
                                    break;
                                }
                            }
                            continue;
                        }
                        app.move_selection(3);
                    }
                    _ => {}
                },
                Event::Resize(_, _) => { /* terminal handles re-draw */ }
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &mut App) {
    // 2026-08-07 — reset the per-frame rect registry. Draw code
    // below repopulates whichever entries apply to the current
    // pane so the click handler sees fresh coordinates every frame.
    app.rects.kanban_cards.clear();
    app.rects.kanban_chevrons.clear();
    app.rects.kanban_chips.clear();
    app.rects.kanban_cols = [None; 4];
    app.rects.modal_close = None;
    app.rects.pr_show_more.clear();
    app.rects.version_chip = None;
    app.rects.refresh_chip = None;
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
    } else {
        // #1094 — no picker open ⇒ purge stale click rects so a
        // lingering hit doesn't misroute the next left-click.
        app.rects.picker_body = None;
        app.rects.picker_rows.clear();
    }
    // 2026-08-07 — ticket-detail modal (the big one). Sits on top of
    // the picker modals so a click on the modal's close button never
    // dispatches to a lower widget.
    if app.detail_modal.is_some() {
        draw_detail_modal(f, size, app);
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

fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
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
        && let Some(tab_cfg) = app.cfg.tabs.get(app.active_tab).cloned()
    {
        draw_tree_table(f, area, app, &tab_cfg);
        return;
    }

    // 2026-08-18 (#1001) — split off a 1-row title strip above the
    // table so we can render the title without the wrapping Block
    // border. The border was inconsistent with Bitbucket + kanban
    // panes and visually redundant with the bufferline tab. Filter
    // strip stacks below the title when present.
    let title_h: u16 = 1;
    let filter_h: u16 = if app.filter.is_some() { 1 } else { 0 };
    let mut constraints: Vec<Constraint> = vec![Constraint::Length(title_h)];
    if filter_h > 0 {
        constraints.push(Constraint::Length(filter_h));
    }
    constraints.push(Constraint::Min(1));
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let title_area = parts[0];
    let filter_area = if filter_h > 0 { Some(parts[1]) } else { None };
    let table_area = parts[parts.len() - 1];
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
        format!("{} · {} of {} tickets", tab.name, visible.len(), total)
    } else {
        format!("{} · {} tickets", tab.name, total)
    };
    // Title on its own row, no wrapping border. Matches Bitbucket
    // pane style (no border on the flat table). #1001.
    let title_para = Paragraph::new(Line::from(vec![Span::styled(
        title,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(title_para, title_area);

    let table = Table::new(rows, widths)
        .header(header)
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
fn draw_kanban_board(f: &mut Frame, area: Rect, app: &mut App) {
    // 2026-08-07 — toolbar row above the columns, Jira-Cloud-style:
    // [Board ▾] [🔍 Search] [👥 Assignees ▾] [Version ▾] [Epic ▾]
    // [Type ▾] [Label ▾] [⚡ Quick filters ▾]. Board / Search /
    // Version chips wired; the rest scaffold + toast "coming soon".
    let (toolbar_area, area) = {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        (parts[0], parts[1])
    };
    draw_kanban_toolbar(f, toolbar_area, app);
    // Draw the classic `/` filter strip below the toolbar when active.
    let (filter_area, area) = if app.filter.is_some() {
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
    // 2026-08-07 — respect the `/` text filter (matches issue key or
    // summary, case-insensitive) when bucketing kanban cards. Was:
    // filter only applied to flat/tree tables; kanban ignored it and
    // showed everything regardless of what the user typed.
    let filter_visible: std::collections::HashSet<usize> =
        app.visible_indices().into_iter().collect();
    let tab = app.active();
    // 2026-08-06 — added Testing column (In PR Review + Testing + QA
    // statuses) between In Progress and Done. the default status_order
    // default is "Testing, In PR Review, In Progress, To Do, Done" so
    // "in-flight-but-not-live" is a real bucket worth its own column.
    // Also added `team` config filter: `[[tabs]] team = "web"` narrows
    // to issues whose component name OR label contains the string
    // (case-insensitive substring — flexible for common Jira
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
        // Also probe the configured team custom-field (a
        // select-type field id set via `team_field_id` in
        // `~/.config/mnml-tracker-jira.toml`). Value shape is
        // `{"value":"...", ...}`. No-op when unset.
        let custom_hit = app
            .cfg
            .team_field_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .and_then(|id| crate::app::team_value_of(issue, id))
            .map(|v| v.to_ascii_lowercase().contains(needle))
            .unwrap_or(false);
        comp_hit || label_hit || custom_hit
    };
    // #1004 (2026-08-18) — client-side Type + Label filters. Both
    // are case-insensitive equal (not substring) because these
    // fields carry discrete values (Story / Task / Bug; specific
    // label strings) — substring would spuriously match "story"
    // inside "backstory-note", etc.
    let type_filter: Option<String> = app
        .cfg
        .tabs
        .get(app.active_tab)
        .and_then(|t| t.issue_type.clone())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_ascii_lowercase());
    let type_matches = |issue: &crate::jira::Issue| -> bool {
        let Some(needle) = &type_filter else {
            return true;
        };
        issue
            .fields
            .issuetype
            .as_ref()
            .map(|t| t.name.to_ascii_lowercase() == *needle)
            .unwrap_or(false)
    };
    let label_filter: Option<String> = app
        .cfg
        .tabs
        .get(app.active_tab)
        .and_then(|t| t.label.clone())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_ascii_lowercase());
    let label_matches = |issue: &crate::jira::Issue| -> bool {
        let Some(needle) = &label_filter else {
            return true;
        };
        issue
            .fields
            .labels
            .iter()
            .any(|l| l.to_ascii_lowercase() == *needle)
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
        // #1004 (2026-08-18) — Type + Label AND on top of team.
        if !type_matches(issue) {
            continue;
        }
        if !label_matches(issue) {
            continue;
        }
        if !filter_visible.is_empty() && !filter_visible.contains(&i) {
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
    let titles = [
        format!(" To Do ({}) ", todo.len()),
        format!(" In Progress ({}) ", in_prog.len()),
        format!(" Testing ({}) ", testing.len()),
        format!(" Done ({}) ", done.len()),
    ];
    let buckets: [Vec<usize>; 4] = [todo, in_prog, testing, done];
    let active_flags = [
        matches!(selected_col, Col::Todo),
        matches!(selected_col, Col::InProgress),
        matches!(selected_col, Col::Testing),
        matches!(selected_col, Col::Done),
    ];
    for i in 0..4 {
        app.rects.kanban_cols[i] = Some(cols[i]);
        draw_kanban_column(f, cols[i], &titles[i], &buckets[i], i, active_flags[i], app);
    }
}

/// One kanban column. Highlighted border when it contains the cursor.
///
/// 2026-08-07 — now also:
///   - registers per-card mouse rects in `app.rects.kanban_cards` so
///     a click opens the detail modal (instead of missing by ~1 row);
///   - registers per-card chevron rects so `▸` toggles inline expand;
///   - renders expanded-card extras (first line of description, labels)
///     when the key is in `app.kanban_expanded`;
///   - applies `app.kanban_col_scroll[col_idx]` as a top skip so the
///     column scrolls independently.
fn draw_kanban_column(
    f: &mut Frame,
    area: Rect,
    title: &str,
    issue_indices: &[usize],
    col_idx: usize,
    is_active: bool,
    app: &mut App,
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

    // Snapshot only what we need from `app` so we can build lines
    // AND push to `app.rects` without borrow conflict. Selection +
    // selected + issues live behind `app.active()`, cloned here.
    let selected_idx = app.active().selected;
    let selection: std::collections::BTreeSet<String> = app.selection.clone();
    let expanded: std::collections::HashSet<String> = app.kanban_expanded.clone();
    let scroll_off = app.kanban_col_scroll[col_idx];
    // Build a working snapshot of the per-card fields we render.
    struct CardSnap {
        key: String,
        summary: String,
        issuetype_lc: String,
        assignee: Option<String>,
        labels: Vec<String>,
        actions: Vec<(String, String)>, // (label, color_slot)
    }
    let snaps: Vec<(usize, CardSnap)> = issue_indices
        .iter()
        .map(|&i| {
            let issue = &app.active().issues[i];
            let buttons = crate::dispatch::buttons_for_ticket(issue);
            (
                i,
                CardSnap {
                    key: issue.key.clone(),
                    summary: issue.fields.summary.clone(),
                    issuetype_lc: issue
                        .fields
                        .issuetype
                        .as_ref()
                        .map(|t| t.name.to_ascii_lowercase())
                        .unwrap_or_default(),
                    assignee: issue
                        .fields
                        .assignee
                        .as_ref()
                        .map(|a| a.display_name.clone()),
                    labels: issue.fields.labels.clone(),
                    actions: buttons
                        .into_iter()
                        .map(|b| (b.label().to_string(), b.color_slot().to_string()))
                        .collect(),
                },
            )
        })
        .collect();

    // Walk cards, track absolute Y so we can register card + chevron
    // rects at the correct screen position after subtracting scroll.
    let mut lines: Vec<Line> = Vec::new();
    // Line records: (issue_index, key, height_in_lines). Used post-hoc
    // to compute per-card rects after we know how many lines each took.
    let mut card_line_map: Vec<(usize, String, usize)> = Vec::new();

    for (issue_idx, s) in &snaps {
        let is_focused = *issue_idx == selected_idx;
        let bulk_selected = selection.contains(&s.key);
        let is_expanded = expanded.contains(&s.key);
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
            Style::default().fg(key_color).add_modifier(Modifier::BOLD)
        };
        let (type_glyph, type_color) = match s.issuetype_lc.as_str() {
            "bug" => ("\u{F188}", Color::Red),
            "story" => ("\u{F02D}", Color::Green),
            "task" => ("\u{F0139}", Color::Blue),
            "epic" => ("\u{F0E7}", Color::Magenta),
            "sub-task" | "subtask" => ("\u{F149}", Color::DarkGray),
            "spike" => ("\u{F0EB}", Color::Yellow),
            _ => ("\u{F02B}", Color::DarkGray),
        };
        // Chevron (▸/▾) as a clickable widget on the KEY line.
        let chevron_char = if is_expanded { "▾" } else { "▸" };
        let mut card_lines_here = 0;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{chevron_char} "),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{type_glyph} "),
                Style::default().fg(type_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", s.key), key_style),
        ]));
        card_lines_here += 1;

        // Wrap summary to inner width - 2 for padding.
        let wrap_w = (inner.width as usize).saturating_sub(4).max(10);
        let summary: String = s.summary.chars().take(wrap_w * 2).collect();
        let mut chunk = String::new();
        for word in summary.split_whitespace() {
            if chunk.chars().count() + word.chars().count() + 1 > wrap_w {
                lines.push(Line::from(Span::styled(
                    format!("    {chunk}"),
                    Style::default().fg(Color::Gray),
                )));
                card_lines_here += 1;
                chunk.clear();
            }
            if !chunk.is_empty() {
                chunk.push(' ');
            }
            chunk.push_str(word);
        }
        if !chunk.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {chunk}"),
                Style::default().fg(Color::Gray),
            )));
            card_lines_here += 1;
        }
        if let Some(name) = &s.assignee {
            lines.push(Line::from(Span::styled(
                format!("    · {name}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
            card_lines_here += 1;
        }
        // Expanded — show label chips + a description hint. Description
        // isn't in `Fields` yet, so we just note it's collapsed.
        if is_expanded && !s.labels.is_empty() {
            let mut spans: Vec<Span> = vec![Span::raw("    ")];
            for (li, l) in s.labels.iter().take(4).enumerate() {
                if li > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!("#{l}"),
                    Style::default().fg(Color::Blue),
                ));
            }
            if s.labels.len() > 4 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("+{}", s.labels.len() - 4),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(spans));
            card_lines_here += 1;
        }
        if is_expanded {
            lines.push(Line::from(Span::styled(
                "    (click card for full details)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC | Modifier::DIM),
            )));
            card_lines_here += 1;
        }
        if !s.actions.is_empty() {
            let mut spans: Vec<Span> = vec![Span::raw("    ")];
            for (i, (label, slot)) in s.actions.iter().enumerate() {
                let color = match slot.as_str() {
                    "green" => Color::Green,
                    "red" => Color::Red,
                    "yellow" => Color::Yellow,
                    _ => Color::White,
                };
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    label.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(spans));
            card_lines_here += 1;
        }
        // Blank separator.
        lines.push(Line::from(""));
        card_lines_here += 1;

        card_line_map.push((*issue_idx, s.key.clone(), card_lines_here));
    }

    // Apply per-column scroll.
    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll_off as usize).collect();
    let has_more_below = {
        let total_lines: usize = card_line_map.iter().map(|(_, _, h)| h).sum();
        total_lines.saturating_sub(scroll_off as usize) > inner.height as usize
    };
    let text = ratatui::text::Text::from(visible_lines);
    f.render_widget(
        Paragraph::new(text).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );

    // 2026-08-07 — compute per-card screen rects post-render. We know
    // each card's height in lines; walk the map, tracking absolute Y.
    // Register both the whole-card rect (click opens modal) and the
    // 1-cell chevron rect (click toggles expand).
    let mut y_line: i32 = -(scroll_off as i32);
    for (issue_idx, key, height) in &card_line_map {
        let start_y = inner.y as i32 + y_line;
        let end_y = start_y + *height as i32;
        // Register whole-card rect if any part is visible.
        if end_y > inner.y as i32 && start_y < (inner.y + inner.height) as i32 {
            let clamped_y = start_y.max(inner.y as i32) as u16;
            let clamped_end = (end_y.min((inner.y + inner.height) as i32)) as u16;
            let visible_h = clamped_end.saturating_sub(clamped_y);
            if visible_h > 0 {
                let card_rect = Rect {
                    x: inner.x,
                    y: clamped_y,
                    width: inner.width,
                    height: visible_h,
                };
                app.rects.kanban_cards.push((card_rect, *issue_idx));
                // Chevron sits on the KEY line (first row of the card).
                if start_y >= inner.y as i32 && start_y < (inner.y + inner.height) as i32 {
                    let chev_rect = Rect {
                        x: inner.x,
                        y: start_y as u16,
                        width: 1,
                        height: 1,
                    };
                    app.rects.kanban_chevrons.push((chev_rect, key.clone()));
                }
            }
        }
        y_line += *height as i32;
    }

    // Bottom-of-column "↓ more" hint when there's off-screen content.
    if has_more_below && inner.height >= 1 {
        let hint_y = inner.y + inner.height - 1;
        let hint_rect = Rect {
            x: inner.x,
            y: hint_y,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ↓ more · j/k scroll ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ))),
            hint_rect,
        );
    }
}

fn draw_tree_table(f: &mut Frame, area: Rect, app: &mut App, tab_cfg: &crate::config::Tab) {
    // Snapshot the row list + per-row issue-key so we can register
    // rects (needs `&mut app`) after the table body is drawn.
    let rows = app
        .active()
        .tree_rows(tab_cfg, &app.cfg)
        .unwrap_or_default();
    // Compute PrShowMore issue-keys parallel to `rows` — used post-
    // render to register mouse rects on the "show N more" rows.
    let show_more_keys: Vec<Option<String>> = rows
        .iter()
        .map(|r| match r {
            crate::tree::VisibleRow::PrShowMore { issue_idx, .. } => {
                Some(app.active().issues[*issue_idx].key.clone())
            }
            _ => None,
        })
        .collect();
    // 2026-08-18 (#991) — snapshot the two tab-state strings we need
    // in the title before letting the borrow end, so we can mutably
    // register the version-chip rect on app.rects later without
    // holding a &tab across the mutation.
    let tab = app.active();
    let tab_name = tab.name.clone();
    let tab_jql = tab.jql.clone();
    let tab_selected = tab.selected;
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
    let fixv = extract_fix_version(&tab_jql);
    let ticket_count = rows
        .iter()
        .filter(|r| matches!(r, crate::tree::VisibleRow::Ticket { .. }))
        .count();
    let ticket_suffix = if ticket_count == 1 { "" } else { "s" };

    // 2026-08-18 (#1001) — split off a 1-row title strip above the
    // table. Was wrapped in Block::borders(ALL) with title-in-border;
    // border removed for visual parity with Bitbucket + Boards.
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    let title_area = parts[0];
    let body_area = parts[1];

    // 2026-08-18 (#991) — render the version segment as a
    // clickable chip with `▾` dropdown indicator. Click opens the
    // tab-view fix-version picker (same as pressing `f` on this
    // tab). Discoverability win over the hidden `f` keychord.
    let bold = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD);
    let leading = format!("{tab_name} · ");
    let leading_w = leading.chars().count() as u16;
    let chip_label = match &fixv {
        Some(v) => format!(" {v} ▾ "),
        None => " Set fixVersion ▾ ".to_string(),
    };
    let chip_w = chip_label.chars().count() as u16;
    let chip_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let trailing = format!(" · {ticket_count} ticket{ticket_suffix}");
    let spans: Vec<Span<'static>> = vec![
        Span::styled(leading, bold),
        Span::styled(chip_label, chip_style),
        Span::styled(trailing, bold),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), title_area);
    // Chip rect (computed here; assigned after render_stateful_widget
    // consumes table_rows and drops the &app borrow).
    let chip_rect = Rect {
        x: title_area.x + leading_w,
        y: title_area.y,
        width: chip_w,
        height: 1,
    };
    // 2026-08-19 (#1053) — right-aligned refresh chip. Renders as
    // a second Paragraph on the same title row so mouse-only users
    // can trigger `refresh_active` without the `r` keychord. Skips
    // when the title area is too narrow to hold both chips comfortably.
    let refresh_label = " ⟳ Refresh ";
    let refresh_w = refresh_label.chars().count() as u16;
    let refresh_rect: Option<Rect> = if title_area.width > leading_w + chip_w + refresh_w + 2 {
        let x = title_area.x + title_area.width - refresh_w;
        let r = Rect {
            x,
            y: title_area.y,
            width: refresh_w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(refresh_label, chip_style))),
            r,
        );
        Some(r)
    } else {
        None
    };

    let table = Table::new(table_rows, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut state = TableState::default();
    state.select(Some(tab_selected.min(rows.len().saturating_sub(1))));
    f.render_stateful_widget(table, body_area, &mut state);
    // Register the version-chip click rect now that table_rows is
    // consumed and the &app borrow has ended (#991).
    app.rects.version_chip = Some(chip_rect);
    // 2026-08-19 (#1053) — register the refresh-chip click rect.
    app.rects.refresh_chip = refresh_rect;

    // 2026-08-07 — register mouse rects for "show N more" rows.
    // Post-#1001 (2026-08-18): body starts at body_area.y + 1
    // (header row), no border row above. Was `+ 2` when a border
    // added an extra row.
    for (i, maybe_key) in show_more_keys.iter().enumerate() {
        if let Some(key) = maybe_key {
            let y = body_area.y + 1 + i as u16;
            if y < body_area.y + body_area.height {
                let r = Rect {
                    x: body_area.x,
                    y,
                    width: body_area.width,
                    height: 1,
                };
                app.rects.pr_show_more.push((r, key.clone()));
            }
        }
    }
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
            let src = if pr.source.branch.is_empty() {
                "?".to_string()
            } else {
                pr.source.branch.clone()
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
            // #1051 (2026-08-19). Expanded PR-row context: repo slug
            // + source→destination branch + PR id, so users can see
            // which repo they're merging into without clicking
            // through. `repository_name` is populated by the
            // dev-status endpoint when the issue touches Bitbucket;
            // renders as "—" for entries that come back without it.
            let repo_label = if pr.repository_name.is_empty() {
                "—".to_string()
            } else {
                pr.repository_name.clone()
            };
            let label = format!(
                "        {chevron}{status:<7} {repo}  {id}  {src} → {dest}{approval_hint}",
                status = pr.status,
                repo = repo_label,
                id = pr.id,
                src = src,
                dest = dest,
                approval_hint = approval_hint,
            );
            let status_color = match pr.status.to_ascii_uppercase().as_str() {
                "MERGED" => Color::Magenta,
                "OPEN" | "DRAFT" | "IN_REVIEW" => Color::Green,
                "DECLINED" | "SUPERSEDED" => Color::DarkGray,
                _ => Color::Gray,
            };
            // #1051 (2026-08-19). Action chips on the title cell:
            //   OPEN / DRAFT / IN_REVIEW  → [ Review ] [ Merge ] [ Open ]
            //   MERGED / DECLINED / etc   → [ Open ]
            // `[ Review ]` (existing, keyboard `V`) launches an AI
            // reviewer for the PR. `[ Merge ]` + `[ Open ]` are
            // VISUAL only for now — click routing follows in a
            // later commit once the tree tab's mouse layer grows a
            // rect registry (matches the kanban-toolbar chip pattern
            // shipped 2026-08-07). Keyboard users can still hit `o`
            // on the focused PR row to open in browser. Fine
            // affordance signal in the meantime.
            let title_cell = {
                use ratatui::text::{Line, Span};
                let mut spans: Vec<Span> = vec![Span::styled(
                    pr.name.clone(),
                    Style::default().fg(Color::DarkGray),
                )];
                if pr.is_open() {
                    spans.push(Span::raw("   "));
                    spans.push(Span::styled(
                        "[ Review ]",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        "[ Merge ]",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    "[ Open ]",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                Cell::from(Line::from(spans))
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
            let resolved = tab.tree.as_ref().and_then(|t| {
                t.pr_cache.get(&issue.key).and_then(|prs| {
                    prs.get(*pr_idx).and_then(|pr| {
                        t.pipeline_cache
                            .get(&(issue.key.clone(), pr.id.clone()))
                            .and_then(|pipelines| pipelines.get(*pipeline_idx))
                            .map(|pl| (pl, pr.repository_name.clone()))
                    })
                })
            });
            let Some((pipeline, repo_name)) = resolved else {
                return Row::new(vec![
                    Cell::from("            → (missing pipeline)")
                        .style(Style::default().fg(Color::DarkGray)),
                ]);
            };
            let (glyph, color) = pipeline_glyph(pipeline.state_label());
            // Format the row Bitbucket-sibling-style — one span-heavy
            // Line so glyph, state, repo, build, branch, date, duration
            // each get their own color / weight instead of one flat
            // string. Matches the visual language in
            // `render_pr_expand_title_cell` on the sibling.
            let state = pipeline.state_label().to_string();
            let build = pipeline.build_number;
            let branch = pipeline.branch_label().to_string();
            let when = pipeline.created_date();
            let dur = pipeline.duration_label();
            let repo = if repo_name.is_empty() {
                "—".to_string()
            } else {
                repo_name
            };
            let line = Line::from(vec![
                Span::raw("            "),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{state:<11} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(repo, Style::default().fg(Color::Gray)),
                Span::raw("  "),
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
        VisibleRow::PrShowMore { hidden, .. } => Row::new(vec![
            // 2026-08-18 (#994) — was "show N more" with a staircase
            // reveal (+3 per click). Now one click reveals everything.
            // 2026-08-19 (#1052) — dropped the leading `▸ ` chevron
            // (looks like a tiny arrow before the link) and added
            // UNDERLINED so the row reads as a hyperlink chip instead
            // of another tree branch.
            Cell::from(format!(
                "        Show all {} PR{}",
                hidden,
                if *hidden == 1 { "" } else { "s" }
            ))
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
        ]),
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

/// 2026-08-07 — Jira-Cloud-style filter toolbar above kanban columns.
/// Chip layout:
///   `[Board: <name> ▾] [🔍 Search: <q>] [Assignees ▾] [Version ▾]`
///   `[Epic ▾] [Type ▾] [Label ▾] [⚡ Quick filters ▾]`
///
/// This first cut is VISUAL only — chips show their current value
/// or a "▾" placeholder. Click routing follows in a second commit
/// once the tracker's mouse layer grows a rect registry (it's
/// currently table-row-only). Board picker + search + version
/// chips duplicate existing keyboard entry points (T/`/`/V), so
/// mouse users have a discoverable path even without click routing.
fn draw_kanban_toolbar(f: &mut Frame, area: Rect, app: &mut App) {
    use ratatui::text::{Line, Span};
    let cfg_tab = app.cfg.tabs.get(app.active_tab);
    // 2026-08-07 — standardized chip grammar (design-critic r1 #3):
    //   set:   "<Label>: <value> ▾"
    //   unset: "<Label> ▾"
    // Applies to Board / Team / Version consistently. Search keeps
    // its emoji-prefix + free-text convention (its "value" IS the
    // text the user is typing, no label needed).
    let board_label = cfg_tab
        .and_then(|t| {
            t.board_id.map(|id| {
                // Prefer the cached friendly name when we have one;
                // fall back to the numeric id while the fetch is in
                // flight (or if it failed and we cached the fallback).
                match app.board_name_cache.get(&id) {
                    Some(name) => format!("Board: {name} ▾"),
                    None => format!("Board: {id} ▾"),
                }
            })
        })
        .unwrap_or_else(|| "Board ▾".to_string());
    let search_label = app
        .filter
        .as_ref()
        .map(|f| {
            if f.buffer.is_empty() {
                "🔍 Search".to_string()
            } else {
                format!("🔍 {}", f.buffer)
            }
        })
        .unwrap_or_else(|| "🔍 Search".to_string());
    let team_label = cfg_tab
        .and_then(|t| t.team.as_ref())
        .map(|t| format!("Team: {t} ▾"))
        .unwrap_or_else(|| "Team ▾".to_string());
    // 2026-08-07 — Version chip reflects the currently-filtered
    // fixVersion in its label. Extracts from the tab's JQL (reuse
    // `extract_fix_version`, the same helper the tree-table title
    // uses) so the chip reads as an active filter when set.
    let version_label = cfg_tab
        .and_then(|t| t.jql.as_ref())
        .and_then(|jql| extract_fix_version(jql))
        .map(|v| format!("Version: {v} ▾"))
        .unwrap_or_else(|| "Version ▾".to_string());
    let version_active = cfg_tab
        .and_then(|t| t.jql.as_ref())
        .and_then(|jql| extract_fix_version(jql))
        .is_some();
    // Compact chip helper: `[ <label> ]` in dim gray, with the
    // active-value chips in cyan for contrast.
    let chip = |label: &str, active: bool| -> Span<'static> {
        let color = if active { Color::Cyan } else { Color::DarkGray };
        Span::styled(
            format!(" [ {label} ] "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    };
    // Register each chip's rect so the click handler can dispatch.
    // We measure widths inline so the register-order matches render-order.
    use crate::app::ChipKind;
    // 2026-08-17 (task #887) — sprint chip. Visibility rule is
    // "optimistic until confirmed kanban": show on any tab with a
    // board_id UNLESS the sprint cache is known-empty (i.e. we
    // fetched and Jira returned no sprints — the board is kanban).
    // A None cache (not fetched yet) keeps the chip visible so the
    // user has an entry point to trigger the fetch. On scrum boards,
    // clicking populates the cache and the chip's label swaps from
    // the placeholder to the current sprint name on the next paint.
    let tab_state = app.tabs.get(app.active_tab);
    let selected_sprint_id = tab_state.and_then(|s| s.selected_sprint_id);
    let sprints_cache = tab_state.and_then(|s| s.sprints_cache.as_ref());
    let has_board_id = cfg_tab.is_some_and(|t| t.board_id.is_some());
    let cache_confirmed_empty = sprints_cache.is_some_and(|v| v.is_empty());
    let has_sprints = has_board_id && !cache_confirmed_empty;
    // The current sprint name we show on the chip. Priority:
    //   1) if the user pinned a sprint, look it up in the cache;
    //   2) otherwise take the sole "active" sprint from the cache;
    //   3) otherwise fall back to a bare "Sprint" label.
    let sprint_chip_label: String = if let Some(cache) = sprints_cache {
        let name = if let Some(id) = selected_sprint_id {
            cache.iter().find(|s| s.id == id).map(|s| s.name.clone())
        } else {
            cache
                .iter()
                .find(|s| s.state.eq_ignore_ascii_case("active"))
                .map(|s| s.name.clone())
        };
        match name {
            Some(n) => format!("Sprint: {n} ▾"),
            None => "Sprint ▾".to_string(),
        }
    } else {
        "Sprint ▾".to_string()
    };
    let sprint_active = selected_sprint_id.is_some();
    // 2026-08-17 (task #893) — active-quick-filter count for the
    // toolbar chip: `⚡ Quick filters (2) ▾` when two are active.
    let active_qf_count = tab_state
        .map(|s| s.active_quick_filter_ids.len())
        .unwrap_or(0);
    let quickfilters_label = if active_qf_count > 0 {
        format!("⚡ Quick filters ({active_qf_count}) ▾")
    } else {
        "⚡ Quick filters ▾".to_string()
    };
    let quickfilters_active = active_qf_count > 0;
    let mut entries: Vec<(String, bool, ChipKind)> = Vec::new();
    entries.push((
        board_label,
        cfg_tab.is_some_and(|t| t.board_id.is_some()),
        ChipKind::Board,
    ));
    if has_sprints {
        entries.push((sprint_chip_label, sprint_active, ChipKind::Sprint));
    }
    entries.push((
        search_label,
        app.filter.as_ref().is_some_and(|f| !f.buffer.is_empty()),
        ChipKind::Search,
    ));
    entries.push((
        team_label,
        cfg_tab.is_some_and(|t| t.team.is_some()),
        ChipKind::Team,
    ));
    entries.push((version_label, version_active, ChipKind::Version));
    entries.push(("Epic ▾".to_string(), false, ChipKind::Epic));
    entries.push(("Type ▾".to_string(), false, ChipKind::Type));
    entries.push(("Label ▾".to_string(), false, ChipKind::Label));
    entries.push((
        quickfilters_label,
        quickfilters_active,
        ChipKind::QuickFilters,
    ));
    // 2026-08-17 (task #893) — settings gear. Only visible when the
    // tab has a board_id, since the URL we open is board-scoped.
    if cfg_tab.is_some_and(|t| t.board_id.is_some()) {
        entries.push(("⚙ Settings".to_string(), false, ChipKind::BoardSettings));
    }
    app.rects.kanban_chips.clear();
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor_x: u16 = area.x;
    for (label, active, kind) in &entries {
        let s = chip(label, *active);
        let w = s.content.chars().count() as u16;
        if cursor_x + w <= area.x + area.width {
            app.rects.kanban_chips.push((
                Rect {
                    x: cursor_x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                *kind,
            ));
        }
        cursor_x = cursor_x.saturating_add(w);
        spans.push(s);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
            " ↑↓ · / filter · Space pick · . actions · t move · a assignee · T team · w watch · d details · q "
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
fn draw_field_picker(f: &mut Frame, screen: Rect, app: &mut App) {
    // #1094 (2026-08-20) — reset per-draw click rects so a shrunk
    // visible window doesn't leave phantom hit-targets from the
    // prior frame.
    app.rects.picker_body = None;
    app.rects.picker_rows.clear();
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
        crate::app::FieldKind::TicketAction => "action",
        crate::app::FieldKind::Sprint => "sprint",
        crate::app::FieldKind::QuickFilter => "quick filters",
        crate::app::FieldKind::IssueType => "type",
        crate::app::FieldKind::Label => "label",
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
    } else if matches!(picker.kind, crate::app::FieldKind::TicketAction) {
        " actions ".to_string()
    } else if matches!(picker.kind, crate::app::FieldKind::Sprint) {
        " switch sprint ".to_string()
    } else if matches!(picker.kind, crate::app::FieldKind::QuickFilter) {
        " toggle quick filters (Space) ".to_string()
    } else if matches!(picker.kind, crate::app::FieldKind::FixVersion) && target_count == 1 {
        // Disambiguate from the tab-view picker — "on ticket" makes
        // it obvious this is a per-row assign, not a view switch.
        // Users who pressed `f` expecting a view switch bounced off
        // this picker for months (task #989).
        let focused = app.focused_key().unwrap_or_default();
        if focused.is_empty() {
            " assign fixVersion on ticket ".to_string()
        } else {
            format!(" assign fixVersion on {focused} ")
        }
    } else if target_count == 1 {
        format!(" set {field_label} ")
    } else {
        format!(" set {field_label} × {target_count} ")
    };

    let mut body: Vec<Line> = Vec::new();
    let mut row_rects: Vec<(Rect, usize)> = Vec::new();
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
            // #1094 — capture terminal-space Rect per visible item so
            // the mouse handler can commit on click. Body layout inside
            // the bordered area: row 0 = filter, row 1 = blank, row
            // 2..2+N = items. Content-inner starts at (area.x+1,
            // area.y+1) with width area.width-2.
            row_rects = visible[start..end]
                .iter()
                .enumerate()
                .map(|(k, &idx)| {
                    (
                        Rect {
                            x: area.x + 1,
                            y: area.y + 1 + 2 + k as u16,
                            width: area.width.saturating_sub(2),
                            height: 1,
                        },
                        idx,
                    )
                })
                .collect();
            for &idx in &visible[start..end] {
                let (id, label) = &picker.items[idx];
                let style = if idx == picker.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                // Multi-select pickers (task #893 quick filters)
                // render a `[x]` / `[ ]` box before the label so the
                // user can see the current selection at a glance and
                // knows Space toggles rather than commits.
                let prefix = match (picker.multi_selected.as_ref(), idx == picker.selected) {
                    (Some(multi), true) => {
                        if multi.contains(id) {
                            "  ▸ [x] ".to_string()
                        } else {
                            "  ▸ [ ] ".to_string()
                        }
                    }
                    (Some(multi), false) => {
                        if multi.contains(id) {
                            "    [x] ".to_string()
                        } else {
                            "    [ ] ".to_string()
                        }
                    }
                    (None, true) => "  ▸ ".to_string(),
                    (None, false) => "    ".to_string(),
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
    let hint = if picker.multi_selected.is_some() {
        "  type to filter · ↑↓ move · Space toggle · Enter apply · Esc cancel"
    } else {
        "  type to filter · ↑↓ move · Enter commit · Esc cancel"
    };
    body.push(Line::from(Span::styled(
        hint,
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
    // #1094 — publish rects for the mouse handler AFTER render, so any
    // early-return above (e.g. `!picker.loaded`) leaves them empty.
    app.rects.picker_body = Some(area);
    app.rects.picker_rows = row_rects;
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
        // Task #890 — hotkey-hint cluster, dim comment fg.
        Column::Actions => Cell::from("t a f w d .").style(Style::default().fg(Color::DarkGray)),
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

/// 2026-08-07 — point-in-rect test in absolute terminal coords.
fn rect_hit(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// 2026-08-07 — kanban toolbar chip click dispatch. Most chips route
/// to the existing keyboard-triggered pickers; the placeholder ones
/// toast a friendly "coming soon" so the user knows it registered.
async fn handle_chip_click(app: &mut App, kind: crate::app::ChipKind) {
    use crate::app::ChipKind;
    match kind {
        ChipKind::Board => {
            // v1: no picker yet — clicking cycles a toast reminding
            // the user how to switch (config-driven for now).
            app.status =
                "Board selection is config-driven — edit `board_id` in mnml-tracker-jira.toml"
                    .into();
        }
        ChipKind::Sprint => app.open_sprint_picker().await,
        ChipKind::Search => app.open_filter(),
        ChipKind::Team => app.open_team_picker(),
        ChipKind::Version => app.open_tab_fix_version_picker().await,
        ChipKind::QuickFilters => app.open_quickfilter_picker().await,
        ChipKind::BoardSettings => app.open_board_settings(),
        // #1004 (2026-08-18) — Type + Label pickers wire up (client-
        // side render filters). Epic still deferred pending epic-link
        // custom-field handling (needs project-scoped field-id lookup
        // which varies per Jira instance).
        ChipKind::Type => app.open_issue_type_picker(),
        ChipKind::Label => app.open_label_picker(),
        ChipKind::Epic => {
            app.status = "Epic filter — coming soon (needs project epic-link field)".to_string();
        }
    }
}

/// 2026-08-07 — big card detail modal. Opens on card-click (or `D`
/// on the focused card). Content is configured via
/// `[detail_modal] fields = [...]` in the TOML config; unset ⇒ the
/// built-in default field list.
///
/// Layout: 80% width × 80% height, centered. Header row shows the
/// key + status + close chip. Below: two columns — left is compact
/// labeled fields (assignee, priority, etc.), right is long text
/// (description + long custom fields), scrollable via j/k or wheel.
fn draw_detail_modal(f: &mut Frame, screen: Rect, app: &mut App) {
    use ratatui::text::{Line, Span};
    let Some(modal) = app.detail_modal.as_ref() else {
        return;
    };
    // 80% × 80% centered.
    let w = (screen.width as u32 * 8 / 10) as u16;
    let h = (screen.height as u32 * 8 / 10) as u16;
    let x = screen.x + (screen.width.saturating_sub(w)) / 2;
    let y = screen.y + (screen.height.saturating_sub(h)) / 2;
    let area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    // Dim backdrop with a filled block so nothing bleeds through.
    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", modal.key),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Close × in the top-right of the header row.
    if inner.width >= 4 {
        let close_x = inner.x + inner.width - 4;
        let close_rect = Rect {
            x: close_x,
            y: inner.y,
            width: 3,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " × ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))),
            close_rect,
        );
        app.rects.modal_close = Some(close_rect);
    }

    // Loading / error state before we have JSON data.
    if let Some(err) = &modal.error {
        let p = Paragraph::new(format!(
            "Failed to load {}:\n{err}\n\nEsc to close.",
            modal.key
        ))
        .style(Style::default().fg(Color::Red))
        .wrap(ratatui::widgets::Wrap { trim: false });
        f.render_widget(p, inner);
        return;
    }
    let Some(data) = &modal.data else {
        let p = Paragraph::new(format!("loading {}…", modal.key))
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, inner);
        return;
    };

    // Two-column split under the header. Header takes 1 row.
    let content_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    let two = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_area);

    // Header row — key · summary · status.
    let summary = data
        .pointer("/fields/summary")
        .and_then(|v| v.as_str())
        .unwrap_or("(no summary)");
    let status = data
        .pointer("/fields/status/name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let header = Line::from(vec![
        Span::styled(
            format!(" {} · ", modal.key),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(summary.to_string(), Style::default().fg(Color::Gray)),
        Span::raw("  "),
        Span::styled(
            format!("[{status}]"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let header_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width.saturating_sub(4),
        height: 1,
    };
    f.render_widget(Paragraph::new(header), header_rect);

    // Left column: labeled short fields, one per configured entry.
    let alias = app.cfg.detail_modal.field_alias.clone();
    let fields = app.cfg.detail_modal.fields.clone();
    let mut left_lines: Vec<Line> = Vec::new();
    let mut long_text_lines: Vec<Line> = Vec::new();
    for spec in &fields {
        let id = spec.resolve_id(&alias);
        let label = spec.resolve_label(&alias);
        // Fields that live on the right (long text).
        let is_long = matches!(id.as_str(), "description" | "environment");
        // Skip fields already in the header.
        if matches!(id.as_str(), "title" | "summary" | "status") {
            continue;
        }
        let value = resolve_field_value(data, &id);
        if is_long {
            long_text_lines.push(Line::from(vec![Span::styled(
                format!("{label}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]));
            for line in value.lines() {
                long_text_lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::Gray),
                )));
            }
            long_text_lines.push(Line::from(""));
        } else {
            left_lines.push(Line::from(vec![
                Span::styled(
                    format!("{label:<12}: "),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(value, Style::default().fg(Color::White)),
            ]));
        }
    }
    f.render_widget(
        Paragraph::new(ratatui::text::Text::from(left_lines))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        two[0],
    );
    // Right pane: scrollable long text. Apply modal.scroll as skip.
    let visible: Vec<Line> = long_text_lines
        .into_iter()
        .skip(modal.scroll as usize)
        .collect();
    f.render_widget(
        Paragraph::new(ratatui::text::Text::from(visible))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        two[1],
    );
}

/// 2026-08-07 — extract a human-readable value for a Jira field id
/// from the raw JSON returned by `/rest/api/3/issue/{key}`. Handles
/// the common shapes: string, {name}, [{name}], user (displayName),
/// ADF description, custom-field {value}.
fn resolve_field_value(data: &serde_json::Value, field_id: &str) -> String {
    // Header-only fields have their own render paths.
    let raw = if field_id == "summary" || field_id == "title" {
        data.pointer("/fields/summary")
    } else if field_id == "status" {
        data.pointer("/fields/status/name")
    } else if field_id == "type" || field_id == "issuetype" {
        data.pointer("/fields/issuetype/name")
    } else if field_id == "priority" {
        data.pointer("/fields/priority/name")
    } else if field_id == "assignee" {
        data.pointer("/fields/assignee/displayName")
    } else if field_id == "reporter" {
        data.pointer("/fields/reporter/displayName")
    } else if field_id == "labels" {
        return join_string_array(data.pointer("/fields/labels"));
    } else if field_id == "components" {
        return join_named_array(data.pointer("/fields/components"));
    } else if field_id == "fix_version" || field_id == "fixversions" {
        return join_named_array(data.pointer("/fields/fixVersions"));
    } else if field_id == "sprint" {
        return sprint_label(data.pointer("/fields/customfield_10020"))
            .unwrap_or_else(|| "—".into());
    } else if field_id == "parent" {
        return data
            .pointer("/fields/parent/fields/summary")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".into());
    } else if field_id == "description" {
        return adf_to_plain(data.pointer("/fields/description"));
    } else if field_id == "environment" {
        return adf_to_plain(data.pointer("/fields/environment"));
    } else {
        // Custom fields — check /fields/{id}
        let v = data.pointer(&format!("/fields/{field_id}"));
        return json_to_display(v);
    };
    match raw {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) if v.is_null() => "—".into(),
        Some(v) => v.to_string(),
        None => "—".into(),
    }
}

fn join_string_array(v: Option<&serde_json::Value>) -> String {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return "—".into();
    };
    if arr.is_empty() {
        return "—".into();
    }
    arr.iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_named_array(v: Option<&serde_json::Value>) -> String {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return "—".into();
    };
    if arr.is_empty() {
        return "—".into();
    }
    arr.iter()
        .filter_map(|x| x.pointer("/name").and_then(|n| n.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sprint_label(v: Option<&serde_json::Value>) -> Option<String> {
    let arr = v?.as_array()?;
    let names: Vec<String> = arr
        .iter()
        .filter_map(|s| {
            // Sprint entries come back as strings (legacy) or
            // objects with `name` (new shape). Handle both.
            s.as_str()
                .and_then(|raw| {
                    // Legacy: `com.atlassian.greenhopper...[id=1,rapidViewId=…,state=ACTIVE,name=Sprint 3,...]`
                    if let Some(start) = raw.find("name=") {
                        let tail = &raw[start + 5..];
                        Some(tail.split(',').next().unwrap_or("").to_string())
                    } else {
                        Some(raw.to_string())
                    }
                })
                .or_else(|| {
                    s.pointer("/name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
        })
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn adf_to_plain(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else {
        return "—".into();
    };
    // Older Jira responses use a plain string; newer use ADF JSON.
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    // Depth-first walk of the ADF tree, concatenating any `text`
    // leaves. Paragraphs become blank-line-separated.
    fn walk(n: &serde_json::Value, out: &mut String) {
        if let Some(t) = n.get("text").and_then(|v| v.as_str()) {
            out.push_str(t);
        }
        if let Some(content) = n.get("content").and_then(|c| c.as_array()) {
            for child in content {
                walk(child, out);
            }
        }
        // Paragraph-level nodes get a newline break.
        if let Some(t) = n.get("type").and_then(|v| v.as_str())
            && matches!(t, "paragraph" | "heading" | "listItem")
        {
            out.push('\n');
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    if out.trim().is_empty() {
        "—".into()
    } else {
        out
    }
}

fn json_to_display(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else { return "—".into() };
    if v.is_null() {
        return "—".into();
    }
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(inner) = v.pointer("/value").and_then(|s| s.as_str()) {
        return inner.to_string();
    }
    if let Some(inner) = v.pointer("/name").and_then(|s| s.as_str()) {
        return inner.to_string();
    }
    // Object with `content` (ADF): render like description.
    if v.is_object() && v.get("content").is_some() {
        return adf_to_plain(Some(v));
    }
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .filter_map(|x| {
                x.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| {
                        x.pointer("/name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| {
                        x.pointer("/value")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    v.to_string()
}
