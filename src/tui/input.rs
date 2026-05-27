//! Input handling — key events and text editing.

use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyModifiers};

use super::app::{App, ChatLine, Pane};

/// Result of handling a key event.
pub enum HandleResult {
    /// Continue running.
    Continue,
    /// Send the current input buffer as a chat message.
    SendMessage(String),
    /// Quit the TUI.
    Quit,
}

pub fn handle_key(app: &mut App, event: CrosstermEvent) -> HandleResult {
    let CrosstermEvent::Key(key) = event else {
        return HandleResult::Continue;
    };

    // Global keybindings (work regardless of focus)
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('q')) => return HandleResult::Quit,
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.focus = app.focus.next();
            return HandleResult::Continue;
        }
        (KeyModifiers::SHIFT, KeyCode::BackTab) => {
            app.focus = app.focus.prev();
            return HandleResult::Continue;
        }
        (KeyModifiers::NONE, KeyCode::Char('1')) => {
            app.focus = Pane::Skills;
            return HandleResult::Continue;
        }
        (KeyModifiers::NONE, KeyCode::Char('2')) => {
            app.focus = Pane::Memory;
            return HandleResult::Continue;
        }
        (KeyModifiers::NONE, KeyCode::Char('3')) => {
            app.focus = Pane::Chat;
            return HandleResult::Continue;
        }
        (KeyModifiers::NONE, KeyCode::Char('4')) => {
            app.focus = Pane::Input;
            return HandleResult::Continue;
        }
        _ => {}
    }

    // Pane-specific keybindings
    match app.focus {
        Pane::Input => handle_input_keys(app, key),
        Pane::Skills => handle_skills_keys(app, key),
        Pane::Memory => handle_memory_keys(app, key),
        Pane::Chat => handle_chat_keys(app, key),
    }
}

fn handle_input_keys(app: &mut App, key: crossterm::event::KeyEvent) -> HandleResult {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if app.input.multiline {
                app.input.insert('\n');
                HandleResult::Continue
            } else {
                let msg = app.input.buffer.clone();
                if msg.is_empty() {
                    HandleResult::Continue
                } else {
                    app.input.clear();
                    HandleResult::SendMessage(msg)
                }
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Enter) => {
            if app.input.multiline {
                let msg = app.input.buffer.clone();
                if msg.is_empty() {
                    HandleResult::Continue
                } else {
                    app.input.clear();
                    HandleResult::SendMessage(msg)
                }
            } else {
                app.input.insert('\n');
                HandleResult::Continue
            }
        }
        (KeyModifiers::NONE, KeyCode::Char(c)) => {
            app.input.insert(c);
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Backspace) => {
            app.input.backspace();
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Delete) => {
            app.input.delete();
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Left) => {
            app.input.move_left();
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Right) => {
            app.input.move_right();
            HandleResult::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            app.input.move_home();
            HandleResult::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            app.input.move_end();
            HandleResult::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            app.input.buffer.clear();
            app.input.cursor = 0;
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Esc) => {
            app.input.clear();
            HandleResult::Continue
        }
        _ => HandleResult::Continue,
    }
}

fn handle_skills_keys(app: &mut App, key: crossterm::event::KeyEvent) -> HandleResult {
    let count = app.agent.skills.skills.len();
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            if app.skills_selected > 0 {
                app.skills_selected -= 1;
            }
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            if app.skills_selected + 1 < count {
                app.skills_selected += 1;
            }
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Enter) => {
            // Copy skill info to chat
            if let Some(skill) = app.agent.skills.skills.get(app.skills_selected) {
                let cat = skill.category.as_deref().unwrap_or("other");
                let _msg = format!("/skills");
                // Just show skill details as a chat line
                app.chat_lines.push(ChatLine::System(
                    format!("[{}] {} — {}", cat, skill.name, skill.description),
                ));
            }
            HandleResult::Continue
        }
        _ => HandleResult::Continue,
    }
}

fn handle_memory_keys(app: &mut App, key: crossterm::event::KeyEvent) -> HandleResult {
    let total = app.agent.memory.user_entries.len() + app.agent.memory.memory_entries.len();
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            if app.memory_selected > 0 {
                app.memory_selected -= 1;
            }
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            if app.memory_selected + 1 < total {
                app.memory_selected += 1;
            }
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Enter) => {
            let user_count = app.agent.memory.user_entries.len();
            let entry = if app.memory_selected < user_count {
                app.agent.memory.user_entries.get(app.memory_selected)
                    .map(|s| s.as_str())
            } else {
                app.agent.memory.memory_entries.get(app.memory_selected - user_count)
                    .map(|s| s.as_str())
            };
            if let Some(text) = entry {
                let display: String = text.chars().take(200).collect();
                app.chat_lines.push(ChatLine::System(
                    format!("Memory entry: {}", display),
                ));
            }
            HandleResult::Continue
        }
        _ => HandleResult::Continue,
    }
}

fn handle_chat_keys(app: &mut App, key: crossterm::event::KeyEvent) -> HandleResult {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            app.chat_scroll = app.chat_scroll.saturating_sub(1);
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            app.chat_scroll += 1;
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::PageUp) => {
            app.chat_scroll = app.chat_scroll.saturating_sub(10);
            HandleResult::Continue
        }
        (KeyModifiers::NONE, KeyCode::PageDown) => {
            app.chat_scroll += 10;
            HandleResult::Continue
        }
        _ => HandleResult::Continue,
    }
}