//! CLI definition and REPL loop.

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde_json::Value;

use crate::api::{LlmClient, Message};
use crate::config::Config;
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::prompt;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use crate::tools;

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

    // Create LLM client early for memory management
    let model = args.model.clone().unwrap_or_else(|| config.model_id());
    let api_key = config.api_key();
    let client = LlmClient::new(&config.base_url(), api_key.as_deref(), &model);

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
        let result = run_single(&args, &config, &soul, &mut memory, &skills, &client, &model, &mcp, &tools_payload, user_prompt).await;
        mcp.shutdown_all().await;
        return result;
    }

    let result = run_repl(&args, &config, &soul, &mut memory, &skills, client, model, &mcp, &tools_payload).await;
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
        }
        Commands::Prompt => {
            let system_prompt = prompt::build_system_prompt(soul, memory, skills, config);
            println!("{}", "═══ SYSTEM PROMPT ═══".bright_cyan());
            println!("{}", system_prompt);
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
    mcp: &McpManager,
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
    _model: &str,
    mcp: &McpManager,
    tools_payload: &Option<Value>,
    user_prompt: &str,
) -> Result<()> {
    let system_content = match &args.system {
        Some(s) => s.clone(),
        None => prompt::build_system_prompt(soul, memory, skills, config),
    };

    let mut messages = vec![
        Message::system(&system_content),
        Message::user(user_prompt),
    ];

    let max_turns = config.max_turns();

    for _ in 0..max_turns {
        let result = if args.no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            messages.push(Message::assistant_with_tools(tool_calls.clone(), result.content));

            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = dispatch_tool(&tc.function.name, &tc_args, memory, mcp).await;
                messages.push(Message::tool_result(&tc.id, output));
            }
        } else {
            if let Some(content) = &result.content
                && args.no_stream
            {
                println!("{}", content);
            }
            return Ok(());
        }
    }

    eprintln!("{}", "Max agent turns reached.".yellow());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_repl(
    args: &Args,
    config: &Config,
    soul: &Soul,
    memory: &mut MemoryStore,
    skills: &SkillsIndex,
    mut client: LlmClient,
    mut model: String,
    mcp: &McpManager,
    tools_payload: &Option<Value>,
) -> Result<()> {
    let system_prompt = if let Some(ref sys_override) = args.system {
        sys_override.clone()
    } else {
        prompt::build_system_prompt(soul, memory, skills, config)
    };

    let mut messages = vec![Message::system(&system_prompt)];
    let max_turns = config.max_turns();

    print_banner(&model, soul, skills, tools_payload, mcp);

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
                            messages = vec![Message::system(&system_prompt)];
                            println!("{}", "Conversation cleared.".dimmed());
                            continue;
                        }
                        "/help" => {
                            print_help();
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
                            println!("Model: {} | Base: {} | Max: {}",
                                model, config.base_url(), max_turns);
                            continue;
                        }
                        "/prompt" => {
                            println!("{}", system_prompt);
                            continue;
                        }
                        "/tools" => {
                            print_tools(mcp);
                            continue;
                        }
                        _ => {
                            // Handle /model <name>, /retry, /undo
                            if let Some(new_model) = line.strip_prefix("/model ") {
                                let new_model = new_model.trim().to_string();
                                if new_model.is_empty() {
                                    println!("{} Usage: /model <model-name>", "Error:".red());
                                } else {
                                    client = LlmClient::new(&config.base_url(), config.api_key().as_deref(), &new_model);
                                    model = new_model.clone();
                                    println!("{} Switched to model {}", "✓".green(), model.bright_white());
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
                                    match run_agent_loop(&client, &mut messages, tools_payload, max_turns, args.no_stream, memory, mcp).await {
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

                messages.push(Message::user(&line));

                match run_agent_loop(&client, &mut messages, tools_payload, max_turns, args.no_stream, memory, mcp).await {
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
    mcp: &McpManager,
) -> Result<()> {
    for _ in 0..max_turns {
        let result = if no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            messages.push(Message::assistant_with_tools(tool_calls.clone(), result.content));

            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = dispatch_tool(&tc.function.name, &tc_args, memory, mcp).await;
                messages.push(Message::tool_result(&tc.id, output));
            }
        } else {
            if let Some(content) = &result.content
                && no_stream
            {
                println!("{}", content);
            }
            return Ok(());
        }
    }

    eprintln!("{}", "Max agent turns reached.".yellow());
    Ok(())
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

fn print_banner(model: &str, soul: &Soul, skills: &SkillsIndex, tools_payload: &Option<Value>, mcp: &McpManager) {
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

    println!(
        "\n  {} {}",
        "✦".bright_magenta(),
        name.bright_cyan().bold()
    );
    println!(
        "  {} model={} | tools={}{} | skills={} | /help for commands\n",
        "  ↳".dimmed(),
        model.bright_white(),
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

fn print_help() {
    let commands = vec![
        ("/help", "Show this help"),
        ("/clear", "Clear conversation history"),
        ("/soul", "Show current SOUL.md content"),
        ("/memory", "Show loaded memory"),
        ("/skills", "List discovered skills"),
        ("/tools", "List all available tools (built-in + MCP)"),
        ("/model <n>", "Switch model mid-session"),
        ("/retry", "Re-send the last user message"),
        ("/undo", "Remove last exchange from history"),
        ("/config", "Show resolved configuration"),
        ("/prompt", "Show assembled system prompt"),
        ("/exit", "Exit the REPL"),
    ];

    println!("{}", "═══ COMMANDS ═══".bright_cyan());
    for (cmd, desc) in commands {
        println!("  {:<12} {}", cmd.bright_green(), desc.dimmed());
    }
}

fn print_init_guidance() {
    let home = crate::home::enchanter_home();
    println!(
        "\n  {} Initialized {}\n",
        "✦".bright_magenta(),
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
}