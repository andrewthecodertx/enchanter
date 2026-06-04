//! System prompt assembly — SOUL, context, and volatile tiers.
//!
//! The tiered prompt structure builds the system prompt in layers:
//!   1. SOUL — persona definition from ~/.enchanter/SOUL.md
//!   2. CONTEXT — environment block (model, user, cwd, host, platform)
//!   3. SKILLS — discovered skills index
//!   4. INSTRUCTIONS — tool usage guidance
//!   5. VOLATILE — memory entries, user profile, session timestamp
//!
//! This layered assembly follows the pattern established by hermes-agent's
//! prompt_builder.py (hermes-agent/agent/prompt_builder.py), which assembles
//! system prompts as: identity slot → AGENTS.md/context files → skills index →
//! memory → environment hints → tool enforcement. enchanter simplifies this
//! by removing the injection-scanning and .hermes.md discovery, keeping the
//! core tier structure.
//!
//! OpenCode uses a similar pattern (opencode/packages/opencode/src/session/system.ts):
//! provider-specific base prompt → environment block → skills → tool instructions.
//! The environment block format ("Model: X, Working directory: Y, Platform: Z")
//! is adapted from OpenCode's system.ts environment() function.
//!
//! The "═══ SECTION ═══" delimiter style for sections and subsections is adapted
//! from hermes-agent's section formatting convention
//! (hermes-agent/agent/prompt_builder.py), which uses the same double-line
//! section markers for memory blocks and context files.

pub mod inspect;

use crate::config::Config;
use crate::memory::MemoryStore;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use chrono::Local;

/// Build system prompt using the default model from config.
#[allow(dead_code)]
pub fn build_system_prompt(
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
    config: &Config,
) -> String {
    build_system_prompt_with_model(soul, memory, skills, config, &config.model_id())
}

/// Build system prompt with an explicit model name (used after /model switching).
pub fn build_system_prompt_with_model(
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
    config: &Config,
    model: &str,
) -> String {
    build_prompt_layers(soul, memory, skills, config, model).assemble()
}

/// Build structured prompt layers for inspection (diff & budget).
/// This is the canonical way to produce a PromptLayers — all prompt assembly
/// flows through here, ensuring the layers are always consistent with what
/// the model actually receives.
pub fn build_prompt_layers(
    soul: &Soul,
    memory: &MemoryStore,
    skills: &SkillsIndex,
    config: &Config,
    model: &str,
) -> inspect::PromptLayers {
    use inspect::PromptLayer;

    let mut layers = Vec::new();

    // SOUL
    layers.push(PromptLayer {
        name: "SOUL".to_string(),
        content: soul.content.clone(),
    });

    // CONTEXT
    layers.push(PromptLayer {
        name: "CONTEXT".to_string(),
        content: build_environment_block(config, model),
    });

    // SKILLS
    if !skills.skills.is_empty() {
        layers.push(PromptLayer {
            name: "SKILLS".to_string(),
            content: skills.format_index_for_prompt(),
        });
    }

    // INSTRUCTIONS
    layers.push(PromptLayer {
        name: "INSTRUCTIONS".to_string(),
        content: String::from(
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
             GitHub, etc.). Use them when relevant.",
        ),
    });

    // VOLATILE
    let memory_block = memory.format_for_prompt();
    if !memory_block.is_empty() {
        layers.push(PromptLayer {
            name: "VOLATILE".to_string(),
            content: memory_block,
        });
    }

    // Timestamp
    let now = Local::now();
    layers.push(PromptLayer {
        name: "SESSION".to_string(),
        content: format!(
            "Current date: {} | Session started: {}",
            now.format("%Y-%m-%d"),
            now.format("%Y-%m-%d %H:%M %Z")
        ),
    });

    inspect::PromptLayers { layers }
}

fn build_environment_block(_config: &Config, model: &str) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Model: {}", model));

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
        } else if cfg!(target_os = "windows") {
            "Windows"
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
            summary: None,
        };
        let skills = SkillsIndex::default();
        let config = Config::default();

        let prompt = build_system_prompt(&soul, &memory, &skills, &config);
        assert!(prompt.contains("project uses rust"));
        assert!(prompt.contains("Andrew"));
    }

    #[test]
    fn prompt_with_model_override() {
        let soul = Soul {
            content: "I am Test.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore::default();
        let skills = SkillsIndex::default();
        let config = Config::default();

        let prompt = build_system_prompt_with_model(&soul, &memory, &skills, &config, "qwen3");
        assert!(prompt.contains("Model: qwen3"));
    }

    #[test]
    fn build_prompt_layers_has_expected_structure() {
        let soul = Soul {
            content: "Test SOUL.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore::default();
        let skills = SkillsIndex::default();
        let config = Config::default();

        let layers = build_prompt_layers(&soul, &memory, &skills, &config, "test-model");
        let names: Vec<&str> = layers.layers.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"SOUL"));
        assert!(names.contains(&"CONTEXT"));
        assert!(names.contains(&"INSTRUCTIONS"));
        assert!(names.contains(&"SESSION"));
        // No VOLATILE when memory is empty
        assert!(!names.contains(&"VOLATILE"));
    }

    #[test]
    fn build_prompt_layers_includes_memory() {
        let soul = Soul {
            content: "Test SOUL.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore {
            memory_entries: vec!["some fact".to_string()],
            user_entries: vec![],
            summary: None,
        };
        let skills = SkillsIndex::default();
        let config = Config::default();

        let layers = build_prompt_layers(&soul, &memory, &skills, &config, "test-model");
        let names: Vec<&str> = layers.layers.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"VOLATILE"));
    }

    #[test]
    fn assembled_prompt_matches_old_format() {
        let soul = Soul {
            content: "I am Bot.".to_string(),
            source: PathBuf::from("<test>"),
        };
        let memory = MemoryStore {
            memory_entries: vec!["key fact".to_string()],
            user_entries: vec!["user pref".to_string()],
            summary: None,
        };
        let skills = SkillsIndex::default();
        let config = Config::default();

        let old_prompt = build_system_prompt(&soul, &memory, &skills, &config);
        let layers = build_prompt_layers(&soul, &memory, &skills, &config, &config.model_id());
        let new_prompt = layers.assemble();

        // Both should produce identical output
        assert_eq!(old_prompt, new_prompt);
    }
}
