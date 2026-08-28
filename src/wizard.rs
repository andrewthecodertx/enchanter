//! First-run setup wizard and `enchanter config` editing.
//!
//! The guided first-run setup (interactive provider/model/API-key prompts with
//! masked input and a confirmation summary) follows the pattern of hermes-agent's
//! setup-hermes.sh bootstrap, which walks a new user through provider selection
//! instead of dropping them into a bare config file
//! (hermes-agent/setup-hermes.sh). Config files are kept minimal and
//! uncommented per project convention; the scaffold default is just
//! `config_version` + the model default, and everything else is added through
//! the wizard, `--set`, or hand-editing.
//!
//! Masked API-key entry uses rpassword (the library Claude Code's credential
//! storage uses for terminal secrets, claude-code/src/credentials/). Atomic
//! config writes (write to `.tmp` then rename) avoid a truncated config.yaml if
//! the process dies mid-write; on Unix, configs containing an API key get mode
//! 0600 so the key isn't world-readable.

use anyhow::{Context, Result, bail};
use colored::Colorize;

use crate::config::Config;
use crate::home;

/// A built-in provider preset offered in the setup wizard.
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub default_model: &'static str,
    pub base_url: &'static str,
    pub key_required: bool,
    pub key_hint: &'static str,
}

/// Provider presets offered by the wizard:
/// - openai: the default; key required.
/// - openrouter: multi-provider gateway; key required.
/// - groq: fast hosted models; key required.
/// - local: Ollama-compatible local endpoint; no key.
/// - other: any OpenAI-compatible provider; key optional.
pub fn presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            id: "openai",
            label: "OpenAI",
            default_model: "gpt-4.1-mini",
            base_url: "https://api.openai.com/v1/chat/completions",
            key_required: true,
            key_hint: "an OpenAI API key (https://platform.openai.com/api-keys)",
        },
        ProviderPreset {
            id: "openrouter",
            label: "OpenRouter",
            default_model: "deepseek/deepseek-v4-flash-0731",
            base_url: "https://openrouter.ai/api/v1/chat/completions",
            key_required: true,
            key_hint: "an OpenRouter API key (https://openrouter.ai/keys)",
        },
        ProviderPreset {
            id: "groq",
            label: "Groq",
            default_model: "llama-3.3-70b-versatile",
            base_url: "https://api.groq.com/openai/v1/chat/completions",
            key_required: true,
            key_hint: "a Groq API key (https://console.groq.com/keys)",
        },
        ProviderPreset {
            id: "local",
            label: "Local (Ollama)",
            default_model: "llama3",
            base_url: "http://localhost:11434/v1/chat/completions",
            key_required: false,
            key_hint: "no key needed for Ollama (leave blank)",
        },
        ProviderPreset {
            id: "other",
            label: "Other (OpenAI-compatible)",
            default_model: "gpt-4.1-mini",
            base_url: "https://api.example.com/v1/chat/completions",
            key_required: false,
            key_hint: "the provider's API key (optional)",
        },
    ]
}

/// Whether the config has a usable default connection: a model is set AND
/// either an API key resolves (config/env) or the base URL points at a local
/// endpoint (which needs no key).
pub fn is_configured(cfg: &Config) -> bool {
    if cfg.model.default.is_none() {
        return false;
    }
    if cfg.api_key().is_some() {
        return true;
    }
    let base = cfg.base_url();
    base.starts_with("http://localhost") || base.starts_with("http://127.0.0.1")
}

/// True when stdin is an interactive terminal (not a pipe).
pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn config_path() -> std::path::PathBuf {
    home::enchanter_home().join("config.yaml")
}

fn mem_dir() -> std::path::PathBuf {
    home::enchanter_home().join("memories")
}

fn flush_stdout() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn as_mapping_mut(value: &mut serde_yaml::Value) -> &mut serde_yaml::Mapping {
    if !value.is_mapping() {
        *value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    value.as_mapping_mut().expect("mapping guaranteed")
}

fn has_api_key(value: &serde_yaml::Value) -> bool {
    value
        .get("model")
        .and_then(|m| m.get("api_key"))
        .and_then(|k| k.as_str())
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(unix)]
fn set_owner_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path) {}

/// Write a YAML `Value` as a config file. Atomic: writes to `config.yaml.tmp`
/// then renames. When the config contains an api_key, permissions are
/// restricted to owner-only on Unix.
pub fn write_config_value(value: &serde_yaml::Value, path: &std::path::Path) -> Result<()> {
    let yaml = serde_yaml::to_string(value)?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &yaml).with_context(|| format!("writing {}", tmp.display()))?;
    if has_api_key(value) {
        set_owner_only(&tmp);
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn read_config_value(path: &std::path::Path) -> serde_yaml::Value {
    if path.exists() {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
            .unwrap_or_else(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    }
}

/// Seed MEMORY.md / USER.md with friendly starter content when absent.
/// Existing files are never touched.
pub fn seed_memory_files() -> Result<()> {
    seed_memory_files_at(&mem_dir())
}

fn seed_memory_files_at(mem_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(mem_dir).context("creating memories directory")?;
    let memory_content = "# Notes\n\nEntries here (\u{00a7}-delimited) are loaded into the prompt automatically.\nThey accumulate as you work; old entries get summarized automatically.\n";
    let user_content = "# About you\n\nNotes about the user are loaded into the prompt automatically.\nAdd preferences, background, and context you want your agent to remember.\n";
    for (name, content) in [("MEMORY.md", memory_content), ("USER.md", user_content)] {
        let path = mem_dir.join(name);
        if !path.exists() {
            std::fs::write(&path, content)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

// ── interactive flows ──

/// Read a line from stdin (trimmed). Empty on EOF/error so a broken pipe
/// during guidance doesn't cascade.
fn read_line() -> String {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(_) => line.trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Prompt for a masked API key via rpassword. Falls back to plain stdin reading
/// when terminal echo control is unavailable (rare).
fn prompt_masked(prompt: &str) -> Result<String> {
    match rpassword::prompt_password(prompt) {
        Ok(v) => Ok(v.trim().to_string()),
        Err(_) => {
            eprint!("{}", prompt);
            Ok(read_line())
        }
    }
}

/// Interactive provider/model/key flow shared by the first-run wizard and
/// `enchanter config --edit`. `existing` prefills found values; `seed_files`
/// is true only on first run. Returns Ok(true) when the config was written.
fn run_flow(existing: Option<&Config>, seed_files: bool, title: &str) -> Result<bool> {
    if !stdin_is_tty() {
        println!(
            "{} No interactive terminal detected — use `enchanter config --set model=... --set api_key=...` instead.",
            "Note:".yellow()
        );
        return Ok(false);
    }

    println!(
        "\n  {} {}",
        "⟡".bright_magenta(),
        title.bright_white().bold()
    );
    println!(
        "  {} Enchanter needs a model and an API endpoint to talk to an LLM.",
        "↳".dimmed()
    );
    println!(
        "  {} Choose a provider below, or pick Local to use Ollama. The result is saved to {}.",
        "↳".dimmed(),
        config_path().display().to_string().dimmed()
    );

    let preset_list = presets();
    println!("\n  Providers:");
    for (i, p) in preset_list.iter().enumerate() {
        let key_req = if p.key_required {
            "key required"
        } else {
            "no key needed"
        };
        println!(
            "    {} {:<10} {} {} ({})",
            format!("[{}]", i + 1).bright_green(),
            p.label,
            p.default_model.dimmed(),
            p.base_url.dimmed(),
            key_req.dimmed()
        );
    }

    let prefill_model = existing.map(Config::model_id);
    let prefill_base = existing.map(Config::base_url);

    let choice = loop {
        print!("  Select provider [1-{}] (default 1): ", preset_list.len());
        flush_stdout();
        let line = read_line();
        if line.is_empty() {
            break 1usize;
        }
        if let Ok(n) = line.parse::<usize>()
            && (1..=preset_list.len()).contains(&n)
        {
            break n;
        }
        println!(
            "  {} Please enter a number 1-{}.",
            "✗".red(),
            preset_list.len()
        );
    };
    let preset = &preset_list[choice - 1];

    println!(
        "\n  {} {}\n",
        "→".bright_cyan(),
        preset.label.bright_green()
    );

    let default_model = prefill_model.unwrap_or_else(|| preset.default_model.to_string());
    print!("  Model (default {}): ", default_model.bold());
    flush_stdout();
    let model = {
        let line = read_line();
        if line.is_empty() { default_model } else { line }
    };

    let default_base = prefill_base.unwrap_or_else(|| preset.base_url.to_string());
    print!("  Base URL (default {}): ", default_base.dimmed());
    flush_stdout();
    let base_url = {
        let line = read_line();
        if line.is_empty() { default_base } else { line }
    };

    println!(
        "\n  {} {}",
        "API key:".bright_green(),
        preset.key_hint.dimmed()
    );
    let api_key = {
        let opt = prompt_masked("  API key (blank to skip): ")?;
        if opt.is_empty() && preset.key_required {
            let retry = prompt_masked(&format!(
                "  A key is usually required for {}. Enter one (or blank to skip): ",
                preset.id
            ))?;
            if retry.is_empty() { None } else { Some(retry) }
        } else if opt.is_empty() {
            None
        } else {
            Some(opt)
        }
    };

    println!("\n  Summary:");
    println!("    Model:    {}", model.bold());
    println!("    Base URL: {}", base_url.dimmed());
    println!(
        "    API key:  {}",
        if api_key.is_some() {
            "set ✓".green()
        } else {
            "not set (not needed for local providers)".dimmed()
        }
    );
    print!("  Save this configuration? [Y/n] ");
    flush_stdout();
    let confirm = read_line();
    if !confirm.is_empty() && !matches!(confirm.as_str(), "y" | "Y" | "yes" | "YES") {
        println!("  {} Configuration not saved.", "✗".red());
        return Ok(false);
    }

    // Preserve unrelated top-level keys (agent/mcp/security/providers) verbatim.
    let path = config_path();
    let mut root = read_config_value(&path);
    let model_map = as_mapping_mut(&mut root)
        .entry(serde_yaml::Value::from("model"))
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let serde_yaml::Value::Mapping(m) = model_map {
        m.insert(
            serde_yaml::Value::from("default"),
            serde_yaml::Value::from(model.clone()),
        );
        m.insert(
            serde_yaml::Value::from("base_url"),
            serde_yaml::Value::from(base_url.clone()),
        );
        match &api_key {
            Some(key) => {
                m.insert(
                    serde_yaml::Value::from("api_key"),
                    serde_yaml::Value::from(key.clone()),
                );
            }
            None => {
                m.remove(serde_yaml::Value::from("api_key"));
            }
        }
    }

    write_config_value(&root, &path)?;

    if seed_files {
        seed_memory_files()?;
    }

    println!(
        "  {} Saved {}",
        "✓".green(),
        path.display().to_string().bright_white()
    );
    Ok(true)
}

/// First-run interactive setup wizard. Seeds memory files and prints full
/// guidance. Non-interactive stdin returns Ok(false) without writing.
pub fn run_wizard() -> Result<bool> {
    run_flow(None, true, "Setup wizard")
}

/// Interactive edit of an existing config — `enchanter config --edit`.
/// Same flow, prefilled with resolved values; never seeds memory files.
pub fn run_config_edit() -> Result<bool> {
    let config = Config::load()?;
    run_flow(Some(&config), false, "Edit config")
}

// ── non-interactive sets ──

/// Assign a possibly-dotted key (`model`, `base_url`, `api_key`, or
/// `providers.<name>.<field>`) on the root config mapping. A None value
/// removes the key.
fn split_dotted_assign(
    root: &mut serde_yaml::Value,
    key: &str,
    mut value: Option<serde_yaml::Value>,
) -> Result<()> {
    let parts: Vec<&str> = match key {
        "model" => vec!["model", "default"],
        "base_url" => vec!["model", "base_url"],
        "api_key" => vec!["model", "api_key"],
        _ => {
            let p: Vec<&str> = key.split('.').collect();
            if p.len() < 2 || p[0] != "providers" {
                bail!(
                    "Unknown config key '{}' (expected model, base_url, api_key, or providers.<name>.<field>)",
                    key
                );
            }
            p
        }
    };

    let mut node = root;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Consume `value` only on the final component.
            let map = as_mapping_mut(node);
            if let Some(v) = value.take() {
                map.insert(serde_yaml::Value::from(part.to_string()), v);
            } else {
                map.remove(serde_yaml::Value::from(part.to_string()));
            }
        } else {
            let map = as_mapping_mut(node);
            node = map
                .entry(serde_yaml::Value::from(part.to_string()))
                .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
    }
    Ok(())
}

/// Apply `--set KEY=VALUE` pairs to config.yaml and rewrite it. Supported keys:
/// `model`, `base_url`, `api_key` (under the top-level `model:` section), and
/// dotted `providers.<name>.<field>` for named providers. All other top-level
/// keys are preserved verbatim. Empty values remove the key.
pub fn apply_config_sets(pairs: &[(String, String)]) -> Result<()> {
    write_config_sets(&config_path(), pairs)?;

    let cfg = Config::load()?;
    println!(
        "  {} Updated {}",
        "✓".green(),
        config_path().display().to_string().bright_white()
    );
    println!("  Model:      {}", cfg.model_id());
    println!("  Base URL:   {}", cfg.base_url());
    println!(
        "  API key:    {}",
        if cfg.api_key().is_some() {
            "configured ✓".green()
        } else {
            "not set (not needed for local providers)".dimmed()
        }
    );
    Ok(())
}

fn write_config_sets(path: &std::path::Path, pairs: &[(String, String)]) -> Result<()> {
    let mut root = if path.exists() {
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_yaml::from_str::<serde_yaml::Value>(&contents)
            .with_context(|| format!("parsing config YAML from {}", path.display()))?
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    {
        let root_map = as_mapping_mut(&mut root);
        // Only materialize sections that this batch of keys actually touches,
        // so a providers-only --set doesn't leave an empty `model: {}`.
        if pairs
            .iter()
            .any(|(k, _)| matches!(k.as_str(), "model" | "base_url" | "api_key"))
        {
            root_map
                .entry(serde_yaml::Value::from("model"))
                .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        if pairs.iter().any(|(k, _)| k.starts_with("providers.")) {
            root_map
                .entry(serde_yaml::Value::from("providers"))
                .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
    }

    for (key, value) in pairs {
        let target = if value.is_empty() {
            None
        } else {
            // Flat model fields are always strings. Dotted provider fields let
            // numbers/bools through (e.g. context_window) but keep everything
            // else — including ${VAR} references in api_key — as plain text.
            let parsed = serde_yaml::from_str::<serde_yaml::Value>(value);
            let v = match parsed {
                Ok(serde_yaml::Value::Bool(b))
                    if !matches!(key.as_str(), "model" | "base_url" | "api_key") =>
                {
                    serde_yaml::Value::Bool(b)
                }
                Ok(serde_yaml::Value::Number(n))
                    if !matches!(key.as_str(), "model" | "base_url" | "api_key") =>
                {
                    serde_yaml::Value::Number(n)
                }
                _ => serde_yaml::Value::from(value.as_str()),
            };
            Some(v)
        };
        split_dotted_assign(&mut root, key, target)?;
    }

    write_config_value(&root, path)
}

// ── guidance ──

/// Print setup guidance explaining what was created and the next steps.
/// `fresh` is true for first-run.
pub fn print_guidance(fresh: bool) {
    let home_loc = home::enchanter_home();
    if fresh {
        println!(
            "\n  {} Setup complete. Configuration saved to {}\n",
            "✓".green(),
            home_loc.display().to_string().bright_white()
        );
    }
    println!("  {} What was created:", "⟡".bright_magenta());
    println!(
        "    {}/SOUL.md       — your agent's persona & standing instructions. Edit it to shape how the agent behaves; it is loaded into every session's system prompt.",
        home_loc.display()
    );
    println!(
        "    {}/config.yaml   — model, base_url, api_key, named providers, MCP servers. The wizard wrote the minimum; everything else has sensible defaults.",
        home_loc.display()
    );
    println!(
        "    {}/memories/     — MEMORY.md (notes) & USER.md (about you). Entries load into the prompt and old ones summarize automatically.",
        home_loc.display()
    );
    println!("  {} What you can do next:", "↳".dimmed());
    println!(
        "    - Add named providers with: enchanter config --edit   (switch with /model <name> in the REPL)"
    );
    println!(
        "    - Add MCP servers to {}/config.yaml (see README for the schema)",
        home_loc.display()
    );
    println!("    - Review resolved settings any time with: enchanter config");
    println!("    - Check token budget with: enchanter prompt --budget");
    println!("\n  Sample first commands:");
    println!("    enchanter                    # start the interactive REPL");
    println!("    enchanter -p 'hello'         # one-shot prompt, no REPL");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_sane() {
        let ps = presets();
        assert_eq!(ps.len(), 5);
        for p in &ps {
            assert!(!p.id.is_empty());
            assert!(!p.base_url.is_empty());
        }
        assert!(!presets().iter().any(|p| p.id == "local" && p.key_required));
        assert!(presets().iter().any(|p| p.id == "openai" && p.key_required));
        assert!(
            presets()
                .iter()
                .any(|p| p.id == "openrouter" && p.key_required)
        );
    }

    #[test]
    fn is_configured_behavior() {
        let cfg = Config::default();
        assert!(!is_configured(&cfg));

        let mut cfg = Config::default();
        cfg.model.default = Some("gpt-4.1-mini".to_string());
        assert!(!is_configured(&cfg)); // model set, no key, remote URL → not configured

        cfg.model.base_url = Some("http://localhost:11434/v1/chat/completions".to_string());
        assert!(is_configured(&cfg)); // localhost needs no key

        cfg.model.base_url = Some("https://api.openai.com/v1/chat/completions".to_string());
        assert!(!is_configured(&cfg));

        cfg.model.api_key = Some("sk-test".to_string());
        assert!(is_configured(&cfg));
    }

    #[test]
    fn write_minimal_yaml_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        let mut model = serde_yaml::Mapping::new();
        model.insert(
            serde_yaml::Value::from("default"),
            serde_yaml::Value::from("gpt-4.1-mini"),
        );
        model.insert(
            serde_yaml::Value::from("base_url"),
            serde_yaml::Value::from("https://api.openai.com/v1/chat/completions"),
        );
        model.insert(
            serde_yaml::Value::from("api_key"),
            serde_yaml::Value::from("sk-test"),
        );
        let mut root = serde_yaml::Mapping::new();
        root.insert(
            serde_yaml::Value::from("config_version"),
            serde_yaml::Value::from(1u64),
        );
        root.insert(
            serde_yaml::Value::from("model"),
            serde_yaml::Value::Mapping(model),
        );

        write_config_value(&serde_yaml::Value::Mapping(root), &path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("null"));
        assert!(raw.contains("config_version: 1"));

        let cfg: Config = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(cfg.config_version, 1);
        assert_eq!(cfg.model.default.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            cfg.model.base_url.as_deref(),
            Some("https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(cfg.model.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn apply_config_sets_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "config_version: 1\nmodel:\n  default: old-model\nagent:\n  max_turns: 5\n",
        )
        .unwrap();

        write_config_sets(
            &path,
            &[
                ("model".to_string(), "new-model".to_string()),
                ("api_key".to_string(), "sk-new".to_string()),
            ],
        )
        .unwrap();

        let cfg: Config = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.model.default.as_deref(), Some("new-model"));
        assert_eq!(cfg.model.api_key.as_deref(), Some("sk-new"));
        assert_eq!(cfg.agent.max_turns, Some(5)); // unrelated section preserved

        // Dotted providers.<name>.<field> creates the providers map.
        write_config_sets(
            &path,
            &[(
                "providers.groq.model".to_string(),
                "llama-3.3-70b-versatile".to_string(),
            )],
        )
        .unwrap();
        let cfg: Config = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            cfg.providers
                .get("groq")
                .and_then(|p| p.model.clone())
                .as_deref(),
            Some("llama-3.3-70b-versatile")
        );

        // Empty value removes the api_key.
        write_config_sets(&path, &[("api_key".to_string(), String::new())]).unwrap();
        let cfg: Config = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.model.api_key.as_deref(), None);
        assert_eq!(cfg.model.default.as_deref(), Some("new-model"));
    }

    #[test]
    fn seed_memory_files() {
        let dir = tempfile::tempdir().unwrap();
        seed_memory_files_at(dir.path()).unwrap();
        assert!(dir.path().join("MEMORY.md").exists());
        assert!(dir.path().join("USER.md").exists());

        // Existing files are left untouched.
        std::fs::write(dir.path().join("MEMORY.md"), "keep me").unwrap();
        seed_memory_files_at(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap(),
            "keep me"
        );
    }
}
