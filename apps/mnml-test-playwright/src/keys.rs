//! Keyboard chord → action mapping.

use crate::app::App;
use crate::trace::EventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Reload,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Toggle(EventKind),
    ErrorsOnly,
    ShowAll,
}

pub fn handle(key: KeyEvent, _app: &App) -> Option<Action> {
    let m = key.modifiers;
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Reload),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('a') => Some(Action::Toggle(EventKind::Action)),
        KeyCode::Char('c') => Some(Action::Toggle(EventKind::Console)),
        KeyCode::Char('e') => Some(Action::Toggle(EventKind::Error)),
        KeyCode::Char('s') => Some(Action::Toggle(EventKind::Stdio)),
        KeyCode::Char('E') => Some(Action::ErrorsOnly),
        KeyCode::Char('R') => Some(Action::ShowAll),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Reload => app.refresh(),
        Action::Up => move_selection(app, -1),
        Action::Down => move_selection(app, 1),
        Action::PageUp => move_selection(app, -10),
        Action::PageDown => move_selection(app, 10),
        Action::Home => move_selection(app, -(i32::MAX as isize)),
        Action::End => move_selection(app, i32::MAX as isize),
        Action::Toggle(k) => app.pane.toggle_kind(k),
        Action::ErrorsOnly => app.pane.errors_only_preset(),
        Action::ShowAll => app.pane.show_all_kinds(),
    }
    false
}

fn move_selection(app: &mut App, delta: isize) {
    let vis = app.pane.visible_indices();
    if vis.is_empty() {
        return;
    }
    let cur = vis
        .iter()
        .position(|&i| i == app.pane.selected)
        .unwrap_or(0);
    let next = (cur as isize + delta).clamp(0, (vis.len() - 1) as isize) as usize;
    app.pane.selected = vis[next];
}
