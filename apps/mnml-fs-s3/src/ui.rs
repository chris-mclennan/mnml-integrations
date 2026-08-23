//! Crossterm event loop + ratatui draw. Standalone (non-blit)
//! mode — owns the terminal, sets up an alt-screen, polls
//! crossterm events.

use crate::app::{App, UploadOverlay, UploadProgress, UploadState, UploadTask};
use crate::keys;
use crate::picker::FilePicker;
use crate::s3::{self, Entry};
use crate::upload::fmt_rate;
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
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Table};
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
        // Drain background events (S3 listings + upload progress) so
        // the next draw sees the fresh state.
        let _ = app.drain();
        terminal.draw(|f| draw(f, app))?;
        // Poll shorter when uploads are running so the progress bar
        // updates smoothly (~10 fps); longer otherwise to keep CPU
        // near zero.
        let poll_ms = if app.is_upload_running() { 100 } else { 250 };
        if event::poll(Duration::from_millis(poll_ms))?
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

    // Overlays are drawn last so they sit on top of the body.
    match app.upload_overlay.as_ref() {
        Some(UploadOverlay::Pick(p)) => draw_picker(f, area, app, p),
        Some(UploadOverlay::Progress(pg)) => draw_progress(f, area, pg),
        None => {}
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

/// Center a `w × h` rect inside `area`, wipe it, and hand back the
/// rect. Wiping stops the underlying table from bleeding through the
/// border corners.
fn centered_card(f: &mut Frame, area: Rect, w: u16, h: u16) -> Rect {
    let card_x = area.x + (area.width.saturating_sub(w)) / 2;
    let card_y = area.y + (area.height.saturating_sub(h)) / 2;
    let card = Rect::new(card_x, card_y, w, h);
    let bg_lines: Vec<Line> = (0..card.height)
        .map(|_| Line::from(Span::raw(" ".repeat(card.width as usize))))
        .collect();
    f.render_widget(Paragraph::new(bg_lines), card);
    card
}

fn draw_picker(f: &mut Frame, area: Rect, app: &App, picker: &FilePicker) {
    let tab = app.active();
    // Card is wider than the old prompt so long filenames fit.
    let card_w = (area.width as u32 * 8 / 10).min(96) as u16;
    let card_h = (area.height as u32 * 8 / 10).min(28) as u16;
    let card = centered_card(f, area, card_w, card_h);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Upload → s3://{}/{} ", tab.bucket, tab.prefix))
        .style(Style::default().fg(Color::Cyan));
    let inner = block.inner(card);
    f.render_widget(block, card);

    // Layout inside the card:
    //   line 1: cwd
    //   line 2: divider
    //   body:   entries table
    //   line -2: divider
    //   line -1: hints + selection count
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // cwd
            Constraint::Length(1), // divider
            Constraint::Min(1),    // body
            Constraint::Length(1), // divider
            Constraint::Length(1), // hints
        ])
        .split(inner);

    let cwd_line = Paragraph::new(Line::from(vec![
        Span::styled(" cwd ", Style::default().fg(Color::Yellow).bg(Color::Black)),
        Span::raw(" "),
        Span::styled(
            picker.cwd.display().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(cwd_line, layout[0]);

    let divider = Paragraph::new(Span::styled(
        "─".repeat(layout[1].width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(divider.clone(), layout[1]);
    f.render_widget(divider, layout[3]);

    // Rows. Show `[✓]` when in selection, `[ ]` on unselected files,
    // ` /` on directories.
    if let Some(err) = &picker.error {
        let para = Paragraph::new(Line::from(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        )));
        f.render_widget(para, layout[2]);
    } else if picker.entries.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            " (empty directory) ",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(para, layout[2]);
    } else {
        // Render a scrolling window centered on `picker.row`.
        let body_h = layout[2].height as usize;
        let n = picker.entries.len();
        // Window start: keep row roughly centered.
        let mut start = picker.row.saturating_sub(body_h / 2);
        if start + body_h > n {
            start = n.saturating_sub(body_h);
        }
        let end = (start + body_h).min(n);
        let rows: Vec<Row> = picker.entries[start..end]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let idx = start + offset;
                let focused = idx == picker.row;
                let selected = picker.selected.iter().any(|p| p == &entry.path);
                let marker = if focused { "▸" } else { " " };
                let box_ = if entry.is_dir {
                    "  "
                } else if selected {
                    "✓ "
                } else {
                    "  "
                };
                let glyph = if entry.is_dir { "📁" } else { "📄" };
                let name_style = match (focused, selected, entry.is_dir) {
                    (true, _, _) => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    (_, true, _) => Style::default().fg(Color::Green),
                    (_, _, true) => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                };
                let size = if entry.is_dir {
                    String::new()
                } else {
                    s3::fmt_size(entry.size)
                };
                Row::new(vec![
                    Span::styled(marker, Style::default().fg(Color::Cyan)),
                    Span::styled(box_, Style::default().fg(Color::Green)),
                    Span::raw(glyph),
                    Span::styled(entry.name.clone(), name_style),
                    Span::styled(size, Style::default().fg(Color::Green)),
                    Span::styled(entry.modified.clone(), Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(1),  // focus marker
            Constraint::Length(2),  // selection box
            Constraint::Length(3),  // glyph
            Constraint::Min(20),    // name
            Constraint::Length(10), // size
            Constraint::Length(12), // date
        ];
        f.render_widget(Table::new(rows, widths), layout[2]);
    }

    let sel_count = picker.selected.len();
    let hint_left = if sel_count == 0 {
        " Space select · Enter fire (focused) · A all · ←/BS up · Esc cancel ".to_string()
    } else {
        format!(
            " {sel_count} selected · Space toggle · Enter upload all · C clear · ←/BS up · Esc cancel "
        )
    };
    let hints = Paragraph::new(Line::from(vec![Span::styled(
        hint_left,
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(hints, layout[4]);
}

fn draw_progress(f: &mut Frame, area: Rect, pg: &UploadProgress) {
    // Card sized to fit up to 10 rows on screen at once (each task
    // is 3 rows: name/state, gauge, rate/size — plus 4 chrome rows).
    let visible_tasks = pg.tasks.len().min(10) as u16;
    let card_h = (visible_tasks * 3 + 4).min(area.height.saturating_sub(2));
    let card_w = (area.width as u32 * 8 / 10).min(88) as u16;
    let card = centered_card(f, area, card_w, card_h);

    let done = pg
        .tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Done))
        .count();
    let failed = pg
        .tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Failed(_)))
        .count();
    let running = pg
        .tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Running))
        .count();
    let queued = pg
        .tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Queued))
        .count();
    let title = format!(
        " Uploading → s3://{}/{} · {done}/{} done · {running} running · {queued} queued · {failed} failed ",
        pg.bucket,
        pg.prefix,
        pg.tasks.len()
    );
    let border_color = if pg.all_done && failed > 0 {
        Color::Red
    } else if pg.all_done {
        Color::Green
    } else {
        Color::Cyan
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(border_color));
    let inner = block.inner(card);
    f.render_widget(block, card);

    // Split inner: N task rows, then a hint line.
    let mut constraints: Vec<Constraint> = pg
        .tasks
        .iter()
        .take(visible_tasks as usize)
        .map(|_| Constraint::Length(3))
        .collect();
    constraints.push(Constraint::Length(1)); // hint
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, task) in pg.tasks.iter().take(visible_tasks as usize).enumerate() {
        draw_task_row(f, rows[i], task);
    }

    let hint = if pg.all_done {
        " All done — press any key to close ".to_string()
    } else {
        " Uploads continue in background · Esc / Ctrl+C to hide ".to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[visible_tasks as usize],
    );
}

fn draw_task_row(f: &mut Frame, area: Rect, task: &UploadTask) {
    let (badge, badge_style) = match &task.state {
        UploadState::Queued => ("queued ", Style::default().fg(Color::DarkGray)),
        UploadState::Running => ("running", Style::default().fg(Color::Cyan)),
        UploadState::Done => (
            "done   ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        UploadState::Failed(_) => (
            "failed ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title row
            Constraint::Length(1), // gauge
            Constraint::Length(1), // detail row
        ])
        .split(area);

    // Title row: "[state] name  →  s3://.../key"
    let title = Line::from(vec![
        Span::raw(" ["),
        Span::styled(badge, badge_style),
        Span::raw("] "),
        Span::styled(&task.name, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("  →  {}", task.key),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(title), split[0]);

    // Gauge — bounded [0, 100]. On failure show 100% red for
    // visual weight; on queued show 0.
    let pct = if task.total > 0 {
        ((task.done as f64 / task.total as f64) * 100.0).clamp(0.0, 100.0) as u16
    } else if matches!(task.state, UploadState::Done) {
        100
    } else {
        0
    };
    let gauge_style = match &task.state {
        UploadState::Failed(_) => Style::default().fg(Color::Red).bg(Color::DarkGray),
        UploadState::Done => Style::default().fg(Color::Green).bg(Color::DarkGray),
        UploadState::Running => Style::default().fg(Color::Cyan).bg(Color::DarkGray),
        UploadState::Queued => Style::default().fg(Color::DarkGray).bg(Color::Black),
    };
    let gauge = Gauge::default()
        .gauge_style(gauge_style)
        .percent(pct)
        .label("");
    f.render_widget(gauge, split[1]);

    // Detail row: "12.4 MiB / 45.7 MiB · 3.2 MiB/s · 42%" or the
    // failure message.
    let detail = match &task.state {
        UploadState::Failed(msg) => Line::from(vec![
            Span::raw(" "),
            Span::styled(
                truncate(msg, area.width.saturating_sub(3) as usize),
                Style::default().fg(Color::Red),
            ),
        ]),
        UploadState::Queued => Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("waiting · {}", s3::fmt_size(task.total)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ => Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!(
                    "{} / {} · {} · {pct}%",
                    s3::fmt_size(task.done),
                    s3::fmt_size(task.total),
                    fmt_rate(task.rate_bps),
                ),
                Style::default().fg(Color::White),
            ),
        ]),
    };
    f.render_widget(Paragraph::new(detail), split[2]);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.into();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
