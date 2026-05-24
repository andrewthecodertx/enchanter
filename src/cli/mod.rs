//! CLI definition and REPL loop.
//!
//! The REPL interaction pattern (persistent loop with slash commands) borrows from
//! hermes-agent's conversation_loop (hermes-agent/agent/conversation_loop.py) and
//! Claude Code's REPL UX (claude-code/src/main.tsx). Slash commands /clear, /help,
//! /model, /retry, /undo follow the convention established by hermes-agent
//! (hermes-agent/cli.py slash command handling).
//!
//! The agent turn loop (call model → check for tool_calls → dispatch tools →
//! append results → repeat until text-only response or max_turns) follows the
//! standard agentic loop pattern used by hermes-agent
//! (hermes-agent/agent/conversation_loop.py), Claude Code
//! (claude-code/src/agent/agent.ts), and OpenCode
//! (opencode/packages/opencode/src/session/).
//!
//! Session summarization on exit (calling the LLM with a truncated conversation,
//! timeout with fallback) is adapted from hermes-agent's background_review
//! pattern (hermes-agent/agent/background_review.py).
//!
//! The /model provider-switching pattern (named provider presets with
//! inheritance from defaults) is informed by hermes-agent's config.yaml
//! provider resolution (hermes-agent/hermes_cli/config.py).

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde_json::Value;

use crate::api::{LlmClient, Message};
use crate::config::{Config, ResolvedModel};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::prompt;
use crate::session::Session;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use crate::summary;
use crate::tools;

/// Format bytes in human-readable form.
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{}MB", bytes / (1024 * 1024))
    }
}

#[derive(Parser, Debug)]
#[command(name = "enchanter", version, about = "A focused AI agent harness")]
pub struct Args {
    #[arg(short, long)]
    pub model: Option<String>,

    #[arg(short, long)]
    pub system: Option<String>,

    #[arg(short, long)]
    pub prompt: Option<String>,

    #[arg(long)]
    pub no_stream: bool,

    #[arg(long)]
    pub no_tools: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Soul,
    Memory,
    Skills,
    Config,
    Prompt,
    /// List or show session history
    Sessions {
        /// Show a specific session by ID
        id: Option<String>,
    },
}

pub async fn run(args: Args) -> Result<()> {
    if crate::home::init_home()? {
        print_init_guidance();
    }

    let config = Config::load()?;
    let soul = Soul::load_or_fallback()?;
    let mut memory = MemoryStore::load()?;
    let skills = SkillsIndex::discover()?;

    if let Some(cmd) = &args.command {
        return handle_command(cmd, &config, &soul, &memory, &skills);
    }

    // Resolve initial model: -m flag > config
    let resolved = if let Some(model_flag) = &args.model {
        // If -m matches a named provider, use that provider's settings
        config.resolve_provider(model_flag)
            .unwrap_or_else(|| {
                // Otherwise, use the flag as a bare model name with default provider settings
                let default = config.resolve_default();
                ResolvedModel {
                    model: model_flag.clone(),
                    base_url: default.base_url,
                    api_key: default.api_key,
                }
            })
    } else {
        config.resolve_default()
    };

    let client = LlmClient::new(&resolved.base_url, resolved.api_key.as_deref(), &resolved.model);

    // Cap + summarize memory if needed
    let mem_config = config.memory_config().clone();
    if let Err(e) = memory.manage(&client, &mem_config).await {
        eprintln!("{} memory management: {}", "Warning:".yellow(), e);
    }

    // Start MCP servers
    let mut mcp = McpManager::new();
    if !args.no_tools && !config.mcp.servers.is_empty() {
        mcp.start_all(&config.mcp.servers).await;
    }

    // Build combined tools payload
    let tools_payload = build_tools(&args, &mcp);

    if let Some(user_prompt) = &args.prompt {
        let mut session = Session::new(&resolved.model)?;
        let result = run_single(&args, &config, &soul, &mut memory, &skills, &client, &resolved, &mut mcp, &tools_payload, user_prompt, &mut session).await;
        mcp.shutdown_all().await;
        return result;
    }

    let result = run_repl(&args, &config, &soul, &mut memory, &skills, client, resolved, &mut mcp, &tools_payload).await;
    mcp.shutdown_all().await;
    result
}

fn handle_command(
    cmd: &Commands,
    config: &Config,
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
) -> Result<()> {
    match cmd {
        Commands::Soul => {
            println!("{}", "═══ SOUL.MD ═══".bright_cyan());
            println!("{}", soul.content);
            println!("{}", format!("Source: {}", soul.source.display()).dimmed());
        }
        Commands::Memory => {
            println!("{}", "═══ MEMORY ═══".bright_cyan());
            if memory.memory_entries.is_empty() && memory.user_entries.is_empty() {
                println!("(empty)");
            } else {
                if !memory.user_entries.is_empty() {
                    println!("{}", "── USER ──".bright_blue());
                    for entry in &memory.user_entries {
                        println!("  {}", entry.chars().take(100).collect::<String>());
                    }
                }
                if !memory.memory_entries.is_empty() {
                    println!("{}", "── NOTES ──".bright_blue());
                    for (i, entry) in memory.memory_entries.iter().enumerate() {
                        println!(
                            "  [{}] {}",
                            i + 1,
                            entry.chars().take(100).collect::<String>()
                        );
                    }
                }
            }
        }
        Commands::Skills => {
            println!("{}", "═══ SKILLS ═══".bright_cyan());
            if skills.skills.is_empty() {
                println!("(none found)");
            } else {
                for skill in &skills.skills {
                    let cat = skill
                        .category
                        .as_deref()
                        .map(|c| format!("[{}] ", c))
                        .unwrap_or_default();
                    println!(
                        "  {}{}{}",
                        cat.bright_green(),
                        skill.name.bold(),
                        if skill.description.is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", skill.description)
                        }
                        .dimmed()
                    );
                }
            }
        }
        Commands::Config => {
            println!("{}", "═══ CONFIG ═══".bright_cyan());
            println!("  Model:      {}", config.model_id());
            println!("  Base URL:   {}", config.base_url());
            println!("  Max turns:  {}", config.max_turns());
            println!(
                "  API key:    {}",
                if config.api_key().is_some() {
                    "configured ✓".green()
                } else {
                    "not set (not needed for local providers)".dimmed()
                }
            );
            if !config.providers.is_empty() {
                println!("  Providers:");
                let mut providers: Vec<_> = config.providers.keys().collect();
                providers.sort();
                for name in providers {
                    if let Some(p) = config.providers.get(name) {
                        let model = p.model.as_deref().unwrap_or("(default)");
                        let base = p.base_url.as_deref().unwrap_or("(default)");
                        let key_status = if p.api_key.is_some() { "✓" } else { "—" };
                        println!("    {} → {} | {} | key: {}", name.bright_green(), model, base, key_status);
                    }
                }
            }
        }
        Commands::Prompt => {
            let system_prompt = prompt::build_system_prompt(soul, memory, skills, config);
            println!("{}", "═══ SYSTEM PROMPT ═══".bright_cyan());
            println!("{}", system_prompt);
        }
        Commands::Sessions { id } => {
            match id {
                Some(session_id) => {
                    // Show a specific session
                    match Session::load(&session_id) {
                        Ok(entries) => {
                            let messages = Session::entries_to_messages(&entries);
                            if messages.is_empty() {
                                println!("{} Session {} is empty.", "Note:".dimmed(), session_id);
                            } else {
                                println!("{} Session {} ({} messages)", "═══ SESSION ═══".bright_cyan(), session_id, messages.len());
                                for msg in &messages {
                                    match msg.role.as_str() {
                                        "system" => continue,
                                        "user" => println!("{} {}", "⟩".bright_blue(), msg.content.as_deref().unwrap_or("")),
                                        "assistant" => {
                                            if let Some(content) = &msg.content {
                                                println!("{} {}", "⟨".bright_green(), content.chars().take(200).collect::<String>());
                                            } else if msg.tool_calls.is_some() {
                                                println!("{} [used tools]", "⟨".dimmed());
                                            }
                                        }
                                        "tool" => continue,
                                        _ => continue,
                                    }
                                }
                            }
                        }
                        Err(e) => println!("{} Session '{}' not found: {}", "Error:".red(), session_id, e),
                    }
                }
                None => {
                    // List all sessions
                    let sessions = Session::list_all()?;
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("{}", "═══ SESSIONS ═══".bright_cyan());
                        for s in &sessions {
                            let short_id = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                            println!(
                                "  {}{}  {} msgs, {}",
                                short_id.bright_white(),
                                "…".dimmed(),
                                format!("{}", s.message_count).bright_blue(),
                                format_bytes(s.file_size).dimmed()
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Build the combined tools JSON payload: built-in + MCP tools.
fn build_tools(args: &Args, mcp: &McpManager) -> Option<Value> {
    if args.no_tools {
        return None;
    }
    let mut all_tools = tools::tools_json();
    all_tools.extend(mcp.all_tools_json());
    Some(Value::Array(all_tools))
}

/// Dispatch a tool call — built-in tools first, then MCP.
async fn dispatch_tool(
    name: &str,
    args: &Value,
    memory: &mut MemoryStore,
    mcp: &mut McpManager,
) -> String {
    // Check if it's a built-in tool
    let built_in_names = ["exec_command", "read_file", "write_file", "edit_file",
                          "search_files", "list_directory", "memory"];
    if built_in_names.contains(&name) {
        return tools::dispatch(name, args, memory);
    }

    // Try MCP dispatch (tools are prefixed server_name:tool_name)
    if name.contains(':') {
        match mcp.dispatch(name, args).await {
            Some(Ok(result)) => result,
            Some(Err(e)) => format!("MCP error: {}", e),
            None => format!("Unknown tool: {}", name),
        }
    } else {
        format!("Unknown tool: {}", name)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_single(
    args: &Args,
    config: &Config,
    soul: &Soul,
    memory: &mut MemoryStore,
    skills: &SkillsIndex,
    client: &LlmClient,
    resolved: &ResolvedModel,
    mcp: &mut McpManager,
    tools_payload: &Option<Value>,
    user_prompt: &str,
    session: &mut Session,
) -> Result<()> {
    let system_content = match &args.system {
        Some(s) => s.clone(),
        None => prompt::build_system_prompt_with_model(soul, memory, skills, config, &resolved.model),
    };

    let system_msg = Message::system(&system_content);
    let user_msg = Message::user(user_prompt);

    // Persist initial messages
    session.append(&system_msg)?;
    session.append(&user_msg)?;

    let mut messages = vec![system_msg, user_msg];
    let max_turns = config.max_turns();

    for _ in 0..max_turns {
        let result = if args.no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            let assistant_msg = Message::assistant_with_tools(tool_calls.clone(), result.content);
            session.append(&assistant_msg)?;
            messages.push(assistant_msg);

            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = dispatch_tool(&tc.function.name, &tc_args, memory, mcp).await;
                let tool_msg = Message::tool_result(&tc.id, output);
                session.append(&tool_msg)?;
                messages.push(tool_msg);
            }
        } else {
            if let Some(content) = &result.content
                && args.no_stream
            {
                println!("{}", content);
            }
            if let Some(content) = &result.content {
                session.append(&Message::assistant(content))?;
            }
            return Ok(());
        }
    }

    anyhow::bail!("Max agent turns reached ({}). The agent exceeded its turn limit without producing a final response.", max_turns);
}

#[allow(clippy::too_many_arguments)]
async fn run_repl(
    args: &Args,
    config: &Config,
    soul: &Soul,
    memory: &mut MemoryStore,
    skills: &SkillsIndex,
    mut client: LlmClient,
    mut resolved: ResolvedModel,
    mcp: &mut McpManager,
    tools_payload: &Option<Value>,
) -> Result<()> {
    let system_prompt = if let Some(ref sys_override) = args.system {
        sys_override.clone()
    } else {
        prompt::build_system_prompt_with_model(soul, memory, skills, config, &resolved.model)
    };

    let mut messages = vec![Message::system(&system_prompt)];
    let max_turns = config.max_turns();

    // Create a session for conversation persistence
    let mut session = Session::new(&resolved.model)?;
    if let Err(e) = session.append(&messages[0]) {
        eprintln!("{} Could not create session file: {}", "Warning:".yellow(), e);
    }

    print_banner(&resolved.model, &resolved.base_url, soul, skills, tools_payload, mcp, session.id());

    let mut rl = rustyline::DefaultEditor::new()?;

    loop {
        let readline = rl.readline("⟩ ");
        match readline {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                if line.starts_with('/') {
                    match line.as_str() {
                        "/exit" | "/quit" => break,
                        "/clear" => {
                            let fresh_prompt = if let Some(ref sys_override) = args.system {
                                sys_override.clone()
                            } else {
                                prompt::build_system_prompt_with_model(soul, memory, skills, config, &resolved.model)
                            };
                            messages = vec![Message::system(&fresh_prompt)];
                            // Start a fresh session for the cleared conversation
                            match Session::new(&resolved.model) {
                                Ok(mut new_session) => {
                                    if let Err(e) = new_session.append(&messages[0]) {
                                        eprintln!("{} Could not create session: {}", "Warning:".yellow(), e);
                                    }
                                    session = new_session;
                                }
                                Err(e) => eprintln!("{} Could not create session: {}", "Warning:".yellow(), e),
                            }
                            println!("{}", "Conversation cleared.".dimmed());
                            continue;
                        }
                        "/help" => {
                            print_help(config);
                            continue;
                        }
                        "/soul" => {
                            println!("{}", soul.content);
                            continue;
                        }
                        "/memory" => {
                            println!("{}", memory.format_for_prompt());
                            continue;
                        }
                        "/skills" => {
                            println!("{}", skills.format_index_for_prompt());
                            continue;
                        }
                        "/config" => {
                            let key_status = if resolved.api_key.is_some() {
                                "configured ✓".green()
                            } else {
                                "not set (not needed for local providers)".dimmed()
                            };
                            println!("  Model:    {}", resolved.model.bright_white());
                            println!("  Base URL: {}", resolved.base_url.bright_white());
                            println!("  API key:  {}", key_status);
                            println!("  Max:      {}", max_turns);
                            continue;
                        }
                        "/prompt" => {
                            println!("{}", messages.first().map(|m| m.content.as_deref().unwrap_or("")).unwrap_or(""));
                            continue;
                        }
                        "/tools" => {
                            print_tools(mcp);
                            continue;
                        }
                        "/sessions" => {
                            match Session::list_all() {
                                Ok(sessions) => {
                                    if sessions.is_empty() {
                                        println!("No sessions found.");
                                    } else {
                                        println!("{}", "═══ SESSIONS ═══".bright_cyan());
                                        for s in &sessions {
                                            let short_id = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                                            println!(
                                                "  {}{}  {} msgs, {}",
                                                short_id.bright_white(),
                                                "…".dimmed(),
                                                format!("{}", s.message_count).bright_blue(),
                                                format_bytes(s.file_size).dimmed()
                                            );
                                        }
                                    }
                                }
                                Err(e) => eprintln!("{} Could not list sessions: {}", "Error:".red(), e),
                            }
                            continue;
                        }
                        _ => {
                            // Handle /model <name>, /retry, /undo
                            if let Some(new_name) = line.strip_prefix("/model ") {
                                let new_name = new_name.trim().to_string();
                                if new_name.is_empty() {
                                    println!("{} Usage: /model <name> (provider name from config or model ID)", "Error:".red());
                                } else {
                                    // Try named provider first, then fall back to bare model ID
                                    let new_resolved = config.resolve_provider(&new_name)
                                        .unwrap_or_else(|| {
                                            let default = config.resolve_default();
                                            ResolvedModel {
                                                model: new_name.clone(),
                                                base_url: default.base_url,
                                                api_key: default.api_key,
                                            }
                                        });

                                    let provider_label = if config.providers.contains_key(&new_name) {
                                        format!("{} (provider: {})", new_resolved.model, new_name)
                                    } else {
                                        new_resolved.model.clone()
                                    };

                                    client = LlmClient::new(
                                        &new_resolved.base_url,
                                        new_resolved.api_key.as_deref(),
                                        &new_resolved.model,
                                    );
                                    resolved = new_resolved.clone();

                                    // Refresh system prompt to update the Model: line
                                    if args.system.is_none() {
                                        let refreshed = prompt::build_system_prompt_with_model(soul, memory, skills, config, &resolved.model);
                                        if let Some(sys_msg) = messages.first_mut() {
                                            sys_msg.content = Some(refreshed);
                                        }
                                    }

                                    println!("{} Switched to {}", "✓".green(), provider_label.bright_white());
                                    println!("  {} {} | API key: {}",
                                        "↳".dimmed(),
                                        resolved.base_url.bright_white(),
                                        if resolved.api_key.is_some() { "set" } else { "none" }.dimmed()
                                    );
                                }
                                continue;
                            }
                            if line == "/retry" {
                                let last_user_idx = messages.iter().rposition(|m| m.role == "user");
                                if let Some(idx) = last_user_idx {
                                    if idx + 1 < messages.len() {
                                        messages.truncate(idx + 1);
                                    }
                                    println!("{}", "Retrying last message...".dimmed());
                                    match run_agent_loop(&client, &mut messages, tools_payload, max_turns, args.no_stream, memory, mcp, &mut session).await {
                                        Ok(()) => {}
                                        Err(e) => {
                                            eprintln!("{} {}", "Error:".red(), e);
                                            if !messages.is_empty() && messages.last().is_some_and(|m| m.role == "user") {
                                                messages.pop();
                                            }
                                        }
                                    }
                                } else {
                                    println!("{} No message to retry", "Error:".red());
                                }
                                continue;
                            }
                            if line == "/undo" {
                                let last_user_idx = messages.iter().rposition(|m| m.role == "user");
                                if let Some(idx) = last_user_idx {
                                    messages.truncate(idx);
                                    println!("{}", "Undid last exchange.".dimmed());
                                } else {
                                    println!("{} Nothing to undo", "Error:".red());
                                }
                                continue;
                            }
                        }
                    }
                }

                rl.add_history_entry(&line).ok();

                let user_msg = Message::user(&line);
                session.append(&user_msg)?;
                messages.push(user_msg);

                match run_agent_loop(&client, &mut messages, tools_payload, max_turns, args.no_stream, memory, mcp, &mut session).await {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("{} {}", "Error:".red(), e);
                        messages.pop();
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("{}", "^C".dimmed());
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                break;
            }
            Err(e) => {
                eprintln!("{} {}", "Input error:".red(), e);
                break;
            }
        }
    }

    // Exit summary hook — only on clean exit, only in REPL mode, only if there was a real conversation
    if config.summarize_on_exit() && summary::should_summarize(&messages) {
        eprintln!("{}", "  Generating session summary...".dimmed());
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            summary::generate_session_summary(&client, &messages),
        )
        .await
        {
            Ok(Ok(summary_text)) if !summary_text.is_empty() => {
                if let Err(e) = memory.add_memory(format!("session_summary\n{}", summary_text)) {
                    eprintln!("{} Failed to save session summary: {}", "Warning:".yellow(), e);
                } else {
                    eprintln!("{}", "  Session summary saved to memory.".dimmed());
                }
            }
            Ok(Ok(_)) => {
                // Empty summary (not enough conversation) — skip silently
            }
            Ok(Err(e)) => {
                // LLM call failed — use fallback
                let fallback = summary::fallback_summary(&messages);
                if let Err(e2) = memory.add_memory(format!("session_summary\n{}", fallback)) {
                    eprintln!("{} Failed to save session summary: {}", "Warning:".yellow(), e2);
                } else {
                    eprintln!("{} Session saved (fallback: {})", "  ↳".dimmed(), fallback.dimmed());
                }
                eprintln!("{} Summary generation failed: {}", "Warning:".yellow(), e);
            }
            Err(_) => {
                // Timeout — use fallback
                let fallback = summary::fallback_summary(&messages);
                if let Err(e) = memory.add_memory(format!("session_summary\n{}", fallback)) {
                    eprintln!("{} Failed to save session summary: {}", "Warning:".yellow(), e);
                } else {
                    eprintln!("{} Session saved (fallback: {})", "  ↳".dimmed(), fallback.dimmed());
                }
                eprintln!("{}", "  Summary timed out, using fallback.".dimmed());
            }
        }
    }

    Ok(())
}

/// Run the agent loop: call model, handle tool_calls, repeat until done or max_turns.
async fn run_agent_loop(
    client: &LlmClient,
    messages: &mut Vec<Message>,
    tools_payload: &Option<Value>,
    max_turns: u32,
    no_stream: bool,
    memory: &mut MemoryStore,
    mcp: &mut McpManager,
    session: &mut Session,
) -> Result<()> {
    for _ in 0..max_turns {
        let result = if no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            let assistant_msg = Message::assistant_with_tools(tool_calls.clone(), result.content);
            session.append(&assistant_msg)?;
            messages.push(assistant_msg);

            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = dispatch_tool(&tc.function.name, &tc_args, memory, mcp).await;
                let tool_msg = Message::tool_result(&tc.id, output);
                session.append(&tool_msg)?;
                messages.push(tool_msg);
            }
        } else {
            if let Some(content) = &result.content
                && no_stream
            {
                println!("{}", content);
            }
            if let Some(content) = &result.content {
                session.append(&Message::assistant(content))?;
            }
            return Ok(());
        }
    }

    anyhow::bail!("Max agent turns reached ({}). The agent exceeded its turn limit without producing a final response.", max_turns);
}

/// Print a visual indicator for a tool call.
fn print_tool_call(name: &str, arguments: &str) {
    let display_args = if arguments.len() > 80 {
        format!("{}...", &arguments[..80])
    } else {
        arguments.to_string()
    };
    println!(
        "  {} {}{}",
        "⚙".bright_yellow(),
        name.bright_white(),
        format!("({})", display_args).dimmed()
    );
}

fn print_banner(model: &str, base_url: &str, soul: &Soul, skills: &SkillsIndex, tools_payload: &Option<Value>, mcp: &McpManager, session_id: &str) {
    let name = soul
        .content
        .lines()
        .next()
        .unwrap_or("Enchanter")
        .trim_start_matches('#')
        .trim();

    let tool_count = tools_payload
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let mcp_count = mcp.total_tool_count();
    let mcp_display = if mcp_count > 0 {
        format!(" ({} MCP)", mcp_count)
    } else {
        String::new()
    };

    let short_session_id = if session_id.len() > 8 { &session_id[..8] } else { session_id };
    println!(
        "\n  {} {}  session={}",
        "⟡".bright_magenta(),
        name.bright_cyan().bold(),
        short_session_id.dimmed()
    );
    // Shorten common base URLs for display
    let short_url = base_url
        .trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq");
    println!(
        "  {} model={} | provider={} | tools={}{} | skills={} | /help for commands\n",
        "  ↳".dimmed(),
        model.bright_white(),
        short_url.bright_white(),
        tool_count.to_string().bright_white(),
        mcp_display.dimmed(),
        skills.skills.len().to_string().bright_white()
    );
}

fn print_tools(mcp: &McpManager) {
    println!("{}", "═══ TOOLS ═══".bright_cyan());

    println!("{}", "── BUILT-IN ──".bright_blue());
    for tool in tools::tool_definitions() {
        println!(
            "  {}{}",
            tool.name.bright_white(),
            if tool.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", tool.description.lines().next().unwrap_or(""))
            }
            .dimmed()
        );
    }

    let servers = mcp.server_names();
    if !servers.is_empty() {
        println!("{}", "── MCP ──".bright_blue());
        for server_name in servers {
            println!("  [{}]", server_name.bright_green());
            let mcp_tools = mcp.all_tools_json();
            for tool in &mcp_tools {
                if let Some(name) = tool.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str())
                    && name.starts_with(&format!("{}:", server_name))
                {
                    let short_name = &name[server_name.len() + 1..];
                    let desc = tool.get("function")
                        .and_then(|f| f.get("description"))
                        .and_then(|d| d.as_str())
                        .map(|d| format!(" — {}", d.lines().next().unwrap_or("")))
                        .unwrap_or_default();
                    println!("    {}{}", short_name.bright_white(), desc.dimmed());
                }
            }
        }
    }

    let total = tools::tool_definitions().len() + mcp.total_tool_count();
    println!("\n  {} total tools", total);
}

fn print_help(config: &Config) {
    let commands = vec![
        ("/help", "Show this help"),
        ("/clear", "Clear conversation history"),
        ("/soul", "Show current SOUL.md content"),
        ("/memory", "Show loaded memory"),
        ("/skills", "List discovered skills"),
        ("/tools", "List all available tools (built-in + MCP)"),
        ("/model <name>", "Switch model or provider (see config.yaml providers)"),
        ("/retry", "Re-send the last user message"),
        ("/undo", "Remove last exchange from history"),
        ("/sessions", "List session history"),
        ("/config", "Show resolved configuration"),
        ("/prompt", "Show assembled system prompt"),
        ("/exit", "Exit the REPL"),
    ];

    println!("{}", "═══ COMMANDS ═══".bright_cyan());
    for (cmd, desc) in commands {
        println!("  {:<12} {}", cmd.bright_green(), desc.dimmed());
    }

    if !config.providers.is_empty() {
        println!("\n  {}Providers in config:", "↳".dimmed());
        let mut providers: Vec<_> = config.providers.keys().collect();
        providers.sort();
        for name in providers {
            if let Some(p) = config.providers.get(name) {
                let model = p.model.as_deref().unwrap_or("(default)");
                println!("    {} → {}", name.bright_green(), model);
            }
        }
    }
}

fn print_init_guidance() {
    let home = crate::home::enchanter_home();
    println!(
        "\n  {} Initialized {}\n",
        "⟡".bright_magenta(),
        home.display().to_string().bright_white()
    );
    println!("  Created:");
    println!("    {}/SOUL.md       — edit to set your agent's persona", home.display());
    println!("    {}/config.yaml   — set model, base_url, api_key, MCP servers", home.display());
    println!("    {}/memories/     — MEMORY.md & USER.md go here", home.display());
    println!("    {}/skills/       — drop in SKILL.md files", home.display());
    println!();
    println!(
        "  {} Configure a provider (examples in config.yaml):",
        "Next:".bright_yellow()
    );
    println!("    OpenAI:      export ENCHANTER_API_KEY=your-key");
    println!("    OpenRouter:  export ENCHANTER_API_KEY=your-key  +  base_url in config.yaml");
    println!("    Ollama:      export ENCHANTER_BASE_URL=http://localhost:11434/v1");
    println!();
    println!(
        "  {} https://andrewthecoder.com/projects/enchanter",
        "Docs:".bright_cyan()
    );
}