//! CLI definition and REPL loop.

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

use crate::config::Config;
use crate::memory::MemoryStore;
use crate::prompt;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use crate::api::{LlmClient, Message};

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

    let system_content = match &args.system {
        Some(s) => s.clone(),
        None => prompt::build_system_prompt(soul, memory, skills, config),
    };

    let messages = vec![
        Message::system(&system_content),
        Message::user(user_prompt),
    ];

    let client = LlmClient::new(&config.base_url(), api_key.as_deref(), &model);

    if args.no_stream {
        let response = client.chat(messages).await?;
        println!("{}", response);
    } else {
        client.chat_stream(messages).await?;
    }

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

    let system_prompt = if let Some(ref sys_override) = args.system {
        sys_override.clone()
    } else {
        prompt::build_system_prompt(soul, memory, skills, config)
    };

    let mut messages = vec![Message::system(&system_prompt)];
    let client = LlmClient::new(&config.base_url(), api_key.as_deref(), &model);

    print_banner(&model, soul, skills);

    let mut rl = rustyline::DefaultEditor::new()?;
    let max_turns = config.max_turns();

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

                match client.chat_stream(messages.clone()).await {
                    Ok(response) => {
                        messages.push(Message::assistant(&response));
                    }
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

fn print_banner(model: &str, soul: &Soul, skills: &SkillsIndex) {
    let name = soul
        .content
        .lines()
        .next()
        .unwrap_or("Enchanter")
        .trim_start_matches('#')
        .trim();

    println!(
        "\n  {} {}",
        "⟡".bright_magenta(),
        name.bright_cyan().bold()
    );
    println!(
        "  {} model={} | skills={} | /help for commands\n",
        "  ↳".dimmed(),
        model.bright_white(),
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