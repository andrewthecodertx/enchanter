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

pub mod state;
pub mod render;
pub mod model_info;

use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use crossterm::cursor::{Show, Hide};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::agent::AgentSession;
use crate::protocol::Event;
use crate::tui::state::{ChatLine, Focus, TuiState};

/// Run the full-screen TUI. Returns the agent session so the caller can
/// do exit summaries, MCP shutdown, etc. — same contract as run_repl.
pub async fn run_tui(agent: AgentSession) -> Result<AgentSession> {
    // Setup terminal.
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new(&agent);

    // Query model context info in the background (best-effort).
    let base_url = agent.resolved.base_url.clone();
    let api_key = agent.resolved.api_key.clone();
    let (info_tx, mut info_rx) = mpsc::channel::<std::collections::HashMap<String, state::ModelContextInfo>>(1);
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
    ).await;

    // Cleanup terminal.
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
    terminal.flush()?;

    result?;

    // Recover agent from Option (should always be Some at this point).
    let agent = agent_slot.unwrap_or_else(|| {
        panic!("agent was not recovered from spawned task on exit");
    });
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
) -> Result<()> {
    loop {
        // Render the current state.
        terminal.draw(|frame| render::render(frame, state))?;

        // Check if the agent task has finished — recover the agent.
        if state.is_streaming && agent_handle.is_some() {
            // Try to poll the join handle without blocking.
            match tokio::time::timeout(Duration::from_millis(0), agent_handle.as_mut().unwrap()).await {
                Ok(join_res) => {
                    *agent_handle = None;
                    state.is_streaming = false;
                    match join_res {
                        Ok(Ok(returned_agent)) => {
                            *agent_slot = Some(returned_agent);
                            if let Some(ref agent) = *agent_slot {
                                state.update_tokens(agent);
                                state.model_name = agent.resolved.model.clone();
                            }
                            state.status_message.clear();
                        }
                        Ok(Err(e)) => {
                            state.push_chat_line(ChatLine::Error(format!("agent error: {}", e)));
                            state.is_streaming = false;
                        }
                        Err(e) => {
                            state.push_chat_line(ChatLine::Error(format!("task join error: {}", e)));
                            state.is_streaming = false;
                        }
                    }
                }
                Err(_) => {
                    // Still running — not ready yet.
                }
            }
        }

        // Poll for events.
        tokio::select! {
            biased;

            // Terminal events (keyboard, resize).
            ev = poll_crossterm() => {
                match ev {
                    Some(Ok(CrosstermEvent::Key(key))) => {
                        let action = handle_key(key, state, agent_slot, agent_handle, event_tx).await?;
                        if action == LoopAction::Quit {
                            break;
                        }
                    }
                    Some(Ok(CrosstermEvent::Resize(_, _))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(anyhow::anyhow!("Terminal error: {}", e)),
                    None => {}
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

        // Small sleep to avoid busy-looping when nothing is happening.
        if !state.is_streaming {
            sleep(Duration::from_millis(16)).await;
        }
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum LoopAction {
    Continue,
    Quit,
}

/// Poll crossterm events with a timeout, returning an async-compatible future.
async fn poll_crossterm() -> Option<std::io::Result<CrosstermEvent>> {
    tokio::task::spawn_blocking(|| {
        if event::poll(Duration::from_millis(50)).ok()? {
            event::read().ok().map(Ok)
        } else {
            None
        }
    })
    .await
    .ok()
    .flatten()
}

/// Handle a key press. Returns Quit if the app should exit.
async fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
    agent_handle: &mut Option<tokio::task::JoinHandle<Result<AgentSession>>>,
    event_tx: &mpsc::Sender<Event>,
) -> Result<LoopAction> {
    // Global keys.
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Ok(LoopAction::Quit);
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

/// Handle keys in the input pane.
async fn handle_input_key(
    key: KeyEvent,
    state: &mut TuiState,
    agent_slot: &mut Option<AgentSession>,
    agent_handle: &mut Option<tokio::task::JoinHandle<Result<AgentSession>>>,
    event_tx: &mpsc::Sender<Event>,
) -> Result<LoopAction> {
    match key.code {
        KeyCode::Enter => {
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
            if !state.input_history.is_empty() {
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
            if let Some(i) = state.history_index {
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
            state.chat_scroll = 0;
            LoopAction::Continue
        }
        KeyCode::End => {
            state.chat_scroll = state.chat_lines.len();
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
                        state.status_message = format!("Session {} selected (resume not yet implemented)", &entry.id[..8.min(entry.id.len())]);
                    }
                    LoopAction::Continue
                }
                Focus::Skills => {
                    if let Some(skill) = state.selected_skill() {
                        state.status_message = format!("Skill: {} — {}", skill.name, skill.description);
                    }
                    LoopAction::Continue
                }
                _ => LoopAction::Continue,
            }
        }
        _ => LoopAction::Continue,
    }
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
        "quit" | "q" | "exit" => Ok(LoopAction::Quit),
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
                None => format!("Context: {} tokens", crate::status_bar::fmt_tokens(state.tokens)),
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
                "Commands: /quit /sessions /model <name> /context /clear /help".to_string()
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
            state.append_to_last_assistant(&text);
        }
        Event::ToolCall { name, .. } => {
            state.push_chat_line(ChatLine::ToolCall(name));
        }
        Event::ToolResult { content, .. } => {
            // Truncate for display — full content is in the session log.
            let truncated = if content.lines().count() > 10 {
                let lines: Vec<&str> = content.lines().take(10).collect();
                format!("{}\n... ({} more lines)", lines.join("\n"), content.lines().count() - 10)
            } else {
                content
            };
            state.push_chat_line(ChatLine::ToolResult(String::new(), truncated));
        }
        Event::Compacted { removed_messages, budget_tokens } => {
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