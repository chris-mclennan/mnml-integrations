//! ratatui rendering + the main event loop.

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
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, TableState, Tabs,
    },
};
use std::io::Stdout;
use std::time::{Duration, Instant};

/// Filter a repo's PR list down to what should be visible in an
/// expanded row group. Policy depends on the tab spec:
///
/// - `show_all == true` — always return every PR (the user clicked
///   "[ Show N older ]" and asked to reveal the pile).
/// - Mine tab (`tab.spec.mine_only`) — return every OPEN PR plus the
///   FIRST MERGED PR seen (a peek at your last landed change);
///   everything else is hidden until show_all. This assumes the fetch
///   already returns PRs in updated_on-descending order, which is
///   the Bitbucket Cloud default. Was: bypass all filtering, which
///   flooded expansions with 50+ historical merges per repo.
/// - Otherwise (workspace-wide firehose views) — 24-hour recency
///   window, matching the historical behavior.
///
/// Also returns the count of PRs that were filtered out — used to
/// drive the "[ Show N older PRs ]" footer.
fn visible_prs_for_render<'a>(
    prs: &'a [crate::bitbucket::PullRequest],
    tab: &crate::app::TabState,
    show_all: bool,
) -> Vec<&'a crate::bitbucket::PullRequest> {
    if show_all {
        return prs.iter().collect();
    }
    if tab.spec.mine_only {
        let mut merged_shown = false;
        return prs
            .iter()
            .filter(|pr| {
                if pr.state.eq_ignore_ascii_case("OPEN") {
                    return true;
                }
                if pr.state.eq_ignore_ascii_case("MERGED") && !merged_shown {
                    merged_shown = true;
                    return true;
                }
                false
            })
            .collect();
    }
    prs.iter()
        .filter(|pr| {
            pr.updated_on
                .as_deref()
                .and_then(crate::app::hours_since)
                .map(|h| h <= crate::app::RECENT_WINDOW_HOURS)
                .unwrap_or(true)
        })
        .collect()
}

/// Count of PRs hidden by `visible_prs_for_render` at the current
/// state. Zero when show_all is set (nothing is hidden).
fn hidden_pr_count_for_render(
    prs: &[crate::bitbucket::PullRequest],
    tab: &crate::app::TabState,
    show_all: bool,
) -> usize {
    if show_all {
        return 0;
    }
    prs.len().saturating_sub(visible_prs_for_render(prs, tab, show_all).len())
}

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    // tree-redesign 2026-07-15 — force-clear the pty terminal on
    // startup. Without this, when mnml re-launches the sibling into
    // an already-open Pty pane, the previous process's cell contents
    // linger — ratatui's diff-only writer keeps them because its
    // fresh internal buffer matches "space" at those positions and
    // no explicit write goes out. Symptom: garbled overlapping
    // frames (user report 2026-07-15).
    terminal.clear()?;

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
        if app.cfg.refresh_interval_secs > 0
            && last_refresh.elapsed().as_secs() >= app.cfg.refresh_interval_secs
        {
            app.refresh_active().await;
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
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // #1000 — footer chord chip hit-test first,
                        // so a click on the bottom row doesn't
                        // accidentally fall through to whatever row
                        // math thinks is at that y (which for a
                        // 1-row-tall status bar is technically None,
                        // but this keeps the intent explicit and
                        // survives future footer-height changes).
                        let footer_hit = app
                            .hint_chip_rects
                            .iter()
                            .find(|(r, _)| {
                                m.column >= r.x
                                    && m.column < r.x + r.width
                                    && m.row >= r.y
                                    && m.row < r.y + r.height
                            })
                            .map(|(_, key)| *key);
                        if let Some(key) = footer_hit {
                            if let Some(action) = keys::handle(key, app) {
                                let quit = keys::apply(action, app).await;
                                if quit {
                                    break;
                                }
                                last_refresh = Instant::now();
                            }
                        } else if let Some(idx) = tab_at(m.column, m.row, app) {
                            app.switch_tab(idx);
                            // If clicked tab has no data yet, kick
                            // off a refresh so it populates (mirror
                            // the number-key path in keys::apply).
                            if app.tabs[idx].last_fetched.is_none() {
                                app.refresh_active().await;
                                last_refresh = Instant::now();
                            }
                        } else if let Some(kind) = filter_chip_at(m.column, m.row, app) {
                            use crate::app::FilterChip;
                            match kind {
                                FilterChip::Status => {
                                    // #1103 — Status chip cycles tabs
                                    // (each tab is a PR-state slice).
                                    // Preserves the pre-existing `m`
                                    // chord behavior. Skipped when
                                    // there's only one tab (nothing
                                    // to cycle to).
                                    if app.tabs.len() > 1 {
                                        let next = (app.active_tab + 1) % app.tabs.len();
                                        app.switch_tab(next);
                                        if app.tabs[app.active_tab].last_fetched.is_none() {
                                            app.refresh_active().await;
                                            last_refresh = Instant::now();
                                        }
                                    } else {
                                        app.status = "only one tab configured".into();
                                    }
                                }
                                FilterChip::Author => {
                                    // #1116 audit SEV-1 (2026-08-21)
                                    // — Author chip now owns the
                                    // mine_only toggle (the separate
                                    // `All` chip that used to do this
                                    // has been retired since its
                                    // label lied about the semantics).
                                    // Only meaningful on PR-family
                                    // tabs; toggle_active_mine_only
                                    // rejects other kinds internally.
                                    app.toggle_active_mine_only();
                                    app.refresh_active().await;
                                    last_refresh = Instant::now();
                                }
                                FilterChip::All => {
                                    // Legacy variant, chip retired.
                                    // Kept in the enum so old rects
                                    // registered during a prior draw
                                    // don't panic on a stray click.
                                }
                                FilterChip::ActionRefresh => {
                                    // #1053-analog (2026-08-21) —
                                    // right-side Refresh chip, mirror
                                    // of Jira Work's refresh chip.
                                    app.refresh_active().await;
                                    last_refresh = Instant::now();
                                }
                                FilterChip::Search
                                | FilterChip::TargetBranch
                                | FilterChip::Branch
                                | FilterChip::PipelineType
                                | FilterChip::TriggerType
                                | FilterChip::ActionRunPipeline
                                | FilterChip::ActionSchedules
                                | FilterChip::ActionCaches
                                | FilterChip::ActionUsage => {
                                    // Visual placeholders — no click
                                    // action yet. Follow-up: Search =
                                    // free-text `/` prompt; Author =
                                    // account picker; TargetBranch /
                                    // Branch = branch picker;
                                    // PipelineType / TriggerType =
                                    // enum picker; Run pipeline /
                                    // Schedules / Caches / Usage =
                                    // navigate to Bitbucket Cloud in
                                    // the browser.
                                    app.status =
                                        "filter not wired yet (round-1 visual)".into();
                                }
                            }
                        } else if let Some(row_idx) = table_row_at(m.row, app) {
                            // 2026-07-19 — click a tree row: focus
                            // it, and if it's a repo header, toggle
                            // its expand. 2026-07-24 — merged PR
                            // rows also toggle (opens the post-merge
                            // pipeline sub-line); routed via
                            // `smart_toggle_focused` which handles
                            // both cases. And the synthetic "[ Show
                            // N older ]" footer row activates the
                            // recency filter's show_all flag.
                            let vis = app.active().data.len();
                            if row_idx < vis {
                                app.active_mut().selected = row_idx;
                                if is_show_more_footer_row(app, row_idx) {
                                    app.set_show_all_prs(true);
                                } else if is_repo_header_row(app, row_idx) {
                                    app.tree_toggle_focused_repo();
                                } else {
                                    app.smart_toggle_focused().await;
                                }
                            }
                        }
                    }
                    // With mouse capture on, scroll events on the
                    // sibling's pty go here instead of falling
                    // through to mnml's outer pane. Route to the
                    // same move_selection path Up/Down use so the
                    // row list scrolls under the wheel.
                    MouseEventKind::ScrollUp => app.move_selection(-3),
                    MouseEventKind::ScrollDown => app.move_selection(3),
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

/// Hit-test a click on the top tab strip. Returns the tab index if
/// the click landed on a tab label, else None. Tab strip lives in
/// `Rect { x=0, y=0, w=cols, h=3 }` — the label row is `y == 1`.
/// ratatui `Tabs` renders labels joined by " │ " with one space of
/// leading pad after the block border, so we replay that width math
/// to figure out which label the click column falls in.
/// Given an absolute click row, return the table's visible-row index
/// (0-based) if the click landed on a row line inside the table body,
/// else None. Header + border consume 2 rows at the top of the table
/// area. The table area starts at y=3 when the tab strip is visible,
/// else y=0. Ignores columns — a click anywhere on the row selects
/// that row. tree-redesign 2026-07-19 mouse pass.
fn table_row_at(row: u16, app: &App) -> Option<usize> {
    let show_tabs = !app.hide_tab_strip && app.tabs.len() > 1;
    // #1103 (2026-08-20) — the filter toolbar (1 row) sits above the
    // table on every launch mode. Tabs strip (3 rows) sits above the
    // toolbar when it's visible. Table body starts below both.
    let tab_height: u16 = if show_tabs { 3 } else { 0 };
    let toolbar_height: u16 = 1;
    let area_y: u16 = tab_height + toolbar_height;
    // 2026-07-24 — body starts 1 row below area_y when running
    // inside mnml (no border, just the header row), 2 rows below
    // when standalone (border top + header). All the RepoPrTree /
    // RepoTree renderers skip the border when `inside_mnml()`.
    let body_top = area_y + if inside_mnml() { 1 } else { 2 };
    if row < body_top {
        return None;
    }
    // 2026-07-24 — walk the tab's rows accounting for per-row
    // heights. RepoPrTree merged-PR rows are Row::height(2) when
    // expanded (extra line shows the post-merge pipeline). A naive
    // (click_row - body_top) mapping would drift +1 logical row for
    // every expanded PR above the click point.
    let target = (row - body_top) as usize;
    let mut visual = 0usize;
    match &app.active().data {
        crate::app::TabData::RepoPrTree {
            rows,
            expanded,
            show_all,
        } => {
            let mut logical = 0usize;
            for repo in rows {
                if visual + 1 > target && visual <= target {
                    return Some(logical);
                }
                visual += 1;
                logical += 1;
                if expanded.contains(&repo.slug) {
                    // Mirror the visibility policy from
                    // `draw_repo_pr_tree` via the shared helper so
                    // clicks and keyboard nav land on exactly the
                    // rows the renderer shows.
                    for pr in visible_prs_for_render(&repo.prs, app.active(), *show_all) {
                        let is_expanded_pr = pr.state.eq_ignore_ascii_case("MERGED")
                            && pr.merge_commit.is_some()
                            && app.expanded_prs.contains(&(repo.slug.clone(), pr.id));
                        let h = if is_expanded_pr { 2 } else { 1 };
                        if visual + h > target && visual <= target {
                            return Some(logical);
                        }
                        visual += h;
                        logical += 1;
                    }
                }
            }
            // Synthetic "[ Show N older ]" footer row. Present when
            // show_all=false AND some PRs are hidden by the tab's
            // visibility policy (recency on workspace-wide, or the
            // mine-only merged-cap policy).
            let footer_hidden: usize = rows
                .iter()
                .filter(|r| expanded.contains(&r.slug))
                .map(|r| hidden_pr_count_for_render(&r.prs, app.active(), *show_all))
                .sum();
            if !show_all
                && footer_hidden > 0
                && visual + 1 > target
                && visual <= target
            {
                return Some(logical);
            }
            None
        }
        _ => Some(target),
    }
}

/// 2026-07-24 — is `target` the synthetic "[ Show N older PRs ]"
/// footer row on the active RepoPrTree? True only when the footer
/// is being rendered (show_all=false AND hidden_pr_count > 0) AND
/// the index equals the last logical row.
fn is_show_more_footer_row(app: &App, target: usize) -> bool {
    match &app.active().data {
        crate::app::TabData::RepoPrTree {
            rows,
            expanded,
            show_all,
        } => {
            if *show_all {
                return false;
            }
            // 2026-08-21 f/u — hidden count computed via the shared
            // helper so mine_only + workspace-wide use the same
            // logic. Mine tab hides (merged - 1 shown) + declined;
            // workspace-wide hides recency-old PRs.
            let hidden: usize = rows
                .iter()
                .filter(|r| expanded.contains(&r.slug))
                .map(|r| hidden_pr_count_for_render(&r.prs, app.active(), *show_all))
                .sum();
            if hidden == 0 {
                return false;
            }
            let mut logical = 0usize;
            for repo in rows {
                logical += 1; // header
                if expanded.contains(&repo.slug) {
                    for _ in visible_prs_for_render(&repo.prs, app.active(), *show_all) {
                        logical += 1;
                    }
                }
            }
            target == logical
        }
        _ => false,
    }
}

/// Is the given visible-row index a repo header row (as opposed to
/// an expanded branch / PR child row)? Walks the active tab's
/// RepoTree / RepoPrTree accumulating positions.
fn is_repo_header_row(app: &App, target: usize) -> bool {
    let mut pos: usize = 0;
    match &app.active().data {
        crate::app::TabData::RepoTree { rows, expanded } => {
            for r in rows {
                if pos == target {
                    return true;
                }
                pos += 1;
                if expanded.contains(&r.slug) {
                    if target < pos + r.branches.len() {
                        return false;
                    }
                    pos += r.branches.len();
                }
            }
            false
        }
        crate::app::TabData::RepoPrTree { rows, expanded, .. } => {
            for r in rows {
                if pos == target {
                    return true;
                }
                pos += 1;
                if expanded.contains(&r.slug) {
                    if target < pos + r.prs.len() {
                        return false;
                    }
                    pos += r.prs.len();
                }
            }
            false
        }
        _ => false,
    }
}

fn tab_at(col: u16, row: u16, app: &App) -> Option<usize> {
    // Only the label row (inside the top/bottom border) is clickable.
    if row != 1 {
        return None;
    }
    // ratatui `Tabs` adds a default `padding_left(1)` inside the
    // block border, so labels start at col 2 (border=0, pad=1,
    // label=2..). Reviewer caught this against ratatui-0.29 render.
    let mut x: u16 = 2;
    for (i, t) in app.tabs.iter().enumerate() {
        let n = t.data.len();
        let label = if t.last_fetched.is_some() {
            format!("{}.{} ({n})", i + 1, t.name)
        } else {
            format!("{}.{}", i + 1, t.name)
        };
        let w = label.chars().count() as u16;
        // Label spans [x, x+w). Divider " │ " (3 cols) after all
        // but the last label.
        if col >= x && col < x + w {
            return Some(i);
        }
        x += w + 3;
    }
    None
}

/// `MNML_PANE=1` is set by mnml when spawning a Pty. Skip the outer
/// border block in that case — mnml already draws pane borders and a
/// tab label. 2026-07-20.
fn inside_mnml() -> bool {
    std::env::var_os("MNML_PANE").is_some()
}

/// #1045 (2026-08-20) — honor mnml's `[ui] expand_indicator` so
/// the repo-tree headers paint the same shape mnml's own file
/// tree does. mnml stamps `MNML_EXPAND_INDICATOR` on every Pty
/// child. Values: "triangle" → small `▾/▸` (Neovim/kanban look);
/// "chevron" (default, or env unset) → big `▼/▶` matching mnml's
/// non-Nerd fallback for the chevron mode. Standalone launches
/// (no env) fall through to the chevron default.
fn expand_arrow(expanded: bool) -> &'static str {
    let triangle = std::env::var("MNML_EXPAND_INDICATOR")
        .map(|v| v.eq_ignore_ascii_case("triangle"))
        .unwrap_or(false);
    match (expanded, triangle) {
        (true, true) => "▾",
        (false, true) => "▸",
        (true, false) => "▼",
        (false, false) => "▶",
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    // Hide the tab strip when the caller passed `--only <family>` (mnml
    // split chips) or when only a single tab is configured. Both cases
    // = "this session is a single-purpose view; the switcher just
    // wastes a row."
    let show_tabs = !app.hide_tab_strip && app.tabs.len() > 1;
    // #1103 (2026-08-20) — one-row filter toolbar above the table on
    // every launch mode. Replaces the pre-existing state-switcher
    // chip (which only fired in mnml-hosted mode with >1 tab). The
    // toolbar always renders because filter chips are relevant even
    // when the tabs strip is present.
    let show_toolbar = true;
    let tab_height = if show_tabs { 3 } else { 0 };
    let toolbar_height: u16 = if show_toolbar { 1 } else { 0 };
    let chrome_height = tab_height + toolbar_height;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(chrome_height),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);
    if chrome_height > 0 {
        let chrome = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(tab_height),
                Constraint::Length(toolbar_height),
            ])
            .split(chunks[0]);
        if show_tabs {
            draw_tabs(f, chrome[0], app);
        }
        if show_toolbar {
            draw_filter_toolbar(f, chrome[1], app);
        }
    }
    if app.details_visible {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(chunks[1]);
        draw_table(f, body[0], app);
        draw_detail(f, body[1], app);
    } else {
        draw_table(f, chunks[1], app);
    }
    draw_status(f, chunks[2], app);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(key) = app.focused_key() else {
        let p = Paragraph::new("(no PR focused)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" detail "));
        f.render_widget(p, area);
        return;
    };
    let entry = match app.detail_cache.get(&key) {
        Some(e) => e,
        None => {
            let msg = if app.detail_in_flight.as_ref() == Some(&key) {
                "loading detail…"
            } else {
                "(no detail cached — press d to refresh)"
            };
            let p = Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(" detail "));
            f.render_widget(p, area);
            return;
        }
    };
    let pr = &entry.pr;
    let (ws, repo, id) = (&key.0, &key.1, key.2);
    let me_approved = app
        .me_account_id
        .as_deref()
        .map(|m| pr.approved_by(m))
        .unwrap_or(false);
    let approval_chip = if me_approved {
        Span::styled(
            format!("✓ you approved · {} total", pr.approval_count()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!("○ not approved · {} total", pr.approval_count()),
            Style::default().fg(Color::Yellow),
        )
    };
    let title = format!(" {ws}/{repo}#{id} ");

    let header_lines = vec![
        Line::from(vec![
            Span::styled(
                pr.state.clone(),
                Style::default().fg(state_color(&pr.state)),
            ),
            Span::raw(" · "),
            Span::raw(format!(
                "{} → {}",
                pr.source
                    .as_ref()
                    .and_then(|b| b.branch.as_ref().map(|n| n.name.clone()))
                    .unwrap_or_else(|| "?".into()),
                pr.destination
                    .as_ref()
                    .and_then(|b| b.branch.as_ref().map(|n| n.name.clone()))
                    .unwrap_or_else(|| "?".into()),
            )),
        ]),
        Line::from(format!(
            "author: {} · updated: {}",
            pr.author
                .as_ref()
                .map(|u| u.display_name.as_str())
                .unwrap_or("—"),
            pr.updated_date()
        )),
        Line::from(approval_chip),
        Line::from(""),
        Line::from(Span::styled(
            pr.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    let mut body: Vec<Line> = header_lines;
    if let Some(desc) = &pr.description
        && !desc.raw.trim().is_empty()
    {
        for line in desc.raw.lines() {
            body.push(Line::from(line.to_string()));
        }
        body.push(Line::from(""));
    } else {
        body.push(Line::from(Span::styled(
            "(no description)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        body.push(Line::from(""));
    }

    body.push(Line::from(Span::styled(
        format!("comments ({}, most-recent first):", entry.comments.len()),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )));
    body.push(Line::from(""));

    // Bitbucket returns comments oldest-first; reverse so the detail
    // panel matches the jira viewer's most-recent-first convention.
    for c in entry.comments.iter().rev().take(20) {
        let head = Line::from(vec![
            Span::styled(
                format!("  {} · ", c.author()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(c.created_date(), Style::default().fg(Color::DarkGray)),
        ]);
        body.push(head);
        for line in c.body().lines() {
            body.push(Line::from(format!("    {line}")));
        }
        body.push(Line::from(""));
    }

    let block = Block::default().borders(Borders::ALL).title(title);
    let p = Paragraph::new(body)
        .block(block)
        .scroll((app.details_scroll, 0))
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(p, area);
}

fn state_color(state: &str) -> Color {
    match state {
        "OPEN" => Color::Green,
        "MERGED" => Color::Magenta,
        "DECLINED" => Color::Red,
        "SUPERSEDED" => Color::DarkGray,
        _ => Color::Gray,
    }
}

/// #1103 (2026-08-20) — filter toolbar for BB PRs, one row above the
/// table. Mirrors Bitbucket Cloud's PR filter bar:
///   `[ 🔍 Search ]  [ Status: <state> ▾ ]  [ Author ▾ ]  [ Target branch ▾ ]  [ All ▾ ]`
/// Chip visual language is shared with Jira Boards (`[ Label ]` in
/// DarkGray, `[ Label: value ]` in Cyan bold when active).
/// Populates `app.filter_chip_rects` for click routing.
///
/// Wiring status (round 1):
///   - Status chip → cycles to next tab (mirrors old `m` chord;
///     tabs correspond to states: Open+Draft / Merged / …).
///   - All chip → toggles `mine_only` on the active tab, invalidating
///     the fetch so the next refresh re-queries with the new scope.
///   - Search / Author / Target branch → visual only (follow-up).
/// #1103 f/u4 (2026-08-20) — normalize Bitbucket's uppercase PR
/// state (`OPEN`, `MERGED`, `DECLINED`, `SUPERSEDED`) into the
/// title-cased form Bitbucket Cloud's own filter bar shows
/// (`Open`, `Merged`, `Declined`, `Superseded`). Unknown values
/// pass through as-is so a hand-edited config doesn't blank the
/// chip.
fn title_case_state(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase(),
        None => String::new(),
    }
}

fn draw_filter_toolbar(f: &mut Frame, area: Rect, app: &mut App) {
    if area.height == 0 {
        return;
    }
    use crate::app::{FilterChip, TabKind};
    let tab = &app.tabs[app.active_tab];
    // Left-side (filter) entries and right-side (sort/action)
    // entries are laid out from the two ends of the row, per
    // Bitbucket Cloud's own toolbar. Route by tab kind — PR family
    // and pipeline family have different filter sets.
    let (left, right): (
        Vec<(String, bool, FilterChip)>,
        Vec<(String, bool, FilterChip)>,
    ) = match tab.spec.kind {
        // #1116 audit SEV-1 (2026-08-21) — Branches tab was falling
        // into the PR-family `_` arm and rendering `Author`/`Target
        // branch`/`All` chips that make no sense for a branch list
        // (and where clicking `All` guarantees an error toast because
        // `toggle_active_mine_only` explicitly rejects non-PR tabs).
        // Minimal Branches bar: just the Search placeholder + the
        // right-side Refresh.
        TabKind::Branches => (
            vec![("\u{f349} Search".to_string(), false, FilterChip::Search)],
            vec![("\u{f0450} Refresh".to_string(), false, FilterChip::ActionRefresh)],
        ),
        TabKind::Pipelines | TabKind::WorkspacePipelines => (
            vec![
                ("Branch ▾".to_string(), false, FilterChip::Branch),
                (
                    "Pipeline type ▾".to_string(),
                    false,
                    FilterChip::PipelineType,
                ),
                (format!("Status: {} ▾", tab.name), true, FilterChip::Status),
                (
                    "Trigger type ▾".to_string(),
                    false,
                    FilterChip::TriggerType,
                ),
            ],
            vec![
                (
                    "Run pipeline".to_string(),
                    false,
                    FilterChip::ActionRunPipeline,
                ),
                ("Schedules".to_string(), false, FilterChip::ActionSchedules),
                ("Caches".to_string(), false, FilterChip::ActionCaches),
                ("Usage".to_string(), false, FilterChip::ActionUsage),
                ("\u{f0450} Refresh".to_string(), false, FilterChip::ActionRefresh),
            ],
        ),
        _ => {
            // PR family. Status chip = PR state (Open/Merged/Declined).
            // #1103 f/u6 (2026-08-20) — for WorkspaceOpen/Merged
            // tabs, `spec.state` is empty (set by TabSpec::resolve),
            // so derive the value from tab KIND instead. Falls back
            // to spec.state title-cased for per-repo PR tabs.
            let status_value = match tab.spec.kind {
                TabKind::WorkspaceOpenPRs => "Open".to_string(),
                TabKind::WorkspaceMergedPRs => "Merged".to_string(),
                _ => title_case_state(&tab.spec.state),
            };
            let status_label = format!("Status: {status_value} ▾");
            // Author chip: when mine_only, surface the auth user's
            // display name so the toolbar reads intuitively; the
            // "role" axis (`All`) stays a separate chip and always
            // says All. This maps to Bitbucket Cloud's UI where the
            // Author picker independently narrows to a specific
            // person.
            let author_label = if tab.spec.mine_only {
                match app.me_display_name.as_deref() {
                    Some(name) => format!("Author: {name} ▾"),
                    None => "Author: me ▾".to_string(),
                }
            } else {
                "Author ▾".to_string()
            };
            // #1116 audit SEV-1 (2026-08-21) — collapse Author + All
            // into one working chip. Was: Author LOOKED stateful but
            // did nothing on click; All *was* the mine_only toggle but
            // rendered inversely (label said "All" while turning
            // mine_only ON). Now Author owns both label + click, and
            // the standalone `All` chip is gone.
            (
                vec![
                    ("\u{f349} Search".to_string(), false, FilterChip::Search),
                    (status_label, true, FilterChip::Status),
                    (author_label, tab.spec.mine_only, FilterChip::Author),
                    (
                        "Target branch ▾".to_string(),
                        false,
                        FilterChip::TargetBranch,
                    ),
                ],
                // Right side: refresh chip (Jira Work parity). Sort
                // still surfaces on column headers with ▲/▼ direction
                // indicators (follow-up).
                vec![("\u{f0450} Refresh".to_string(), false, FilterChip::ActionRefresh)],
            )
        }
    };
    let chip = |label: &str, active: bool| -> Span<'static> {
        let color = if active { Color::Cyan } else { Color::DarkGray };
        Span::styled(
            format!(" [ {label} ] "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    };
    // #1116 audit SEV-2 (2026-08-21) — mirror Jira Work's Refresh
    // "primary action" pill: reversed Black-on-Cyan so users can pick
    // it out from the dead-placeholder chips next to it.
    let refresh_pill = |label: &str| -> Span<'static> {
        Span::styled(
            format!(" [ {label} ] "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    app.filter_chip_rects.clear();
    // Left-aligned filter chips.
    let mut left_spans: Vec<Span> = Vec::new();
    let mut cursor_x: u16 = area.x;
    for (label, active, kind) in &left {
        let s = chip(label, *active);
        let w = s.content.chars().count() as u16;
        if cursor_x + w <= area.x + area.width {
            app.filter_chip_rects.push((
                Rect {
                    x: cursor_x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                *kind,
            ));
            left_spans.push(s);
            cursor_x += w;
        }
    }
    let left_para = Paragraph::new(Line::from(left_spans));
    f.render_widget(left_para, area);
    // Right-aligned sort/action chips. Compute total width first,
    // then paint into a right-anchored sub-rect. Skips silently if
    // the row is too narrow to hold both sides.
    if !right.is_empty() {
        let right_spans: Vec<Span> = right
            .iter()
            .map(|(label, active, kind)| match kind {
                FilterChip::ActionRefresh => refresh_pill(label),
                _ => chip(label, *active),
            })
            .collect();
        let right_w: u16 = right_spans
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum();
        if cursor_x + right_w + 2 <= area.x + area.width {
            let right_x = area.x + area.width - right_w;
            let right_rect = Rect {
                x: right_x,
                y: area.y,
                width: right_w,
                height: 1,
            };
            // Register hit rects for each right-side chip.
            let mut cx = right_x;
            for (span, (_, _, kind)) in right_spans.iter().zip(right.iter()) {
                let w = span.content.chars().count() as u16;
                app.filter_chip_rects.push((
                    Rect {
                        x: cx,
                        y: area.y,
                        width: w,
                        height: 1,
                    },
                    *kind,
                ));
                cx += w;
            }
            let right_para = Paragraph::new(Line::from(right_spans));
            f.render_widget(right_para, right_rect);
        }
    }
}

/// Return the FilterChip under the given click position, if any.
/// Used by the mouse handler to route clicks to the chip action.
fn filter_chip_at(col: u16, row: u16, app: &App) -> Option<crate::app::FilterChip> {
    app.filter_chip_rects
        .iter()
        .find(|(r, _)| row == r.y && col >= r.x && col < r.x + r.width)
        .map(|(_, k)| *k)
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let n = t.data.len();
            let label = if t.last_fetched.is_some() {
                format!("{}.{} ({n})", i + 1, t.name)
            } else {
                format!("{}.{}", i + 1, t.name)
            };
            Line::from(label)
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" bitbucket "))
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
    // tree-redesign 2026-07-15 user report — every table variant
    // uses fixed-width columns that don't fill the pane. Cells past
    // the last painted column keep whatever was previously in the
    // pty buffer (previous tab's PR-title text bled into the tree
    // as "Schwarzkop lanagan"-shaped garbage on the right half).
    // Explicit Clear wipes the whole body area before the per-
    // variant renderer paints.
    f.render_widget(Clear, area);
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", tab.name)),
            );
        f.render_widget(p, area);
        return;
    }
    if tab.data.is_empty() && tab.last_fetched.is_some() {
        let p = Paragraph::new(empty_message(tab.spec.kind))
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", tab.name)),
            );
        f.render_widget(p, area);
        return;
    }
    if tab.data.is_empty() {
        let p = Paragraph::new("loading…")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", tab.name)),
            );
        f.render_widget(p, area);
        return;
    }
    match &tab.data {
        crate::app::TabData::PullRequests(prs) => draw_pr_table(f, area, tab, prs, app),
        crate::app::TabData::Pipelines(ps) => draw_pipeline_table(f, area, tab, ps),
        crate::app::TabData::Branches(bs) => draw_branch_table(f, area, tab, bs),
        crate::app::TabData::RepoTree { rows, expanded } => {
            draw_repo_tree(f, area, tab, rows, expanded)
        }
        crate::app::TabData::RepoPrTree {
            rows,
            expanded,
            show_all,
        } => draw_repo_pr_tree(f, area, tab, rows, expanded, *show_all, app),
    }
}

/// tree-redesign 2026-07-15 — per-repo PR tree for
/// workspace_open_prs + workspace_merged_prs. Same shape as
/// draw_repo_tree (`▶/▼ repo-slug` header + indented rows on
/// expand) but child rows are PRs with #ID, STATE, AUTHOR,
/// BRANCH, UPDATED, TITLE columns.
fn draw_repo_pr_tree(
    f: &mut Frame,
    area: Rect,
    tab: &crate::app::TabState,
    rows: &[crate::bitbucket::RepoPrs],
    expanded: &std::collections::HashSet<String>,
    show_all: bool,
    app: &App,
) {
    let header = Row::new(vec![
        // 2026-07-20 — header text lines up with the first letter
        // of the repo name below (which sits at col 3 after " ▶ ").
        // User: "on bitbucket start the col header on first letter
        // of repo name".
        Cell::from("   REPO / #PR"),
        Cell::from("STATE"),
        Cell::from("AUTHOR"),
        Cell::from("BRANCH"),
        Cell::from("UPDATED"),
        Cell::from("TITLE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut table_rows: Vec<Row> = Vec::new();
    for repo in rows {
        let arrow = expand_arrow(expanded.contains(&repo.slug));
        // 2026-08-16 (#948) — three header-row shapes now:
        //   normal     → " N PRs " in dim (existing behavior)
        //   errored    → " 429 · retry in 30s " in red so the row
        //                isn't invisible when a fetch fails
        //   empty+last → " last merged " in dim + author/branch/
        //                date/title cells hijacked from the fallback
        //                PR so an idle repo still surfaces something
        //                useful. Visible-row count stays 1 for
        //                empty repos regardless — the fallback is
        //                inline metadata, not a child row (keeps
        //                the click-mapping math intact).
        let slug_style = if repo.error.is_some() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        let (state_cell, author_cell, branch_cell, date_cell, title_cell) =
            if let Some(err) = &repo.error {
                (
                    Cell::from(err.clone()).style(Style::default().fg(Color::Red)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                )
            } else if let (true, Some(fb)) = (repo.prs.is_empty(), repo.fallback_merged.as_ref()) {
                let author = fb
                    .author
                    .as_ref()
                    .map(|u| u.display_name.clone())
                    .unwrap_or_default();
                let branch = fb
                    .source
                    .as_ref()
                    .and_then(|b| b.branch.as_ref())
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                let dim = Style::default().fg(Color::DarkGray);
                (
                    Cell::from("last merged").style(dim),
                    Cell::from(author).style(dim),
                    Cell::from(branch).style(dim),
                    Cell::from(fb.updated_date()).style(dim),
                    Cell::from(format!("#{} · {}", fb.id, fb.title)).style(dim),
                )
            } else {
                // 2026-08-17 — every collapsed row shows at least one
                // PR inline (user report: "each one should have at
                // least one in it"). Prior shape was just "N PRs" in
                // STATE with empty author/branch/date/title cells,
                // making rows outside the 24-hour recency filter look
                // dead. Now the header preview follows the same
                // per-repo shape as the empty+last_merged fallback,
                // sourced from `repo.prs[0]` (the top of the fetch —
                // Bitbucket returns updated_on-descending). Count
                // stays in STATE so callers still see the total.
                let preview = repo.prs.first();
                let author = preview
                    .and_then(|p| p.author.as_ref())
                    .map(|u| u.display_name.clone())
                    .unwrap_or_default();
                let branch = preview
                    .and_then(|p| p.source.as_ref())
                    .and_then(|b| b.branch.as_ref())
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                let date = preview.map(|p| p.updated_date()).unwrap_or_default();
                let title = preview
                    .map(|p| format!("#{} · {}", p.id, p.title))
                    .unwrap_or_default();
                let dim = Style::default().fg(Color::DarkGray);
                (
                    Cell::from(format!("{} PRs", repo.prs.len())).style(dim),
                    Cell::from(author).style(dim),
                    Cell::from(branch).style(dim),
                    Cell::from(date).style(dim),
                    Cell::from(title).style(dim),
                )
            };
        // 2026-07-20 — leading " " on col 0 gives the triangles a
        // one-cell breathing gap from the pane border.
        table_rows.push(Row::new(vec![
            Cell::from(format!(" {arrow} {}", repo.slug)).style(slug_style),
            state_cell,
            author_cell,
            branch_cell,
            date_cell,
            title_cell,
        ]));
        if expanded.contains(&repo.slug) {
            for pr in visible_prs_for_render(&repo.prs, tab, show_all) {
                let author = pr
                    .author
                    .as_ref()
                    .map(|u| u.display_name.clone())
                    .unwrap_or_default();
                let branch = pr
                    .source
                    .as_ref()
                    .and_then(|b| b.branch.as_ref())
                    .map(|b| b.name.clone())
                    .unwrap_or_default();
                let date = pr.updated_date();
                let state_color = pr_state_color(&pr.state);
                // 2026-07-24 — MERGED PRs with a merge_commit get an
                // expand caret + can show a post-merge pipeline
                // sub-line via Row::height(2) when in `expanded_prs`.
                let is_merged_expandable =
                    pr.state.eq_ignore_ascii_case("MERGED") && pr.merge_commit.is_some();
                let key = (repo.slug.clone(), pr.id);
                let is_pr_expanded = is_merged_expandable && app.expanded_prs.contains(&key);
                let pr_arrow = if !is_merged_expandable {
                    "  "
                } else if is_pr_expanded {
                    "▼ "
                } else {
                    "▶ "
                };
                let id_cell_text = format!("   {pr_arrow}#{}", pr.id);
                let title_cell = if is_pr_expanded {
                    render_pr_expand_title_cell(pr, app.pr_pipeline_cache.get(&key))
                } else {
                    Cell::from(pr.title.clone())
                };
                let row = Row::new(vec![
                    Cell::from(id_cell_text).style(Style::default().fg(Color::Yellow)),
                    Cell::from(pr.state.clone()).style(Style::default().fg(state_color)),
                    Cell::from(author),
                    Cell::from(branch),
                    Cell::from(date),
                    title_cell,
                ]);
                let row = if is_pr_expanded { row.height(2) } else { row };
                table_rows.push(row);
            }
        }
    }
    // Synthetic "[ Show N older PRs ]" footer row. Emitted when
    // show_all=false AND the tab's visibility policy hid at least
    // one PR (recency window on workspace-wide, merged-cap on
    // Mine). Click / Enter on this row toggles show_all → true.
    if !show_all {
        let hidden: usize = rows
            .iter()
            .filter(|r| expanded.contains(&r.slug))
            .map(|r| hidden_pr_count_for_render(&r.prs, tab, show_all))
            .sum();
        if hidden > 0 {
            table_rows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(format!(
                    "[ Show {hidden} more {} ]",
                    if tab.spec.mine_only { "merged" } else { "older" }
                ))
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    let widths = [
        Constraint::Length(30),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(22),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    let total_prs: usize = rows.iter().map(|r| r.prs.len()).sum();
    // 2026-07-20 — title reads "Bitbucket — Pull Requests
    // (<mode>) · N repos · N PRs" where <mode> is the active tab's
    // configured name (e.g. "Open + Draft" or "Merged"). Users
    // toggle between them with `m` (see keys.rs), not via a tab
    // strip. Border block only shows in standalone mode; when
    // inside mnml we skip it to avoid a double-border.
    // 2026-08-18 (#998) — `↔` after the tab name signals the `m`
    // toggle exists so mouse users see it's cyclable state, not a
    // fixed label. Full dropdown chip design deferred; this is the
    // minimum discoverability bump.
    let title = format!(
        " Bitbucket Pull Requests ({} ↔) · {} repos · {} PRs ",
        tab.name,
        rows.len(),
        total_prs
    );
    let mut table = Table::new(table_rows, widths).header(header);
    if !inside_mnml() {
        table = table.block(Block::default().borders(Borders::ALL).title(title));
    }
    table = table
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        // 2026-07-19 — was "▸ " which collided visually with the
        // row's own "▶ / ▼" tree caret (user report: "two
        // triangles, one small and one larger"). Empty string —
        // ratatui reserves highlight_symbol width on EVERY row,
        // not just the selected one, so "  " would push all rows
        // over 2 columns and misalign vs the amplify pane. The
        // `row_highlight_style` bg color alone marks selection.
        .highlight_symbol("");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
    render_scrollbar(f, area, tab.data.len(), tab.selected);
}

/// 2026-07-24 — vertical scrollbar over the rightmost column of
/// the table area. No-op when content fits in the viewport.
/// `position` should be the current cursor row (approximation for
/// scroll offset — ratatui's TableState.offset is opaque, but
/// `selected` is a close-enough proxy for "roughly where the user
/// is looking").
fn render_scrollbar(f: &mut Frame, area: Rect, total: usize, position: usize) {
    let vertical_chrome: u16 = if inside_mnml() { 1 } else { 3 };
    let visible = area.height.saturating_sub(vertical_chrome) as usize;
    if total <= visible {
        return;
    }
    let mut sb_state = ScrollbarState::new(total).position(position);
    let sb = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    f.render_stateful_widget(sb, area, &mut sb_state);
}

/// 2026-07-24 — build the TITLE cell for an expanded MERGED PR
/// row: line 1 = title (unchanged), line 2 = post-merge pipeline
/// status (or a "fetching…" / "no pipeline ran" hint). Paired with
/// `Row::height(2)` in the caller so the cursor still lands only on
/// the title line.
fn render_pr_expand_title_cell(
    pr: &crate::bitbucket::PullRequest,
    cached: Option<&Vec<crate::bitbucket::Pipeline>>,
) -> Cell<'static> {
    use ratatui::text::{Line, Span, Text};
    let mut lines: Vec<Line> = vec![Line::from(pr.title.clone())];
    let sha = pr
        .merge_commit
        .as_ref()
        .map(|c| c.hash.chars().take(7).collect::<String>())
        .unwrap_or_default();
    let dest = pr
        .destination
        .as_ref()
        .and_then(|b| b.branch.as_ref().map(|n| n.name.clone()))
        .unwrap_or_else(|| "?".into());
    let sub = match cached {
        None => Line::from(Span::styled(
            format!("  → fetching pipeline for {sha} on {dest}…"),
            Style::default().fg(Color::DarkGray),
        )),
        Some(v) if v.is_empty() => Line::from(Span::styled(
            format!("  → no pipeline ran on {sha} ({dest})"),
            Style::default().fg(Color::DarkGray),
        )),
        Some(v) => {
            let latest = &v[0];
            let label = latest.state_label();
            let (glyph, color) = pipeline_glyph(&label);
            let build = latest.build_number;
            let dur = latest
                .duration_in_seconds
                .map(|s| format!("{}m {}s", s / 60, s % 60))
                .unwrap_or_else(|| "—".to_string());
            let when = latest
                .created_on
                .clone()
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            let more = if v.len() > 1 {
                format!("  (+{} more)", v.len() - 1)
            } else {
                String::new()
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{label:<11} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("#{build}"), Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled(format!("on {dest}"), Style::default().fg(Color::Gray)),
                Span::raw("  "),
                Span::styled(
                    format!("{when}  {dur}{more}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
    };
    lines.push(sub);
    Cell::from(Text::from(lines))
}

fn pipeline_glyph(label: &str) -> (&'static str, Color) {
    match label {
        "SUCCESSFUL" => ("✓", Color::Green),
        "FAILED" | "ERROR" => ("✗", Color::Red),
        "IN_PROGRESS" | "PENDING" => ("⏵", Color::Yellow),
        "STOPPED" | "HALTED" => ("⊘", Color::DarkGray),
        _ => ("?", Color::DarkGray),
    }
}

fn pr_state_color(state: &str) -> Color {
    match state {
        "OPEN" => Color::Green,
        "MERGED" => Color::Magenta,
        "DECLINED" | "SUPERSEDED" => Color::DarkGray,
        _ => Color::Gray,
    }
}

/// tree-redesign 2026-07-14 phase 2c — the mnml-aws-amplify-shaped
/// repo tree. One row per repo (▶ collapsed / ▼ expanded); when
/// expanded, each of the repo's branches gets an indented row with
/// per-branch pipeline status columns:
///
///     ▼ tattle-mobile        d1o7tswuqhnvpi   tattledevs/tattle-mobile
///           main                PRODUCTION      —                       …
///           develop             DEVELOPMENT     —                       #84  SUCCEED  2026-07-10
///           staging             BETA            —                       …
///           feature/TE-13803    DEVELOPMENT     —                       #2   SUCCEED  2026-06-30
///
/// Cursor moves through VISIBLE rows: repo header AND expanded
/// branch rows count as separate stops (matching amplify). Repo
/// headers are highlighted with the `▸` prefix; branch rows use
/// double-indent so hierarchy stays legible even in the highlight
/// row style.
fn draw_repo_tree(
    f: &mut Frame,
    area: Rect,
    tab: &crate::app::TabState,
    rows: &[crate::bitbucket::RepoPipelines],
    expanded: &std::collections::HashSet<String>,
) {
    let header = Row::new(vec![
        Cell::from("   REPO / BRANCH"),
        Cell::from("STATE"),
        Cell::from("BUILD"),
        Cell::from("RESULT"),
        Cell::from("DATE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    // Flatten the (repo, [branches?]) tree into a Vec of Rows so
    // the standard TableState highlight machinery works.
    let mut table_rows: Vec<Row> = Vec::new();
    for repo in rows {
        let arrow = expand_arrow(expanded.contains(&repo.slug));
        // Repo header row: bold, cyan slug, branch count in dim.
        // Leading " " keeps the arrow off the pane border.
        table_rows.push(Row::new(vec![
            Cell::from(format!(" {arrow} {}", repo.slug)).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(format!("{} branches", repo.branches.len()))
                .style(Style::default().fg(Color::DarkGray)),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]));
        if expanded.contains(&repo.slug) {
            for br in &repo.branches {
                let (state, result, build, date) = match &br.latest_pipeline {
                    None => (
                        String::from("—"),
                        String::from(""),
                        String::from(""),
                        String::from(""),
                    ),
                    Some(pl) => (
                        pl.state_only_label(),
                        pl.result_label(),
                        format!("#{}", pl.build_number),
                        pl.created_date(),
                    ),
                };
                let state_color = pipeline_state_color(&state);
                let result_color = pipeline_result_color(&result);
                table_rows.push(Row::new(vec![
                    Cell::from(format!("     {}", br.name)),
                    Cell::from(state).style(Style::default().fg(state_color)),
                    Cell::from(build).style(Style::default().fg(Color::Yellow)),
                    Cell::from(result).style(Style::default().fg(result_color)),
                    Cell::from(date),
                ]));
            }
        }
    }
    let widths = [
        Constraint::Length(40),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(12),
    ];
    // 2026-07-20 — title = "Bitbucket — Pipelines · N repos".
    // Border block skipped inside mnml.
    let title = format!(" Bitbucket Pipelines · {} repos ", rows.len());
    let mut table = Table::new(table_rows, widths).header(header);
    if !inside_mnml() {
        table = table.block(Block::default().borders(Borders::ALL).title(title));
    }
    table = table
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
    render_scrollbar(f, area, tab.data.len(), tab.selected);
}

/// Result-side color: green SUCCESSFUL, red FAILED / STOPPED,
/// grey CANCELLED / other. Kept distinct from `pipeline_state_color`
/// so the state column (PENDING/IN_PROGRESS/COMPLETED) uses a
/// different palette from the outcome column.
fn pipeline_result_color(result: &str) -> Color {
    match result {
        "SUCCESSFUL" | "SUCCEED" | "PASSED" => Color::Green,
        "FAILED" | "ERROR" | "STOPPED" => Color::Red,
        "" | "—" => Color::DarkGray,
        _ => Color::Gray,
    }
}

fn empty_message(kind: crate::app::TabKind) -> &'static str {
    match kind {
        crate::app::TabKind::PullRequests
        | crate::app::TabKind::WorkspaceOpenPRs
        | crate::app::TabKind::WorkspaceMergedPRs => "(no PRs match this tab)",
        crate::app::TabKind::Pipelines => "(no pipelines have run on this repo)",
        crate::app::TabKind::Branches => "(no branches in this repo)",
        crate::app::TabKind::WorkspacePipelines => "(no repos in scope)",
    }
}

fn draw_pr_table(
    f: &mut Frame,
    area: Rect,
    tab: &crate::app::TabState,
    prs: &[crate::bitbucket::PullRequest],
    _app: &App,
) {
    let header = Row::new(vec![
        Cell::from("REPO"),
        Cell::from("PR"),
        Cell::from("STATE"),
        Cell::from("AUTHOR"),
        Cell::from("BRANCH → DEST"),
        Cell::from("UPDATED"),
        Cell::from("TITLE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = prs
        .iter()
        .map(|p| {
            let repo = p.repo_short();
            let key = format!("#{}", p.id);
            let state = p.state.clone();
            let state_style = Style::default().fg(state_color(&state));
            let author = p
                .author
                .as_ref()
                .map(|u| u.display_name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "—".to_string());
            let branches = format!(
                "{} → {}",
                p.source
                    .as_ref()
                    .and_then(|b| b.branch.as_ref().map(|n| n.name.clone()))
                    .unwrap_or_else(|| "?".into()),
                p.destination
                    .as_ref()
                    .and_then(|b| b.branch.as_ref().map(|n| n.name.clone()))
                    .unwrap_or_else(|| "?".into()),
            );
            let updated = p.updated_date();
            Row::new(vec![
                Cell::from(repo),
                Cell::from(key).style(Style::default().fg(Color::Yellow)),
                Cell::from(state).style(state_style),
                Cell::from(author),
                Cell::from(branches),
                Cell::from(updated),
                Cell::from(p.title.clone()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(24),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(16),
        Constraint::Length(28),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    // #988 (2026-08-20) — matches the `if !inside_mnml()` guards on
    // the RepoTree / RepoPrTree tables (draw_repo_pr_tree line 1123,
    // draw_repo_tree line 1353). Was: unconditional Borders::ALL
    // added an extra top-of-block row in inside-mnml mode that
    // `table_row_at`'s body_top math doesn't account for, so every
    // click on a repo-scoped flat table landed on the row BELOW.
    let mut table = Table::new(rows, widths).header(header);
    if !inside_mnml() {
        table = table.block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        );
    }
    let table = table
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        // 2026-07-19 — was "▸ " which collided visually with the
        // row's own "▶ / ▼" tree caret (user report: "two
        // triangles, one small and one larger"). Empty string —
        // ratatui reserves highlight_symbol width on EVERY row,
        // not just the selected one, so "  " would push all rows
        // over 2 columns and misalign vs the amplify pane. The
        // `row_highlight_style` bg color alone marks selection.
        .highlight_symbol("");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
    render_scrollbar(f, area, tab.data.len(), tab.selected);
}

fn draw_pipeline_table(
    f: &mut Frame,
    area: Rect,
    tab: &crate::app::TabState,
    ps: &[crate::bitbucket::Pipeline],
) {
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATE"),
        Cell::from("BRANCH"),
        Cell::from("COMMIT"),
        Cell::from("TRIGGER"),
        Cell::from("DURATION"),
        Cell::from("CREATED"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = ps
        .iter()
        .map(|p| {
            let state = p.state_label();
            let state_style = Style::default().fg(pipeline_state_color(&state));
            Row::new(vec![
                Cell::from(format!("#{}", p.build_number))
                    .style(Style::default().fg(Color::Yellow)),
                Cell::from(state).style(state_style),
                Cell::from(p.branch_label()),
                Cell::from(p.short_sha()),
                Cell::from(p.trigger_label()),
                Cell::from(p.duration_label()),
                Cell::from(p.created_date()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(24),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(12),
    ];
    // #988 (2026-08-20) — inside-mnml border guard, see draw_pr_table.
    let mut table = Table::new(rows, widths).header(header);
    if !inside_mnml() {
        table = table.block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        );
    }
    let table = table
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        // 2026-07-19 — was "▸ " which collided visually with the
        // row's own "▶ / ▼" tree caret (user report: "two
        // triangles, one small and one larger"). Empty string —
        // ratatui reserves highlight_symbol width on EVERY row,
        // not just the selected one, so "  " would push all rows
        // over 2 columns and misalign vs the amplify pane. The
        // `row_highlight_style` bg color alone marks selection.
        .highlight_symbol("");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
    render_scrollbar(f, area, tab.data.len(), tab.selected);
}

fn draw_branch_table(
    f: &mut Frame,
    area: Rect,
    tab: &crate::app::TabState,
    bs: &[crate::bitbucket::BranchRef],
) {
    let header = Row::new(vec![
        Cell::from("BRANCH"),
        Cell::from("COMMIT"),
        Cell::from("LATEST"),
        Cell::from("AUTHOR"),
        Cell::from("MESSAGE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = bs
        .iter()
        .map(|b| {
            Row::new(vec![
                Cell::from(b.name.clone()),
                Cell::from(b.short_sha()).style(Style::default().fg(Color::Yellow)),
                Cell::from(b.latest_date()),
                Cell::from(b.author_label()),
                Cell::from(b.summary_line()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(32),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(20),
        Constraint::Min(20),
    ];
    // #988 (2026-08-20) — inside-mnml border guard, see draw_pr_table.
    let mut table = Table::new(rows, widths).header(header);
    if !inside_mnml() {
        table = table.block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        );
    }
    let table = table
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        // 2026-07-19 — was "▸ " which collided visually with the
        // row's own "▶ / ▼" tree caret (user report: "two
        // triangles, one small and one larger"). Empty string —
        // ratatui reserves highlight_symbol width on EVERY row,
        // not just the selected one, so "  " would push all rows
        // over 2 columns and misalign vs the amplify pane. The
        // `row_highlight_style` bg color alone marks selection.
        .highlight_symbol("");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
    render_scrollbar(f, area, tab.data.len(), tab.selected);
}

fn pipeline_state_color(state: &str) -> Color {
    match state {
        "SUCCESSFUL" => Color::Green,
        "FAILED" | "ERROR" => Color::Red,
        "STOPPED" | "HALTED" => Color::DarkGray,
        "IN_PROGRESS" | "PENDING" | "RUNNING" => Color::Yellow,
        _ => Color::Gray,
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &mut App) {
    // Status prefix on the left — same as before, uncolored.
    let status_prefix = format!(" {} ", app.status);
    let status_line = Line::from(Span::styled(
        status_prefix.clone(),
        Style::default().fg(Color::White),
    ));
    f.render_widget(Paragraph::new(status_line), area);

    // #1000 (2026-08-18) — split the hint bar into per-chord chips
    // and register a click rect for each so mouse users get parity
    // with the keyboard. Chords with no single keystroke to
    // synthesize (like "1-9 tab", which is a range) fall back to
    // static labels with no click target — the affordance still
    // reads; the user can also click the tab strip. Layout: chord
    // label in a brighter Gray, description in the previous dim
    // DarkGray, each chip separated by " · " painted dim. Cluster
    // right-aligned; drops chips from the FRONT under overflow so
    // quit / refresh stay visible on narrow panes.
    let chips = footer_chips();
    app.hint_chip_rects.clear();
    if area.width == 0 || chips.is_empty() {
        return;
    }
    let prefix_w = status_prefix.chars().count() as u16;
    let budget = area.width.saturating_sub(prefix_w);
    if budget == 0 {
        return;
    }
    let sep = " · ";
    let sep_w = sep.chars().count() as u16;
    let mut total: u16 = 0;
    let mut widths: Vec<u16> = Vec::with_capacity(chips.len());
    for (i, chip) in chips.iter().enumerate() {
        let cw = chip.width();
        widths.push(cw);
        total = total.saturating_add(cw);
        if i + 1 < chips.len() {
            total = total.saturating_add(sep_w);
        }
    }
    let mut first_visible = 0usize;
    while total > budget && first_visible < chips.len() {
        total = total.saturating_sub(widths[first_visible]);
        if first_visible + 1 < chips.len() {
            total = total.saturating_sub(sep_w);
        }
        first_visible += 1;
    }
    if first_visible >= chips.len() {
        return;
    }
    let mut x = area.x + area.width.saturating_sub(total);
    let y = area.y;
    for (i, chip) in chips.iter().enumerate().skip(first_visible) {
        let chip_w = widths[i];
        let chip_rect = Rect {
            x,
            y,
            width: chip_w,
            height: 1,
        };
        f.render_widget(Paragraph::new(chip.render()), chip_rect);
        if let Some(key) = chip.key {
            app.hint_chip_rects.push((chip_rect, key));
        }
        x = x.saturating_add(chip_w);
        if i + 1 < chips.len() {
            let sep_rect = Rect {
                x,
                y,
                width: sep_w,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    sep,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ))),
                sep_rect,
            );
            x = x.saturating_add(sep_w);
        }
    }
}

/// One clickable footer chip. `key` is `None` for advisory chips
/// (e.g. "1-9 tab" — a range, not a single key) so we still paint
/// the label but skip click registration.
struct FooterChip {
    key_label: &'static str,
    description: &'static str,
    key: Option<crossterm::event::KeyEvent>,
}

impl FooterChip {
    fn width(&self) -> u16 {
        // " <key> <desc> " — key + space + desc, with a leading and
        // trailing space so the pill has breathing room.
        (self.key_label.chars().count() + self.description.chars().count() + 3) as u16
    }
    fn render(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                self.key_label.to_string(),
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                self.description.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::raw(" "),
        ])
    }
}

fn footer_chips() -> Vec<FooterChip> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ke = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
    vec![
        // Range-only chip — no single keystroke to synthesize.
        FooterChip {
            key_label: "1-9",
            description: "tab",
            key: None,
        },
        FooterChip {
            key_label: "↑↓/jk",
            description: "move",
            key: Some(ke(KeyCode::Down)),
        },
        FooterChip {
            key_label: "↵",
            description: "expand",
            key: Some(ke(KeyCode::Enter)),
        },
        FooterChip {
            key_label: "o",
            description: "open on web",
            key: Some(ke(KeyCode::Char('o'))),
        },
        FooterChip {
            key_label: "d",
            description: "detail",
            key: Some(ke(KeyCode::Char('d'))),
        },
        FooterChip {
            key_label: "a",
            description: "approve",
            key: Some(ke(KeyCode::Char('a'))),
        },
        FooterChip {
            key_label: "m",
            description: "open↔merged",
            key: Some(ke(KeyCode::Char('m'))),
        },
        FooterChip {
            key_label: "r",
            description: "refresh",
            key: Some(ke(KeyCode::Char('r'))),
        },
        FooterChip {
            key_label: "q",
            description: "quit",
            key: Some(ke(KeyCode::Char('q'))),
        },
    ]
}
