//! Keyboard chord → action mapping. v0.3.

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
    OpenConsole,
    YankArn,
    YankEndpoint,
    HandoffEndpoint,
    EnterPublish,
    PublishChar(char),
    PublishBackspace,
    PublishCommit,
    PublishCancel,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    // Publish mode steals all keys until Enter / Esc.
    if app.publish_editing.is_some() {
        return match key.code {
            KeyCode::Enter => Some(Action::PublishCommit),
            KeyCode::Esc => Some(Action::PublishCancel),
            KeyCode::Backspace => Some(Action::PublishBackspace),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::PublishChar(c))
            }
            _ => None,
        };
    }

    let m = key.modifiers;
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('o') | KeyCode::Enter => Some(Action::OpenConsole),
        KeyCode::Char('y') => Some(Action::YankArn),
        KeyCode::Char('Y') => Some(Action::YankEndpoint),
        KeyCode::Char('L') => Some(Action::HandoffEndpoint),
        KeyCode::Char('P') => Some(Action::EnterPublish),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenConsole => app.open_console(),
        Action::YankArn => app.yank_arn(),
        Action::YankEndpoint => app.yank_endpoint(),
        Action::HandoffEndpoint => app.handoff_endpoint(),
        Action::EnterPublish => app.enter_publish_mode(),
        Action::PublishChar(c) => app.publish_input_char(c),
        Action::PublishBackspace => app.publish_input_backspace(),
        Action::PublishCommit => app.publish_commit(),
        Action::PublishCancel => app.publish_cancel(),
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
