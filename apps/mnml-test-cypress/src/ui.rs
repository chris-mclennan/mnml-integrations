//! Crossterm event loop + ratatui draw.

use crate::app::{App, Row};
use crate::cypress::{self, TestState};
use crate::keys;
use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io;
use std::time::Duration;

pub async fn run(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    let res = main_loop(&mut terminal, app).await;
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

async fn main_loop(
    terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(k) = event::read()?
            && let Some(action) = keys::handle(k, app)
        {
            let quit = keys::apply(action, app);
            if quit {
                break;
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let has_error = app.focused_test().and_then(|t| t.error.as_ref()).is_some();
    let body_constraints = if has_error {
        vec![
            Constraint::Length(3), // header
            Constraint::Min(5),    // tests body
            Constraint::Length(6), // error details
            Constraint::Length(1), // status / hint
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(body_constraints)
        .split(area);

    draw_header(f, chunks[0], app);
    draw_tests(f, chunks[1], app);
    if has_error {
        draw_error(f, chunks[2], app);
        draw_status(f, chunks[3], app);
    } else {
        draw_status(f, chunks[2], app);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.report.stats;
    let dur = cypress::fmt_duration(s.duration_ms);
    let line = Line::from(vec![
        Span::styled(
            "🧪 cypress ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(format!("{}p", s.passes), Style::default().fg(Color::Green)),
        Span::raw(" / "),
        Span::styled(format!("{}f", s.failures), Style::default().fg(Color::Red)),
        Span::raw(" / "),
        Span::styled(
            format!("{}pending", s.pending),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" · "),
        Span::styled(dur, Style::default().fg(Color::Gray)),
    ]);
    let para = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", app.source.display())),
    );
    f.render_widget(para, area);
}

fn draw_tests(f: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            "(no rows match the current filter)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(para, area);
        return;
    }
    // Body is a hand-rolled vertical list (Table is overkill for
    // mixed-row-type rendering). Each row is one Line.
    let inner = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} rows ", app.rows.len()))
        .inner(area);

    // Viewport: keep selected on screen.
    let height = inner.height as usize;
    let scroll = compute_scroll(app.selected, app.rows.len(), height);

    let visible = app
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .collect::<Vec<_>>();

    let lines: Vec<Line> = visible
        .iter()
        .map(|(idx, row)| line_for_row(app, *idx, *idx == app.selected, row))
        .collect();

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} rows ", app.rows.len())),
    );
    f.render_widget(para, area);
}

fn line_for_row<'a>(app: &'a App, _idx: usize, selected: bool, row: &Row) -> Line<'a> {
    let marker = if selected { "▸" } else { " " };
    let sel_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    match row {
        Row::SpecHeader {
            spec_idx,
            passed,
            failed,
        } => {
            let spec = &app.report.specs[*spec_idx];
            let summary = if *failed > 0 {
                format!("({passed}p, {failed}f)")
            } else {
                format!("({passed}p)")
            };
            Line::from(vec![
                Span::styled(marker, sel_style),
                Span::raw(" 📄 "),
                Span::styled(
                    spec.file.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(summary, Style::default().fg(Color::DarkGray)),
            ])
        }
        Row::Test { spec_idx, test_idx } => {
            let t = &app.report.specs[*spec_idx].tests[*test_idx];
            let (glyph_color, title_style) = match t.state {
                TestState::Passed => (Color::Green, sel_style),
                TestState::Failed => (
                    Color::Red,
                    if selected {
                        sel_style.add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    },
                ),
                TestState::Pending => (Color::Yellow, sel_style),
                TestState::Skipped => (Color::DarkGray, sel_style),
                TestState::Unknown => (Color::DarkGray, sel_style),
            };
            Line::from(vec![
                Span::styled(format!("   {marker}"), sel_style),
                Span::styled(
                    format!(" {} ", t.state.glyph()),
                    Style::default().fg(glyph_color),
                ),
                Span::styled(t.title.clone(), title_style),
                Span::raw("  "),
                Span::styled(
                    cypress::fmt_duration(t.duration_ms),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
    }
}

fn draw_error(f: &mut Frame, area: Rect, app: &App) {
    let Some(t) = app.focused_test() else {
        return;
    };
    let Some(err) = &t.error else {
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        err.message.clone(),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    if let Some(stack) = &err.stack {
        // Show just the first 4 stack lines — full stack is in the
        // mochawesome JSON if the user wants it.
        for stack_line in stack.lines().take(4) {
            lines.push(Line::from(Span::styled(
                stack_line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    let para =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" failure "));
    f.render_widget(para, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = "↑↓/jk · F failures-only · y yank spec · r reload · q quit";
    let line = Line::from(vec![
        Span::styled(&app.status, Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn compute_scroll(selected: usize, total: usize, view_h: usize) -> usize {
    if total <= view_h {
        return 0;
    }
    // Keep selected within the middle 60% of the viewport.
    let pad = view_h / 5;
    if selected < pad {
        0
    } else if selected + view_h - pad > total {
        total - view_h
    } else {
        selected - pad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keeps_selected_in_viewport() {
        // 100 items, viewport 10 tall, selected mid-stream
        assert_eq!(compute_scroll(0, 100, 10), 0);
        assert_eq!(compute_scroll(50, 100, 10), 48); // selected - pad(2) = 48
        assert_eq!(compute_scroll(99, 100, 10), 90); // pinned to bottom
        assert_eq!(compute_scroll(5, 8, 10), 0); // smaller than viewport
    }
}
