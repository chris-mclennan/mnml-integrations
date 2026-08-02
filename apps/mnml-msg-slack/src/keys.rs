//! Keyboard chord → action mapping. v0.1.

use crate::app::{App, InputMode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    OpenThread,
    BeginSearch,
    BeginPost,
    BeginThreadReply,
    BeginReaction,
    YankPermalink,
    Refresh,
    HideFocusedChannel,
    TogglePinFocusedChannel,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    // Input bar (one-line text editor)
    InputChar(char),
    InputBackspace,
    InputSubmit,
    InputCancel,
    // Reaction picker
    ReactionLeft,
    ReactionRight,
    ReactionUp,
    ReactionDown,
    ReactionSubmit,
    ReactionCancel,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;

    // Reaction picker first — owns all keys when open.
    if app.reaction_picker.is_some() {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::ReactionCancel),
            KeyCode::Enter => Some(Action::ReactionSubmit),
            KeyCode::Left | KeyCode::Char('h') => Some(Action::ReactionLeft),
            KeyCode::Right | KeyCode::Char('l') => Some(Action::ReactionRight),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::ReactionUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::ReactionDown),
            _ => None,
        };
    }

    // Input bar next.
    if let Some(bar) = &app.input {
        let _ = bar;
        return match key.code {
            KeyCode::Esc => Some(Action::InputCancel),
            KeyCode::Enter => Some(Action::InputSubmit),
            KeyCode::Backspace => Some(Action::InputBackspace),
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::InputCancel),
            KeyCode::Char('u') if m.contains(KeyModifiers::CONTROL) => {
                // Clear-line shortcut: emulate Backspace until empty via
                // a dedicated action would be cleaner; v0.1 just inserts
                // a NAK (skipped) and the user can hammer Backspace.
                None
            }
            KeyCode::Char(c) => Some(Action::InputChar(c)),
            _ => None,
        };
    }

    // Passive (browse) mode.
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter => Some(Action::OpenThread),
        KeyCode::Char('/') => Some(Action::BeginSearch),
        KeyCode::Char('p') => Some(Action::BeginPost),
        KeyCode::Char('T') => Some(Action::BeginThreadReply),
        KeyCode::Char('R') => Some(Action::BeginReaction),
        KeyCode::Char('y') => Some(Action::YankPermalink),
        KeyCode::Char('r') => Some(Action::Refresh),
        // `x` — hide the focused channel from view (persists to
        // config `[channels].hide`). Sibling tree-nav convention.
        KeyCode::Char('x') => Some(Action::HideFocusedChannel),
        // `P` — toggle pin (persists to `[channels].pin`). Capital
        // to avoid clobbering `p` = BeginPost.
        KeyCode::Char('P') => Some(Action::TogglePinFocusedChannel),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::OpenThread => app.open_thread(),
        Action::BeginSearch => app.begin_search(),
        Action::BeginPost => app.begin_post(),
        Action::BeginThreadReply => app.begin_thread_reply(),
        Action::BeginReaction => app.begin_reaction(),
        Action::YankPermalink => app.yank_permalink(),
        Action::Refresh => app.refresh_force(),
        Action::HideFocusedChannel => app.hide_focused_channel(),
        Action::TogglePinFocusedChannel => app.toggle_pin_focused_channel(),
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

        Action::InputChar(c) => {
            if let Some(bar) = &mut app.input {
                bar.buffer.push(c);
            }
        }
        Action::InputBackspace => {
            if let Some(bar) = &mut app.input {
                bar.buffer.pop();
            }
        }
        Action::InputSubmit => app.submit_input(),
        Action::InputCancel => app.cancel_input(),

        Action::ReactionLeft => move_reaction(app, -1),
        Action::ReactionRight => move_reaction(app, 1),
        Action::ReactionUp => move_reaction(app, -6),
        Action::ReactionDown => move_reaction(app, 6),
        Action::ReactionSubmit => app.submit_reaction(),
        Action::ReactionCancel => app.cancel_reaction(),
    }
    false
}

fn move_reaction(app: &mut App, delta: isize) {
    use crate::slack::QUICK_REACTIONS;
    let Some(p) = &mut app.reaction_picker else {
        return;
    };
    let n = QUICK_REACTIONS.len() as isize;
    if n == 0 {
        return;
    }
    let cur = p.selected as isize;
    let next = (cur + delta).rem_euclid(n);
    p.selected = next as usize;
    let _ = InputMode::Search; // keep import warning-free
}
