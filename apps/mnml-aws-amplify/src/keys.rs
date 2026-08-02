//! Keyboard chord → action mapping. v0.1.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Refresh,
    Up,
    Down,
    /// `→` — expand the focused row if it's a collapsed app
    /// header. No-op on branches / already-expanded apps.
    ExpandFocused,
    /// `←` — collapse the focused row if it's an expanded app
    /// header. On a branch row, jumps up to (and collapses) the
    /// parent app header — matches most tree UIs.
    CollapseFocused,
    /// `E` — expand every visible app. Kicks off branch fetches
    /// for any that haven't been loaded yet.
    ExpandAll,
    /// `C` — collapse every app.
    CollapseAll,
    /// Alt-↑ — move the focused app one position earlier in the
    /// user's ordering. Persists to config.
    MoveAppUp,
    /// Alt-↓ — same, one position later.
    MoveAppDown,
    PageUp,
    PageDown,
    Home,
    End,
    /// `Enter` — context-aware primary action. On the "All apps"
    /// tab this opens the console URL (same as `o`); on an App
    /// tab this drills into the focused branch's latest deploy
    /// logs in-app.
    EnterFocused,
    /// `o` — always the AWS console URL, regardless of tab. Kept
    /// so users can still jump to the browser after `Enter`
    /// switched to the in-app logs viewer.
    OpenInBrowser,
    YankUrl,
    HandoffLogs,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    /// `x` — hide the selected app from the "All apps" list.
    /// Adds its app_id to `hidden_app_ids` in config and persists
    /// the file. In show-hidden mode (`H`), toggles the app back
    /// to visible.
    ToggleHideSelected,
    /// `H` — flip the show-hidden view. When on, hidden apps
    /// still show but dimmed; `x` unhides. When off (default),
    /// hidden apps are filtered out of the list entirely.
    ToggleShowHidden,
    /// `X` — clear the entire hidden list. Rescue for the
    /// "accidentally hid all my apps" case, so users don't have
    /// to press `x` on each one to restore. tree-redesign
    /// 2026-07-19.
    UnhideAll,
    /// Esc / q while the logs overlay is open — dismiss overlay.
    CloseLogsView,
    /// j / k / arrows / PgUp / PgDn / g / G while the logs
    /// overlay is open — scroll by `delta` rows.
    LogsScroll(i32),
    /// Esc / q while the deployment-history overlay is open.
    CloseDeploymentHistory,
    /// Up/Down inside the deployment-history table — clamp to
    /// the row range.
    DeploymentHistoryMove(isize),
    /// Enter on a deployment history row — open the LogsView
    /// for that specific job.
    DeploymentHistoryEnter,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    // When the logs overlay is open, keys route to it: Esc / q
    // closes the overlay (NOT the whole app), and j/k/↑↓/g/G
    // scroll. Ctrl+C still quits.
    if app.logs_view.is_some() {
        return match key.code {
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::CloseLogsView),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::LogsScroll(-1)),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::LogsScroll(1)),
            KeyCode::PageUp => Some(Action::LogsScroll(-20)),
            KeyCode::PageDown => Some(Action::LogsScroll(20)),
            KeyCode::Home | KeyCode::Char('g') => Some(Action::LogsScroll(-9999)),
            KeyCode::End | KeyCode::Char('G') => Some(Action::LogsScroll(9999)),
            _ => None,
        };
    }
    // Deployment-history overlay owns the keys next.
    if app.deployment_history.is_some() {
        return match key.code {
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::CloseDeploymentHistory),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::DeploymentHistoryMove(-1)),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::DeploymentHistoryMove(1)),
            KeyCode::PageUp => Some(Action::DeploymentHistoryMove(-10)),
            KeyCode::PageDown => Some(Action::DeploymentHistoryMove(10)),
            KeyCode::Home | KeyCode::Char('g') => Some(Action::DeploymentHistoryMove(-9999)),
            KeyCode::End | KeyCode::Char('G') => Some(Action::DeploymentHistoryMove(9999)),
            KeyCode::Enter => Some(Action::DeploymentHistoryEnter),
            _ => None,
        };
    }
    match key.code {
        // Esc used to quit, but this runs as a Pty pane inside mnml
        // (or any host) — Esc is a strong "cancel" reflex, and users
        // hit it expecting to back out of a sub-view, not to close
        // the whole integration. `q` and Ctrl+C still quit; Esc is
        // now a no-op at the top level (overlays above still use it
        // for close-this-overlay, which is what users actually want).
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Up if m.contains(KeyModifiers::ALT) => Some(Action::MoveAppUp),
        KeyCode::Down if m.contains(KeyModifiers::ALT) => Some(Action::MoveAppDown),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::Right | KeyCode::Char('l') => Some(Action::ExpandFocused),
        KeyCode::Left | KeyCode::Char('h') => Some(Action::CollapseFocused),
        KeyCode::Char('E') => Some(Action::ExpandAll),
        KeyCode::Char('C') => Some(Action::CollapseAll),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter => Some(Action::EnterFocused),
        KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('y') => Some(Action::YankUrl),
        KeyCode::Char('L') => Some(Action::HandoffLogs),
        KeyCode::Char('x') => Some(Action::ToggleHideSelected),
        KeyCode::Char('H') => Some(Action::ToggleShowHidden),
        KeyCode::Char('X') => Some(Action::UnhideAll),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Refresh => app.refresh_active(),
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::ExpandFocused => app.expand_focused(),
        Action::CollapseFocused => app.collapse_focused(),
        Action::ExpandAll => app.expand_all(),
        Action::CollapseAll => app.collapse_all(),
        Action::MoveAppUp => app.move_app_up(),
        Action::MoveAppDown => app.move_app_down(),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::EnterFocused => app.enter_focused(),
        Action::OpenInBrowser => app.open_focused(),
        Action::YankUrl => app.yank_focused_url(),
        Action::HandoffLogs => app.handoff_logs(),
        Action::ToggleHideSelected => app.toggle_hide_selected(),
        Action::ToggleShowHidden => app.toggle_show_hidden(),
        Action::UnhideAll => app.unhide_all(),
        Action::CloseLogsView => app.close_logs_view(),
        Action::LogsScroll(delta) => app.logs_scroll(delta),
        Action::CloseDeploymentHistory => app.close_deployment_history(),
        Action::DeploymentHistoryMove(delta) => app.deployment_history_move(delta),
        Action::DeploymentHistoryEnter => app.deployment_history_enter(),
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
