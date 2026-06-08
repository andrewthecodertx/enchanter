//! Project overlay — per-repo additive layering via `.enchanter/` directories.
//!
//! Global `~/.enchanter/` is the source of truth. Project `.enchanter/` layers
//! on top: it never replaces or overrides global settings, only supplements.
//!
//! - Config: global is authoritative; project overlays add MCP servers and providers
//! - SOUL: global SOUL.md always loads; project SOUL.md is appended as context
//! - Memories: global memories always load; project memories merge in as additional entries
//! - Skills: global skills always discovered; project skills added to the index

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;

/// Discovered project overlay: path to `.enchanter/` and what it provides.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// Path to the discovered `.enchanter/` directory.
    pub path: PathBuf,
    /// Whether a project-level SOUL.md exists.
    pub has_soul: bool,
    /// Whether a project-level config.yaml exists.
    pub has_config: bool,
    /// Whether a project-level memories/ directory exists.
    pub has_memories: bool,
    /// Whether a project-level skills/ directory exists.
    pub has_skills: bool,
    /// Whether a project-level knowledge/ directory exists.
    pub has_knowledge: bool,
}

/// Discover a `.enchanter/` directory by searching from `start_dir` upward
/// to the filesystem root or a VCS root (.git directory).
pub fn discover_overlay(start_dir: &Path) -> Option<PathBuf> {
    let mut current = if start_dir.is_absolute() {
        start_dir.to_path_buf()
    } else {
        std::env::current_dir().ok()?
    };

    loop {
        let candidate = current.join(".enchanter");
        if candidate.is_dir() {
            return Some(candidate);
        }

        // Don't search above a git root
        if current.join(".git").exists() {
            break;
        }

        if !current.pop() {
            break;
        }
    }

    None
}

/// Analyze an overlay directory to determine what it provides.
pub fn analyze_overlay(path: &Path) -> Overlay {
    Overlay {
        path: path.to_path_buf(),
        has_soul: path.join("SOUL.md").exists(),
        has_config: path.join("config.yaml").exists(),
        has_memories: path.join("memories").is_dir(),
        has_skills: path.join("skills").is_dir(),
        has_knowledge: path.join("knowledge").is_dir(),
    }
}

/// Merge project overlay config into global config.
/// Global values always win; project only adds (MCP servers, providers) that don't exist in global.
pub fn merge_configs(global: &Config, project: &Config) -> Config {
    let mut merged = global.clone();

    // Providers: add any project providers that global doesn't have
    for (key, value) in &project.providers {
        if !merged.providers.contains_key(key) {
            merged.providers.insert(key.clone(), value.clone());
        }
    }

    // MCP servers: add any project servers that global doesn't have
    for (key, value) in &project.mcp.servers {
        if !merged.mcp.servers.contains_key(key) {
            merged.mcp.servers.insert(key.clone(), value.clone());
        }
    }

    merged
}

/// Load config: global first, then layer project overlay on top (additive only).
pub fn load_config(overlay: Option<&Overlay>) -> Result<Config> {
    let global_config = Config::load()?;

    if let Some(ov) = overlay
        && ov.has_config
    {
        let path = ov.path.join("config.yaml");
        let project_config = Config::load_from(&path)?;
        return Ok(merge_configs(&global_config, &project_config));
    }

    Ok(global_config)
}

/// Load SOUL: global always, project appended as additional context.
pub fn load_soul(overlay: Option<&Overlay>) -> Result<crate::soul::Soul> {
    let mut soul = crate::soul::Soul::load_or_fallback()?;

    if let Some(ov) = overlay
        && ov.has_soul
    {
        let project_soul_path = ov.path.join("SOUL.md");
        if let Ok(content) = std::fs::read_to_string(&project_soul_path) {
            soul.content.push_str("\n\n");
            soul.content.push_str(&content);
        }
    }

    Ok(soul)
}

/// Load memories: global always, project merged in as additional entries.
pub fn load_memories(overlay: Option<&Overlay>) -> Result<crate::memory::MemoryStore> {
    let mut memory = crate::memory::MemoryStore::load()?;

    if let Some(ov) = overlay
        && ov.has_memories
    {
        let project_mem_dir = ov.path.join("memories");
        memory.merge_from_dir(&project_mem_dir)?;
    }

    Ok(memory)
}

/// Load knowledge store: global always, project entries overlay on top.
/// Project entries override global entries with the same key.
pub fn load_knowledge(overlay: Option<&Overlay>) -> Result<crate::kstore::KnowledgeStore> {
    let mut kstore = crate::kstore::KnowledgeStore::load()?;

    if let Some(ov) = overlay
        && ov.has_knowledge
    {
        let project_knowledge_dir = ov.path.join("knowledge");
        kstore.merge_from_dir(&project_knowledge_dir)?;
    }

    Ok(kstore)
}

/// Discover skills: global always, project added to the index.
pub fn discover_skills(overlay: Option<&Overlay>) -> Result<crate::skills::SkillsIndex> {
    let mut skills = crate::skills::SkillsIndex::discover()?;

    if let Some(ov) = overlay
        && ov.has_skills
    {
        let project_skills_dir = ov.path.join("skills");
        skills.merge_from_dir(&project_skills_dir)?;
    }

    Ok(skills)
}

/// Scaffold a new `.enchanter/` directory in the given project root.
pub fn init_project_overlay(dir: &Path) -> Result<PathBuf> {
    let enchanter_dir = dir.join(".enchanter");

    if enchanter_dir.exists() {
        anyhow::bail!(
            ".enchanter/ directory already exists at {}",
            enchanter_dir.display()
        );
    }

    std::fs::create_dir_all(&enchanter_dir)
        .with_context(|| format!("creating .enchanter/ at {}", enchanter_dir.display()))?;
    std::fs::create_dir_all(enchanter_dir.join("memories"))
        .with_context(|| "creating memories/ directory")?;
    std::fs::create_dir_all(enchanter_dir.join("skills"))
        .with_context(|| "creating skills/ directory")?;
    std::fs::create_dir_all(enchanter_dir.join("knowledge"))
        .with_context(|| "creating knowledge/ directory")?;

    let config_content = "# Enchanter project overlay configuration\n\
        # Project config is additive: it only adds new providers/MCP servers\n\
        # that global config doesn't already define. Global always wins.\n\
        # providers:\n\
        #   my-project-provider:\n\
        #     model: gpt-4.1-mini\n\
        # mcp:\n\
        #   servers:\n\
        #     my-project-server:\n\
        #       command: npx\n\
        #       args: [\"-y\", \"some-mcp-server\"]\n";
    std::fs::write(enchanter_dir.join("config.yaml"), config_content)?;

    let soul_content = "# Project SOUL.md\n\
        # This content is appended to your global SOUL.md as additional context.\n\
        # Global SOUL.md always takes precedence.\n";
    std::fs::write(enchanter_dir.join("SOUL.md"), soul_content)?;

    Ok(enchanter_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_overlay_from_tmp() {
        let result = discover_overlay(Path::new("/tmp"));
        let _ = result;
    }

    #[test]
    fn test_discover_overlay_with_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        let enchanter_dir = dir.path().join(".enchanter");
        std::fs::create_dir_all(&enchanter_dir).unwrap();

        let found = discover_overlay(dir.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), enchanter_dir);
    }

    #[test]
    fn test_discover_overlay_stops_at_git_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".enchanter")).unwrap();

        let subdir = dir.path().join("src").join("module");
        std::fs::create_dir_all(&subdir).unwrap();

        let found = discover_overlay(&subdir);
        assert!(found.is_some());
    }

    #[test]
    fn test_analyze_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let enchanter_dir = dir.path().join(".enchanter");
        std::fs::create_dir_all(&enchanter_dir).unwrap();
        std::fs::write(enchanter_dir.join("SOUL.md"), "# Test").unwrap();
        std::fs::create_dir_all(enchanter_dir.join("memories")).unwrap();

        let overlay = analyze_overlay(&enchanter_dir);
        assert!(overlay.has_soul);
        assert!(!overlay.has_config);
        assert!(overlay.has_memories);
        assert!(!overlay.has_skills);
        assert!(!overlay.has_knowledge);
    }

    #[test]
    fn test_init_project_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let result = init_project_overlay(dir.path()).unwrap();
        assert!(result.exists());
        assert!(result.join("config.yaml").exists());
        assert!(result.join("SOUL.md").exists());
        assert!(result.join("memories").is_dir());
        assert!(result.join("skills").is_dir());
        assert!(result.join("knowledge").is_dir());

        let err = init_project_overlay(dir.path());
        assert!(err.is_err());
    }

    #[test]
    fn test_merge_configs_additive_only() {
        let mut global = Config::default();
        global.model.default = Some("gpt-4".to_string());
        global.providers.insert(
            "global-provider".to_string(),
            crate::config::ProviderConfig {
                model: Some("gpt-4".to_string()),
                base_url: None,
                api_key: None,
            },
        );

        let mut project = Config::default();
        project.providers.insert(
            "project-provider".to_string(),
            crate::config::ProviderConfig {
                model: Some("claude-sonnet-4".to_string()),
                base_url: None,
                api_key: None,
            },
        );

        let merged = merge_configs(&global, &project);
        // Global model wins (not overridden)
        assert_eq!(merged.model.default, Some("gpt-4".to_string()));
        // Global provider preserved
        assert!(merged.providers.contains_key("global-provider"));
        // Project provider added
        assert!(merged.providers.contains_key("project-provider"));
    }

    #[test]
    fn test_merge_configs_global_wins_on_conflict() {
        let mut global = Config::default();
        global.providers.insert(
            "shared-provider".to_string(),
            crate::config::ProviderConfig {
                model: Some("gpt-4".to_string()),
                base_url: None,
                api_key: None,
            },
        );

        let mut project = Config::default();
        project.providers.insert(
            "shared-provider".to_string(),
            crate::config::ProviderConfig {
                model: Some("claude-sonnet-4".to_string()),
                base_url: None,
                api_key: None,
            },
        );

        let merged = merge_configs(&global, &project);
        // Global value wins — project does NOT override
        let provider = merged.providers.get("shared-provider").unwrap();
        assert_eq!(provider.model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_analyze_overlay_with_knowledge() {
        let dir = tempfile::tempdir().unwrap();
        let enchanter_dir = dir.path().join(".enchanter");
        std::fs::create_dir_all(&enchanter_dir).unwrap();
        std::fs::create_dir_all(enchanter_dir.join("knowledge")).unwrap();

        let overlay = analyze_overlay(&enchanter_dir);
        assert!(overlay.has_knowledge);
    }

    #[test]
    fn test_load_knowledge_without_overlay() {
        // Should load global store without error when no overlay
        let result = load_knowledge(None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_load_knowledge_with_overlay_merges() {
        use crate::kstore::{Category, KnowledgeStore, Source};

        let dir = tempfile::tempdir().unwrap();
        let enchanter_dir = dir.path().join(".enchanter");
        std::fs::create_dir_all(enchanter_dir.join("knowledge")).unwrap();

        // Write a project-level kstore.json
        let mut project_store = KnowledgeStore::default();
        project_store.store("project.test_key", "project_value", Category::Project, Source::Told);
        let json = serde_json::to_string_pretty(&project_store).unwrap();
        std::fs::write(enchanter_dir.join("knowledge").join("kstore.json"), json).unwrap();

        let overlay = Overlay {
            path: enchanter_dir.clone(),
            has_soul: false,
            has_config: false,
            has_memories: false,
            has_skills: false,
            has_knowledge: true,
        };

        let merged = load_knowledge(Some(&overlay)).unwrap();
        let entry = merged.get("project.test_key").unwrap();
        assert_eq!(entry.value, "project_value");
    }

    #[test]
    fn test_knowledge_overlay_overrides_global() {
        use crate::kstore::{Category, KnowledgeStore, Source};

        let dir = tempfile::tempdir().unwrap();
        let project_knowledge_dir = dir.path().join("knowledge");
        std::fs::create_dir_all(&project_knowledge_dir).unwrap();

        let mut project_store = KnowledgeStore::default();
        project_store.store("environment.rust_version", "1.85-project", Category::Environment, Source::Told);
        let json = serde_json::to_string_pretty(&project_store).unwrap();
        std::fs::write(project_knowledge_dir.join("kstore.json"), json).unwrap();

        // Create a global store with the same key but different value
        let mut global_store = KnowledgeStore::default();
        global_store.store("environment.rust_version", "1.84", Category::Environment, Source::Observed);
        global_store.store("environment.os", "linux", Category::Environment, Source::Observed);

        // Merge project into global — project key should override
        let overridden = global_store.merge_from_dir(&project_knowledge_dir).unwrap();
        assert_eq!(overridden, 1); // 1 entry was overridden
        assert_eq!(global_store.get("environment.rust_version").unwrap().value, "1.85-project");
        assert_eq!(global_store.get("environment.os").unwrap().value, "linux"); // non-conflicting preserved
    }
}
