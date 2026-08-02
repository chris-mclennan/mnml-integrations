//! Crossterm event loop + ratatui draw. Standalone (non-blit)
//! mode — owns the terminal, sets up an alt-screen, polls
//! crossterm events.

use crate::app::App;
use crate::keys;
use crate::s3::{self, Entry};
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
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
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
        let any = app.drain();
        if any {
            terminal.draw(|f| draw(f, app))?;
        } else {
            terminal.draw(|f| draw(f, app))?;
        }
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(k) = event::read()?
            && let Some(action) = keys::handle(k, app)
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
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tab strip
            Constraint::Length(3), // Breadcrumb header
            Constraint::Min(3),    // Body
            Constraint::Length(1), // Status line
        ])
        .split(area);

    draw_tab_strip(f, chunks[0], app);
    draw_breadcrumb(f, chunks[1], app);
    draw_body(f, chunks[2], app);
    draw_status(f, chunks[3], app);

    // The upload prompt is a centered overlay drawn last so it
    // sits on top of the body.
    if app.upload_prompt.is_some() {
        draw_upload_prompt(f, area, app);
    }
}

fn draw_tab_strip(f: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::with_capacity(app.tabs.len() * 3);
    for (i, tab) in app.tabs.iter().enumerate() {
        let is_active = i == app.active_tab;
        let prefix = if is_active { "▸" } else { " " };
        let style = if is_active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{prefix}{}.{}", i + 1, tab.name),
            style,
        ));
    }
    let block = Block::default().borders(Borders::ALL).title(" s3 ");
    let para = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(para, area);
}

fn draw_breadcrumb(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    let crumb = if tab.prefix.is_empty() {
        format!("{} /", tab.bucket)
    } else {
        format!("{} / {}", tab.bucket, tab.prefix.trim_end_matches('/'))
    };
    let para = Paragraph::new(Line::from(vec![
        Span::styled("📁 ", Style::default().fg(Color::Yellow)),
        Span::styled(crumb, Style::default().add_modifier(Modifier::BOLD)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", tab.name)),
    );
    f.render_widget(para, area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    if let Some(err) = &tab.last_error {
        let para = Paragraph::new(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )))
        .block(Block::default().borders(Borders::ALL).title(" error "));
        f.render_widget(para, area);
        return;
    }
    if tab.loading && tab.items.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            "loading…",
            Style::default().fg(Color::Yellow),
        )))
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(para, area);
        return;
    }
    if tab.items.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(para, area);
        return;
    }

    // Build rows. Highlight the selected one. Show:
    //   prefix:   📁 errors/                                 N objects
    //   object:   📄 build-log.txt              1.2 MB       2026-06-06
    let rows: Vec<Row> = tab
        .items
        .iter()
        .enumerate()
        .map(|(i, e)| row_for_entry(i, i == tab.selected, e))
        .collect();
    let widths = [
        Constraint::Length(1),  // selection marker
        Constraint::Length(3),  // glyph
        Constraint::Min(20),    // name
        Constraint::Length(12), // size
        Constraint::Length(16), // date
    ];
    let table = Table::new(rows, widths).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} entries ", tab.items.len())),
    );
    f.render_widget(table, area);
}

fn row_for_entry(_idx: usize, selected: bool, e: &Entry) -> Row<'_> {
    let marker = if selected { "▸" } else { " " };
    let style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    match e {
        Entry::Prefix(p) => Row::new(vec![
            Span::styled(marker, style),
            Span::styled("📁", Style::default().fg(Color::Yellow)),
            Span::styled(p.name.clone(), style),
            Span::raw(""),
            Span::raw(""),
        ]),
        Entry::Object(o) => {
            let date = if o.last_modified.len() >= 10 {
                o.last_modified[..10].to_string()
            } else {
                o.last_modified.clone()
            };
            Row::new(vec![
                Span::styled(marker, style),
                Span::styled("📄", Style::default().fg(Color::White)),
                Span::styled(o.name.clone(), style),
                Span::styled(s3::fmt_size(o.size), Style::default().fg(Color::Green)),
                Span::styled(date, Style::default().fg(Color::DarkGray)),
            ])
        }
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = "↑↓/jk · Enter open · BS up · y URI · Y presign · o console · u upload · d del · r refresh · q quit";
    let line = Line::from(vec![
        Span::styled(&app.status, Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_upload_prompt(f: &mut Frame, area: Rect, app: &App) {
    let Some(prompt) = app.upload_prompt.as_ref() else {
        return;
    };
    let tab = app.active();
    // Center a 60%-wide, 7-row card.
    let card_w = (area.width as u32 * 6 / 10) as u16;
    let card_h: u16 = 7;
    let card_x = area.x + (area.width.saturating_sub(card_w)) / 2;
    let card_y = area.y + (area.height.saturating_sub(card_h)) / 2;
    let card = Rect::new(card_x, card_y, card_w, card_h);

    // Wipe the area first so the underlying body doesn't bleed
    // through under the border.
    let bg_lines: Vec<Line> = (0..card.height)
        .map(|_| Line::from(Span::raw(" ".repeat(card.width as usize))))
        .collect();
    f.render_widget(Paragraph::new(bg_lines), card);

    let title = format!(" Upload to s3://{}/{} ", tab.bucket, tab.prefix);
    let inner_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Local path:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                format!(" {}", prompt.buffer),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("│", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter to send · Esc to cancel · ~ expanded to $HOME",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let para = Paragraph::new(inner_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(para, card);
}
