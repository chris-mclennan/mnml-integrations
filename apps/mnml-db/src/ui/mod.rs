//! Top-level render + event loop.

mod connections;
mod header;
mod query_editor;
mod results_grid;
mod results_kv;
pub(crate) mod results_tree;
mod schema_tree;

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
    widgets::{Block, Borders, Paragraph},
};
use std::io::Stdout;
use std::time::Duration;

use crate::app::{App, Focus, Overlay};
use crate::keys;

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    // First-run: try to open the active connection eagerly so the
    // header shows the driver's describe() line right away.
    let _ = app.ensure_worker();
    loop {
        crate::theme::poll_refresh();
        let redraw = app.drain();
        terminal.draw(|f| draw(f, app))?;
        if app.should_quit {
            break;
        }
        // Tighter poll if a response just arrived (there's likely
        // more coming) — otherwise sit for the normal 100ms.
        let wait = if redraw { 20 } else { 100 };
        if event::poll(Duration::from_millis(wait))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
        {
            let action = keys::handle(key, app);
            dispatch(action, app);
        }
    }
    Ok(())
}

fn dispatch(action: keys::Action, app: &mut App) {
    use keys::Action::*;
    match action {
        Quit => app.should_quit = true,
        Noop => {}
        CycleFocus => app.cycle_focus(false),
        CycleFocusReverse => app.cycle_focus(true),
        OpenConnPicker => {
            app.overlay = Overlay::ConnPicker {
                index: app.active.unwrap_or(0),
            };
            app.focus = Focus::ConnPicker;
        }
        OpenHistoryPicker => {
            let entries = app
                .active_conn()
                .map(|c| c.history.entries().iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            if entries.is_empty() {
                app.status = "history is empty".into();
            } else {
                app.overlay = Overlay::HistoryPicker { entries, index: 0 };
                app.focus = Focus::HistoryPicker;
            }
        }
        OpenObjectPicker => {
            let candidates = collect_object_candidates(app);
            if candidates.is_empty() {
                app.status =
                    "no objects yet — press Ctrl+Enter after connecting to load the schema".into();
            } else {
                app.overlay = Overlay::ObjectPicker {
                    candidates,
                    query: String::new(),
                    index: 0,
                };
                app.focus = Focus::ObjectPicker;
            }
        }
        CloseOverlay => {
            app.overlay = Overlay::None;
            app.focus = Focus::Editor;
        }
        OverlayUp => overlay_move(app, -1),
        OverlayDown => overlay_move(app, 1),
        OverlayAccept => overlay_accept(app),
        OverlayChar(c) => overlay_type(app, c),
        OverlayBackspace => overlay_backspace(app),
        EditorChar(c) => {
            app.editor.insert(c);
            if let Some(cn) = app.active_conn_mut() {
                cn.history.reset_cursor();
            }
        }
        EditorBackspace => app.editor.backspace(),
        EditorNewline => app.editor.newline(),
        EditorMoveLeft => app.editor.move_left(),
        EditorMoveRight => app.editor.move_right(),
        EditorClear => app.editor.clear(),
        RunStatement => app.run_query(),
        RunAll => app.run_all(),
        TriggerCompletion => app.request_completions(),
        ResultUp => app.move_result_row(-1),
        ResultDown => app.move_result_row(1),
        ResultPageUp => app.move_result_row(-10),
        ResultPageDown => app.move_result_row(10),
        ResultTop => app.move_result_row(-(i32::MAX as isize)),
        ResultBottom => app.move_result_row(i32::MAX as isize),
        ResultFilterChar('\0') => {
            // Sentinel — user pressed `/` to start filtering.
            app.result_filter.clear();
            app.status = "filter: (type to narrow, Esc to clear)".into();
        }
        ResultFilterChar(c) => app.result_filter.push(c),
        ResultFilterBackspace => {
            app.result_filter.pop();
        }
        ResultFilterClear => app.result_filter.clear(),
        ResultExpand => {
            let c = app.result_row;
            app.result_row = results_tree::toggle_at(app, c);
        }
        ResultCollapse => {
            let c = app.result_row;
            app.result_row = results_tree::collapse_at(app, c);
        }
        TreeUp => {
            if app.tree.selected > 0 {
                app.tree.selected -= 1;
            }
        }
        TreeDown => {
            let max = app.tree.visible.len().saturating_sub(1);
            if app.tree.selected < max {
                app.tree.selected += 1;
            }
        }
        TreeExpandOrEnter => tree_expand_or_enter(app),
        TreeCollapse => tree_collapse(app),
        DoubleRowLimit => app.double_row_limit(),
        SwitchConnectionIdx(i) => {
            if i < app.connections.len() {
                app.switch_connection(i);
                let _ = app.ensure_worker();
            }
        }
    }
}

fn tree_expand_or_enter(app: &mut App) {
    let Some(line) = app.tree.visible.get(app.tree.selected).cloned() else {
        return;
    };
    match line {
        crate::app::TreeLine::Namespace(ns) => {
            let was_expanded = app.tree.expanded.contains(&ns);
            if was_expanded {
                app.tree.expanded.remove(&ns);
            } else {
                app.tree.expanded.insert(ns.clone());
                // Kick off object load if not cached.
                let needs_load = app
                    .active_conn()
                    .map(|c| !c.schema.objects.contains_key(&ns))
                    .unwrap_or(false);
                if needs_load {
                    app.request_objects(&ns);
                }
            }
        }
        crate::app::TreeLine::Object { namespace, name } => {
            // Insert `namespace.name` into the editor at the caret.
            let insertion = format!("{namespace}.{name}");
            app.editor.insert_str(&insertion);
            app.focus = Focus::Editor;
        }
    }
}

fn tree_collapse(app: &mut App) {
    let Some(line) = app.tree.visible.get(app.tree.selected).cloned() else {
        return;
    };
    match line {
        crate::app::TreeLine::Namespace(ns) => {
            app.tree.expanded.remove(&ns);
        }
        crate::app::TreeLine::Object { namespace, .. } => {
            app.tree.expanded.remove(&namespace);
            // Move selection back to the namespace line.
            if let Some(pos) =
                app.tree.visible.iter().position(
                    |l| matches!(l, crate::app::TreeLine::Namespace(n) if n == &namespace),
                )
            {
                app.tree.selected = pos;
            }
        }
    }
}

fn collect_object_candidates(app: &App) -> Vec<crate::app::PickerObject> {
    let Some(c) = app.active_conn() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (ns, objs) in c.schema.objects.iter() {
        for o in objs {
            out.push(crate::app::PickerObject {
                namespace: ns.clone(),
                name: o.name.clone(),
            });
        }
    }
    out
}

fn overlay_move(app: &mut App, delta: isize) {
    match &mut app.overlay {
        Overlay::ConnPicker { index } => {
            let n = app.connections.len();
            if n == 0 {
                return;
            }
            let s = (*index as isize + delta).rem_euclid(n as isize) as usize;
            *index = s;
        }
        Overlay::HistoryPicker { entries, index } => {
            let n = entries.len();
            if n == 0 {
                return;
            }
            let s = (*index as isize + delta).rem_euclid(n as isize) as usize;
            *index = s;
        }
        Overlay::ObjectPicker {
            candidates,
            query,
            index,
        } => {
            // tester 2026-07-31 SEV-2 — index against the FILTERED
            // list, not the unfiltered one. Was: cursor moved through
            // all candidates but rendering only showed matches, so
            // Enter after narrowing picked whichever unfiltered item
            // happened to share the highlighted-row index.
            let n = filtered_picker(candidates, query).len();
            if n == 0 {
                return;
            }
            let s = (*index as isize + delta).rem_euclid(n as isize) as usize;
            *index = s;
        }
        Overlay::Completion {
            completions, index, ..
        } => {
            let n = completions.len();
            if n == 0 {
                return;
            }
            let s = (*index as isize + delta).rem_euclid(n as isize) as usize;
            *index = s;
        }
        Overlay::None => {}
    }
}

fn overlay_accept(app: &mut App) {
    // Detach the overlay first so we can borrow app mutably during
    // the follow-up action.
    let taken = std::mem::replace(&mut app.overlay, Overlay::None);
    match taken {
        Overlay::ConnPicker { index } => {
            app.switch_connection(index);
            let _ = app.ensure_worker();
            app.focus = Focus::Editor;
        }
        Overlay::HistoryPicker { entries, index } => {
            if let Some(s) = entries.get(index) {
                app.editor.set(s.clone());
            }
            app.focus = Focus::Editor;
        }
        Overlay::ObjectPicker {
            candidates,
            query,
            index,
        } => {
            // tester 2026-07-31 SEV-2 — filter first, then index. Was
            // indexing the unfiltered candidates so a narrowed pick
            // inserted the wrong object.
            let filtered = filtered_picker(&candidates, &query);
            if let Some(o) = filtered.get(index) {
                app.editor
                    .insert_str(&format!("{}.{}", o.namespace, o.name));
            }
            app.focus = Focus::Editor;
        }
        Overlay::Completion {
            completions, index, ..
        } => {
            if let Some(c) = completions.get(index) {
                // Replace the current word with the completion.
                let word = app.editor.current_word();
                for _ in 0..word.chars().count() {
                    app.editor.backspace();
                }
                app.editor.insert_str(&c.insert);
            }
            app.focus = Focus::Editor;
        }
        Overlay::None => {}
    }
}

fn overlay_type(app: &mut App, c: char) {
    if let Overlay::ObjectPicker { query, index, .. } = &mut app.overlay {
        query.push(c);
        *index = 0;
    }
}

fn overlay_backspace(app: &mut App) {
    if let Overlay::ObjectPicker { query, index, .. } = &mut app.overlay {
        query.pop();
        *index = 0;
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    // Rebuild the visible tree lines before we compute layout — the
    // schema tree renderer uses it and the tree state is authoritative.
    schema_tree::rebuild_visible(app);

    let outer_border = !inside_mnml();
    let area = if outer_border {
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            " mnml-db ",
            Style::default().fg(themed(Color::Cyan)),
        ));
        let inner = block.inner(size);
        f.render_widget(block, size);
        inner
    } else {
        size
    };

    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(3),    // body
            Constraint::Length(1), // status
        ])
        .split(area);

    header::draw(f, cols[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Min(30)])
        .split(cols[1]);

    schema_tree::draw(f, body[0], app);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Min(3)])
        .split(body[1]);

    query_editor::draw(f, right[0], app);
    draw_results(f, right[1], app);

    draw_status(f, cols[2], app);

    // Overlays render last so they float on top.
    draw_overlay(f, area, app);
}

fn draw_results(f: &mut Frame, area: Rect, app: &App) {
    use crate::driver::{QueryResult, ResultKind};
    let kind = app
        .active_conn()
        .and_then(|c| c.result_kind())
        .unwrap_or(ResultKind::Rows);
    match &app.result {
        Some(QueryResult::Rows { .. }) => results_grid::draw(f, area, app),
        Some(QueryResult::KeyValue { .. }) => results_kv::draw(f, area, app),
        Some(QueryResult::Documents { .. }) => results_tree::draw(f, area, app),
        Some(QueryResult::Notice { text, elapsed_ms }) => {
            let body = format!("{text}  ({elapsed_ms}ms)");
            let p = Paragraph::new(body)
                .style(Style::default().fg(themed(Color::Green)))
                .block(Block::default().borders(Borders::ALL).title(" result "));
            f.render_widget(p, area);
        }
        None => {
            let hint = match kind {
                ResultKind::Rows => "(no results yet — run a query with Ctrl+Enter)",
                ResultKind::KeyValue => "(no results yet — run a command with Ctrl+Enter)",
                ResultKind::Document => "(no results yet)",
            };
            let p = Paragraph::new(hint)
                .style(Style::default().fg(themed(Color::DarkGray)))
                .block(Block::default().borders(Borders::ALL).title(" results "));
            f.render_widget(p, area);
        }
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let focus_chip = match app.focus {
        Focus::SchemaTree => "SCHEMA",
        Focus::Editor => "EDITOR",
        Focus::Results => "RESULTS",
        Focus::ConnPicker | Focus::HistoryPicker | Focus::ObjectPicker => "OVERLAY",
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {focus_chip} "),
            Style::default()
                .fg(themed(Color::Black))
                .bg(themed(Color::Cyan)),
        ),
        Span::raw("  "),
        Span::styled(app.status.clone(), Style::default().fg(themed(Color::Gray))),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_overlay(f: &mut Frame, area: Rect, app: &App) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::ConnPicker { index } => draw_conn_picker(f, area, app, *index),
        Overlay::HistoryPicker { entries, index } => draw_history_picker(f, area, entries, *index),
        Overlay::ObjectPicker {
            candidates,
            query,
            index,
        } => draw_object_picker(f, area, candidates, query, *index),
        Overlay::Completion { completions, index } => {
            draw_completion_popup(f, area, completions, *index)
        }
    }
}

fn draw_conn_picker(f: &mut Frame, area: Rect, app: &App, index: usize) {
    let popup = centered(area, 60, 40);
    let lines: Vec<Line> = app
        .connections
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let marker = if i == index { "▸" } else { " " };
            let engine_chip = format!("[{}]", c.spec.engine);
            Line::from(vec![
                Span::raw(format!("{marker} ")),
                Span::styled(
                    c.spec.display_label().to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(engine_chip, Style::default().fg(themed(Color::Cyan))),
                Span::raw("  "),
                Span::styled(
                    c.spec.host.clone().unwrap_or_default(),
                    Style::default().fg(themed(Color::DarkGray)),
                ),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" connections — Enter to switch, Esc to cancel ");
    let p = Paragraph::new(lines).block(block);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(p, popup);
}

fn draw_history_picker(f: &mut Frame, area: Rect, entries: &[String], index: usize) {
    let popup = centered(area, 70, 50);
    let lines: Vec<Line> = entries
        .iter()
        .rev()
        .enumerate()
        .map(|(i, s)| {
            let marker = if i == index { "▸" } else { " " };
            Line::from(vec![
                Span::raw(format!("{marker} ")),
                Span::raw(one_line(s)),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" history — Enter to load, Esc to cancel ");
    let p = Paragraph::new(lines).block(block);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(p, popup);
}

/// Shared filter used by `overlay_move`, `overlay_accept`, and
/// `draw_object_picker` so the three stay in sync (tester
/// 2026-07-31 SEV-2 — they diverged: nav+accept operated on the
/// unfiltered list, render on the filtered one).
pub(crate) fn filtered_picker<'a>(
    candidates: &'a [crate::app::PickerObject],
    query: &str,
) -> Vec<&'a crate::app::PickerObject> {
    let q = query.to_ascii_lowercase();
    candidates
        .iter()
        .filter(|c| {
            let hay = format!("{}.{}", c.namespace, c.name).to_ascii_lowercase();
            hay.contains(&q)
        })
        .collect()
}

fn draw_object_picker(
    f: &mut Frame,
    area: Rect,
    candidates: &[crate::app::PickerObject],
    query: &str,
    index: usize,
) {
    let popup = centered(area, 60, 55);
    let filtered = filtered_picker(candidates, query);
    let mut lines: Vec<Line> = Vec::with_capacity(filtered.len() + 2);
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(themed(Color::Cyan))),
        Span::raw(query.to_string()),
    ]));
    lines.push(Line::from(""));
    for (i, o) in filtered.iter().enumerate() {
        let marker = if i == index { "▸" } else { " " };
        lines.push(Line::from(vec![
            Span::raw(format!("{marker} ")),
            Span::raw(format!("{}.", o.namespace)),
            Span::styled(
                o.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" object picker — Enter to insert, Esc to cancel ");
    let p = Paragraph::new(lines).block(block);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(p, popup);
}

fn draw_completion_popup(
    f: &mut Frame,
    area: Rect,
    completions: &[crate::driver::Completion],
    index: usize,
) {
    // Anchor to top-right — no caret math in v0.1.
    let width = 32u16;
    let height = (completions.len() as u16 + 2).min(12);
    let x = area.x + area.width.saturating_sub(width + 2);
    let y = area.y + 3;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    let lines: Vec<Line> = completions
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, c)| {
            let marker = if i == index { "▸" } else { " " };
            let kind_chip = match c.kind {
                crate::driver::CompletionKind::Keyword => "kw",
                crate::driver::CompletionKind::Table => "tbl",
                crate::driver::CompletionKind::Column => "col",
                crate::driver::CompletionKind::Function => "fn",
                crate::driver::CompletionKind::RedisCommand => "cmd",
            };
            Line::from(vec![
                Span::raw(format!("{marker} ")),
                Span::raw(c.display.clone()),
                Span::raw("  "),
                Span::styled(kind_chip, Style::default().fg(themed(Color::DarkGray))),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" completions ");
    let p = Paragraph::new(lines).block(block);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(p, popup);
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Rendered as one line — replaces newlines with `⏎ `.
fn one_line(s: &str) -> String {
    s.replace('\n', " ⏎ ")
}

/// Are we running inside mnml (as a hosted pane, no outer border
/// needed)? Mirrors the other siblings' convention.
pub fn inside_mnml() -> bool {
    std::env::var_os("MNML_PANE").is_some()
}

/// Route a color through the family palette.
pub fn themed(c: Color) -> Color {
    crate::theme::remap(c)
}
