//! Keyboard chord → action mapping.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle(key: KeyEvent, app: &mut App) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::Right | KeyCode::Char('l') => app.expand_selected(),
        KeyCode::Left | KeyCode::Char('h') => app.collapse_selected(),
        KeyCode::Enter | KeyCode::Char(' ') => {
            let sel = app.selected;
            app.toggle_expand_at(sel);
        }
        // `s` fires start-build on the selected project.
        KeyCode::Char('s') => app.start_build(),
        _ => {}
    }
}
