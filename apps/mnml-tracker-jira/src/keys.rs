//! Keyboard chord → action mapping. Kept small and centralized.

use crate::app::{App, FilterClose};
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
    SwitchTab(usize),
    NextTab,
    PrevTab,
    ToggleDetails,
    DetailScrollUp,
    DetailScrollDown,
    OpenFilter,
    /// Filter-editor key (`Some(ch)` to insert; `None` for backspace).
    FilterEdit(Option<char>),
    FilterCommit,
    FilterCancel,
    /// Drop the committed filter, returning to the full list.
    FilterClear,
    /// `t` — open the status transition picker for the focused ticket.
    OpenTransitionPicker,
    /// Transition-picker arrow keys + digit jumps + commit + cancel.
    TransitionUp,
    TransitionDown,
    TransitionSelect(usize),
    TransitionCommit,
    TransitionCancel,
    /// `w` — toggle watch state on the focused ticket.
    ToggleWatch,
    /// `c` — open the inline comment editor (detail panel must be open).
    OpenCommentEditor,
    CommentEditorInsert(char),
    CommentEditorBackspace,
    CommentEditorEnter,
    CommentEditorSubmit,
    CommentEditorCancel,
    /// `Space` — toggle the focused row in the multi-selection set.
    ToggleSelection,
    /// Clear the multi-selection (part of the Esc cascade).
    ClearSelection,
    /// `a` — open assignee picker; `f` — open fixVersion picker.
    OpenAssigneePicker,
    OpenFixVersionPicker,
    FieldPickerInsert(char),
    FieldPickerBackspace,
    FieldPickerUp,
    FieldPickerDown,
    FieldPickerCommit,
    FieldPickerCancel,
    /// 2026-07-25 — tree tabs only. Enter/Space on the focused
    /// row: toggle group / toggle ticket-expand (fetches PRs) /
    /// open PR URL, depending on the row variant.
    TreeActivate,
    /// 2026-07-26 — Right/l on a tree tab: expand the focused row
    /// if not already expanded. No-op when already expanded (vs.
    /// TreeActivate which toggles). Non-tree tabs: no-op.
    TreeExpand,
    /// 2026-07-26 — Left/h on a tree tab: collapse the focused row.
    /// If cursor is on an already-collapsed group, collapse the
    /// group's parent (or no-op at root). Non-tree tabs: no-op.
    TreeCollapse,
    /// 2026-07-25 — dispatch a ticket-level Claude action for
    /// the focused ticket. Kind is one of "implement" / "fix" /
    /// "triage" — matches TicketButton::kind().
    DispatchTicket(&'static str),
    /// 2026-07-25 — dispatch a Review action for the focused
    /// LinkedPr row.
    DispatchReview,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;
    // Comment editor is a greedy modal — printable text inserts.
    // Ctrl+S posts; Esc cancels; Enter inserts a newline (multi-line).
    if app.comment_editor.is_some() {
        return match key.code {
            KeyCode::Esc => Some(Action::CommentEditorCancel),
            KeyCode::Char('s') if m.contains(KeyModifiers::CONTROL) => {
                Some(Action::CommentEditorSubmit)
            }
            KeyCode::Enter => Some(Action::CommentEditorEnter),
            KeyCode::Backspace => Some(Action::CommentEditorBackspace),
            KeyCode::Char(c) if !m.contains(KeyModifiers::CONTROL) => {
                Some(Action::CommentEditorInsert(c))
            }
            _ => None,
        };
    }
    // Field picker (assignee / fixVersion) — greedy modal with a
    // type-to-filter editor + arrow navigation. Comes before the
    // transition picker so they don't collide on overlapping chords.
    if app.field_picker.is_some() {
        return match key.code {
            KeyCode::Esc => Some(Action::FieldPickerCancel),
            KeyCode::Enter => Some(Action::FieldPickerCommit),
            KeyCode::Up => Some(Action::FieldPickerUp),
            KeyCode::Down => Some(Action::FieldPickerDown),
            KeyCode::Backspace => Some(Action::FieldPickerBackspace),
            KeyCode::Char(c) if !m.contains(KeyModifiers::CONTROL) => {
                Some(Action::FieldPickerInsert(c))
            }
            _ => None,
        };
    }
    // Transition picker is a greedy modal — its keys win first.
    if app.transition_picker.is_some() {
        return match key.code {
            KeyCode::Esc => Some(Action::TransitionCancel),
            KeyCode::Enter => Some(Action::TransitionCommit),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::TransitionUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::TransitionDown),
            KeyCode::Char(c @ '1'..='9') => {
                Some(Action::TransitionSelect((c as u8 - b'1') as usize))
            }
            _ => None,
        };
    }
    // When the filter editor is open it greedily swallows keystrokes —
    // every printable becomes part of the buffer, Esc cancels, Enter
    // commits. The list/tab/detail chords below resume after the
    // filter closes. This branch comes first so that e.g. `q` in the
    // editor types a `q` instead of quitting the app.
    if let Some(f) = app.filter.as_ref()
        && f.editing
    {
        return match key.code {
            KeyCode::Esc => Some(Action::FilterCancel),
            KeyCode::Enter => Some(Action::FilterCommit),
            KeyCode::Backspace => Some(Action::FilterEdit(None)),
            KeyCode::Char(c) if !m.contains(KeyModifiers::CONTROL) => {
                Some(Action::FilterEdit(Some(c)))
            }
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        // Esc cascade: clear selection → clear filter → close
        // detail → quit. Esc-Esc-Esc-Esc safely unwinds.
        KeyCode::Esc => {
            if !app.selection.is_empty() {
                Some(Action::ClearSelection)
            } else if app.filter.is_some() {
                Some(Action::FilterClear)
            } else if app.details_visible {
                Some(Action::ToggleDetails)
            } else {
                Some(Action::Quit)
            }
        }
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        // Ctrl+u / Ctrl+d scroll the detail pane while it's open
        // (only meaningful when there is one — otherwise no-op).
        KeyCode::Char('u') if m.contains(KeyModifiers::CONTROL) => Some(Action::DetailScrollUp),
        KeyCode::Char('d') if m.contains(KeyModifiers::CONTROL) => Some(Action::DetailScrollDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        // 2026-07-25 — tree tabs: Enter routes to tree activation
        // (toggle group / expand ticket / open PR URL). Non-tree
        // tabs keep the flat-table Enter=OpenInBrowser behavior.
        // `o` always opens in browser regardless of tab kind.
        KeyCode::Enter if app.active().tree.is_some() => Some(Action::TreeActivate),
        // 2026-07-26 — vim/tree convention: Right/l expands the
        // focused row (or descends into a child if already
        // expanded); Left/h collapses. Bound only on tree tabs so
        // flat tabs keep their unchanged nav (no-op arrows).
        KeyCode::Right | KeyCode::Char('l') if app.active().tree.is_some() => {
            Some(Action::TreeExpand)
        }
        KeyCode::Left | KeyCode::Char('h') if app.active().tree.is_some() => {
            Some(Action::TreeCollapse)
        }
        KeyCode::Enter | KeyCode::Char('o') => Some(Action::OpenInBrowser),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        // `/` opens the filter editor — substring match against key
        // + summary, case-insensitive, applies live as you type.
        KeyCode::Char('/') => Some(Action::OpenFilter),
        // `t` opens the status-transition picker for the focused
        // ticket (greedy modal; picker keys take over the next event).
        KeyCode::Char('t') => Some(Action::OpenTransitionPicker),
        // `w` toggles watch on the focused ticket.
        KeyCode::Char('w') => Some(Action::ToggleWatch),
        // `c` opens the inline comment editor (only meaningful when the
        // detail panel is already visible).
        KeyCode::Char('c') if app.details_visible => Some(Action::OpenCommentEditor),
        // Space: tree tabs → TreeActivate (same as Enter). Flat
        // tabs → ToggleSelection (bulk-op basket).
        KeyCode::Char(' ') if app.active().tree.is_some() => Some(Action::TreeActivate),
        KeyCode::Char(' ') => Some(Action::ToggleSelection),
        // 2026-07-25 — tree-tab action button chords. Uppercase to
        // avoid stealing common lowercase chords (`i` = insert on
        // future editor modes, etc.). Route through the dispatch
        // layer; TicketButton kind() strings match here.
        KeyCode::Char('I') if app.active().tree.is_some() => {
            Some(Action::DispatchTicket("implement"))
        }
        KeyCode::Char('X') if app.active().tree.is_some() => Some(Action::DispatchTicket("fix")),
        KeyCode::Char('T') if app.active().tree.is_some() => Some(Action::DispatchTicket("triage")),
        KeyCode::Char('V') if app.active().tree.is_some() => Some(Action::DispatchReview),
        // `a` opens the assignee picker; `f` the fixVersion picker.
        // Both work on selection if non-empty, else focused row.
        KeyCode::Char('a') => Some(Action::OpenAssigneePicker),
        KeyCode::Char('f') => Some(Action::OpenFixVersionPicker),
        // `d` (lowercase, no modifiers) toggles the detail pane.
        // Ctrl+d above takes precedence for scroll-down.
        KeyCode::Char('d') => Some(Action::ToggleDetails),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub async fn apply(action: Action, app: &mut App) -> bool {
    // Track selection movement so we can lazy-fetch a new detail
    // when the user arrow-keys to a different row with the panel open.
    let pre_key = app.focused_key();
    match action {
        Action::Quit => return true,
        Action::Refresh => {
            // `r` while the detail pane is visible re-fetches both the
            // list AND the focused ticket's narrative, so a status
            // transition / new comment shows up.
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
        Action::DetailScrollUp => {
            if app.details_visible {
                app.details_scroll = app.details_scroll.saturating_sub(4);
            }
        }
        Action::DetailScrollDown => {
            if app.details_visible {
                app.details_scroll = app.details_scroll.saturating_add(4);
            }
        }
        Action::OpenFilter => app.open_filter(),
        Action::FilterEdit(Some(c)) => app.filter_insert(c),
        Action::FilterEdit(None) => app.filter_backspace(),
        Action::FilterCommit => app.close_filter(FilterClose::Commit),
        Action::FilterCancel => app.close_filter(FilterClose::Cancel),
        Action::FilterClear => app.filter = None,
        Action::OpenTransitionPicker => app.open_transition_picker().await,
        Action::TransitionUp => app.transition_picker_move(-1),
        Action::TransitionDown => app.transition_picker_move(1),
        Action::TransitionSelect(i) => app.transition_picker_select(i),
        Action::TransitionCommit => app.commit_transition().await,
        Action::TransitionCancel => app.close_transition_picker(),
        Action::ToggleWatch => app.toggle_watch().await,
        Action::OpenCommentEditor => app.open_comment_editor(),
        Action::CommentEditorInsert(c) => app.comment_editor_insert(c),
        Action::CommentEditorBackspace => app.comment_editor_backspace(),
        Action::CommentEditorEnter => app.comment_editor_insert('\n'),
        Action::CommentEditorSubmit => app.submit_comment().await,
        Action::CommentEditorCancel => app.close_comment_editor(),
        Action::ToggleSelection => app.toggle_selection(),
        Action::ClearSelection => app.clear_selection(),
        Action::OpenAssigneePicker => app.open_assignee_picker().await,
        Action::OpenFixVersionPicker => app.open_fix_version_picker().await,
        Action::FieldPickerInsert(c) => app.field_picker_filter_insert(c),
        Action::FieldPickerBackspace => app.field_picker_filter_backspace(),
        Action::FieldPickerUp => app.field_picker_move(-1),
        Action::FieldPickerDown => app.field_picker_move(1),
        Action::FieldPickerCommit => app.commit_field_picker().await,
        Action::FieldPickerCancel => app.close_field_picker(),
        Action::TreeActivate => app.tree_activate_focused().await,
        Action::TreeExpand => app.tree_expand_focused().await,
        Action::TreeCollapse => app.tree_collapse_focused(),
        Action::DispatchTicket(kind) => app.dispatch_ticket_action(kind),
        Action::DispatchReview => app.dispatch_review_focused_pr(),
    }
    // After a navigation action, if the focused key changed and the
    // detail pane is open, fetch the new ticket's detail. Reset the
    // pane scroll so a new ticket starts at the top.
    if app.details_visible
        && let post_key = app.focused_key()
        && post_key != pre_key
    {
        app.details_scroll = 0;
        app.ensure_focused_detail().await;
    }
    false
}
