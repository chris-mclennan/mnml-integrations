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
        .block(Block::default().borders(Borders::ALL).title(" gitlab "))
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
            TabData::MergeRequests(_) => "(no MRs match this filter)",
            TabData::Pipelines(_) => "(no recent pipelines)",
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
        TabData::MergeRequests(_) => draw_mrs_table(f, area, tab),
        TabData::Pipelines(_) => draw_pipelines_table(f, area, tab),
    }
}

fn draw_mrs_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let mrs = match &tab.data {
        TabData::MergeRequests(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("!"),
        Cell::from("STATE"),
        Cell::from("PROJECT"),
        Cell::from("SRC → DEST"),
        Cell::from("AUTHOR"),
        Cell::from("UPDATED"),
        Cell::from("TITLE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = mrs
        .iter()
        .map(|m| {
            let iid = format!("!{}", m.iid);
            let state_style = match m.state.as_str() {
                "opened" => Style::default().fg(Color::Green),
                "merged" => Style::default().fg(Color::Blue),
                "closed" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
            };
            let proj = m.project_path_from_url();
            let branches = format!(
                "{} → {}",
                m.source_branch.as_deref().unwrap_or("?"),
                m.target_branch.as_deref().unwrap_or("?")
            );
            let author = m
                .author
                .as_ref()
                .and_then(|u| u.name.clone().or(Some(u.username.clone())))
                .unwrap_or_else(|| "—".into());
            let updated = m
                .updated_at
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".into());
            let title = if m.draft {
                format!("Draft: {}", m.title)
            } else {
                m.title.clone()
            };
            Row::new(vec![
                Cell::from(iid).style(Style::default().fg(Color::Yellow)),
                Cell::from(m.state.clone()).style(state_style),
                Cell::from(proj),
                Cell::from(branches),
                Cell::from(author),
                Cell::from(updated),
                Cell::from(title),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(22),
        Constraint::Length(28),
        Constraint::Length(18),
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

fn draw_pipelines_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let ps = match &tab.data {
        TabData::Pipelines(v) => v,
        _ => return,
    };
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATUS"),
        Cell::from("REF"),
        Cell::from("SHA"),
        Cell::from("SOURCE"),
        Cell::from("UPDATED"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = ps
        .iter()
        .map(|p| {
            let n = format!("#{}", p.id);
            let status = p.status_chip().to_string();
            let status_style = match status.as_str() {
                "success" => Style::default().fg(Color::Green),
                "failed" => Style::default().fg(Color::Red),
                "canceled" => Style::default().fg(Color::Yellow),
                "running" | "pending" | "waiting_for_resource" | "preparing" => {
                    Style::default().fg(Color::Cyan)
                }
                "manual" => Style::default().fg(Color::Magenta),
                "skipped" => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::Gray),
            };
            let r = p.r#ref.clone().unwrap_or_else(|| "—".into());
            let sha = p
                .sha
                .chars()
                .take(8)
                .collect::<String>();
            let source = p.source.clone().unwrap_or_default();
            let updated = p
                .updated_at
                .as_deref()
                .map(format_date)
                .unwrap_or_else(|| "—".into());
            Row::new(vec![
                Cell::from(n).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(r),
                Cell::from(sha),
                Cell::from(source),
                Cell::from(updated),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(24),
        Constraint::Length(10),
        Constraint::Length(14),
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
