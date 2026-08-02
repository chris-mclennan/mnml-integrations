//! Crossterm event loop + ratatui draw. Standalone (non-blit)
//! mode — owns the terminal, sets up an alt-screen, polls
//! crossterm events.

use crate::app::{App, View};
use crate::azure_blob::{self, Entry};
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
        let _any = app.drain();
        terminal.draw(|f| draw(f, app))?;
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
    let block = Block::default().borders(Borders::ALL).title(" Azure Blob ");
    let para = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(para, area);
}

fn draw_breadcrumb(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();
    let crumb = match &tab.view {
        View::Accounts => "Storage Accounts".to_string(),
        View::Containers { account } => format!("{account} / containers"),
        View::Blobs {
            account,
            container,
            prefix,
        } => {
            if prefix.is_empty() {
                format!("{account} / {container}")
            } else {
                format!("{account} / {container} / {}", prefix.trim_end_matches('/'))
            }
        }
    };
    let glyph = match &tab.view {
        View::Accounts => "☁ ",
        View::Containers { .. } => "📦 ",
        View::Blobs { .. } => "📁 ",
    };
    let para = Paragraph::new(Line::from(vec![
        Span::styled(glyph, Style::default().fg(Color::Cyan)),
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

    // Build rows. Highlight the selected one. Column shapes
    // depend on the view kind:
    //   accounts:   ▸ ☁ <name>               <location>     <kind>
    //   containers: ▸ 📦 <name>              <publicAccess>  <date>
    //   blobs:      ▸ 📁 errors/                              <N entries>
    //   blobs:      ▸ 📄 build-log.txt        1.2 MB         2026-06-06
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
        Constraint::Length(16), // col-3 (size / access / kind)
        Constraint::Length(20), // col-4 (date / location)
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
        Entry::Account(a) => Row::new(vec![
            Span::styled(marker, style),
            Span::styled("☁", Style::default().fg(Color::Cyan)),
            Span::styled(a.name.clone(), style),
            Span::styled(a.kind.clone(), Style::default().fg(Color::Yellow)),
            Span::styled(a.location.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Entry::Container(c) => {
            let date = if c.last_modified.len() >= 10 {
                c.last_modified[..10].to_string()
            } else {
                c.last_modified.clone()
            };
            let access = c.public_access.clone().unwrap_or_else(|| "private".into());
            Row::new(vec![
                Span::styled(marker, style),
                Span::styled("📦", Style::default().fg(Color::Magenta)),
                Span::styled(c.name.clone(), style),
                Span::styled(access, Style::default().fg(Color::Yellow)),
                Span::styled(date, Style::default().fg(Color::DarkGray)),
            ])
        }
        Entry::Prefix(p) => Row::new(vec![
            Span::styled(marker, style),
            Span::styled("📁", Style::default().fg(Color::Yellow)),
            Span::styled(p.name.clone(), style),
            Span::raw(""),
            Span::raw(""),
        ]),
        Entry::Blob(b) => {
            let date = if b.last_modified.len() >= 10 {
                b.last_modified[..10].to_string()
            } else {
                b.last_modified.clone()
            };
            Row::new(vec![
                Span::styled(marker, style),
                Span::styled("📄", Style::default().fg(Color::White)),
                Span::styled(b.name.clone(), style),
                Span::styled(
                    azure_blob::fmt_size(b.size),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(date, Style::default().fg(Color::DarkGray)),
            ])
        }
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = "↑↓/jk · Enter open · BS up · y URI · Y SAS · o portal · d del · r refresh · q quit";
    let line = Line::from(vec![
        Span::styled(&app.status, Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}
