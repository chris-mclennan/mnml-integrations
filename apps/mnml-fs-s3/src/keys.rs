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
    PopPrefix,
    YankUri,
    YankPresigned,
    OpenConsole,
    ArmDelete,
    StartUploadPrompt,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    // Upload-prompt keystroke capture — only emitted while the
    // upload prompt is active.
    UploadAppend(char),
    UploadBackspace,
    UploadSubmit,
    UploadCancel,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    // Upload-prompt keystroke capture — when active, all keys go
    // to the buffer except Enter (submit), Esc (cancel),
    // Backspace, Ctrl+C (cancel-as-quit).
    if app.upload_prompt.is_some() {
        return upload_prompt_key(key, ctrl);
    }
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
        KeyCode::Backspace | KeyCode::Char('h') => Some(Action::PopPrefix),
        KeyCode::Char('y') => Some(Action::YankUri),
        KeyCode::Char('Y') => Some(Action::YankPresigned),
        KeyCode::Char('o') => Some(Action::OpenConsole),
        KeyCode::Char('d') => Some(Action::ArmDelete),
        KeyCode::Char('u') => Some(Action::StartUploadPrompt),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

fn upload_prompt_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::UploadCancel),
        KeyCode::Char('c') if ctrl => Some(Action::UploadCancel),
        KeyCode::Enter => Some(Action::UploadSubmit),
        KeyCode::Backspace => Some(Action::UploadBackspace),
        KeyCode::Char(c) => Some(Action::UploadAppend(c)),
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
        Action::PopPrefix => app.pop_prefix(),
        Action::YankUri => app.yank_uri(),
        Action::YankPresigned => app.yank_presigned(),
        Action::OpenConsole => app.open_console(),
        Action::ArmDelete => app.arm_delete(),
        Action::StartUploadPrompt => app.start_upload_prompt(),
        Action::UploadAppend(c) => app.upload_append(c),
        Action::UploadBackspace => app.upload_backspace(),
        Action::UploadSubmit => app.upload_submit(),
        Action::UploadCancel => app.upload_cancel(),
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
