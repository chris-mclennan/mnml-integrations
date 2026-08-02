//! Keypress → Action mapping. Routing:
//!   - When an overlay is open, keys act on the overlay first.
//!   - Otherwise, focused-pane rules apply.
//!
//! See README.md for the canonical key table.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Focus, Overlay};

pub enum Action {
    Quit,
    Noop,

    // Focus / overlays
    CycleFocus,
    CycleFocusReverse,
    OpenConnPicker,
    OpenHistoryPicker,
    OpenObjectPicker,
    CloseOverlay,
    OverlayUp,
    OverlayDown,
    OverlayAccept,
    OverlayChar(char),
    OverlayBackspace,

    // Editor
    EditorChar(char),
    EditorBackspace,
    EditorNewline,
    EditorMoveLeft,
    EditorMoveRight,
    EditorClear,
    RunStatement,
    RunAll,
    TriggerCompletion,

    // Results
    ResultUp,
    ResultDown,
    ResultPageUp,
    ResultPageDown,
    ResultTop,
    ResultBottom,
    ResultFilterChar(char),
    ResultFilterBackspace,
    ResultFilterClear,
    /// Expand the container at the cursor (documents only). No-op on
    /// other result kinds.
    ResultExpand,
    /// Collapse the container the cursor sits inside (documents only).
    ResultCollapse,

    // Schema tree
    TreeUp,
    TreeDown,
    TreeExpandOrEnter,
    TreeCollapse,

    // Misc
    DoubleRowLimit,
    SwitchConnectionIdx(usize),
}

pub fn handle(key: KeyEvent, app: &App) -> Action {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);

    // Always-on quit.
    match key.code {
        KeyCode::Char('c') if ctrl => return Action::Quit,
        _ => {}
    }

    // Overlay routing.
    match &app.overlay {
        Overlay::None => {}
        Overlay::ConnPicker { .. }
        | Overlay::HistoryPicker { .. }
        | Overlay::ObjectPicker { .. }
        | Overlay::Completion { .. } => {
            return match key.code {
                KeyCode::Esc => Action::CloseOverlay,
                KeyCode::Up => Action::OverlayUp,
                KeyCode::Down => Action::OverlayDown,
                KeyCode::Char('p') if ctrl => Action::OverlayUp,
                KeyCode::Char('n') if ctrl => Action::OverlayDown,
                KeyCode::Enter | KeyCode::Tab => Action::OverlayAccept,
                KeyCode::Backspace => Action::OverlayBackspace,
                KeyCode::Char(c) if !ctrl && !alt => Action::OverlayChar(c),
                _ => Action::Noop,
            };
        }
    }

    // Global chords (only when no overlay).
    match key.code {
        KeyCode::Char('k') if ctrl => return Action::OpenConnPicker,
        KeyCode::Char('h') if ctrl => return Action::OpenHistoryPicker,
        KeyCode::Char('p') if ctrl && shift => return Action::OpenObjectPicker,
        KeyCode::Char('P') if ctrl => return Action::OpenObjectPicker,
        KeyCode::F(5) => return Action::RunStatement,
        KeyCode::Enter if ctrl && shift => return Action::RunAll,
        KeyCode::Enter if ctrl => return Action::RunStatement,
        KeyCode::Char(c @ '1'..='9') if alt => {
            return Action::SwitchConnectionIdx((c as u8 - b'1') as usize);
        }
        KeyCode::Tab if !ctrl && !alt => return Action::CycleFocus,
        KeyCode::BackTab => return Action::CycleFocusReverse,
        KeyCode::Char('R') if !ctrl && !alt => return Action::DoubleRowLimit,
        _ => {}
    }

    // Focus-scoped handling.
    match app.focus {
        Focus::SchemaTree => match key.code {
            KeyCode::Up => Action::TreeUp,
            KeyCode::Down => Action::TreeDown,
            KeyCode::Char('k') => Action::TreeUp,
            KeyCode::Char('j') => Action::TreeDown,
            KeyCode::Right | KeyCode::Enter => Action::TreeExpandOrEnter,
            KeyCode::Left => Action::TreeCollapse,
            _ => Action::Noop,
        },
        Focus::Editor => match key.code {
            KeyCode::Backspace => Action::EditorBackspace,
            KeyCode::Enter => Action::EditorNewline,
            KeyCode::Left => Action::EditorMoveLeft,
            KeyCode::Right => Action::EditorMoveRight,
            KeyCode::Char('u') if ctrl => Action::EditorClear,
            KeyCode::Char(' ') if ctrl => Action::TriggerCompletion,
            KeyCode::Char(c) if !ctrl && !alt => Action::EditorChar(c),
            _ => Action::Noop,
        },
        Focus::Results => match key.code {
            // tester 2026-07-31 SEV-2 — the vim-style `k`/`j` arms
            // used to sit above the filter-typing arm, so any search
            // term containing those letters ("junk", "key", …)
            // silently dropped chars AND scrolled underneath the
            // filter row. Route to the filter FIRST when filtering,
            // then fall through to nav when not.
            KeyCode::Char(c) if !ctrl && !alt && !app.result_filter.is_empty() => {
                Action::ResultFilterChar(c)
            }
            KeyCode::Backspace if !app.result_filter.is_empty() => Action::ResultFilterBackspace,
            KeyCode::Esc if !app.result_filter.is_empty() => Action::ResultFilterClear,
            KeyCode::Up | KeyCode::Char('k') => Action::ResultUp,
            KeyCode::Down | KeyCode::Char('j') => Action::ResultDown,
            KeyCode::PageUp => Action::ResultPageUp,
            KeyCode::PageDown => Action::ResultPageDown,
            KeyCode::Home if ctrl => Action::ResultTop,
            KeyCode::End if ctrl => Action::ResultBottom,
            KeyCode::Char('/') => Action::ResultFilterChar('\0'), // sentinel: start empty
            KeyCode::Enter | KeyCode::Right => Action::ResultExpand,
            KeyCode::Left => Action::ResultCollapse,
            _ => Action::Noop,
        },
        Focus::ConnPicker | Focus::HistoryPicker | Focus::ObjectPicker => Action::Noop,
    }
}
