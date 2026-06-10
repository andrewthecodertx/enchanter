//! CLI definition and REPL loop.
//!
//! The REPL interaction pattern (persistent loop with slash commands) borrows from
//! hermes-agent's conversation_loop (hermes-agent/agent/conversation_loop.py) and
//! Claude Code's REPL UX (claude-code/src/main.tsx). Slash commands /clear, /help,
//! /model, /retry, /undo follow the convention established by hermes-agent
//! (hermes-agent/cli.py slash command handling).
//!
//! Session summarization on exit (calling the LLM with a truncated conversation,
//! timeout with fallback) is adapted from hermes-agent's background_review
//! pattern (hermes-agent/agent/background_review.py).
//!
//! The /model provider-switching pattern (named provider presets with
//! inheritance from defaults) is informed by hermes-agent's config.yaml
//! provider resolution (hermes-agent/hermes_cli/config.py).
//!
//! Daemon mode support: enchanter can connect to a background daemon process
//! that keeps MCP servers warm, avoiding cold-start latency. The daemon
//! listens on a Unix socket and the CLI relays requests to it.

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

use crate::agent::{AgentSession, SessionInfo, SessionOptions};
use crate::activity_log::{self, ActivityEvent};
use crate::config::{Config, ResolvedModel};
use crate::recorder::Recorder;
use crate::session::Session;
use crate::summary;

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

    /// Run inline without connecting to the daemon (bypass daemon auto-start).
    #[cfg(unix)]
    #[arg(long)]
    pub no_daemon: bool,

    /// Idle timeout in minutes for the daemon (default: 10).
    #[cfg(unix)]
    #[arg(long)]
    pub idle_timeout: Option<u64>,

    /// Record the full session to a JSONL file (REQ-REC-001).
    #[arg(long)]
    pub record: Option<String>,

    /// Additional field redaction in recordings (REQ-REC-005).
    #[arg(long)]
    pub record_redact: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize a project overlay (.enchanter/) in the current directory
    Init,
    Soul,
    Memory,
    Skills,
    Config,
    /// Show, diff, or budget the assembled system prompt
    Prompt {
        /// Show a diff between the previous and current system prompt
        #[arg(long)]
        diff: bool,
        /// Show a token/character budget breakdown of the system prompt
        #[arg(long)]
        budget: bool,
    },
    /// Replay a recorded session from a JSONL file
    Replay {
        /// Path to the JSONL recording file
        file: String,
        /// Re-run with a different model while preserving harness inputs
        #[arg(long)]
        swap_model: Option<String>,
        /// Error if the current provider/model doesn't match the recording
        #[arg(long)]
        exact: bool,
        /// Tool execution mode: 'live' (default) or 'stubbed' (use recorded outputs)
        #[arg(long, default_value = "live")]
        tools: String,
    },
    /// List or show session history
    Sessions {
        /// Show a specific session by ID
        id: Option<String>,
    },
    /// Daemon management: start, stop, or check status.
    #[cfg(unix)]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Launch the terminal user interface (TUI).
    #[cfg(feature = "tui")]
    Tui,
}

#[derive(Subcommand, Debug)]
#[cfg(unix)]
pub enum DaemonAction {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Show daemon status (model, MCP servers, uptime).
    Status,
}

pub async fn run(args: Args) -> Result<()> {
    if crate::home::init_home()? {
        print_init_guidance();
    }

    // Handle daemon management commands first (Unix only)
    #[cfg(unix)]
    if let Some(Commands::Daemon { action }) = &args.command {
        return handle_daemon_command(action, &args).await;
    }

    // Discover project overlay (.enchanter/ in CWD or parents)
    let overlay = std::env::current_dir()
        .ok()
        .as_ref()
        .and_then(|cwd| crate::overlay::discover_overlay(cwd))
        .map(|path| crate::overlay::analyze_overlay(&path));

    let config = crate::overlay::load_config(overlay.as_ref())?;
    let soul = crate::overlay::load_soul(overlay.as_ref())?;
    let memory = crate::overlay::load_memories(overlay.as_ref())?;
    let kstore = crate::overlay::load_knowledge(overlay.as_ref())?;
    let skills = crate::overlay::discover_skills(overlay.as_ref())?;

    if let Some(cmd) = &args.command {
        #[cfg(feature = "tui")]
        if let Commands::Tui = cmd {
            return run_tui(&args, config, soul, memory, kstore, skills).await;
        }
        return handle_command(cmd, &config, &soul, &memory, &kstore, &skills);
    }

    // Try daemon mode first (unless --no-daemon) — Unix only
    #[cfg(unix)]
    if !args.no_daemon && !args.no_stream {
        if crate::daemon::is_running().await {
            // Daemon is running — use it
            let result = crate::daemon::chat_via_daemon(
                args.prompt.as_deref().unwrap_or(""),
                args.model.clone(),
                args.system.clone(),
                args.no_stream,
                args.no_tools,
            )
            .await;

            match result {
                Ok(Some(text)) => {
                    if args.no_stream {
                        println!("{}", text);
                    }
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Err(e) => {
                    // Fall back to inline mode
                    eprintln!("{} Daemon connection failed: {}", "Warning:".yellow(), e);
                    eprintln!("{} Falling back to inline mode...", "  ↳".dimmed());
                }
            }
        } else if args.prompt.is_some() {
            // Single prompt mode: try auto-starting daemon
            eprintln!("{} Daemon not running, starting it...", "⟡".dimmed());
            let pid = crate::daemon::spawn_daemon(args.idle_timeout)?;
            eprintln!("{} Daemon started (PID {})", "✓".green(), pid);

            if let Ok(()) = crate::daemon::wait_for_socket(60).await {
                let result = crate::daemon::chat_via_daemon(
                    args.prompt.as_deref().unwrap_or(""),
                    args.model.clone(),
                    args.system.clone(),
                    args.no_stream,
                    args.no_tools,
                )
                .await;

                match result {
                    Ok(Some(text)) => {
                        if args.no_stream {
                            println!("{}", text);
                        }
                        return Ok(());
                    }
                    Ok(None) => return Ok(()),
                    Err(e) => {
                        eprintln!("{} Daemon chat failed: {}", "Warning:".yellow(), e);
                        eprintln!("{} Falling back to inline mode...", "  ↳".dimmed());
                    }
                }
            } else {
                eprintln!(
                    "{} Daemon did not become ready, falling back to inline mode",
                    "Warning:".yellow()
                );
            }
        }
    }

    // Inline mode (current behavior)
    // Overlay already discovered above — just reload with overlay
    let config = crate::overlay::load_config(overlay.as_ref())?;
    let soul = crate::overlay::load_soul(overlay.as_ref())?;
    let memory = crate::overlay::load_memories(overlay.as_ref())?;
    let kstore = crate::overlay::load_knowledge(overlay.as_ref())?;
    let skills = crate::overlay::discover_skills(overlay.as_ref())?;

    // Resolve initial model: -m flag > config
    let resolved = if let Some(model_flag) = &args.model {
        config.resolve_provider(model_flag).unwrap_or_else(|| {
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

    // Create agent session
    let mut agent = AgentSession::new(
        config,
        soul,
        memory,
        kstore,
        skills,
        resolved,
        SessionOptions {
            no_stream: args.no_stream,
            no_tools: args.no_tools,
            system_override: args.system.clone(),
        },
    )?;

    // Initialize system prompt in session
    agent.session.append(&agent.messages[0])?;

    // Log session start
    activity_log::log(ActivityEvent::SessionStart {
        session_id: agent.session.id().to_string(),
        model: agent.resolved.model.clone(),
    });
    let session_start = std::time::Instant::now();

    // Install Ctrl+C handler so session-end gets logged even on interrupt.
    // Without this, a SIGINT during streaming kills the process with no cleanup,
    // and the activity log won't show where the hang was.
    #[cfg(unix)]
    {
        let sid = agent.session.id().to_string();
        let start = session_start;
        ctrlc::set_handler(move || {
            activity_log::log(ActivityEvent::SessionEnd {
                session_id: sid.clone(),
                total_turns: 0,
                total_tool_calls: 0,
                duration_secs: start.elapsed().as_secs(),
            });
            // Activity log flushes on every write, so the event is durable.
            // Exit without running destructors (faster, avoids blocking).
            std::process::exit(130); // 128 + SIGINT(2)
        })?;
    }

    // Cap + summarize memory if needed
    let mem_config = agent.config.memory_config().clone();
    if let Err(e) = agent.memory.manage(&agent.client, &mem_config).await {
        eprintln!("{} memory management: {}", "Warning:".yellow(), e);
    }

    // Start MCP servers
    agent.start_mcp().await;

    // Initialize recording if --record flag is set (REQ-REC-001)
    let mut recorder = if let Some(record_path) = &args.record {
        let rec = Recorder::new(std::path::Path::new(record_path), args.record_redact)?;
        Some(rec)
    } else {
        None
    };

    // Record config snapshot at start (REQ-REC-003)
    if let Some(ref mut rec) = recorder {
        let info = agent.info();
        let provider_names = info.mcp_servers;
        rec.record_config_snapshot(
            &info.model,
            &info.base_url,
            info.api_key_set,
            &provider_names,
        )?;
        // Record prompt layer hashes
        let layers = crate::prompt::build_prompt_layers(
            &agent.soul,
            &agent.memory,
            &agent.kstore,
            &agent.skills,
            &agent.config,
            &agent.resolved.model,
        );
        for layer in &layers.layers {
            use std::hash::Hasher;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&layer.content, &mut hasher);
            let hash = format!("{:016x}", hasher.finish());
            rec.record_prompt_hash(&layer.name, &hash)?;
        }
    }

    if let Some(user_prompt) = &args.prompt {
        // Record user message
        if let Some(ref mut rec) = recorder {
            rec.record_user_message(user_prompt)?;
        }
        let result = agent.chat(user_prompt).await;
        // Record assistant response
        if let (Some(rec), Ok(cr)) = (&mut recorder, &result)
            && let Some(ref text) = cr.response
        {
            rec.record_assistant_response(text)?;
        }
        if args.no_stream
            && let Ok(cr) = &result
            && let Some(ref text) = cr.response
        {
            println!("{}", text);
        }
        agent.shutdown_mcp().await;
        activity_log::log(ActivityEvent::SessionEnd {
            session_id: agent.session.id().to_string(),
            total_turns: 0,
            total_tool_calls: 0,
            duration_secs: session_start.elapsed().as_secs(),
        });
        return result.map(|_| ());
    }

    let result = run_repl(&mut agent, &args, recorder.as_mut()).await;
    agent.shutdown_mcp().await;

    // Exit summary
    if agent.config.summarize_on_exit() && summary::should_summarize(&agent.messages) {
        eprintln!("{}", "  Generating session summary...".dimmed());
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            summary::generate_session_summary(&agent.client, &agent.messages),
        )
        .await
        {
            Ok(Ok(summary_text)) if !summary_text.is_empty() => {
                // Record session summary
                if let Some(ref mut rec) = recorder {
                    let _ = rec.record_session_summary(&summary_text);
                }
                if let Err(e) = agent
                    .memory
                    .add_memory(format!("session_summary\n{}", summary_text))
                {
                    eprintln!(
                        "{} Failed to save session summary: {}",
                        "Warning:".yellow(),
                        e
                    );
                } else {
                    eprintln!("{}", "  Session summary saved to memory.".dimmed());
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let fallback = summary::fallback_summary(&agent.messages);
                if let Err(e2) = agent
                    .memory
                    .add_memory(format!("session_summary\n{}", fallback))
                {
                    eprintln!(
                        "{} Failed to save session summary: {}",
                        "Warning:".yellow(),
                        e2
                    );
                } else {
                    eprintln!(
                        "{} Session saved (fallback: {})",
                        "  ↳".dimmed(),
                        fallback.dimmed()
                    );
                }
                eprintln!("{} Summary generation failed: {}", "Warning:".yellow(), e);
            }
            Err(_) => {
                let fallback = summary::fallback_summary(&agent.messages);
                if let Err(e) = agent
                    .memory
                    .add_memory(format!("session_summary\n{}", fallback))
                {
                    eprintln!(
                        "{} Failed to save session summary: {}",
                        "Warning:".yellow(),
                        e
                    );
                } else {
                    eprintln!(
                        "{} Session saved (fallback: {})",
                        "  ↳".dimmed(),
                        fallback.dimmed()
                    );
                }
                eprintln!("{}", "  Summary timed out, using fallback.".dimmed());
            }
        }
    }

    // Memory is auto-saved on every mutation (add_memory/remove/replace all persist immediately),
    // so no explicit save is needed here. But knowledge store needs to be saved.
    if let Err(e) = agent.kstore.save() {
        eprintln!("{} Failed to save knowledge store: {}", "Warning:".yellow(), e);
    }

    // Log session end
    activity_log::log(ActivityEvent::SessionEnd {
        session_id: agent.session.id().to_string(),
        total_turns: 0, // turns tracked per-turn in activity log
        total_tool_calls: 0,
        duration_secs: session_start.elapsed().as_secs(),
    });

    result
}

/// Handle daemon management commands (Unix only).
#[cfg(unix)]
async fn handle_daemon_command(action: &DaemonAction, args: &Args) -> Result<()> {
    match action {
        DaemonAction::Start => {
            // Check if already running
            if crate::daemon::is_running().await {
                eprintln!("{} Daemon is already running.", "Error:".red());
                return Ok(());
            }

            // If we're the spawned daemon child, run in foreground (block until done).
            if std::env::var("__ENCHANTER_DAEMON_FOREGROUND").is_ok() {
                crate::daemon::run_daemon(args.idle_timeout).await?;
                return Ok(());
            }

            // Otherwise, spawn a background daemon and wait for it to become ready.
            eprintln!("{} Starting daemon...", "⟡".bright_cyan());
            let pid = crate::daemon::spawn_daemon(args.idle_timeout)?;
            eprintln!("{} Daemon started (PID {})", "✓".green(), pid);

            // Wait for the daemon to become ready
            eprintln!("{} Waiting for daemon to become ready...", "  ↳".dimmed());
            match crate::daemon::wait_for_socket(30).await {
                Ok(()) => {
                    eprintln!(
                        "{} Daemon is ready on {}",
                        "✓".green(),
                        crate::daemon::socket_path().display()
                    );
                }
                Err(e) => {
                    eprintln!("{} Daemon did not become ready: {}", "Warning:".yellow(), e);
                }
            }
            Ok(())
        }
        DaemonAction::Stop => {
            eprintln!("{} Stopping daemon...", "⟡".bright_cyan());
            match crate::daemon::stop_daemon().await {
                Ok(()) => {
                    eprintln!("{} Daemon stopped.", "✓".green());
                    Ok(())
                }
                Err(e) => {
                    eprintln!("{} Could not stop daemon: {}", "Error:".red(), e);
                    // Try to clean up stale files
                    let sock = crate::daemon::socket_path();
                    let pid = crate::daemon::pid_path();
                    if sock.exists() {
                        std::fs::remove_file(&sock).ok();
                        eprintln!("{} Removed stale socket", "  ↳".dimmed());
                    }
                    if pid.exists() {
                        std::fs::remove_file(&pid).ok();
                        eprintln!("{} Removed stale PID file", "  ↳".dimmed());
                    }
                    Ok(())
                }
            }
        }
        DaemonAction::Status => {
            // Don't treat status failure as an error — just show info
            let _ = crate::daemon::print_status().await;
            Ok(())
        }
    }
}

/// Launch the TUI.
#[cfg(feature = "tui")]
async fn run_tui(
    args: &Args,
    config: Config,
    soul: crate::soul::Soul,
    memory: crate::memory::MemoryStore,
    kstore: crate::kstore::KnowledgeStore,
    skills: crate::skills::SkillsIndex,
) -> Result<()> {
    let resolved = if let Some(model_flag) = &args.model {
        config.resolve_provider(model_flag).unwrap_or_else(|| {
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

    let mut agent = AgentSession::new(
        config,
        soul,
        memory,
        kstore,
        skills,
        resolved,
        SessionOptions {
            no_stream: false, // streaming in TUI
            no_tools: args.no_tools,
            system_override: args.system.clone(),
        },
    )?;

    agent.session.append(&agent.messages[0])?;

    // Cap + summarize memory if needed
    let mem_config = agent.config.memory_config().clone();
    if let Err(e) = agent.memory.manage(&agent.client, &mem_config).await {
        eprintln!("{} memory management: {}", "Warning:".yellow(), e);
    }

    crate::tui::run(agent).await
}

fn handle_command(
    cmd: &Commands,
    config: &Config,
    soul: &crate::soul::Soul,
    memory: &crate::memory::MemoryStore,
    kstore: &crate::kstore::KnowledgeStore,
    skills: &crate::skills::SkillsIndex,
) -> Result<()> {
    match cmd {
        Commands::Init => {
            let cwd = std::env::current_dir()?;
            match crate::overlay::init_project_overlay(&cwd) {
                Ok(path) => {
                    println!(
                        "{} Created project overlay at {}",
                        "✓".green(),
                        path.display()
                    );
                }
                Err(e) => {
                    // Check if it's the "already exists" case
                    let msg = e.to_string();
                    if msg.contains("already exists") {
                        println!("{} {}", "✗".red(), msg);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
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
            let turns_display = match config.max_turns() {
                Some(n) => n.to_string(),
                None => "unlimited".to_string(),
            };
            let soft_display = match config.soft_limit() {
                Some(n) => n.to_string(),
                None => "n/a".to_string(),
            };
            println!(
                "  Max turns:  {} (soft limit: {})",
                turns_display, soft_display
            );
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
                        println!(
                            "    {} → {} | {} | key: {}",
                            name.bright_green(),
                            model,
                            base,
                            key_status
                        );
                    }
                }
            }
        }
        Commands::Prompt { diff, budget } => {
            let layers = crate::prompt::build_prompt_layers(
                soul,
                memory,
                kstore,
                skills,
                config,
                &config.model_id(),
            );
            if *diff {
                // Diff against itself (no previous state for CLI — show current layers)
                // A meaningful diff requires two turns, so we show the current structure
                println!("{}", "═══ PROMPT DIFF ═══".bright_cyan());
                println!(
                    "{}",
                    "(No previous prompt to diff against — showing current layer structure.)"
                        .dimmed()
                );
                println!();
                for layer in &layers.layers {
                    println!("  {} {}", "●".bright_cyan(), layer.name.bold());
                }
                println!();
                println!(
                    "{}",
                    "Use /prompt diff in a REPL session to diff between turns.".dimmed()
                );
            } else if *budget {
                let report = layers.budget();
                println!("{}", report.render(4000));
            } else {
                // Default: show the assembled system prompt (existing behavior)
                let system_prompt = layers.assemble();
                println!("{}", "═══ SYSTEM PROMPT ═══".bright_cyan());
                println!("{}", system_prompt);
            }
        }
        Commands::Replay {
            file,
            swap_model,
            exact,
            tools,
        } => {
            let path = std::path::PathBuf::from(file);
            let tools_mode =
                crate::replay::parse_tools_mode(tools).map_err(|e| anyhow::anyhow!("{}", e))?;
            crate::replay::replay_session(&path, swap_model.as_deref(), *exact, &tools_mode)?;
        }
        Commands::Sessions { id } => match id {
            Some(session_id) => match Session::load(session_id) {
                Ok(entries) => {
                    let messages = Session::entries_to_messages(&entries);
                    if messages.is_empty() {
                        println!("{} Session {} is empty.", "Note:".dimmed(), session_id);
                    } else {
                        println!(
                            "{} Session {} ({} messages)",
                            "═══ SESSION ═══".bright_cyan(),
                            session_id,
                            messages.len()
                        );
                        for msg in &messages {
                            match msg.role.as_str() {
                                "system" => continue,
                                "user" => println!(
                                    "{} {}",
                                    "⟩".bright_blue(),
                                    msg.content.as_deref().unwrap_or("")
                                ),
                                "assistant" => {
                                    if let Some(content) = &msg.content {
                                        println!(
                                            "{} {}",
                                            "⟨".bright_green(),
                                            content.chars().take(200).collect::<String>()
                                        );
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
                Err(e) => println!(
                    "{} Session '{}' not found: {}",
                    "Error:".red(),
                    session_id,
                    e
                ),
            },
            None => {
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
        },
        #[cfg(unix)]
        Commands::Daemon { .. } => {
            // Handled in run() before this
            unreachable!()
        }
        #[cfg(feature = "tui")]
        Commands::Tui => {
            // Handled in run() before this
            unreachable!()
        }
    }
    Ok(())
}

async fn run_repl(
    agent: &mut AgentSession,
    _args: &Args,
    mut recorder: Option<&mut Recorder>,
) -> Result<()> {
    let info = agent.info();
    print_banner(&info);

    if recorder.is_some() {
        println!("  {} Session recording is active", "●".red());
    }

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
                        "/exit" | "/quit" | "/bye" => break,
                        "/clear" => {
                            agent.clear()?;
                            println!("{}", "Conversation cleared.".dimmed());
                            continue;
                        }
                        "/help" => {
                            print_help(&agent.config);
                            continue;
                        }
                        "/soul" => {
                            println!("{}", agent.soul.content);
                            continue;
                        }
                        "/memory" => {
                            println!("{}", agent.memory.format_for_prompt());
                            continue;
                        }
                        "/skills" => {
                            println!("{}", agent.skills.format_index_for_prompt());
                            continue;
                        }
                        "/config" => {
                            let info = agent.info();
                            let key_status = if info.api_key_set {
                                "configured ✓".green()
                            } else {
                                "not set (not needed for local providers)".dimmed()
                            };
                            println!("  Model:    {}", info.model.bright_white());
                            println!("  Base URL: {}", info.base_url.bright_white());
                            println!("  API key:  {}", key_status);
                            println!("  Max:      {} (soft: {})",
                                info.max_turns.map_or("unlimited".to_string(), |n| n.to_string()),
                                info.soft_limit.map_or("n/a".to_string(), |n| n.to_string())
                            );
                            continue;
                        }
                        "/prompt" => {
                            println!(
                                "{}",
                                agent
                                    .messages
                                    .first()
                                    .map(|m| m.content.as_deref().unwrap_or(""))
                                    .unwrap_or("")
                            );
                            continue;
                        }
                        cmd if cmd.starts_with("/prompt diff") => {
                            let layers = crate::prompt::build_prompt_layers(
                                &agent.soul,
                                &agent.memory,
                                &agent.kstore,
                                &agent.skills,
                                &agent.config,
                                &agent.resolved.model,
                            );
                            if let Some(prev_layers) = &agent.previous_prompt_layers {
                                let diff_result = layers.diff(prev_layers);
                                println!("{}", diff_result.render());
                            } else {
                                println!("{}", "═══ PROMPT DIFF ═══".bright_cyan());
                                println!("{}", "(No previous prompt to diff against — this is the first turn.)".dimmed());
                                println!();
                                println!("Current layers:");
                                for layer in &layers.layers {
                                    println!("  {} {}", "●".bright_cyan(), layer.name.bold());
                                }
                            }
                            // Store current layers for next diff
                            agent.previous_prompt_layers = Some(layers);
                            continue;
                        }
                        cmd if cmd.starts_with("/prompt budget") => {
                            let layers = crate::prompt::build_prompt_layers(
                                &agent.soul,
                                &agent.memory,
                                &agent.kstore,
                                &agent.skills,
                                &agent.config,
                                &agent.resolved.model,
                            );
                            let report = layers.budget();
                            println!("{}", report.render(4000));
                            continue;
                        }
                        "/tools" => {
                            print_tools(&agent.mcp);
                            continue;
                        }
                        "/log" => {
                            let log_path = crate::home::enchanter_home().join("logs/activity.jsonl");
                            if !log_path.exists() {
                                println!("No activity log found at {}", log_path.display());
                            } else {
                                match std::fs::read_to_string(&log_path) {
                                    Ok(contents) => {
                                        let lines: Vec<&str> = contents.lines().rev().take(30).collect();
                                        let reversed: Vec<&str> = lines.into_iter().rev().collect();
                                        println!("{}", "═══ RECENT ACTIVITY LOG ═══".bright_cyan());
                                        for line in &reversed {
                                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                                let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("?");
                                                let evt_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                                                let summary = match evt_type {
                                                    "api_call_start" => format!("API → model={}", v.get("model").and_then(|m| m.as_str()).unwrap_or("?")),
                                                    "api_call_end" => format!("API ✓ {}ms, tool_calls={}", v.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0), v.get("has_tool_calls").and_then(|b| b.as_bool()).unwrap_or(false)),
                                                    "api_call_error" => format!("API ✗ {}ms: {}", v.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0), v.get("error").and_then(|e| e.as_str()).unwrap_or("?")),
                                                    "stream_timeout" => format!("STREAM TIMEOUT after {}s", v.get("elapsed_secs").and_then(|s| s.as_u64()).unwrap_or(0)),
                                                    "tool_call_start" => format!("TOOL → {} {}", v.get("name").and_then(|n| n.as_str()).unwrap_or("?"), v.get("arguments").and_then(|a| a.as_str()).unwrap_or("").chars().take(80).collect::<String>()),
                                                    "tool_call_end" => format!("TOOL ✓ {} {}ms", v.get("name").and_then(|n| n.as_str()).unwrap_or("?"), v.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0)),
                                                    "tool_call_error" => format!("TOOL ✗ {}: {}", v.get("name").and_then(|n| n.as_str()).unwrap_or("?"), v.get("error").and_then(|e| e.as_str()).unwrap_or("?")),
                                                    "turn_start" => format!("TURN {} start", v.get("turn").and_then(|t| t.as_u64()).unwrap_or(0)),
                                                    "turn_end" => format!("TURN {} end, {}ms, {} tool_calls", v.get("turn").and_then(|t| t.as_u64()).unwrap_or(0), v.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0), v.get("tool_calls").and_then(|t| t.as_u64()).unwrap_or(0)),
                                                    "session_start" => format!("SESSION START model={}", v.get("model").and_then(|m| m.as_str()).unwrap_or("?")),
                                                    "session_end" => format!("SESSION END {}s", v.get("duration_secs").and_then(|s| s.as_u64()).unwrap_or(0)),
                                                    "max_turns_reached" => format!("MAX TURNS turn={} limit={}", v.get("turn").and_then(|t| t.as_u64()).unwrap_or(0), v.get("limit").and_then(|l| l.as_str()).unwrap_or("?")),
                                                    "compacted" => format!("COMPACTION removed={} msgs", v.get("removed_messages").and_then(|r| r.as_u64()).unwrap_or(0)),
                                                    _ => evt_type.to_string(),
                                                };
                                                println!("  {} {}", ts.dimmed(), summary);
                                            }
                                        }
                                        println!();
                                        println!("  {} {}", "↳".dimmed(), format!("({} bytes, {} lines)", std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0), contents.lines().count()).dimmed());
                                    }
                                    Err(e) => println!("{} Cannot read log: {}", "Error:".red(), e),
                                }
                            }
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
                                            let short_id =
                                                if s.id.len() > 8 { &s.id[..8] } else { &s.id };
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
                                Err(e) => {
                                    eprintln!("{} Could not list sessions: {}", "Error:".red(), e)
                                }
                            }
                            continue;
                        }
                        _ => {
                            // Handle /model <name>, /retry, /undo
                            if let Some(new_name) = line.strip_prefix("/model ") {
                                let old_model = agent.resolved.model.clone();
                                let new_name = new_name.trim().to_string();
                                if new_name.is_empty() {
                                    println!(
                                        "{} Usage: /model <name> (provider name from config or model ID)",
                                        "Error:".red()
                                    );
                                } else {
                                    match agent.switch_model(&new_name) {
                                        Ok(provider_label) => {
                                            // Record model change
                                            if let Some(ref mut rec) = recorder {
                                                rec.record_model_change(&old_model, &new_name)?;
                                            }
                                            let info = agent.info();
                                            println!(
                                                "{} Switched to {}",
                                                "✓".green(),
                                                provider_label.bright_white()
                                            );
                                            println!(
                                                "  {} {} | API key: {}",
                                                "↳".dimmed(),
                                                info.base_url.bright_white(),
                                                if info.api_key_set { "set" } else { "none" }
                                                    .dimmed()
                                            );
                                        }
                                        Err(e) => eprintln!("{} {}", "Error:".red(), e),
                                    }
                                }
                                continue;
                            }
                            if line == "/retry" {
                                match agent.retry().await {
                                    Ok(cr) => {
                                        if agent.no_stream
                                            && let Some(ref text) = cr.response
                                        {
                                            println!("{}", text);
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("{} {}", "Error:".red(), e);
                                        // Remove the failed user message if it's still there
                                        if !agent.messages.is_empty()
                                            && agent
                                                .messages
                                                .last()
                                                .is_some_and(|m| m.role == "user")
                                        {
                                            agent.messages.pop();
                                        }
                                    }
                                }
                                continue;
                            }
                            if line == "/undo" {
                                if agent.undo() {
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

                // Record user message
                if let Some(ref mut rec) = recorder {
                    rec.record_user_message(&line)?;
                }

                match agent.chat(&line).await {
                    Ok(cr) => {
                        // Record assistant response
                        if let Some(ref mut rec) = recorder
                            && let Some(ref text) = cr.response
                        {
                            rec.record_assistant_response(text)?;
                        }
                        if agent.no_stream
                            && let Some(ref text) = cr.response
                        {
                            println!("{}", text);
                        }
                    }
                    Err(e) => {
                        eprintln!("{} {}", "Error:".red(), e);
                        agent.messages.pop();
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

    Ok(())
}

fn print_banner(info: &SessionInfo) {
    let name = "Enchanter";

    let mcp_display = if info.mcp_tool_count > 0 {
        format!(" ({} MCP)", info.mcp_tool_count)
    } else {
        String::new()
    };

    let short_session_id = if info.session_id.len() > 8 {
        &info.session_id[..8]
    } else {
        &info.session_id
    };
    println!(
        "\n  {} {}  session={}",
        "⟡".bright_magenta(),
        name.bright_cyan().bold(),
        short_session_id.dimmed()
    );
    let short_url = info
        .base_url
        .trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq");
    println!(
        "  {} model={} | provider={} | tools={}{} | skills={} | /help for commands\n",
        "  ↳".dimmed(),
        info.model.bright_white(),
        short_url.bright_white(),
        info.tool_count.to_string().bright_white(),
        mcp_display.dimmed(),
        info.skill_count.to_string().bright_white()
    );
}

fn print_tools(mcp: &crate::mcp::McpManager) {
    println!("{}", "═══ TOOLS ═══".bright_cyan());

    println!("{}", "── BUILT-IN ──".bright_blue());
    for tool in crate::tools::tool_definitions() {
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
                if let Some(name) = tool
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    && name.starts_with(&format!("{}:", server_name))
                {
                    let short_name = &name[server_name.len() + 1..];
                    let desc = tool
                        .get("function")
                        .and_then(|f| f.get("description"))
                        .and_then(|d| d.as_str())
                        .map(|d| format!(" — {}", d.lines().next().unwrap_or("")))
                        .unwrap_or_default();
                    println!("    {}{}", short_name.bright_white(), desc.dimmed());
                }
            }
        }
    }

    let total = crate::tools::tool_definitions().len() + mcp.total_tool_count();
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
        (
            "/model <name>",
            "Switch model or provider (see config.yaml providers)",
        ),
        ("/retry", "Re-send the last user message"),
        ("/undo", "Remove last exchange from history"),
        ("/sessions", "List session history"),
        ("/config", "Show resolved configuration"),
        ("/prompt", "Show assembled system prompt"),
        (
            "/prompt diff",
            "Show diff of system prompt from previous turn",
        ),
        (
            "/prompt budget",
            "Show token/character budget per prompt layer",
        ),
        ("/exit, /quit, /bye", "Exit the REPL"),
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
    println!(
        "    {}/SOUL.md       — edit to set your agent's persona",
        home.display()
    );
    println!(
        "    {}/config.yaml   — set model, base_url, api_key, MCP servers",
        home.display()
    );
    println!(
        "    {}/memories/     — MEMORY.md & USER.md go here",
        home.display()
    );
}
