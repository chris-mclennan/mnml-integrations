//! ratatui rendering + the main event loop.

use crate::app::App;
use crate::keys;
use crate::trace::EventKind;
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::io::Stdout;
use std::time::Duration;

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
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
        {
            if let Some(action) = keys::handle(key, app) {
                let quit = keys::apply(action, app);
                if quit {
                    break;
                }
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
    draw_header(f, chunks[0], app);
    draw_events(f, chunks[1], app);
    draw_status(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let p = &app.pane;
    let chips: Vec<Span> = p
        .filter
        .header_chips()
        .iter()
        .map(|(label, on)| {
            let style = if *on {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM)
            };
            Span::styled(format!(" {label} "), style)
        })
        .collect();
    let mut line = vec![
        Span::styled(
            format!(" {} ", p.test_title),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · filters:"),
    ];
    line.extend(chips);
    let block = Block::default().borders(Borders::ALL).title(" trace ");
    let para = Paragraph::new(Line::from(line)).block(block);
    f.render_widget(para, area);
}

fn draw_events(f: &mut Frame, area: Rect, app: &App) {
    let p = &app.pane;
    let vis = p.visible_indices();
    if vis.is_empty() {
        let para = Paragraph::new(if p.events.is_empty() {
            "(no events parsed from this trace.zip — file might not be a Playwright trace)"
        } else {
            "(everything filtered — toggle a kind with a / c / e / s, or `R` to show all)"
        })
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(para, area);
        return;
    }
    let body_rows = area.height.saturating_sub(2) as usize;
    let sel_in_vis = vis.iter().position(|&idx| idx == p.selected).unwrap_or(0);
    let items: Vec<ListItem> = vis
        .iter()
        .map(|&idx| {
            let ev = &p.events[idx];
            let glyph_style = match ev.kind {
                EventKind::Action => Style::default().fg(Color::Cyan),
                EventKind::Console => Style::default().fg(Color::Gray),
                EventKind::Error => Style::default().fg(Color::Red),
                EventKind::Stdio => Style::default().fg(Color::Yellow),
            };
            let line = Line::from(vec![
                Span::styled(format!("{} ", ev.kind.glyph()), glyph_style),
                Span::styled(
                    format!("{:>8.3}s  ", ev.at_ms as f64 / 1000.0),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(ev.title.clone()),
            ]);
            ListItem::new(line)
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " events ({}/{})",
            vis.len(),
            p.events.len()
        )))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    // Clamp the rendered window so the selection is on-screen without
    // ratatui doing its own scroll math (ratatui's ListState defaults
    // are good enough — we just seed the offset).
    let offset = sel_in_vis.saturating_sub(body_rows / 2);
    *state.offset_mut() = offset;
    state.select(Some(sel_in_vis));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint =
        " ↑↓/jk move · a/c/e/s toggle kind · E errors-only · R show-all · r reload · q quit ";
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
