//! Knowledge store — structured key-value facts that persist across sessions.
//!
//! Unlike the memory system (flat narrative text entries), the knowledge store
//! captures discrete, typed facts that the agent learns or is told. Keys are
//! dot-namespaced identifiers (e.g., `project.rust_version`, `user.email`).
//! Values are short strings. Categories group related facts for search.
//!
//! The store is persisted as a single JSON file at
//! `~/.enchanter/knowledge/kstore.json`, making it human-readable,
//! git-friendly, and portable. On load, entries are indexed in a HashMap for
//! O(1) key lookup. On save, the full store is serialized.
//!
//! Design principle: prefer explicit capture (`knowledge store(key, value)`)
//! over inference. The agent should ask once, store the answer, and never ask
//! again. This reduces LLM call count and token waste on repeated questions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::home;

// ── Data model ──

/// Source of a knowledge entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Directly observed from tool output, filesystem, or runtime.
    Observed,
    /// Explicitly told by the user.
    Told,
    /// Inferred by the agent from context.
    Inferred,
}

impl Default for Source {
    fn default() -> Self {
        Self::Told
    }
}

impl Source {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Told => "told",
            Self::Inferred => "inferred",
        }
    }
}

impl std::str::FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "observed" => Ok(Self::Observed),
            "told" => Ok(Self::Told),
            "inferred" => Ok(Self::Inferred),
            other => Err(format!(
                "unknown source '{}'. Use: observed, told, inferred",
                other
            )),
        }
    }
}

/// Category of knowledge for grouping and search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Environment,
    Project,
    Preference,
    Decision,
    Fact,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Project => "project",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::Fact => "fact",
        }
    }
}

impl std::str::FromStr for Category {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "environment" => Ok(Self::Environment),
            "project" => Ok(Self::Project),
            "preference" => Ok(Self::Preference),
            "decision" => Ok(Self::Decision),
            "fact" => Ok(Self::Fact),
            other => Err(format!(
                "unknown category '{}'. Use: environment, project, preference, decision, fact",
                other
            )),
        }
    }
}

/// A single knowledge entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub key: String,
    pub value: String,
    pub category: Category,
    #[serde(default)]
    pub source: Source,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_confidence() -> f32 {
    1.0
}

/// The knowledge store: in-memory HashMap backed by a JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeStore {
    pub entries: HashMap<String, KnowledgeEntry>,
}

impl Default for KnowledgeStore {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl KnowledgeStore {
    /// Load the knowledge store from disk. Returns empty store if file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = kstore_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let store: KnowledgeStore = serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(store)
    }

    /// Save the knowledge store to disk.
    pub fn save(&self) -> Result<()> {
        let dir = knowledge_dir();
        std::fs::create_dir_all(&dir).context("creating knowledge directory")?;
        let path = kstore_path();
        let content = serde_json::to_string_pretty(self).context("serializing knowledge store")?;
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Store a fact. Creates or updates.
    pub fn store(&mut self, key: &str, value: &str, category: Category, source: Source) {
        let now = chrono::Utc::now().to_rfc3339();
        let existing = self.entries.get(key).map(|e| e.created_at.clone());
        let entry = KnowledgeEntry {
            key: key.to_string(),
            value: value.to_string(),
            category,
            source,
            confidence: 1.0,
            created_at: existing.unwrap_or_else(|| now.clone()),
            updated_at: now,
        };
        self.entries.insert(key.to_string(), entry);
    }

    /// Get a fact by exact key.
    pub fn get(&self, key: &str) -> Option<&KnowledgeEntry> {
        self.entries.get(key)
    }

    /// Remove a fact by key. Returns true if it existed.
    pub fn forget(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    /// Search for entries whose keys start with the given prefix.
    pub fn search(&self, prefix: &str) -> Vec<&KnowledgeEntry> {
        let mut results: Vec<&KnowledgeEntry> = self
            .entries
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, v)| v)
            .collect();
        results.sort_by(|a, b| a.key.cmp(&b.key));
        results
    }

    /// List all entries, grouped by category.
    pub fn list_by_category(&self) -> Vec<(String, Vec<&KnowledgeEntry>)> {
        let mut groups: HashMap<String, Vec<&KnowledgeEntry>> = HashMap::new();
        for entry in self.entries.values() {
            groups
                .entry(entry.category.as_str().to_string())
                .or_default()
                .push(entry);
        }
        let mut result: Vec<(String, Vec<&KnowledgeEntry>)> = groups
            .into_iter()
            .map(|(cat, mut entries)| {
                entries.sort_by(|a, b| a.key.cmp(&b.key));
                (cat, entries)
            })
            .collect();
        result.sort_by(|(a, _), (b, _)| a.cmp(b));
        result
    }

    /// Format the knowledge store for injection into the system prompt.
    /// Compact format to minimize token usage.
    pub fn format_for_prompt(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let groups = self.list_by_category();
        let mut lines = Vec::new();

        for (category, entries) in &groups {
            lines.push(format!("[{}]", category));
            for entry in entries {
                lines.push(format!("  {}: {}", entry.key, entry.value));
            }
        }

        lines.join("\n")
    }

    /// Merge entries from a project-level kstore overlay.
    /// Project entries override global entries with the same key.
    /// Returns the number of entries overridden.
    pub fn merge_from_dir(&mut self, dir: &std::path::Path) -> Result<usize> {
        let path = dir.join("kstore.json");
        if !path.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading project {}", path.display()))?;
        let project_store: KnowledgeStore = serde_json::from_str(&content)
            .with_context(|| format!("parsing project {}", path.display()))?;

        let overridden = project_store
            .entries
            .keys()
            .filter(|k| self.entries.contains_key(*k))
            .count();

        for (k, v) in project_store.entries {
            self.entries.insert(k, v);
        }

        Ok(overridden)
    }
}

fn knowledge_dir() -> PathBuf {
    home::enchanter_home().join("knowledge")
}

fn kstore_path() -> PathBuf {
    knowledge_dir().join("kstore.json")
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_get() {
        let mut store = KnowledgeStore::default();
        store.store(
            "project.rust_version",
            "1.85",
            Category::Environment,
            Source::Observed,
        );
        let entry = store.get("project.rust_version").unwrap();
        assert_eq!(entry.value, "1.85");
        assert_eq!(entry.category, Category::Environment);
        assert_eq!(entry.source, Source::Observed);
    }

    #[test]
    fn store_updates_existing() {
        let mut store = KnowledgeStore::default();
        store.store("user.name", "Andrew", Category::Fact, Source::Told);
        assert_eq!(store.get("user.name").unwrap().value, "Andrew");

        store.store("user.name", "Andrew S Erwin", Category::Fact, Source::Told);
        assert_eq!(store.get("user.name").unwrap().value, "Andrew S Erwin");
        // created_at should be preserved
        assert!(!store.get("user.name").unwrap().created_at.is_empty());
    }

    #[test]
    fn forget_removes_entry() {
        let mut store = KnowledgeStore::default();
        store.store("test.key", "value", Category::Fact, Source::Observed);
        assert!(store.forget("test.key"));
        assert!(store.get("test.key").is_none());
        assert!(!store.forget("test.key")); // already removed
    }

    #[test]
    fn search_by_prefix() {
        let mut store = KnowledgeStore::default();
        store.store(
            "project.rust_version",
            "1.85",
            Category::Environment,
            Source::Observed,
        );
        store.store(
            "project.name",
            "enchanter",
            Category::Project,
            Source::Observed,
        );
        store.store("user.name", "Andrew", Category::Fact, Source::Told);

        let results = store.search("project.");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "project.name");
        assert_eq!(results[1].key, "project.rust_version");
    }

    #[test]
    fn list_by_category() {
        let mut store = KnowledgeStore::default();
        store.store(
            "project.rust_version",
            "1.85",
            Category::Environment,
            Source::Observed,
        );
        store.store(
            "project.name",
            "enchanter",
            Category::Project,
            Source::Observed,
        );
        store.store(
            "user.style",
            "minimal comments",
            Category::Preference,
            Source::Told,
        );

        let groups = store.list_by_category();
        assert_eq!(groups.len(), 3); // environment, preference, project
    }

    #[test]
    fn format_for_prompt_compact() {
        let mut store = KnowledgeStore::default();
        store.store(
            "project.rust_version",
            "1.85",
            Category::Environment,
            Source::Observed,
        );
        store.store(
            "user.style",
            "minimal comments",
            Category::Preference,
            Source::Told,
        );

        let formatted = store.format_for_prompt();
        assert!(formatted.contains("[environment]"));
        assert!(formatted.contains("project.rust_version: 1.85"));
        assert!(formatted.contains("[preference]"));
        assert!(formatted.contains("user.style: minimal comments"));
    }

    #[test]
    fn format_empty_store() {
        let store = KnowledgeStore::default();
        assert!(store.format_for_prompt().is_empty());
    }

    #[test]
    fn category_parse() {
        use std::str::FromStr;
        assert_eq!(
            Category::from_str("environment").unwrap(),
            Category::Environment
        );
        assert_eq!(Category::from_str("PROJECT").unwrap(), Category::Project);
        assert_eq!(
            Category::from_str("Preference").unwrap(),
            Category::Preference
        );
        assert!(Category::from_str("unknown").is_err());
    }

    #[test]
    fn source_parse_and_str() {
        use std::str::FromStr;
        assert_eq!(Source::from_str("observed").unwrap(), Source::Observed);
        assert_eq!(Source::from_str("TOLD").unwrap(), Source::Told);
        assert_eq!(Source::from_str("Inferred").unwrap(), Source::Inferred);
        assert!(Source::from_str("guessed").is_err());

        assert_eq!(Source::Observed.as_str(), "observed");
        assert_eq!(Source::Told.as_str(), "told");
        assert_eq!(Source::Inferred.as_str(), "inferred");
    }

    #[test]
    fn merge_from_dir() {
        let mut global = KnowledgeStore::default();
        global.store(
            "project.rust_version",
            "1.85",
            Category::Environment,
            Source::Observed,
        );
        global.store("user.name", "Andrew", Category::Fact, Source::Told);

        let dir = tempfile::tempdir().unwrap();
        let mut project = KnowledgeStore::default();
        project.store(
            "project.rust_version",
            "1.86",
            Category::Environment,
            Source::Observed,
        );
        project.store("project.custom", "value", Category::Project, Source::Told);
        let json = serde_json::to_string_pretty(&project).unwrap();
        std::fs::write(dir.path().join("kstore.json"), json).unwrap();

        let overridden = global.merge_from_dir(dir.path()).unwrap();
        assert_eq!(overridden, 1); // project.rust_version was overridden
        assert_eq!(global.get("project.rust_version").unwrap().value, "1.86");
        assert_eq!(global.get("user.name").unwrap().value, "Andrew");
        assert_eq!(global.entries.len(), 3);
    }
}
