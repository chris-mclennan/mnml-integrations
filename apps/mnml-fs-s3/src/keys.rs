//! Keyboard chord → action mapping.

use crate::app::{App, UploadOverlay};
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
    // Upload overlay — picker phase.
    PickerUp,
    PickerDown,
    PickerPageUp,
    PickerPageDown,
    PickerHome,
    PickerEnd,
    PickerEnter,
    PickerPop,
    PickerToggle,
    PickerSelectAll,
    PickerClearSelection,
    // Upload overlay — either phase.
    UploadCancel,
    UploadCloseWhenDone,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    // Route to the overlay-specific handlers when open.
    match app.upload_overlay.as_ref() {
        Some(UploadOverlay::Pick(_)) => return picker_key(key, ctrl),
        Some(UploadOverlay::Progress(pg)) => return progress_key(key, ctrl, pg.all_done),
        None => {}
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

fn picker_key(key: KeyEvent, ctrl: bool) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::UploadCancel),
        KeyCode::Char('c') if ctrl => Some(Action::UploadCancel),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::PickerUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::PickerDown),
        KeyCode::PageUp => Some(Action::PickerPageUp),
        KeyCode::PageDown => Some(Action::PickerPageDown),
        KeyCode::Home => Some(Action::PickerHome),
        KeyCode::End => Some(Action::PickerEnd),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Some(Action::PickerEnter),
        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => Some(Action::PickerPop),
        KeyCode::Char(' ') => Some(Action::PickerToggle),
        KeyCode::Char('a') if ctrl => Some(Action::PickerSelectAll),
        KeyCode::Char('A') => Some(Action::PickerSelectAll),
        KeyCode::Char('C') => Some(Action::PickerClearSelection),
        _ => None,
    }
}

fn progress_key(key: KeyEvent, ctrl: bool, all_done: bool) -> Option<Action> {
    // While uploads run, only Esc / Ctrl+C dismiss (workers keep
    // running in background). Once all are done, any key closes.
    match key.code {
        KeyCode::Esc => Some(Action::UploadCancel),
        KeyCode::Char('c') if ctrl => Some(Action::UploadCancel),
        _ if all_done => Some(Action::UploadCloseWhenDone),
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
        Action::PickerUp => {
            if let Some(p) = app.picker_mut() {
                p.move_row(-1);
            }
        }
        Action::PickerDown => {
            if let Some(p) = app.picker_mut() {
                p.move_row(1);
            }
        }
        Action::PickerPageUp => {
            if let Some(p) = app.picker_mut() {
                p.move_row(-10);
            }
        }
        Action::PickerPageDown => {
            if let Some(p) = app.picker_mut() {
                p.move_row(10);
            }
        }
        Action::PickerHome => {
            if let Some(p) = app.picker_mut() {
                p.home();
            }
        }
        Action::PickerEnd => {
            if let Some(p) = app.picker_mut() {
                p.end();
            }
        }
        Action::PickerEnter => {
            let fired = app.picker_mut().and_then(|p| p.enter());
            if let Some(paths) = fired {
                app.upload_fire(paths);
            }
        }
        Action::PickerPop => {
            if let Some(p) = app.picker_mut() {
                p.pop();
            }
        }
        Action::PickerToggle => {
            if let Some(p) = app.picker_mut() {
                p.toggle();
            }
        }
        Action::PickerSelectAll => {
            if let Some(p) = app.picker_mut() {
                p.select_all_files();
            }
        }
        Action::PickerClearSelection => {
            if let Some(p) = app.picker_mut() {
                p.clear_selection();
            }
        }
        Action::UploadCancel | Action::UploadCloseWhenDone => app.upload_cancel(),
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
