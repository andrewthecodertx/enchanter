//! CLI definition and REPL loop.

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde_json::Value;

use crate::api::{LlmClient, Message};
use crate::config::Config;
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
    let memory = MemoryStore::load()?;
    let skills = SkillsIndex::discover()?;

    if let Some(cmd) = &args.command {
        return handle_command(cmd, &config, &soul, &memory, &skills);
    }

    if let Some(user_prompt) = &args.prompt {
        return run_single(&args, &config, &soul, &memory, &skills, user_prompt).await;
    }

    run_repl(&args, &config, &soul, &memory, &skills).await
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

/// Build the tools JSON payload (or None if --no-tools).
fn build_tools(args: &Args) -> Option<Value> {
    if args.no_tools {
        return None;
    }
    Some(serde_json::Value::Array(tools::tools_json()))
}

async fn run_single(
    args: &Args,
    config: &Config,
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
    user_prompt: &str,
) -> Result<()> {
    let model = args.model.clone().unwrap_or_else(|| config.model_id());
    let api_key = config.api_key();
    let tools_payload = build_tools(args);

    let system_content = match &args.system {
        Some(s) => s.clone(),
        None => prompt::build_system_prompt(soul, memory, skills, config),
    };

    let mut messages = vec![
        Message::system(&system_content),
        Message::user(user_prompt),
    ];

    let client = LlmClient::new(&config.base_url(), api_key.as_deref(), &model);
    let max_turns = config.max_turns();

    for _ in 0..max_turns {
        let result = if args.no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            // Push assistant message with tool calls
            messages.push(Message::assistant_with_tools(tool_calls.clone(), result.content));

            // Execute each tool call and push results
            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = tools::dispatch(&tc.function.name, &args);
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

async fn run_repl(
    args: &Args,
    config: &Config,
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
) -> Result<()> {
    let model = args.model.clone().unwrap_or_else(|| config.model_id());
    let api_key = config.api_key();
    let tools_payload = build_tools(args);

    let system_prompt = if let Some(ref sys_override) = args.system {
        sys_override.clone()
    } else {
        prompt::build_system_prompt(soul, memory, skills, config)
    };

    let mut messages = vec![Message::system(&system_prompt)];
    let client = LlmClient::new(&config.base_url(), api_key.as_deref(), &model);
    let max_turns = config.max_turns();

    print_banner(&model, soul, skills, &tools_payload);

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
                        _ => {}
                    }
                }

                rl.add_history_entry(&line).ok();

                messages.push(Message::user(&line));

                match run_agent_loop(&client, &mut messages, &tools_payload, max_turns, args.no_stream).await {
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
) -> Result<()> {
    for _ in 0..max_turns {
        let result = if no_stream {
            client.chat(messages.clone(), tools_payload.clone()).await?
        } else {
            client.chat_stream(messages.clone(), tools_payload.clone()).await?
        };

        if result.has_tool_calls() {
            let tool_calls = result.tool_calls.unwrap();
            // Push assistant message with tool calls
            messages.push(Message::assistant_with_tools(tool_calls.clone(), result.content));

            // Execute each tool call and push results
            for tc in &tool_calls {
                print_tool_call(&tc.function.name, &tc.function.arguments);
                let args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
                let output = tools::dispatch(&tc.function.name, &args);
                messages.push(Message::tool_result(&tc.id, output));
            }
        } else {
            // Model responded with plain text — we're done
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

fn print_banner(model: &str, soul: &Soul, skills: &SkillsIndex, tools_payload: &Option<Value>) {
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

    println!(
        "\n  {} {}",
        "⟡".bright_magenta(),
        name.bright_cyan().bold()
    );
    println!(
        "  {} model={} | tools={} | skills={} | /help for commands\n",
        "  ↳".dimmed(),
        model.bright_white(),
        tool_count.to_string().bright_white(),
        skills.skills.len().to_string().bright_white()
    );
}

fn print_help() {
    let commands = vec![
        ("/help", "Show this help"),
        ("/clear", "Clear conversation history"),
        ("/soul", "Show current SOUL.md content"),
        ("/memory", "Show loaded memory"),
        ("/skills", "List discovered skills"),
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
        "⟡".bright_magenta(),
        home.display().to_string().bright_white()
    );
    println!("  Created:");
    println!("    {}/SOUL.md       — edit to set your agent's persona", home.display());
    println!("    {}/config.yaml   — set model, base_url, api_key", home.display());
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