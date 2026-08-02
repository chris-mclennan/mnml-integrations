//! Keyboard chord → action mapping. v0.1.

use crate::app::{App, ConfirmState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    OpenDashboard,
    Yank,
    RequestPurge,
    ConfirmYes,
    ConfirmNo,
    ToggleDevMode,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;

    // Confirmation prompts intercept keys before the normal map.
    if !matches!(app.confirm, ConfirmState::None) {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::ConfirmYes),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Action::ConfirmNo),
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('o') | KeyCode::Enter => Some(Action::OpenDashboard),
        KeyCode::Char('y') => Some(Action::Yank),
        KeyCode::Char('X') => Some(Action::RequestPurge),
        KeyCode::Char('D') => Some(Action::ToggleDevMode),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenDashboard => app.open_dashboard(),
        Action::Yank => app.yank(),
        Action::RequestPurge => app.request_purge(),
        Action::ConfirmYes => app.confirm_yes(),
        Action::ConfirmNo => app.confirm_no(),
        Action::ToggleDevMode => app.toggle_dev_mode(),
        Action::Refresh => app.refresh_active(),
        Action::NextTab => {
            let next = (app.active_tab + 1) % app.tabs.len();
            app.switch_tab(next);
        }
        Action::PrevTab => {
            let prev = if app.active_tab == 0 {
                app.tabs.len() - 1
            } else {
                app.active_tab - 1
            };
            app.switch_tab(prev);
        }
        Action::SwitchTab(i) => {
            app.switch_tab(i);
        }
    }
    false
}
