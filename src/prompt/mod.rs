//! System prompt assembly — SOUL, context, and volatile tiers.

use crate::config::Config;
use crate::memory::MemoryStore;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use chrono::Local;

pub fn build_system_prompt(
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
    config: &Config,
) -> String {
    let mut sections = Vec::new();

    // SOUL
    sections.push(soul.content.clone());

    // CONTEXT
    sections.push(build_environment_block(config));

    if !skills.skills.is_empty() {
        sections.push(skills.format_index_for_prompt());
    }

    sections.push(String::from(
        "You have tools available. Use them to take action — do not describe what you \
         would do without actually doing it. Every response should either (a) contain tool \
         calls that make progress, or (b) deliver a final result to the user. \
         Prefer tool calls over describing steps.\n\
         \n\
         Canonical tools:\n\
         - exec_command: run a shell command (builds, tests, git, package managers)\n\
         - read_file: read a file's contents (with line offset/limit)\n\
         - write_file: write content to a file (creates parents, overwrites)\n\
         - list_directory: list directory entries (names, types, sizes)\n\
         \n\
         MCP tools may also be available for specialty operations (image generation, \
         GitHub, etc.). Use them when relevant."
    ));

    // VOLATILE
    let memory_block = memory.format_for_prompt();
    if !memory_block.is_empty() {
        sections.push(memory_block);
    }

    let now = Local::now();
    sections.push(format!(
        "Current date: {} | Session started: {}",
        now.format("%Y-%m-%d"),
        now.format("%Y-%m-%d %H:%M %Z")
    ));

    sections.join("\n\n")
}

fn build_environment_block(config: &Config) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Model: {}", config.model_id()));

    if let Ok(username) = std::env::var("USER") {
        lines.push(format!("User: {}", username));
    }

    if let Ok(cwd) = std::env::var("PWD") {
        lines.push(format!("Working directory: {}", cwd));
    }

    lines.push(format!(
        "Host: {}",
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_else(|_| "unknown".to_string())
    ));

    lines.push(format!(
        "Platform: {}",
        if cfg!(target_os = "linux") {
            "Linux"
        } else if cfg!(target_os = "macos") {
            "macOS"
        } else {
            "unknown"
        }
    ));

    format!("═══ ENVIRONMENT ═══\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soul::Soul;
    use std::path::PathBuf;

    #[test]
    fn prompt_contains_soul() {
        let soul = Soul {
            content: "I am Enchanter.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore::default();
        let skills = SkillsIndex::default();
        let config = Config::default();

        let prompt = build_system_prompt(&soul, &memory, &skills, &config);
        assert!(prompt.contains("I am Enchanter."));
        assert!(prompt.contains("ENVIRONMENT"));
    }

    #[test]
    fn prompt_includes_memory() {
        let soul = Soul {
            content: "I am a test.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore {
            memory_entries: vec!["project uses rust".to_string()],
            user_entries: vec!["User is Andrew".to_string()],
        };
        let skills = SkillsIndex::default();
        let config = Config::default();

        let prompt = build_system_prompt(&soul, &memory, &skills, &config);
        assert!(prompt.contains("project uses rust"));
        assert!(prompt.contains("Andrew"));
    }
}