//! Keyboard chord → action mapping.

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
    /// `y` — copy the focused row's URL to the OS clipboard.
    /// Restores the pre-split `bitbucket.copy_selected_pr_url`
    /// / `bitbucket.copy_selected_url` palette commands.
    YankUrl,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    ToggleDetails,
    DetailScrollUp,
    DetailScrollDown,
    ToggleApproval,
    // tree-redesign 2026-07-14 phase 2d — workspace_pipelines
    // repo-tree navigation + config toggles that persist to
    // `~/.config/mnml-forge-bitbucket.toml`.
    /// Space / Enter on a repo row → toggle expand/collapse
    /// (matches mnml's file tree: Enter/Space activates).
    ToggleFocusedRepo,
    /// Right arrow on tree → expand focused repo (or descend to
    /// first child if already expanded). Matches mnml's file tree
    /// `Right` / `l` (`tree.expand_or_descend`).
    /// tree-redesign 2026-07-15.
    ExpandFocused,
    /// Left arrow on tree → collapse focused repo (or ascend to
    /// parent if on a child row). Matches mnml's file tree `Left`
    /// / `h` (`tree.collapse_or_ascend`).
    /// tree-redesign 2026-07-15.
    CollapseFocused,
    /// `e` — expand every repo in the tree. Lowercase to avoid
    /// clashing with `G` / `E` (End) semantics.
    ExpandAllRepos,
    /// `c` — collapse every repo in the tree.
    CollapseAllRepos,
    /// `x` — hide the focused repo (adds slug to `hidden_repos`,
    /// persists). Tree tab only.
    HideFocusedRepo,
    /// `H` — clear the entire `hidden_repos` list (unhide-all,
    /// persists). Not per-row because there's no UI to browse
    /// hidden repos yet.
    UnhideAll,
    /// `s` — cycle scope `all → recent → explicit → all`,
    /// invalidate cache, refresh. Persists. Single-chord
    /// cycle keeps the keyboard surface small; distinguishing
    /// A/R/E chords would collide with existing bindings.
    CycleScope,
    /// `Alt-↑` — move focused repo up in `repo_order`. Persists.
    ReorderRepoUp,
    /// `Alt-↓` — move focused repo down in `repo_order`. Persists.
    ReorderRepoDown,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let on_tree = matches!(
        app.active().data,
        crate::app::TabData::RepoTree { .. } | crate::app::TabData::RepoPrTree { .. }
    );
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if ctrl => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        // `Ctrl+U` / `Ctrl+D` scroll the detail pane when open. These
        // win over the plain `d` toggle below because of the
        // modifier check.
        KeyCode::Char('u') if ctrl => Some(Action::DetailScrollUp),
        KeyCode::Char('d') if ctrl => Some(Action::DetailScrollDown),
        // tree-redesign 2026-07-14 — Alt-↑ / Alt-↓ reorder before
        // the plain arrow-key nav below so the modifier form wins.
        KeyCode::Up if alt && on_tree => Some(Action::ReorderRepoUp),
        KeyCode::Down if alt && on_tree => Some(Action::ReorderRepoDown),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        // tree-redesign 2026-07-15 — Right/Left match mnml's file
        // tree convention (Right = expand-or-descend, Left =
        // collapse-or-ascend). vim-style `l` / `h` too.
        KeyCode::Right | KeyCode::Char('l') if on_tree => Some(Action::ExpandFocused),
        KeyCode::Left | KeyCode::Char('h') if on_tree => Some(Action::CollapseFocused),
        // Space always toggles expand on tree; Enter also toggles
        // (matches mnml's file tree `Enter | Space => activate`).
        KeyCode::Char(' ') if on_tree => Some(Action::ToggleFocusedRepo),
        KeyCode::Enter if on_tree => Some(Action::ToggleFocusedRepo),
        KeyCode::Enter | KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Char('y') => Some(Action::YankUrl),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        // 2026-07-20 — `m` toggles between the two PR-family tabs
        // (Open / Merged) without going through the tab strip.
        // Powers the "hotkey to switch to merged not as a tab"
        // ask for the split "Bitbucket — Pull Requests" chip.
        KeyCode::Char('m') => Some(Action::NextTab),
        // tree-redesign phase 2d — tree-only chords. Gated on
        // `on_tree` so they don't shadow the PR-tab bindings
        // (`e` isn't used elsewhere but `c`/`x` conflict with
        // future-proofing).
        KeyCode::Char('e') if on_tree => Some(Action::ExpandAllRepos),
        KeyCode::Char('c') if on_tree => Some(Action::CollapseAllRepos),
        KeyCode::Char('x') if on_tree => Some(Action::HideFocusedRepo),
        KeyCode::Char('H') if on_tree => Some(Action::UnhideAll),
        KeyCode::Char('s') if on_tree => Some(Action::CycleScope),
        // `d` (no modifiers) toggles the right-half detail panel.
        KeyCode::Char('d') => Some(Action::ToggleDetails),
        // `a` approve/unapprove — only meaningful with the detail
        // panel open (otherwise approve_pr would fire on a stale
        // approval state). The app method gates on details_visible.
        KeyCode::Char('a') => Some(Action::ToggleApproval),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    // Track focused key so we can lazy-fetch a new detail when the
    // user arrow-keys to a different row with the panel open.
    let pre_key = app.focused_key();
    match action {
        Action::Quit => return true,
        Action::Refresh => {
            if app.details_visible {
                app.invalidate_focused_detail();
            }
            app.refresh_active().await;
            if app.details_visible {
                app.ensure_focused_detail().await;
            }
        }
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
        Action::ToggleDetails => app.toggle_details().await,
        Action::DetailScrollUp => app.scroll_detail(-4),
        Action::DetailScrollDown => app.scroll_detail(4),
        Action::ToggleApproval => {
            if app.details_visible {
                app.toggle_approval().await;
            }
        }
        // tree-redesign 2026-07-14 phase 2d — repo-tree actions.
        // Config mutations persist via `crate::config::save`.
        // Errors surface on `app.status` (matches how other action
        // handlers report — `refresh_active`'s failure branch etc.).
        Action::ToggleFocusedRepo => app.smart_toggle_focused().await,
        Action::ExpandFocused => app.smart_expand_focused().await,
        Action::CollapseFocused => app.smart_collapse_focused(),
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
    }
    // After a navigation action, if the focused key changed and the
    // detail pane is open, fetch the new PR's detail. Reset the pane
    // scroll so a new PR starts at the top.
    if app.details_visible {
        let post_key = app.focused_key();
        if post_key != pre_key {
            app.details_scroll = 0;
            app.ensure_focused_detail().await;
        }
    }
    false
}
