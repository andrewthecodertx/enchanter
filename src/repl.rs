//! Line-oriented REPL — simple stdin/stdout, no alternate screen buffer.
//!
//! Prints output line-by-line, reads input via stdin. No raw mode, no
//! alternate screen, no crossterm event polling. Streaming events from
//! the agent are printed as they arrive.
//!
//! A status bar line is printed above each prompt using reverse-video
//! styling. It shows context token usage, model, and session ID.

use std::io::{self, Write};

use anyhow::Result;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::AgentSession;
use crate::protocol::Event;
use crate::status_bar;

/// Action from parsing user input.
enum Action {
    Send(String),
    Retry,
    Quit,
}

/// Run the interactive REPL. Returns the agent session so the caller can
/// do exit summaries, MCP shutdown, etc.
pub async fn run_repl(agent: AgentSession) -> Result<AgentSession> {
    let info = agent.info();
    let short_url = info
        .base_url
        .trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq");

    let session_id = info.session_id.clone();

    println!();
    println!(
        "⟡ Enchanter v{}  session={}",
        env!("CARGO_PKG_VERSION"),
        &session_id[..8.min(session_id.len())]
    );
    println!(
        "  model={} | provider={} | tools={} | /help for commands",
        info.model, short_url, info.tool_count
    );
    println!();

    // Agent is stored in Option — None while a spawned task has it
    let mut agent: Option<AgentSession> = Some(agent);

    loop {
        // Print status bar line above the prompt
        draw_status_bar(agent.as_ref().unwrap());

        // Print prompt
        print!("⟩ ");
        io::stdout().flush()?;

        // Read input
        let mut input = String::new();
        if io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
            // EOF (Ctrl+D)
            println!();
            break;
        }
        let input = input.trim();

        if input.is_empty() {
            // Empty line — loop will redraw bar above next prompt
            continue;
        }

        // Parse slash commands (only ones that don't need ownership)
        let action = if input.starts_with('/') {
            match input {
                "/exit" | "/quit" | "/bye" => Action::Quit,
                "/retry" => Action::Retry,
                _ => {
                    // Slash commands that operate on &mut AgentSession
                    let a = agent.as_mut().expect("agent must be Some when idle");
                    handle_slash_command(input, a);
                    continue;
                }
            }
        } else {
            Action::Send(input.to_string())
        };

        match action {
            Action::Quit => {
                break;
            }
            Action::Retry => {
                let a = agent.take().expect("agent must be Some when idle");
                match a.retry_events_spawned() {
                    Ok((handle, mut rx)) => {
                        stream_events(&mut rx).await;
                        match handle.await {
                            Ok(Ok(returned_agent)) => {
                                agent = Some(returned_agent);
                            }
                            Ok(Err(e)) => {
                                eprintln!("✗ agent error: {}", e);
                            }
                            Err(e) => {
                                eprintln!("✗ task join error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("✗ {}", e);
                    }
                }
            }
            Action::Send(msg) => {
                let a = agent.take().expect("agent must be Some when idle");
                match a.chat_events_spawned(&msg) {
                    Ok((handle, mut rx)) => {
                        stream_events(&mut rx).await;
                        match handle.await {
                            Ok(Ok(returned_agent)) => {
                                agent = Some(returned_agent);
                            }
                            Ok(Err(e)) => {
                                eprintln!("✗ agent error: {}", e);
                            }
                            Err(e) => {
                                eprintln!("✗ task join error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("✗ {}", e);
                    }
                }
            }
        }
    }

    // Recover agent from Option
    let agent = agent.unwrap_or_else(|| {
        panic!("agent was not recovered from spawned task on exit");
    });
    Ok(agent)
}

/// Print the status bar line above the prompt.
fn draw_status_bar(agent: &AgentSession) {
    let tokens = agent.estimated_context_tokens();
    let model = &agent.resolved.model;
    let budget = status_bar::model_context_size(model);
    let session_id = agent.session.id().to_string();
    status_bar::print_bar(model, tokens, budget, &session_id);
}

/// Drain and print streaming events from the agent.
async fn stream_events(rx: &mut UnboundedReceiver<Event>) {
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx.recv()).await {
            Ok(Some(event)) => match event {
                Event::Content { text } => {
                    print!("{}", text);
                    io::stdout().flush().ok();
                }
                Event::ToolCall {
                    name,
                    id: _,
                    arguments: _,
                } => {
                    println!();
                    println!("  ⟩ {}", name);
                }
                Event::ToolResult { id: _, content } => {
                    for line in content.lines().take(5) {
                        println!("│ {}", line);
                    }
                    let total_lines = content.lines().count();
                    if total_lines > 5 {
                        println!("│ ... ({} more lines)", total_lines - 5);
                    }
                }
                Event::Compacted {
                    removed_messages,
                    budget_tokens,
                } => {
                    println!(
                        "── compacted {} messages (~{} tokens) ──",
                        removed_messages, budget_tokens
                    );
                }
                Event::Done => {
                    println!();
                    println!();
                    return;
                }
                Event::Error { message } => {
                    eprintln!("✗ {}", message);
                    println!();
                    return;
                }
                _ => {}
            },
            Ok(None) => {
                // Channel closed
                println!();
                return;
            }
            Err(_) => {
                // Timeout — extremely unlikely (5 min)
                println!();
                println!("── response timeout ──");
                return;
            }
        }
    }
}

// ── Slash commands ──

fn handle_slash_command(cmd: &str, agent: &mut AgentSession) {
    match cmd {
        "/help" => {
            println!("── commands ──");
            println!("  /help        Show this help");
            println!("  /exit        Quit enchanter");
            println!("  /clear       Clear conversation");
            println!("  /model NAME  Switch provider/model");
            println!("  /retry       Retry last exchange");
            println!("  /undo        Undo last exchange");
            println!("  /ctx         Show context token usage");
            println!("  /config      Show current config");
            println!("  /memory      Show memory");
            println!("  /soul        Show soul file");
            println!("  /skills      Show skills");
            println!("  /tools       Show available tools");
            println!("  /sessions    List sessions");
            println!("  /log         Show recent activity");
            println!();
            println!("  Tip: use --resume <session_id> at startup to continue a previous session");
        }
        "/clear" => {
            if let Err(e) = agent.clear() {
                eprintln!("✗ {}", e);
            } else {
                println!("── conversation cleared ──");
            }
        }
        "/config" => {
            let info = agent.info();
            let tokens = agent.estimated_context_tokens();
            println!("  model:    {}", info.model);
            println!("  base_url: {}", info.base_url);
            println!(
                "  api_key:  {}",
                if info.api_key_set { "set" } else { "none" }
            );
            println!(
                "  max:      {} (soft: {})",
                info.max_turns
                    .map_or("unlimited".to_string(), |n| n.to_string()),
                info.soft_limit.map_or("n/a".to_string(), |n| n.to_string())
            );
            let budget = status_bar::model_context_size(&agent.resolved.model);
            if let Some(b) = budget {
                let pct = ((tokens as f64 / b as f64) * 100.0) as u8;
                println!(
                    "  context:  ~{} / {} ({}%)",
                    status_bar::fmt_tokens(tokens),
                    status_bar::fmt_tokens(b),
                    pct
                );
            } else {
                println!("  context:  ~{} tokens", status_bar::fmt_tokens(tokens));
            }
        }
        "/memory" => {
            print!("{}", agent.memory.format_for_prompt());
        }
        "/soul" => {
            println!("{}", agent.soul.content);
        }
        "/skills" => {
            println!("{}", agent.skills.format_index_for_prompt());
        }
        "/tools" => {
            let tools = agent.tools_payload();
            let count = tools
                .as_ref()
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("── {} tools ──", count);
            if let Some(arr) = tools.as_ref().and_then(|v| v.as_array()) {
                for t in arr.iter().take(30) {
                    let name = t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("?");
                    println!("  {}", name);
                }
                if arr.len() > 30 {
                    println!("  ... ({} more)", arr.len() - 30);
                }
            }
        }
        "/ctx" | "/context" => {
            let tokens = agent.estimated_context_tokens();
            let budget = status_bar::model_context_size(&agent.resolved.model);
            if let Some(b) = budget {
                let pct = ((tokens as f64 / b as f64) * 100.0) as u8;
                println!(
                    "── context: ~{} / {} tokens ({}%) ──",
                    status_bar::fmt_tokens(tokens),
                    status_bar::fmt_tokens(b),
                    pct
                );
            } else {
                println!(
                    "── context: ~{} tokens (budget unknown for {}) ──",
                    status_bar::fmt_tokens(tokens),
                    agent.resolved.model
                );
            }
        }
        "/undo" => {
            if agent.undo() {
                println!("── undid last exchange ──");
            } else {
                eprintln!("✗ nothing to undo");
            }
        }
        "/log" => {
            let log_path = crate::home::enchanter_home().join("logs/activity.jsonl");
            if !log_path.exists() {
                println!("no activity log at {}", log_path.display());
            } else {
                match std::fs::read_to_string(&log_path) {
                    Ok(contents) => {
                        let lines: Vec<&str> = contents.lines().rev().take(15).collect();
                        let reversed: Vec<&str> = lines.into_iter().rev().collect();
                        println!("── recent activity ──");
                        for line in &reversed {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("?");
                                let evt = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                                println!("  {} {}", ts, evt);
                            }
                        }
                    }
                    Err(e) => eprintln!("✗ cannot read log: {}", e),
                }
            }
        }
        "/sessions" => match crate::session::Session::list_all() {
            Ok(sessions) => {
                if sessions.is_empty() {
                    println!("no sessions found");
                } else {
                    println!("── sessions ──");
                    for s in &sessions {
                        let short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                        println!("  {}  {} msgs", short, s.message_count);
                    }
                }
            }
            Err(e) => eprintln!("✗ {}", e),
        },
        _ => {
            if let Some(new_name) = cmd.strip_prefix("/model ") {
                let new_name = new_name.trim();
                if new_name.is_empty() {
                    println!("usage: /model <name>");
                } else {
                    match agent.switch_model(new_name) {
                        Ok(label) => println!("✓ switched to {}", label),
                        Err(e) => eprintln!("✗ {}", e),
                    }
                }
            } else {
                eprintln!("✗ unknown command: {}", cmd);
            }
        }
    }
}
