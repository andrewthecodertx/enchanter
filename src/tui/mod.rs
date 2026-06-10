//! TUI mode — rich terminal interface for Enchanter.
//!
//! Activated via `enchanter tui` subcommand. Provides a multi-pane, keyboard-driven
//! interface similar to lazygit, with panes for skills, memory, chat, and input.
//!
//! The agent is stored in `Option<AgentSession>` so it can be moved into a spawned
//! tokio task for real-time streaming. When idle, the agent is `Some`; during
//! streaming, it's `None` and the JoinHandle tracks the background task.

pub mod app;
pub mod commands;
pub mod input;
pub mod render;

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event as CrosstermEvent},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::Terminal;
use ratatui_crossterm::CrosstermBackend;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::AgentSession;
use crate::protocol::Event;

use self::app::{App, ChatLine};
use self::commands::CommandResult;
use self::input::HandleResult;

/// Run the TUI application.
pub async fn run(agent: AgentSession) -> Result<()> {
    // Terminal setup
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Set up panic hook to restore terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let result = run_app(&mut terminal, agent).await;

    // Terminal teardown
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent: AgentSession,
) -> Result<()> {
    let mut app = App::new(agent);

    // Welcome message
    app.chat_lines.push(ChatLine::System(format!(
        "Enchanter TUI — model={} | /help for commands",
        app.info.model
    )));
    app.chat_lines.push(ChatLine::System(
        "Keys: Tab=cycle | 1-4=jump | Ctrl+Q=quit | Ctrl+C=cancel | Ctrl+M=multiline | End=scroll bottom".into(),
    ));

    // Start MCP servers
    app.agent.as_mut().expect("agent present at init").start_mcp().await;

    // Event receiver + join handle during streaming
    let mut event_rx: Option<UnboundedReceiver<Event>> = None;
    let mut running = true;

    while running {
        // Draw current state
        terminal.draw(|f| render::draw(f, &app))?;

        if let Some(rx) = &mut event_rx {
            // ── Streaming mode: drain events from the channel ──
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        match event {
                            Event::Done => {
                                app.finalize_stream();
                                app.streaming = false;
                                app.turn += 1;
                                app.chat_auto_scroll = true;
                                event_rx = None;
                                break;
                            }
                            Event::Error { message } => {
                                app.finalize_stream();
                                app.chat_lines.push(ChatLine::Error(message));
                                app.streaming = false;
                                app.chat_auto_scroll = true;
                                event_rx = None;
                                break;
                            }
                            _ => {
                                app.handle_event(event);
                            }
                        }
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        // No more events ready — break out of drain loop
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        // Channel closed — streaming ended without Done
                        app.finalize_stream();
                        app.streaming = false;
                        app.chat_auto_scroll = true;
                        event_rx = None;
                        break;
                    }
                }
            }

            // If streaming finished, recover the agent from the spawned task
            if event_rx.is_none() {
                if let Some(handle) = app.agent_handle.take() {
                    match handle.await {
                        Ok(Ok(returned_agent)) => {
                            app.return_agent(returned_agent);

                            // Run memory management after recovering agent
                            {
                                let mem_config = app.get_agent().config.memory_config().clone();
                                let result = {
                                    let agent = app.get_agent_mut();
                                    let client = &agent.client;
                                    agent.memory.manage(client, &mem_config).await
                                };
                                if let Err(e) = result {
                                    app.chat_lines.push(ChatLine::System(format!("Memory management: {}", e)));
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            app.chat_lines.push(ChatLine::Error(format!("Agent error: {}", e)));
                        }
                        Err(e) => {
                            app.chat_lines.push(ChatLine::Error(format!("Task join error: {}", e)));
                        }
                    }
                }
            }

            // If we're still streaming, check for terminal key events
            if event_rx.is_some() {
                if event::poll(Duration::from_millis(0))? {
                    let key_event = event::read()?;
                    match handle_streaming_key(&key_event) {
                        StreamingAction::Quit => {
                            running = false;
                        }
                        StreamingAction::Cancel => {
                            app.finalize_stream();
                            app.chat_lines.push(ChatLine::System("Cancelled.".into()));
                            app.streaming = false;
                            app.chat_auto_scroll = true;
                            event_rx = None;
                            // Try to recover agent even on cancel
                            if let Some(handle) = app.agent_handle.take() {
                                match handle.await {
                                    Ok(Ok(returned_agent)) => {
                                        app.return_agent(returned_agent);
                                    }
                                    _ => {
                                        // Agent task failed or was cancelled
                                    }
                                }
                            }
                        }
                        StreamingAction::CycleFocus => {
                            app.focus = app.focus.next();
                        }
                        StreamingAction::CycleFocusBack => {
                            app.focus = app.focus.prev();
                        }
                        StreamingAction::Nothing => {}
                    }
                }
                // Brief sleep to avoid spinning
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        } else {
            // ── Idle mode: wait for user input ──
            if event::poll(Duration::from_millis(100))? {
                let event = event::read()?;
                match input::handle_key(&mut app, event) {
                    HandleResult::Continue => {}
                    HandleResult::Quit => {
                        running = false;
                    }
                    HandleResult::SendMessage(msg) => {
                        let maybe_rx = handle_user_message(&mut app, msg).await;
                        if let Some(rx) = maybe_rx {
                            event_rx = Some(rx);
                        }
                    }
                }
            }
        }
    }

    // Shutdown: stop MCP servers
    if let Some(agent) = app.agent.as_mut() {
        agent.shutdown_mcp().await;
    }

    // Session summary on exit (like the REPL does)
    if let Some(agent) = app.agent.as_ref() {
        if agent.config.summarize_on_exit() && crate::summary::should_summarize(&agent.messages) {
            eprintln!("  Generating session summary...");
            let client = &agent.client;
            let messages = &agent.messages;
            match tokio::time::timeout(
                Duration::from_secs(10),
                crate::summary::generate_session_summary(client, messages),
            )
            .await
            {
                Ok(Ok(summary_text)) if !summary_text.is_empty() => {
                    if let Err(e) = app.get_agent_mut().memory.add_memory(format!("session_summary\n{}", summary_text)) {
                        eprintln!("  Failed to save session summary: {}", e);
                    } else {
                        eprintln!("  Session summary saved to memory.");
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let fallback = crate::summary::fallback_summary(messages);
                    let _ = app.get_agent_mut().memory.add_memory(format!("session_summary\n{}", fallback));
                    eprintln!("  Summary generation failed: {}", e);
                }
                Err(_) => {
                    let fallback = crate::summary::fallback_summary(messages);
                    let _ = app.get_agent_mut().memory.add_memory(format!("session_summary\n{}", fallback));
                    eprintln!("  Summary timed out, using fallback.");
                }
            }
        }
    }

    Ok(())
}

/// Handle a user message (chat or slash command). Returns Some(event_rx) if streaming started.
async fn handle_user_message(app: &mut App, msg: String) -> Option<UnboundedReceiver<Event>> {
    // Handle slash commands that don't require spawning
    if msg.starts_with('/') {
        match commands::handle_command(app, &msg) {
            CommandResult::Done => None,
            CommandResult::Quit => None,
            CommandResult::SendMessage(msg) => {
                if msg == "/retry" {
                    handle_retry(app).await
                } else {
                    handle_chat(app, msg).await
                }
            }
        }
    } else {
        handle_chat(app, msg).await
    }
}

/// Start a chat with real-time streaming. Spawns the agent loop on a background task.
async fn handle_chat(app: &mut App, msg: String) -> Option<UnboundedReceiver<Event>> {
    app.chat_lines.push(ChatLine::User(msg.clone()));
    app.chat_auto_scroll = true;
    app.streaming = true;
    app.current_stream_text.clear();

    // Take the agent out of App to move it into the spawned task
    let the_agent = app.take_agent();

    let result = the_agent.chat_events_spawned(&msg);
    match result {
        Ok((handle, rx)) => {
            app.agent_handle = Some(handle);
            Some(rx)
        }
        Err(e) => {
            // chat_events_spawned failed — the agent was consumed by the method
            // on error. We can't recover it. Show the error and mark streaming as done.
            app.chat_lines.push(ChatLine::Error(format!("Error: {}", e)));
            app.streaming = false;
            None
        }
    }
}

/// Start a retry with real-time streaming. Same pattern as handle_chat.
async fn handle_retry(app: &mut App) -> Option<UnboundedReceiver<Event>> {
    app.chat_lines.push(ChatLine::System("Retrying...".into()));
    app.chat_auto_scroll = true;
    app.streaming = true;
    app.current_stream_text.clear();

    let the_agent = app.take_agent();

    let result = the_agent.retry_events_spawned();
    match result {
        Ok((handle, rx)) => {
            app.agent_handle = Some(handle);
            Some(rx)
        }
        Err(e) => {
            // retry_events_spawned failed — agent was consumed. Can't recover.
            app.chat_lines.push(ChatLine::Error(format!("Retry error: {}", e)));
            app.streaming = false;
            None
        }
    }
}

/// Action to take during streaming based on a key press.
enum StreamingAction {
    Nothing,
    Quit,
    Cancel,
    CycleFocus,
    CycleFocusBack,
}

/// Handle key events during streaming (limited set).
fn handle_streaming_key(event: &CrosstermEvent) -> StreamingAction {
    let CrosstermEvent::Key(key) = event else {
        return StreamingAction::Nothing;
    };
    match (key.modifiers, key.code) {
        (crossterm::event::KeyModifiers::CONTROL, crossterm::event::KeyCode::Char('c')) => {
            StreamingAction::Cancel
        }
        (crossterm::event::KeyModifiers::CONTROL, crossterm::event::KeyCode::Char('q')) => {
            StreamingAction::Quit
        }
        (crossterm::event::KeyModifiers::NONE, crossterm::event::KeyCode::Tab) => {
            StreamingAction::CycleFocus
        }
        (crossterm::event::KeyModifiers::SHIFT, crossterm::event::KeyCode::BackTab) => {
            StreamingAction::CycleFocusBack
        }
        _ => StreamingAction::Nothing,
    }
}