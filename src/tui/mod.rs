//! TUI — full-screen pane-based terminal UI for Enchanter.
//!
//! Entry point: `run_tui()`. Sets up raw mode, alternate screen, creates the
//! App state, and enters the event loop.
//!
//! The event loop multiplexes between:
//! - crossterm terminal events (keyboard, resize)
//! - agent streaming events (content tokens, tool calls, done)
//!
//! Agent ownership follows the same pattern as the REPL: the AgentSession is
//! stored in an Option — None while a background task owns it, restored when
//! the task completes and returns it via JoinHandle.

pub mod model_info;
pub mod render;
pub mod state;

use std::io::stdout;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::agent::AgentSession;
use crate::protocol::Event;
use crate::tui::state::{ChatLine, Focus, TuiState};

/// Run the full-screen TUI. Returns the agent session so the caller can
/// do exit summaries, MCP shutdown, etc. — same contract as run_repl.
pub async fn run_tui(agent: AgentSession) -> Result<AgentSession> {
    // Setup terminal.
    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        Hide,
        EnableMouseCapture,
        // Kitty keyboard protocol: disambiguate escape codes so we get
        // accurate modifier info (Shift+Enter, Alt+Enter, etc.).
        // Not all terminals support this — those that don't will just ignore it.
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new(&agent);

    // Dedicated OS thread for crossterm events.
    // crossterm's event::poll/read are blocking sync calls. Spawning a new
    // spawn_blocking per loop iteration causes races — multiple threads compete
    // for crossterm's global event source and events get silently dropped.
    // A single long-lived thread owns the event source and forwards events
    // through an unbounded tokio channel (sync send from the OS thread).
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<CrosstermEvent>();
    thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(50)).is_ok()
                && let Ok(ev) = event::read()
                    && key_tx.send(ev).is_err() {
                        break; // receiver dropped — TUI exiting
                    }
        }
    });

    // Query model context info in the background (best-effort).
    let base_url = agent.resolved.base_url.clone();
    let api_key = agent.resolved.api_key.clone();
    let (info_tx, mut info_rx) =
        mpsc::channel::<std::collections::HashMap<String, state::ModelContextInfo>>(1);
    tokio::spawn(async move {
        let info = model_info::fetch_model_context_info(&base_url, api_key.as_deref()).await;
        let _ = info_tx.send(info).await;
    });

    // Agent is stored in Option — None while a spawned task has it.
    let mut agent_slot: Option<AgentSession> = Some(agent);

    // Channel for agent events from a background task.
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(128);
    // Track the join handle so we can recover the agent when streaming finishes.
    let mut agent_handle: Option<tokio::task::JoinHandle<Result<AgentSession>>> = None;

    let result = run_event_loop(
        &mut terminal,
        &mut state,
        &mut agent_slot,
        &mut agent_handle,
        &mut event_rx,
        &event_tx,
        &mut info_rx,
        &mut key_rx,
    )
    .await;

    // Cleanup terminal.
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags, DisableMouseCapture);
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
    terminal.flush()?;

    result?;

    // Recover agent from Option. Under normal flow, the agent is always
    // restored here — either it was never taken (quit while idle), or the
    // event loop waited for the join handle before returning (pending_quit).
    // The fallback handles any unexpected edge case by waiting on the handle.
    let agent = match agent_slot {
        Some(a) => a,
        None if agent_handle.is_some() => {
            // Agent still in a background task — await it.
            let handle = agent_handle.take().unwrap();
            match handle.await {
                Ok(Ok(a)) => a,
                Ok(Err(e)) => return Err(e),
                Err(e) => return Err(anyhow::anyhow!("task join error: {}", e)),
            }
        }
        None => {
            return Err(anyhow::anyhow!(
                "agent was not recovered from spawned task on exit"
            ));
        }
    };
    Ok(agent)
}

/// Main event loop — multiplexes terminal input and agent streaming events.
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
    agent_handle: &mut Option<tokio::task::JoinHandle<Result<AgentSession>>>,
    event_rx: &mut mpsc::Receiver<Event>,
    event_tx: &mpsc::Sender<Event>,
    info_rx: &mut mpsc::Receiver<std::collections::HashMap<String, state::ModelContextInfo>>,
    key_rx: &mut mpsc::UnboundedReceiver<CrosstermEvent>,
) -> Result<()> {
    loop {
        // Render the current state.
        terminal.draw(|frame| render::render(frame, state))?;

        // Animate the spinner while streaming (even before first token).
        if state.is_streaming {
            state.spinner_frame = (state.spinner_frame + 1) % 4;
        }

        // Check if the agent task has finished — recover the agent.
        if state.is_streaming && agent_handle.is_some() {
            // Try to poll the join handle without blocking.
            match tokio::time::timeout(Duration::from_millis(0), agent_handle.as_mut().unwrap())
                .await
            {
                Ok(join_res) => {
                    *agent_handle = None;
                    state.is_streaming = false;
                    state.has_first_content = false;
                    match join_res {
                        Ok(Ok(returned_agent)) => {
                            *agent_slot = Some(returned_agent);
                            if let Some(ref agent) = *agent_slot {
                                state.update_tokens(agent);
                                state.model_name = agent.resolved.model.clone();
                            }
                            state.status_message.clear();
                            // If user requested quit while streaming, exit now.
                            if state.pending_quit {
                                return Ok(());
                            }
                        }
                        Ok(Err(e)) => {
                            state.push_chat_line(ChatLine::Error(format!("agent error: {}", e)));
                            state.is_streaming = false;
                            // Even on agent error, respect pending quit.
                            if state.pending_quit {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            state
                                .push_chat_line(ChatLine::Error(format!("task join error: {}", e)));
                            state.is_streaming = false;
                            if state.pending_quit {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(_) => {
                    // Still running — not ready yet.
                }
            }
        }

        // Poll for events. The timeout ensures the spinner animates even when
        // no events arrive (e.g., waiting for first LLM token).
        let poll_timeout = if state.is_streaming {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(30)
        };
        tokio::select! {
            biased;

            // Timeout — re-render to animate the spinner.
            _ = tokio::time::sleep(poll_timeout) => {
                // Just loop to re-render with next spinner frame.
            }

            // Terminal events (keyboard, mouse, resize) from dedicated thread.
            ev = key_rx.recv() => {
                match ev {
                    Some(CrosstermEvent::Key(key)) => {
                        let action = handle_key(key, state, agent_slot, agent_handle, event_tx).await?;
                        if action == LoopAction::Quit {
                            break;
                        }
                    }
                    Some(CrosstermEvent::Mouse(mouse)) => {
                        handle_mouse(mouse, state);
                    }
                    Some(CrosstermEvent::Resize(_, _)) => {
                        // Re-render on next iteration.
                    }
                    _ => {}
                }
            }

            // Agent streaming events.
            ev = event_rx.recv() => {
                if let Some(agent_event) = ev {
                    handle_agent_event(agent_event, state);
                }
            }

            // Model info query result.
            info = info_rx.recv() => {
                if let Some(info) = info {
                    state.model_context = info;
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum LoopAction {
    Continue,
    Quit,
}

/// Handle a key press. Returns Quit if the app should exit.
async fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
    agent_handle: &mut Option<tokio::task::JoinHandle<Result<AgentSession>>>,
    event_tx: &mpsc::Sender<Event>,
) -> Result<LoopAction> {
    // With kitty keyboard enhancement, we get release events too — ignore them.
    if key.kind == KeyEventKind::Release {
        return Ok(LoopAction::Continue);
    }

    // Global keys.
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // If streaming, defer quit until the agent task finishes to avoid
            // losing the agent (it's owned by the background task). The event
            // loop will break after the join handle resolves.
            if state.is_streaming {
                state.pending_quit = true;
                state.status_message = "Finishing response, then quitting...".to_string();
                return Ok(LoopAction::Continue);
            }
            return Ok(LoopAction::Quit);
        }
        // Ctrl+Arrow — spatial pane navigation.
        // Ctrl+Left = move left, Ctrl+Right = move right,
        // Ctrl+Up = move up, Ctrl+Down = move down.
        KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_left();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_right();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_up();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_down();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Tab => {
            state.focus = state.focus.next();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::BackTab => {
            state.focus = state.focus.prev();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        // Ctrl+H/J/K/L — spatial pane navigation (vim-style).
        // H = left, J = down, K = up, L = right.
        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_left();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_down();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_up();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = state.focus.move_right();
            state.reset_list_cursor();
            return Ok(LoopAction::Continue);
        }
        _ => {}
    }

    // Pane-specific keys.
    match state.focus {
        Focus::Input => handle_input_key(key, state, agent_slot, agent_handle, event_tx).await,
        Focus::Chat => Ok(handle_chat_key(key, state)),
        Focus::Models => Ok(handle_list_key(key, state, agent_slot).await),
        Focus::Sessions => Ok(handle_list_key(key, state, agent_slot).await),
        Focus::Skills => Ok(handle_list_key(key, state, agent_slot).await),
    }
}

/// Handle a mouse event — click to focus a pane, and in sidebar lists,
/// set the cursor to the clicked row.
fn handle_mouse(mouse: MouseEvent, state: &mut TuiState) {
    match mouse.kind {
        MouseEventKind::Down(_) => {
            // Left-click (and middle/right-click) to focus a pane.
            if let Some(target_focus) = state.pane_areas.hit_test(mouse.column, mouse.row) {
                state.focus = target_focus;
                // For sidebar list panes, also set the cursor to the clicked item.
                if matches!(
                    target_focus,
                    Focus::Models | Focus::Sessions | Focus::Skills
                )
                    && let Some(index) = state
                        .pane_areas
                        .list_index_for_click(target_focus, mouse.row)
                    {
                        let len = match target_focus {
                            Focus::Models => state.models.len(),
                            Focus::Sessions => state.sessions.len(),
                            Focus::Skills => state.skills.len(),
                            _ => 0,
                        };
                        if index < len {
                            state.list_cursor = index;
                        }
                    }
            }
        }
        MouseEventKind::ScrollUp => {
            // Mouse wheel up — scroll within the focused pane.
            match state.focus {
                Focus::Chat => state.scroll_chat_up(3),
                Focus::Models | Focus::Sessions | Focus::Skills => state.list_up(),
                _ => {}
            }
        }
        MouseEventKind::ScrollDown => {
            // Mouse wheel down — scroll within the focused pane.
            match state.focus {
                Focus::Chat => state.scroll_chat_down(3),
                Focus::Models | Focus::Sessions | Focus::Skills => {
                    state.list_down(state.current_list_len());
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Handle keys in the input pane.
async fn handle_input_key(
    key: KeyEvent,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
    agent_handle: &mut Option<tokio::task::JoinHandle<Result<AgentSession>>>,
    event_tx: &mpsc::Sender<Event>,
) -> Result<LoopAction> {
    match key.code {
        // Enter with Shift or Alt = insert newline (multi-line input).
        // Plain Enter = send message.
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                state.input_buffer.insert(state.input_cursor, '\n');
                state.input_cursor += 1;
                return Ok(LoopAction::Continue);
            }
            let text = std::mem::take(&mut state.input_buffer);
            state.input_cursor = 0;
            if text.trim().is_empty() {
                return Ok(LoopAction::Continue);
            }
            state.input_history.push(text.clone());

            // Check for slash commands.
            if text.starts_with('/') {
                return handle_slash_command(&text, state, agent_slot).await;
            }

            // Don't send if already streaming or agent is unavailable.
            if state.is_streaming || agent_slot.is_none() {
                state.status_message = "Wait for current response to finish.".to_string();
                // Restore the text since we couldn't send it.
                state.input_buffer = text;
                state.input_cursor = state.input_buffer.len();
                return Ok(LoopAction::Continue);
            }

            // Take ownership of the agent.
            let agent = agent_slot.take().expect("agent must be Some when idle");

            state.push_chat_line(ChatLine::User(text.clone()));
            state.is_streaming = true;
            state.has_first_content = false;
            state.status_message.clear();

            match agent.chat_events_spawned(&text) {
                Ok((handle, mut rx)) => {
                    *agent_handle = Some(handle);
                    // Forward events from the agent's channel to our event channel.
                    let tx = event_tx.clone();
                    tokio::spawn(async move {
                        while let Some(ev) = rx.recv().await {
                            if tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    });
                }
                Err(e) => {
                    // chat_events_spawned failed — agent wasn't consumed.
                    // But chat_events_spawned takes self by value, so we need to
                    // reconstruct. Since the error happens before spawning, the
                    // agent should still be available... but it's moved.
                    // Actually, since chat_events_spawned takes self, the agent
                    // is moved into the call. On Err, the agent is dropped.
                    // We can't recover it — mark as error and let user restart.
                    state.push_chat_line(ChatLine::Error(format!("{}", e)));
                    state.is_streaming = false;
                    // agent was consumed by chat_events_spawned (it takes self).
                    // We can't recover it. This is an edge case — session append failed.
                }
            }

            Ok(LoopAction::Continue)
        }
        KeyCode::Char(c) => {
            state.input_buffer.insert(state.input_cursor, c);
            state.input_cursor += 1;
            Ok(LoopAction::Continue)
        }
        KeyCode::Backspace => {
            if state.input_cursor > 0 {
                state.input_cursor -= 1;
                state.input_buffer.remove(state.input_cursor);
            }
            Ok(LoopAction::Continue)
        }
        KeyCode::Delete => {
            if state.input_cursor < state.input_buffer.len() {
                state.input_buffer.remove(state.input_cursor);
            }
            Ok(LoopAction::Continue)
        }
        KeyCode::Left => {
            if state.input_cursor > 0 {
                state.input_cursor -= 1;
            }
            Ok(LoopAction::Continue)
        }
        KeyCode::Right => {
            if state.input_cursor < state.input_buffer.len() {
                state.input_cursor += 1;
            }
            Ok(LoopAction::Continue)
        }
        KeyCode::Home => {
            state.input_cursor = 0;
            Ok(LoopAction::Continue)
        }
        KeyCode::End => {
            state.input_cursor = state.input_buffer.len();
            Ok(LoopAction::Continue)
        }
        KeyCode::Up => {
            // In multi-line input, Up moves cursor to previous line.
            if state.input_buffer.contains('\n') {
                let (row, col) = render::cursor_row_col(&state.input_buffer, state.input_cursor);
                if row > 0 {
                    // Move to end of previous line (or same col if it fits).
                    let new_cursor =
                        move_cursor_to_line(&state.input_buffer, state.input_cursor, row - 1, col);
                    state.input_cursor = new_cursor;
                }
            } else if !state.input_history.is_empty() {
                state.history_index = match state.history_index {
                    None => Some(state.input_history.len() - 1),
                    Some(i) if i > 0 => Some(i - 1),
                    Some(i) => Some(i),
                };
                if let Some(i) = state.history_index {
                    state.input_buffer = state.input_history[i].clone();
                    state.input_cursor = state.input_buffer.len();
                }
            }
            Ok(LoopAction::Continue)
        }
        KeyCode::Down => {
            // In multi-line input, Down moves cursor to next line.
            if state.input_buffer.contains('\n') {
                let (row, col) = render::cursor_row_col(&state.input_buffer, state.input_cursor);
                let total_lines = state.input_buffer.lines().count();
                if row < total_lines - 1 || state.input_buffer.ends_with('\n') {
                    let new_cursor =
                        move_cursor_to_line(&state.input_buffer, state.input_cursor, row + 1, col);
                    state.input_cursor = new_cursor;
                }
            } else if let Some(i) = state.history_index {
                if i + 1 < state.input_history.len() {
                    state.history_index = Some(i + 1);
                    state.input_buffer = state.input_history[i + 1].clone();
                } else {
                    state.history_index = None;
                    state.input_buffer.clear();
                }
                state.input_cursor = state.input_buffer.len();
            }
            Ok(LoopAction::Continue)
        }
        _ => Ok(LoopAction::Continue),
    }
}

/// Handle keys in the chat pane (scrolling).
fn handle_chat_key(key: KeyEvent, state: &mut TuiState) -> LoopAction {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll_chat_up(1);
            LoopAction::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.scroll_chat_down(1);
            LoopAction::Continue
        }
        KeyCode::PageUp => {
            state.scroll_chat_up(10);
            LoopAction::Continue
        }
        KeyCode::PageDown => {
            state.scroll_chat_down(10);
            LoopAction::Continue
        }
        KeyCode::Home => {
            state.auto_scroll = false;
            state.chat_scroll = 0;
            LoopAction::Continue
        }
        KeyCode::End => {
            state.scroll_to_bottom();
            LoopAction::Continue
        }
        _ => LoopAction::Continue,
    }
}

/// Handle keys in sidebar list panes.
async fn handle_list_key(
    key: KeyEvent,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
) -> LoopAction {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.list_up();
            LoopAction::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.list_down(state.current_list_len());
            LoopAction::Continue
        }
        KeyCode::Enter => {
            match state.focus {
                Focus::Models => {
                    if let Some(entry) = state.selected_model() {
                        let name = entry.name.clone();
                        if let Some(ref mut agent) = *agent_slot {
                            match agent.switch_model(&name) {
                                Ok(msg) => {
                                    state.model_name = agent.resolved.model.clone();
                                    state.status_message = msg.clone();
                                    state.push_chat_line(ChatLine::System(msg));
                                    state.update_tokens(agent);
                                    state.models = state::build_model_list(agent);
                                }
                                Err(e) => {
                                    state.status_message = format!("Error: {}", e);
                                }
                            }
                        }
                    }
                    LoopAction::Continue
                }
                Focus::Sessions => {
                    // TODO: session resume — needs loading logic.
                    if let Some(entry) = state.selected_session() {
                        let display =
                            if let Some(uuid_part) = entry.id.strip_prefix("enchanter_tui_") {
                                format!("tui:{}", &uuid_part[..8.min(uuid_part.len())])
                            } else {
                                entry.id[..8.min(entry.id.len())].to_string()
                            };
                        state.status_message =
                            format!("Session {} selected (resume not yet implemented)", display);
                    }
                    LoopAction::Continue
                }
                Focus::Skills => {
                    if let Some(skill) = state.selected_skill() {
                        state.status_message =
                            format!("Skill: {} — {}", skill.name, skill.description);
                    }
                    LoopAction::Continue
                }
                _ => LoopAction::Continue,
            }
        }
        _ => LoopAction::Continue,
    }
}

/// Move cursor from current position to a target line at a given column.
/// Returns the new byte offset.
fn move_cursor_to_line(
    buffer: &str,
    _current_cursor: usize,
    target_row: usize,
    target_col: usize,
) -> usize {
    let mut row = 0;
    let mut result = 0;

    for (i, ch) in buffer.char_indices() {
        if row == target_row {
            // We're on the target line — advance to target_col chars.
            let mut col = 0;
            let mut byte_pos = i;
            for (j, c) in buffer[i..].char_indices() {
                if c == '\n' || col >= target_col {
                    break;
                }
                col += 1;
                byte_pos = i + j + c.len_utf8();
            }
            result = byte_pos;
            if result < i {
                result = i;
            }
            return result;
        }
        if ch == '\n' {
            row += 1;
        }
    }

    // If target_row is beyond the last line, go to end of buffer.
    if buffer.ends_with('\n') && row == target_row {
        return buffer.len();
    }
    buffer.len()
}

/// Handle slash commands (same as REPL).
async fn handle_slash_command(
    text: &str,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
) -> Result<LoopAction> {
    let parts: Vec<&str> = text.trim().splitn(2, ' ').collect();
    let cmd = parts[0].trim_start_matches('/');

    match cmd {
        "quit" | "q" | "exit" => {
            if state.is_streaming {
                state.pending_quit = true;
                state.status_message = "Finishing response, then quitting...".to_string();
                Ok(LoopAction::Continue)
            } else {
                Ok(LoopAction::Quit)
            }
        }
        "sessions" => {
            state.sessions = state::build_session_list();
            state.status_message = format!("Loaded {} sessions", state.sessions.len());
            Ok(LoopAction::Continue)
        }
        "model" | "m" => {
            if let Some(name) = parts.get(1) {
                if let Some(ref mut agent) = *agent_slot {
                    match agent.switch_model(name.trim()) {
                        Ok(msg) => {
                            state.model_name = agent.resolved.model.clone();
                            state.status_message = msg.clone();
                            state.push_chat_line(ChatLine::System(msg));
                            state.update_tokens(agent);
                            state.models = state::build_model_list(agent);
                        }
                        Err(e) => {
                            state.status_message = format!("Error: {}", e);
                        }
                    }
                }
            } else {
                state.status_message = "Usage: /model <name>".to_string();
            }
            Ok(LoopAction::Continue)
        }
        "context" | "ctx" => {
            if let Some(ref agent) = *agent_slot {
                state.update_tokens(agent);
            }
            let budget = state.context_budget();
            let msg = match budget {
                Some(b) => format!(
                    "Context: {} / {} tokens ({}%)",
                    crate::status_bar::fmt_tokens(state.tokens),
                    crate::status_bar::fmt_tokens(b),
                    (state.tokens as f64 / b as f64 * 100.0) as u8
                ),
                None => format!(
                    "Context: {} tokens",
                    crate::status_bar::fmt_tokens(state.tokens)
                ),
            };
            state.status_message = msg.clone();
            state.push_chat_line(ChatLine::System(msg));
            Ok(LoopAction::Continue)
        }
        "clear" | "cls" => {
            state.chat_lines.clear();
            Ok(LoopAction::Continue)
        }
        "help" | "h" | "?" => {
            state.push_chat_line(ChatLine::System(
                "Commands: /quit /sessions /model <name> /context /clear /help".to_string(),
            ));
            Ok(LoopAction::Continue)
        }
        _ => {
            state.push_chat_line(ChatLine::Error(format!("Unknown command: {}", cmd)));
            Ok(LoopAction::Continue)
        }
    }
}

/// Handle an agent streaming event — update chat view.
fn handle_agent_event(ev: Event, state: &mut TuiState) {
    match ev {
        Event::Content { text } => {
            state.has_first_content = true;
            state.append_to_last_assistant(&text);
        }
        Event::ToolCall { name, .. } => {
            state.has_first_content = true;
            state.push_chat_line(ChatLine::ToolCall(name));
        }
        Event::ToolResult { content, .. } => {
            // Truncate for display — full content is in the session log.
            let truncated = if content.lines().count() > 10 {
                let lines: Vec<&str> = content.lines().take(10).collect();
                format!(
                    "{}\n... ({} more lines)",
                    lines.join("\n"),
                    content.lines().count() - 10
                )
            } else {
                content
            };
            state.push_chat_line(ChatLine::ToolResult(String::new(), truncated));
        }
        Event::Compacted {
            removed_messages,
            budget_tokens,
        } => {
            state.push_chat_line(ChatLine::Compacted(format!(
                "── compacted {} messages (~{} tokens) ──",
                removed_messages,
                crate::status_bar::fmt_tokens(budget_tokens),
            )));
        }
        Event::Done => {
            // Streaming is done — the join handle will return the agent.
            // The event loop polls the handle and recovers the agent.
        }
        Event::Error { message } => {
            state.push_chat_line(ChatLine::Error(message));
        }
        _ => {}
    }
}
