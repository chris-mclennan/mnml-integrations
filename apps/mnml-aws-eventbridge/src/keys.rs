//! Keyboard chord → action mapping.

use crate::app::{App, EditFocus, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle(key: KeyEvent, app: &mut App) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match app.mode {
        Mode::Browse => browse(key, ctrl, app),
        Mode::Edit => edit(key, ctrl, app),
        Mode::Saving => {
            if key.code == KeyCode::Esc {
                app.status = "waiting for save to complete…".to_string();
            }
        }
    }
}

fn browse(key: KeyEvent, ctrl: bool, app: &mut App) {
    match key.code {
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('r') => app.refresh_list(),
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
        // `e` enters the edit overlay for the selected row.
        KeyCode::Char('e') => app.enter_edit(),
        // `t` toggles ENABLED ⇄ DISABLED on the selected schedule.
        KeyCode::Char('t') => app.toggle_state(),
        _ => {}
    }
}

fn edit(key: KeyEvent, ctrl: bool, app: &mut App) {
    match key.code {
        KeyCode::Esc => app.cancel_edit(),
        KeyCode::Char('s') if ctrl => app.save_edit(),
        KeyCode::Tab => {
            app.edit.focus = match app.edit.focus {
                EditFocus::Expression => EditFocus::Input,
                EditFocus::Input => EditFocus::Expression,
            };
        }
        KeyCode::Left => app.edit.move_left(),
        KeyCode::Right => app.edit.move_right(),
        KeyCode::Up => app.edit.move_up(),
        KeyCode::Down => app.edit.move_down(),
        KeyCode::Home => app.edit.move_home(),
        KeyCode::End => app.edit.move_end(),
        KeyCode::Char('a') if ctrl => app.edit.move_home(),
        KeyCode::Char('e') if ctrl => app.edit.move_end(),
        KeyCode::Delete => app.edit.delete_forward(),
        KeyCode::Backspace => app.edit.backspace(),
        KeyCode::Enter => app.edit.insert_newline(),
        KeyCode::Char(c) if !ctrl => app.edit.insert_char(c),
        _ => {}
    }
}
