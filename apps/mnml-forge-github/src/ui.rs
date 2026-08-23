//! ratatui rendering + the main event loop.

use crate::app::{App, TabData, TabKind, TabState, count_recent_prs};
use crate::github::{PullRequest, RepoActions, RepoPrs};
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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
};
use std::collections::HashSet;
use std::io::Stdout;
use std::time::{Duration, Instant};

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
                Event::Resize(_, _) => {}
                _ => {}
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
    draw_body(f, chunks[1], app);
    draw_status(f, chunks[2], app);
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
        .block(Block::default().borders(Borders::ALL).title(" github "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if tab.data.is_empty() && tab.last_fetched.is_none() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if tab.data.is_empty() {
        let empty = match &tab.data {
            TabData::Issues(_) => "(no issues match this query)",
            TabData::Actions(_) => "(no recent runs for this repo)",
            TabData::RepoPrTree { .. } => "(no repos in the current scope)",
            TabData::RepoActionsTree { .. } => "(no repos in the current scope)",
        };
        let p = Paragraph::new(empty).style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    match &tab.data {
        TabData::Issues(_) => draw_issues_table(f, area, tab),
        TabData::Actions(_) => draw_actions_table(f, area, tab),
        TabData::RepoPrTree {
            rows,
            expanded,
            show_all,
        } => draw_repo_pr_tree(f, area, tab, rows, expanded, *show_all),
        TabData::RepoActionsTree { rows, expanded } => {
            draw_repo_actions_tree(f, area, tab, rows, expanded)
        }
    }
}

fn draw_issues_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let items = match &tab.data {
        TabData::Issues(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("KIND"),
        Cell::from("REPO"),
        Cell::from("KEY"),
        Cell::from("STATE"),
        Cell::from("AUTHOR"),
        Cell::from("UPDATED"),
        Cell::from("TITLE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = items
        .iter()
        .map(|i| {
            let kind = if i.is_pr() { "PR" } else { "issue" };
            let kind_style = if i.is_pr() {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let author = i
                .user
                .as_ref()
                .map(|u| u.login.clone())
                .unwrap_or_else(|| "—".to_string());
            let updated = i
                .updated_at
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".to_string());
            let state_style = match i.state.as_str() {
                "open" => Style::default().fg(Color::Green),
                "closed" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
            };
            Row::new(vec![
                Cell::from(kind.to_string()).style(kind_style),
                Cell::from(i.repo_short()),
                Cell::from(format!("#{}", i.number)).style(Style::default().fg(Color::Yellow)),
                Cell::from(i.state.clone()).style(state_style),
                Cell::from(author),
                Cell::from(updated),
                Cell::from(i.title.clone()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(6),
        Constraint::Length(22),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_actions_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let runs = match &tab.data {
        TabData::Actions(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATUS"),
        Cell::from("EVENT"),
        Cell::from("BRANCH"),
        Cell::from("ACTOR"),
        Cell::from("UPDATED"),
        Cell::from("WORKFLOW"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = runs
        .iter()
        .map(|r| {
            let status = r.status_chip().to_string();
            let status_style = status_color(&status);
            let branch = r.head_branch.clone().unwrap_or_else(|| "—".to_string());
            let actor = r
                .actor
                .as_ref()
                .map(|u| u.login.clone())
                .unwrap_or_else(|| "—".to_string());
            let updated = r
                .updated_at
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".to_string());
            let title = r
                .display_title
                .clone()
                .or_else(|| r.name.clone())
                .unwrap_or_else(|| format!("run {}", r.id));
            Row::new(vec![
                Cell::from(format!("#{}", r.run_number)).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(r.event.clone()),
                Cell::from(branch),
                Cell::from(actor),
                Cell::from(updated),
                Cell::from(title),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(16),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_repo_pr_tree(
    f: &mut Frame,
    area: Rect,
    tab: &TabState,
    rows: &[RepoPrs],
    expanded: &HashSet<String>,
    show_all: bool,
) {
    let header = Row::new(vec![
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
            } else {
                let preview = repo.prs.first();
                let dim = Style::default().fg(Color::DarkGray);
                let author = preview
                    .and_then(|p| p.user.as_ref())
                    .map(|u| u.login.clone())
                    .unwrap_or_default();
                let branch = preview
                    .and_then(|p| p.head.as_ref())
                    .map(|h| h.git_ref.clone())
                    .unwrap_or_default();
                let date = preview
                    .and_then(|p| p.updated_at.as_deref())
                    .map(format_date)
                    .unwrap_or_default();
                let title = preview
                    .map(|p| format!("#{} · {}", p.number, p.title))
                    .unwrap_or_default();
                (
                    Cell::from(format!("{} PRs", repo.prs.len())).style(dim),
                    Cell::from(author).style(dim),
                    Cell::from(branch).style(dim),
                    Cell::from(date).style(dim),
                    Cell::from(title).style(dim),
                )
            };
        table_rows.push(Row::new(vec![
            Cell::from(format!(" {arrow} {}", repo.slug)).style(slug_style),
            state_cell,
            author_cell,
            branch_cell,
            date_cell,
            title_cell,
        ]));
        if expanded.contains(&repo.slug) {
            let (visible, _hidden) = count_recent_prs(&repo.prs, show_all);
            for pr in visible_prs_for_render(&repo.prs, show_all).take(visible) {
                let author = pr
                    .user
                    .as_ref()
                    .map(|u| u.login.clone())
                    .unwrap_or_default();
                let branch = pr
                    .head
                    .as_ref()
                    .map(|h| h.git_ref.clone())
                    .unwrap_or_default();
                let date = pr
                    .updated_at
                    .as_deref()
                    .map(format_date)
                    .unwrap_or_default();
                let state = pr.state_chip();
                table_rows.push(Row::new(vec![
                    Cell::from(format!("   #{}", pr.number))
                        .style(Style::default().fg(Color::Yellow)),
                    Cell::from(state.to_string()).style(Style::default().fg(pr_state_color(state))),
                    Cell::from(author),
                    Cell::from(branch),
                    Cell::from(date),
                    Cell::from(pr.title.clone()),
                ]));
            }
        }
    }
    // Synthetic "[ Show N older PRs ]" footer.
    if !show_all {
        let hidden: usize = rows
            .iter()
            .filter(|r| expanded.contains(&r.slug))
            .map(|r| count_recent_prs(&r.prs, false).1)
            .sum();
        if hidden > 0 {
            table_rows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(format!("[ Show {hidden} older ]")).style(
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
    let mine_chip = if tab.spec.kind == TabKind::WorkspaceOpenPrs
        || tab.spec.kind == TabKind::WorkspaceMergedPrs
    {
        if tab.spec.mine_only { " · mine" } else { "" }
    } else {
        ""
    };
    let title = format!(
        " GitHub Pull Requests ({}{}) · {} repos · {} PRs ",
        tab.name,
        mine_chip,
        rows.len(),
        total_prs
    );
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
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_repo_actions_tree(
    f: &mut Frame,
    area: Rect,
    tab: &TabState,
    rows: &[RepoActions],
    expanded: &HashSet<String>,
) {
    let header = Row::new(vec![
        Cell::from("   REPO / RUN"),
        Cell::from("STATUS"),
        Cell::from("EVENT"),
        Cell::from("BRANCH"),
        Cell::from("ACTOR"),
        Cell::from("UPDATED"),
        Cell::from("WORKFLOW"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut table_rows: Vec<Row> = Vec::new();
    for repo in rows {
        let arrow = expand_arrow(expanded.contains(&repo.slug));
        let slug_style = if repo.error.is_some() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        let (status_cell, event_cell, branch_cell, actor_cell, date_cell, wf_cell) =
            if let Some(err) = &repo.error {
                (
                    Cell::from(err.clone()).style(Style::default().fg(Color::Red)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                )
            } else if let Some(latest) = repo.runs.first() {
                let dim = Style::default().fg(Color::DarkGray);
                let status = latest.status_chip().to_string();
                let branch = latest.head_branch.clone().unwrap_or_default();
                let actor = latest
                    .actor
                    .as_ref()
                    .map(|u| u.login.clone())
                    .unwrap_or_default();
                let date = latest
                    .updated_at
                    .as_deref()
                    .map(format_date)
                    .unwrap_or_default();
                let wf = latest
                    .display_title
                    .clone()
                    .or_else(|| latest.name.clone())
                    .unwrap_or_default();
                (
                    Cell::from(status).style(status_color(latest.status_chip())),
                    Cell::from(latest.event.clone()).style(dim),
                    Cell::from(branch).style(dim),
                    Cell::from(actor).style(dim),
                    Cell::from(date).style(dim),
                    Cell::from(wf).style(dim),
                )
            } else {
                let dim = Style::default().fg(Color::DarkGray);
                (
                    Cell::from("no runs").style(dim),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                )
            };
        table_rows.push(Row::new(vec![
            Cell::from(format!(" {arrow} {}", repo.slug)).style(slug_style),
            status_cell,
            event_cell,
            branch_cell,
            actor_cell,
            date_cell,
            wf_cell,
        ]));
        if expanded.contains(&repo.slug) {
            for run in &repo.runs {
                let status = run.status_chip().to_string();
                let branch = run.head_branch.clone().unwrap_or_default();
                let actor = run
                    .actor
                    .as_ref()
                    .map(|u| u.login.clone())
                    .unwrap_or_default();
                let date = run
                    .updated_at
                    .as_deref()
                    .map(format_date)
                    .unwrap_or_default();
                let wf = run
                    .display_title
                    .clone()
                    .or_else(|| run.name.clone())
                    .unwrap_or_default();
                table_rows.push(Row::new(vec![
                    Cell::from(format!("   #{}", run.run_number))
                        .style(Style::default().fg(Color::Yellow)),
                    Cell::from(status.clone()).style(status_color(&status)),
                    Cell::from(run.event.clone()),
                    Cell::from(branch),
                    Cell::from(actor),
                    Cell::from(date),
                    Cell::from(wf),
                ]));
            }
        }
    }
    let widths = [
        Constraint::Length(30),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(22),
        Constraint::Length(16),
        Constraint::Length(12),
        Constraint::Min(20),
    ];
    let title = format!(" GitHub Actions ({}) · {} repos ", tab.name, rows.len());
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
    state.select(Some(tab.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = if app.on_tree() {
        " 1-9 tab · ←→/hl expand · Enter/Space toggle · e/c all · x hide · H unhide · s scope · m mine · r refresh · q quit "
    } else {
        " 1-9 tab · ↑↓/jk move · Enter/o open · y yank · r refresh · q quit "
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

fn format_date(s: &str) -> String {
    s.split('T').next().unwrap_or(s).to_string()
}

fn status_color(chip: &str) -> Style {
    match chip {
        "success" => Style::default().fg(Color::Green),
        "failure" => Style::default().fg(Color::Red),
        "cancelled" => Style::default().fg(Color::Yellow),
        "running" | "in_progress" => Style::default().fg(Color::Cyan),
        "queued" => Style::default().fg(Color::Blue),
        _ => Style::default().fg(Color::Gray),
    }
}

fn pr_state_color(chip: &str) -> Color {
    match chip {
        "merged" => Color::Magenta,
        "closed" => Color::DarkGray,
        "draft" => Color::Yellow,
        _ => Color::Green,
    }
}

fn expand_arrow(expanded: bool) -> &'static str {
    if expanded { "▼" } else { "▶" }
}

/// Iterator over the visible PRs in a repo, respecting the recency
/// filter. Extracted so `focused_pr` (in `app.rs`) and the tree
/// renderer stay in sync.
fn visible_prs_for_render(
    prs: &[PullRequest],
    show_all: bool,
) -> impl Iterator<Item = &PullRequest> {
    prs.iter().filter(move |p| {
        if show_all {
            return true;
        }
        let ts = p.merged_at.as_deref().or(p.updated_at.as_deref());
        ts.and_then(crate::app::hours_since)
            .map(|h| h <= crate::app::RECENT_WINDOW_HOURS)
            .unwrap_or(true)
    })
}
