//! Keyboard chord → action mapping. v0.1.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Refresh,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    EnterFocused,
    PopView,
    YankUri,
    YankPresigned,
    OpenPortal,
    ArmDelete,
    SwitchTab(usize),
    NextTab,
    PrevTab,
}

pub fn handle(key: KeyEvent, _app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if ctrl => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter => Some(Action::EnterFocused),
        KeyCode::Backspace | KeyCode::Char('h') => Some(Action::PopView),
        KeyCode::Char('y') => Some(Action::YankUri),
        KeyCode::Char('Y') => Some(Action::YankPresigned),
        KeyCode::Char('o') => Some(Action::OpenPortal),
        KeyCode::Char('d') => Some(Action::ArmDelete),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    // If a confirmation is pending, the next key must be `y` to
    // confirm; anything else cancels and falls through to the
    // normal handling (so the user can cancel + scroll in one move
    // by hitting `j` etc.).
    if app.pending_confirm.is_some() {
        match action {
            Action::YankUri => {
                // `y` confirms the pending action — we re-purpose
                // YankUri here since both are bound to `y`.
                app.confirm();
                return false;
            }
            _ => {
                app.cancel_confirm();
                // Fall through to the action below.
            }
        }
    }
    match action {
        Action::Quit => return true,
        Action::Refresh => app.refresh_active(),
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::EnterFocused => app.enter_focused(),
        Action::PopView => app.pop_view(),
        Action::YankUri => app.yank_uri(),
        Action::YankPresigned => app.yank_presigned(),
        Action::OpenPortal => app.open_portal(),
        Action::ArmDelete => app.arm_delete(),
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
