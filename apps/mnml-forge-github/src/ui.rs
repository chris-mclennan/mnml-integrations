//! ratatui rendering + the main event loop.

use crate::app::{App, TabRows, TabState};
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
            let n = t.rows.len();
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

fn draw_table(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if tab.rows.is_empty() && tab.last_fetched.is_some() {
        let empty_text = match &tab.rows {
            TabRows::Issues(_) => "(no issues match this query)",
            TabRows::Actions(_) => "(no recent runs for this repo)",
        };
        let p = Paragraph::new(empty_text).style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if tab.rows.is_empty() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    match &tab.rows {
        TabRows::Issues(_) => draw_issues_table(f, area, tab),
        TabRows::Actions(_) => draw_actions_table(f, area, tab),
    }
}

fn draw_issues_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let items = match &tab.rows {
        TabRows::Issues(v) => v,
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
            let repo = i.repo_short();
            let key = format!("#{}", i.number);
            let state = i.state.clone();
            let state_style = match state.as_str() {
                "open" => Style::default().fg(Color::Green),
                "closed" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
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
            Row::new(vec![
                Cell::from(kind.to_string()).style(kind_style),
                Cell::from(repo),
                Cell::from(key).style(Style::default().fg(Color::Yellow)),
                Cell::from(state).style(state_style),
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
    let runs = match &tab.rows {
        TabRows::Actions(v) => v,
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
            let n = format!("#{}", r.run_number);
            let status = r.status_chip().to_string();
            let status_style = match status.as_str() {
                "success" => Style::default().fg(Color::Green),
                "failure" => Style::default().fg(Color::Red),
                "cancelled" => Style::default().fg(Color::Yellow),
                "running" | "in_progress" => Style::default().fg(Color::Cyan),
                "queued" => Style::default().fg(Color::Blue),
                _ => Style::default().fg(Color::Gray),
            };
            let event = r.event.clone();
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
                Cell::from(n).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(event),
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
