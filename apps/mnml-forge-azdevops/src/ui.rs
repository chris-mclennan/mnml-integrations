//! ratatui rendering + the main event loop.

use crate::app::{App, TabData, TabState};
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
    draw_table(f, chunks[1], app);
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
        .block(Block::default().borders(Borders::ALL).title(" azure devops "))
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
    if tab.data.is_empty() && tab.last_fetched.is_some() {
        let empty_text = match &tab.data {
            TabData::PullRequests(_) => "(no PRs match this filter)",
            TabData::Builds(_) => "(no recent builds)",
        };
        let p = Paragraph::new(empty_text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if tab.data.is_empty() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    match &tab.data {
        TabData::PullRequests(_) => draw_prs_table(f, area, tab),
        TabData::Builds(_) => draw_builds_table(f, area, tab),
    }
}

fn draw_prs_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let prs = match &tab.data {
        TabData::PullRequests(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("ID"),
        Cell::from("STATUS"),
        Cell::from("REPO"),
        Cell::from("SRC → DEST"),
        Cell::from("AUTHOR"),
        Cell::from("CREATED"),
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
            let id = format!("#{}", p.id);
            let status = p.status.clone();
            let status_style = match status.as_str() {
                "active" => Style::default().fg(Color::Green),
                "completed" => Style::default().fg(Color::Blue),
                "abandoned" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
            };
            let repo = p.repository.as_ref().map(|r| r.name.clone()).unwrap_or_default();
            let branches = format!(
                "{} → {}",
                p.source_branch_short(),
                p.target_branch_short()
            );
            let author = p
                .created_by
                .as_ref()
                .map(|i| i.display_name.clone())
                .unwrap_or_else(|| "—".into());
            let created = p
                .creation_date
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".into());
            Row::new(vec![
                Cell::from(id).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(repo),
                Cell::from(branches),
                Cell::from(author),
                Cell::from(created),
                Cell::from(p.title.clone()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(28),
        Constraint::Length(20),
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

fn draw_builds_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let builds = match &tab.data {
        TabData::Builds(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATUS"),
        Cell::from("DEFINITION"),
        Cell::from("BRANCH"),
        Cell::from("REQUESTED BY"),
        Cell::from("QUEUED"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = builds
        .iter()
        .map(|b| {
            let n = b.build_number.clone();
            let status = b.status_chip().to_string();
            let status_style = match status.as_str() {
                "succeeded" => Style::default().fg(Color::Green),
                "failed" => Style::default().fg(Color::Red),
                "canceled" | "cancelling" => Style::default().fg(Color::Yellow),
                "partiallySucceeded" => Style::default().fg(Color::Magenta),
                "running" | "inProgress" => Style::default().fg(Color::Cyan),
                "queued" | "notStarted" => Style::default().fg(Color::Blue),
                _ => Style::default().fg(Color::Gray),
            };
            let def = b
                .definition
                .as_ref()
                .map(|d| d.name.clone())
                .unwrap_or_default();
            let branch = b.source_branch_short();
            let actor = b
                .requested_for
                .as_ref()
                .map(|i| i.display_name.clone())
                .unwrap_or_else(|| "—".into());
            let queued = b
                .queue_time
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".into());
            Row::new(vec![
                Cell::from(n).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(def),
                Cell::from(branch),
                Cell::from(actor),
                Cell::from(queued),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(16),
        Constraint::Length(12),
        Constraint::Length(24),
        Constraint::Length(22),
        Constraint::Length(20),
        Constraint::Length(12),
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

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter/o open · r refresh · q quit ";
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
