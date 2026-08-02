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
    OpenDesktop,
    YankId,
    TailLogs,
    ExecShell,
    StopContainer,
    StartContainer,
    EnterRmConfirm,
    ConfirmRm,
    CancelRm,
    HandoffEcr,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    // RM-confirmation overlay steals y / n / Esc and nothing else.
    if app.rm_pending.is_some() {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(Action::ConfirmRm),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Action::CancelRm),
            _ => None,
        };
    }

    let m = key.modifiers;
    match key.code {
        // 2026-06-08 sibling-sweep fix: Esc no longer quits the TUI
        // (footgun — every overlay uses Esc to cancel + the muscle
        // memory escapes to the normal map and closes the app). Keep
        // `q` and `Ctrl+C` for quit; Esc reserved for overlay-cancel
        // (the rm-confirmation overlay above already uses it).
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char('o') => Some(Action::OpenDesktop),
        KeyCode::Char('y') => Some(Action::YankId),
        KeyCode::Char('l') => Some(Action::TailLogs),
        KeyCode::Char('e') => Some(Action::ExecShell),
        KeyCode::Char('s') => Some(Action::StopContainer),
        KeyCode::Char('S') => Some(Action::StartContainer),
        KeyCode::Char('R') => Some(Action::EnterRmConfirm),
        KeyCode::Char('L') => Some(Action::HandoffEcr),
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
        Action::OpenDesktop => app.open_docker_desktop(),
        Action::YankId => app.yank_id(),
        Action::TailLogs => app.tail_logs(),
        Action::ExecShell => app.exec_shell(),
        Action::StopContainer => app.stop_or_start(false),
        Action::StartContainer => app.stop_or_start(true),
        Action::EnterRmConfirm => app.enter_rm_confirm(),
        Action::ConfirmRm => app.confirm_rm(),
        Action::CancelRm => app.cancel_rm(),
        Action::HandoffEcr => app.handoff_ecr(),
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
