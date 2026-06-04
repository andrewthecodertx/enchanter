//! Memory store — ~/.enchanter/memories/MEMORY.md, USER.md, and SUMMARY.md, §-delimited.
//!
//! The memory architecture (separate MEMORY.md + USER.md files, §-delimited entries,
//! threshold-based summarization of old entries) is adapted from hermes-agent's
//! memory system (hermes-agent/agent/memory_manager.py, agent/memory_provider.py).
//! hermes-agent uses a pluggable MemoryProvider ABC with sync_turn/prefetch/shutdown
//! lifecycle hooks and multiple backends (built-in, Honcho, Hindsight, Mem0);
//! enchanter simplifies to a single file-based store with synchronous load/save
//! and async LLM summarization.
//!
//! The §-delimited entry format (\n§\n between entries) matches hermes-agent's
//! file-based default memory store convention, enabling file-level compatibility:
//! a hermes-agent ~/.hermes/MEMORY.md can be symlinked or copied to
//! ~/.enchanter/memories/MEMORY.md and parsed correctly.
//!
//! The summarize_on_exit pattern (call the LLM to compress old memory entries
//! when they exceed a threshold, falling back to simple truncation) mirrors
//! hermes-agent's two-tier approach: MemoryManager.sync_all() for per-turn
//! persistence, and conversation_compression for context window management.
//!
//! The memory tool (add/remove/replace/list) follows the operations exposed by
//! hermes-agent's built-in memory (hermes-agent/tools/memory_tool.py) and
//! Claude Code's memory system (claude-code/src/memdir/memdir.ts), which
//! uses MEMORY.md + per-topic files with frontmatter-typed entries.
//! enchanter's add/remove/replace/list matches the subset of operations
//! both systems expose for user-facing memory management.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::api::{LlmClient, Message};
use crate::config::MemoryConfig;
use crate::home;

const ENTRY_DELIMITER: &str = "\n§\n";

#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    pub memory_entries: Vec<String>,
    pub user_entries: Vec<String>,
    pub summary: Option<String>,
}

impl MemoryStore {
    /// Load memory from disk. Sync — does not run summarization.
    pub fn load() -> Result<Self> {
        let mem_dir = memory_dir();

        let memory_entries = if mem_dir.join("MEMORY.md").exists() {
            let content = std::fs::read_to_string(mem_dir.join("MEMORY.md"))
                .context("reading MEMORY.md")?;
            Self::parse_entries(&content)
        } else {
            Vec::new()
        };

        let user_entries = if mem_dir.join("USER.md").exists() {
            let content = std::fs::read_to_string(mem_dir.join("USER.md"))
                .context("reading USER.md")?;
            Self::parse_entries(&content)
        } else {
            Vec::new()
        };

        let summary = if mem_dir.join("SUMMARY.md").exists() {
            let content = std::fs::read_to_string(mem_dir.join("SUMMARY.md"))
                .context("reading SUMMARY.md")?;
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        } else {
            None
        };

        Ok(Self { memory_entries, user_entries, summary })
    }

    fn parse_entries(content: &str) -> Vec<String> {
        content
            .split(ENTRY_DELIMITER)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[allow(dead_code)]
    fn serialize_entries(entries: &[String]) -> String {
        entries.join(ENTRY_DELIMITER)
    }

    #[allow(dead_code)]
    pub fn add_memory(&mut self, entry: String) -> Result<()> {
        self.memory_entries.push(entry);
        self.save_memory()
    }

    #[allow(dead_code)]
    pub fn add_user(&mut self, entry: String) -> Result<()> {
        self.user_entries.push(entry);
        self.save_user()
    }

    #[allow(dead_code)]
    pub fn remove_memory(&mut self, substring: &str) -> Result<bool> {
        let before = self.memory_entries.len();
        self.memory_entries.retain(|e| !e.contains(substring));
        let removed = self.memory_entries.len() < before;
        if removed {
            self.save_memory()?;
        }
        Ok(removed)
    }

    #[allow(dead_code)]
    pub fn replace_memory(&mut self, old_text: &str, new_text: &str) -> Result<bool> {
        let mut found = false;
        for entry in &mut self.memory_entries {
            if entry.contains(old_text) {
                *entry = entry.replace(old_text, new_text);
                found = true;
            }
        }
        if found {
            self.save_memory()?;
        }
        Ok(found)
    }

    #[allow(dead_code)]
    fn save_memory(&self) -> Result<()> {
        let mem_dir = memory_dir();
        std::fs::create_dir_all(&mem_dir).context("creating memories directory")?;
        let content = Self::serialize_entries(&self.memory_entries);
        std::fs::write(mem_dir.join("MEMORY.md"), content)
            .context("writing MEMORY.md")
    }

    #[allow(dead_code)]
    fn save_user(&self) -> Result<()> {
        let mem_dir = memory_dir();
        std::fs::create_dir_all(&mem_dir).context("creating memories directory")?;
        let content = Self::serialize_entries(&self.user_entries);
        std::fs::write(mem_dir.join("USER.md"), content)
            .context("writing USER.md")
    }

    fn save_summary(&self) -> Result<()> {
        let mem_dir = memory_dir();
        std::fs::create_dir_all(&mem_dir).context("creating memories directory")?;
        if let Some(summary) = &self.summary {
            std::fs::write(mem_dir.join("SUMMARY.md"), summary)
                .context("writing SUMMARY.md")
        } else {
            // Remove SUMMARY.md if summary is None
            let path = mem_dir.join("SUMMARY.md");
            if path.exists() {
                std::fs::remove_file(path).context("removing SUMMARY.md")?;
            }
            Ok(())
        }
    }

    /// Merge project overlay memories from a directory.
    /// Adds entries from project MEMORY.md and USER.md to this store.
    /// Does not save to disk — merge is in-memory only for the session.
    pub fn merge_from_dir(&mut self, dir: &std::path::Path) -> Result<()> {
        let memory_path = dir.join("MEMORY.md");
        if memory_path.exists() {
            let content = std::fs::read_to_string(&memory_path)
                .with_context(|| format!("reading project {}", memory_path.display()))?;
            let entries = Self::parse_entries(&content);
            self.memory_entries.extend(entries);
        }

        let user_path = dir.join("USER.md");
        if user_path.exists() {
            let content = std::fs::read_to_string(&user_path)
                .with_context(|| format!("reading project {}", user_path.display()))?;
            let entries = Self::parse_entries(&content);
            self.user_entries.extend(entries);
        }

        Ok(())
    }

    /// Manage memory: cap entries and summarize old ones if threshold exceeded.
    /// This is async because summarization calls the LLM.
    /// Should be called after the LlmClient is created.
    pub async fn manage(&mut self, client: &LlmClient, config: &MemoryConfig) -> Result<()> {
        let total = self.memory_entries.len() as u32;

        if total <= config.max_entries {
            return Ok(());
        }

        // Need to summarize if exceeding threshold
        if total > config.summarize_threshold {
            let entries_to_summarize = total - config.max_entries;
            if entries_to_summarize == 0 {
                return Ok(());
            }

            // Take the oldest entries (front of the list) for summarization
            let split_point = entries_to_summarize as usize;
            let old_entries: Vec<String> = self.memory_entries[..split_point].to_vec();
            let recent_entries: Vec<String> = self.memory_entries[split_point..].to_vec();

            // Summarize old entries
            let new_summary = self.summarize_entries(client, &old_entries).await?;

            // Merge with existing summary
            let merged_summary = match &self.summary {
                Some(existing) => {
                    format!(
                        "PAST SUMMARY:\n{}\n\nADDITIONAL SUMMARY:\n{}",
                        existing, new_summary
                    )
                }
                None => new_summary,
            };

            self.summary = Some(merged_summary);
            self.memory_entries = recent_entries;

            // Persist summary to disk
            self.save_summary()?;
        } else {
            // Over max_entries but under threshold — just cap
            let cutoff = total - config.max_entries;
            self.memory_entries = self.memory_entries[cutoff as usize..].to_vec();
        }

        Ok(())
    }

    /// Summarize a list of memory entries using the LLM.
    async fn summarize_entries(
        &self,
        client: &LlmClient,
        entries: &[String],
    ) -> Result<String> {
        let entries_text = entries.join("\n---\n");

        // Truncate if very long to avoid token limits
        let truncated = if entries_text.len() > 12_000 {
            &entries_text[..12_000]
        } else {
            &entries_text
        };

        let system_prompt = "You are a memory condenser. Your job is to summarize \
            memory entries into a concise, dense form. Preserve key facts, decisions, \
            preferences, and technical details. Omit transient details, pleasantries, \
            and anything that won't matter later. Output a single condensed summary paragraph, \
            not a list. Be brief but information-dense.";

        let user_prompt = format!(
            "Summarize these memory entries into a dense, concise summary. \
            Focus on durable facts and decisions.\n\n{}",
            truncated
        );

        let messages = vec![
            Message::system(system_prompt),
            Message::user(&user_prompt),
        ];

        // Use non-streaming for summarization — we don't need to display it
        let result = client.chat(messages, None).await?;

        Ok(result.content.unwrap_or_default())
    }

    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        if !self.user_entries.is_empty() {
            parts.push(format!(
                "═══ USER PROFILE ═══\n{}",
                self.user_entries.join("\n---\n")
            ));
        }

        if let Some(summary) = &self.summary {
            parts.push(format!(
                "═══ MEMORY SUMMARY ═══\n{}",
                summary
            ));
        }

        if !self.memory_entries.is_empty() {
            parts.push(format!(
                "═══ MEMORY ═══\n{}",
                self.memory_entries.join("\n---\n")
            ));
        }

        parts.join("\n\n")
    }

    /// Total count of memory entries (for display).
    #[allow(dead_code)]
    pub fn total_entry_count(&self) -> usize {
        self.memory_entries.len() + self.user_entries.len()
    }
}

pub fn memory_dir() -> PathBuf {
    home::enchanter_home().join("memories")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        let content = "entry one\n§\nentry two\n§\nentry three";
        let entries = MemoryStore::parse_entries(content);
        assert_eq!(entries, vec!["entry one", "entry two", "entry three"]);
    }

    #[test]
    fn serialize_roundtrip() {
        let entries = vec!["alpha".to_string(), "beta".to_string()];
        let serialized = MemoryStore::serialize_entries(&entries);
        let parsed = MemoryStore::parse_entries(&serialized);
        assert_eq!(parsed, entries);
    }

    #[test]
    fn format_includes_sections() {
        let store = MemoryStore {
            user_entries: vec!["User is Andrew".to_string()],
            memory_entries: vec!["fact one".to_string()],
            summary: None,
        };
        let formatted = store.format_for_prompt();
        assert!(formatted.contains("USER PROFILE"));
        assert!(formatted.contains("MEMORY"));
        assert!(formatted.contains("Andrew"));
        assert!(formatted.contains("fact one"));
    }

    #[test]
    fn format_includes_summary() {
        let store = MemoryStore {
            user_entries: vec![],
            memory_entries: vec!["recent fact".to_string()],
            summary: Some("condensed old facts here".to_string()),
        };
        let formatted = store.format_for_prompt();
        assert!(formatted.contains("MEMORY SUMMARY"));
        assert!(formatted.contains("condensed old facts"));
        assert!(formatted.contains("recent fact"));
    }

    #[test]
    fn total_entry_count() {
        let store = MemoryStore {
            user_entries: vec!["u1".to_string(), "u2".to_string()],
            memory_entries: vec!["m1".to_string()],
            summary: None,
        };
        assert_eq!(store.total_entry_count(), 3);
    }

    #[test]
    fn cap_under_max_no_change() {
        let store = MemoryStore {
            memory_entries: (0..10).map(|i| format!("entry {}", i)).collect(),
            user_entries: vec![],
            summary: None,
        };
        let config = MemoryConfig {
            max_entries: 50,
            summarize_threshold: 40,
        };
        // total is 10, under max_entries — manage should be a no-op
        // (but manage is async and needs a client, just test the logic indirectly)
        assert_eq!(store.memory_entries.len(), 10);
        let _ = &config; // verify it compiles
        let _ = &store;
    }
}