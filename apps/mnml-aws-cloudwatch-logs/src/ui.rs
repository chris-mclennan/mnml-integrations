//! ratatui rendering + the main event loop.

use crate::app::{App, TabState};
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
    widgets::{Block, Borders, Paragraph, Tabs},
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
        app.drain();
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && let Some(action) = keys::handle(key, app)
        {
            let quit = keys::apply(action, app).await;
            if quit {
                break;
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
    draw_logs(f, chunks[1], app.active());
    draw_status(f, chunks[2], app);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let suffix = if t.data.pane.is_some() {
                " · tailing"
            } else {
                ""
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, suffix))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" cloudwatch logs "),
        )
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_logs(f: &mut Frame, area: Rect, tab: &TabState) {
    if let Some(err) = &tab.data.last_error {
        // Common case: the launching sibling passed a log group that
        // doesn't exist yet (branch that never built, resource that
        // never emitted logs). AWS' raw message is unhelpful — swap
        // it for something that names the group and hints at why.
        let text = if err.contains("ResourceNotFoundException") || err.contains("does not exist") {
            format!(
                "log group `{}` not found in region {}\n\nlikely causes:\n  • no logs written yet (resource hasn't run / built)\n  • different region — pass --region\n  • retention policy expired the group\n\npress q to quit and try again once logs exist",
                tab.spec.log_group,
                tab.spec.region.as_deref().unwrap_or("<default>"),
            )
        } else {
            format!("error: {err}")
        };
        let p = Paragraph::new(text)
            .style(Style::default().fg(Color::Red))
            .wrap(ratatui::widgets::Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let Some(pane) = tab.data.pane.as_ref() else {
        let p = Paragraph::new("(spawning `aws logs tail`…)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    };
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = pane.lines.len();
    let start = if pane.scroll == usize::MAX {
        total.saturating_sub(body_rows)
    } else {
        pane.scroll.min(total.saturating_sub(body_rows.max(1)))
    };
    let lines: Vec<Line> = pane.lines[start..]
        .iter()
        .take(body_rows)
        .map(|ln| {
            let style = match ln.severity {
                crate::log_tail::LineSeverity::Error => Style::default().fg(Color::Red),
                crate::log_tail::LineSeverity::Warn => Style::default().fg(Color::Yellow),
                crate::log_tail::LineSeverity::Info => Style::default().fg(Color::Cyan),
                crate::log_tail::LineSeverity::Debug => Style::default().fg(Color::DarkGray),
                crate::log_tail::LineSeverity::Plain => Style::default().fg(Color::Gray),
            };
            Line::from(Span::styled(ln.text.clone(), style))
        })
        .collect();
    let title = match &pane.log_stream {
        Some(s) => format!(" {} · {} ", tab.name, s),
        None => format!(" {} · {} ", tab.name, pane.log_group),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk scroll · o console · y line · q quit ";
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
