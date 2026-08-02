//! Keyboard chord → action mapping. v0.1.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    OpenWeb,
    YankId,
    RequestPublish,
    RequestUnsubscribe,
    ConfirmYes,
    ConfirmNo,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
}

/// Two key-routing layers. When a confirm is pending, `y/n/Esc` act
/// on it; everything else is forwarded to the normal map (so the
/// user can still navigate / read while the prompt is up).
pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => return Some(Action::ConfirmYes),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                return Some(Action::ConfirmNo);
            }
            // Allow Ctrl+C to bail out of everything.
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => return Some(Action::Quit),
            _ => return None,
        }
    }
    match key.code {
        // 2026-06-08 sibling-sweep fix: Esc no longer quits the TUI.
        // Quitting via Esc is a footgun (every overlay uses Esc to
        // cancel — muscle memory propagates to the normal map and the
        // user closes the whole app). Keep `q` and `Ctrl+C` for quit;
        // Esc reserved for overlay-cancel only.
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('o') | KeyCode::Enter => Some(Action::OpenWeb),
        KeyCode::Char('y') => Some(Action::YankId),
        KeyCode::Char('p') => Some(Action::RequestPublish),
        KeyCode::Char('X') => Some(Action::RequestUnsubscribe),
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
        Action::OpenWeb => app.open_web(),
        Action::YankId => app.yank_id(),
        Action::RequestPublish => app.request_publish(),
        Action::RequestUnsubscribe => app.request_unsubscribe(),
        Action::ConfirmYes => app.confirm_yes(),
        Action::ConfirmNo => app.confirm_no(),
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
