//! Memory store — ~/enchanter/memories/MEMORY.md and USER.md, §-delimited.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::home;

const ENTRY_DELIMITER: &str = "\n§\n";

#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    pub memory_entries: Vec<String>,
    pub user_entries: Vec<String>,
}

impl MemoryStore {
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

        Ok(Self { memory_entries, user_entries })
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

    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        if !self.user_entries.is_empty() {
            parts.push(format!(
                "═══ USER PROFILE ═══\n{}",
                self.user_entries.join("\n---\n")
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
        };
        let formatted = store.format_for_prompt();
        assert!(formatted.contains("USER PROFILE"));
        assert!(formatted.contains("MEMORY"));
        assert!(formatted.contains("Andrew"));
        assert!(formatted.contains("fact one"));
    }
}