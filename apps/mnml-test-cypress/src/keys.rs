//! Keyboard chord → action mapping. v0.1.

use crate::app::App;
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
    YankSpecPath,
    ToggleFailedOnly,
}

pub fn handle(key: KeyEvent, _app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if ctrl => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Reload),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('y') => Some(Action::YankSpecPath),
        KeyCode::Char('F') => Some(Action::ToggleFailedOnly),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Reload => app.reload(),
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::YankSpecPath => app.yank_focused_spec_path(),
        Action::ToggleFailedOnly => app.toggle_failed_only(),
    }
    false
}
