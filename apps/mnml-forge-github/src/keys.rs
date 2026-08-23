//! Keyboard chord → action mapping.
//!
//! workspace-tabs 2026-08-22 — tree chords mirror the design in
//! mnml-forge-bitbucket 0.3.29 so users' muscle memory works across
//! both siblings (see [integration tree nav convention] memory).

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
    OpenInBrowser,
    YankUrl,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    // Tree chords (workspace_* tabs only).
    ToggleFocusedRepo,
    ExpandFocused,
    CollapseFocused,
    ExpandAllRepos,
    CollapseAllRepos,
    HideFocusedRepo,
    UnhideAll,
    CycleScope,
    ReorderRepoUp,
    ReorderRepoDown,
    /// `m` — flip the active tab's `mine_only` post-fetch filter and
    /// refresh. No-op on non-workspace-PR tabs.
    ToggleMineOnly,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let on_tree = app.on_tree();
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if ctrl => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        // Alt-↑ / Alt-↓ reorder before the plain arrow-key nav so
        // the modifier form wins.
        KeyCode::Up if alt && on_tree => Some(Action::ReorderRepoUp),
        KeyCode::Down if alt && on_tree => Some(Action::ReorderRepoDown),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Right | KeyCode::Char('l') if on_tree => Some(Action::ExpandFocused),
        KeyCode::Left | KeyCode::Char('h') if on_tree => Some(Action::CollapseFocused),
        KeyCode::Char(' ') if on_tree => Some(Action::ToggleFocusedRepo),
        KeyCode::Enter if on_tree => Some(Action::ToggleFocusedRepo),
        KeyCode::Enter | KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('y') => Some(Action::YankUrl),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        // Tree-only chords, gated so they don't shadow legacy tab bindings.
        KeyCode::Char('e') if on_tree => Some(Action::ExpandAllRepos),
        KeyCode::Char('c') if on_tree => Some(Action::CollapseAllRepos),
        KeyCode::Char('x') if on_tree => Some(Action::HideFocusedRepo),
        KeyCode::Char('H') if on_tree => Some(Action::UnhideAll),
        KeyCode::Char('s') if on_tree => Some(Action::CycleScope),
        KeyCode::Char('m') if on_tree => Some(Action::ToggleMineOnly),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Refresh => app.refresh_active().await,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenInBrowser => app.open_focused(),
        Action::YankUrl => app.yank_focused_url(),
        Action::NextTab => {
            let next = (app.active_tab + 1) % app.tabs.len();
            app.switch_tab(next);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::PrevTab => {
            let prev = if app.active_tab == 0 {
                app.tabs.len() - 1
            } else {
                app.active_tab - 1
            };
            app.switch_tab(prev);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::SwitchTab(i) => {
            app.switch_tab(i);
            if app.tabs[app.active_tab].last_fetched.is_none() {
                app.refresh_active().await;
            }
        }
        Action::ToggleFocusedRepo => {
            if app.focus_is_show_more_footer() {
                app.set_show_all_prs(true);
            } else {
                app.tree_toggle_focused_repo();
            }
        }
        Action::ExpandFocused => app.tree_expand_focused(),
        Action::CollapseFocused => app.tree_collapse_focused(),
        Action::ExpandAllRepos => app.tree_expand_all(),
        Action::CollapseAllRepos => app.tree_collapse_all(),
        Action::HideFocusedRepo => {
            if let Err(e) = app.tree_hide_focused_repo().await {
                app.status = format!("hide: {e}");
            }
        }
        Action::UnhideAll => {
            if let Err(e) = app.tree_unhide_all().await {
                app.status = format!("unhide: {e}");
            }
        }
        Action::CycleScope => {
            if let Err(e) = app.tree_cycle_scope().await {
                app.status = format!("scope: {e}");
            }
        }
        Action::ReorderRepoUp => {
            if let Err(e) = app.tree_reorder_focused(-1) {
                app.status = format!("reorder: {e}");
            }
        }
        Action::ReorderRepoDown => {
            if let Err(e) = app.tree_reorder_focused(1) {
                app.status = format!("reorder: {e}");
            }
        }
        Action::ToggleMineOnly => {
            app.toggle_active_mine_only();
            app.refresh_active().await;
        }
    }
    false
}
