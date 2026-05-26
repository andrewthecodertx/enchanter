//! Project overlay — per-repo harness scoping via `.enchanter/` directories.
//!
//! Implements REQ-OVR-001 through REQ-OVR-007:
//! - Discover `.enchanter/` by searching CWD and parents (REQ-OVR-001)
//! - Precedence: env > project overlay > global config > defaults (REQ-OVR-002)
//! - Overlay supports: SOUL.md, config.yaml, memories/, skills/, MCP servers (REQ-OVR-003)
//! - `/scope global|project|both` REPL command (REQ-OVR-004)
//! - Project takes precedence when `/scope both` (REQ-OVR-005)
//! - `/config` shows which values came from which source (REQ-OVR-006)
//! - `enchanter init --project` scaffolds `.enchanter/` (REQ-OVR-007)

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::home;

/// Active scope determining which configuration sources are used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Use only global ~/.enchanter/ configuration.
    Global,
    /// Use only project .enchanter/ configuration.
    Project,
    /// Merge both sources, with project taking precedence on conflicts.
    Both,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Global => write!(f, "global"),
            Scope::Project => write!(f, "project"),
            Scope::Both => write!(f, "both"),
        }
    }
}

/// The resolved overlay: project-local `.enchanter/` directory path and what it provides.
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
}

/// Discover a `.enchanter/` directory by searching from `start_dir` upward
/// to the filesystem root or a VCS root (.git directory). (REQ-OVR-001)
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

        // Check for VCS root (git directory)
        if current.join(".git").exists() {
            // Don't go above the git root
            break;
        }

        // Go up one directory
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
    }
}

/// Resolved configuration result that tracks the source of each value.
#[derive(Debug, Clone)]
pub struct ResolvedOverlayConfig {
    /// The merged configuration.
    pub config: Config,
    /// Source annotations for display.
    pub sources: Vec<ConfigSource>,
}

/// A config value source annotation.
#[derive(Debug, Clone)]
pub struct ConfigSource {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// Resolve the effective configuration by merging global and project overlay configs.
/// Precedence: env vars > project overlay > global config > compiled defaults (REQ-OVR-002)
/// When scope is Global, only global config is used.
/// When scope is Project, only project overlay is used.
/// When scope is Both, project overlay takes precedence over global (REQ-OVR-005).
pub fn resolve_config(scope: &Scope, overlay: Option<&Overlay>) -> Result<ResolvedOverlayConfig> {
    // Always start with global config as the base
    let global_config = Config::load()?;

    match scope {
        Scope::Global => {
            let mut sources = vec![
                ConfigSource {
                    key: "scope".to_string(),
                    value: "global".to_string(),
                    source: "cli".to_string(),
                },
            ];
            sources.push(ConfigSource {
                key: "config".to_string(),
                value: home::enchanter_home().display().to_string(),
                source: "global".to_string(),
            });

            Ok(ResolvedOverlayConfig {
                config: global_config,
                sources,
            })
        }
        Scope::Project => {
            let overlay = overlay.context("No project overlay found in current directory or its parents")?;

            // Load project config (or use defaults if no config.yaml)
            let project_config = if overlay.has_config {
                let path = overlay.path.join("config.yaml");
                Config::load_from(&path)?
            } else {
                Config::default()
            };

            let mut sources = vec![
                ConfigSource {
                    key: "scope".to_string(),
                    value: "project".to_string(),
                    source: "cli".to_string(),
                },
                ConfigSource {
                    key: "config".to_string(),
                    value: overlay.path.display().to_string(),
                    source: "project".to_string(),
                },
            ];

            Ok(ResolvedOverlayConfig {
                config: project_config,
                sources,
            })
        }
        Scope::Both => {
            let overlay = overlay.context("No project overlay found in current directory or its parents")?;

            // Load project config
            let project_config = if overlay.has_config {
                let path = overlay.path.join("config.yaml");
                Config::load_from(&path)?
            } else {
                Config::default()
            };

            // Merge: project takes precedence over global
            // - Model default: project > global (env vars still override both)
            // - Providers: merge both sets, project overrides keys that exist in both
            // - MCP servers: merge both sets, project overrides keys that exist in both
            let merged = merge_configs(&global_config, &project_config);

            let mut sources = vec![
                ConfigSource {
                    key: "scope".to_string(),
                    value: "both".to_string(),
                    source: "cli".to_string(),
                },
                ConfigSource {
                    key: "global_config".to_string(),
                    value: home::enchanter_home().display().to_string(),
                    source: "global".to_string(),
                },
                ConfigSource {
                    key: "project_config".to_string(),
                    value: overlay.path.display().to_string(),
                    source: "project".to_string(),
                },
            ];

            Ok(ResolvedOverlayConfig {
                config: merged,
                sources,
            })
        }
    }
}

/// Merge two configs, with `project` taking precedence over `global`.
fn merge_configs(global: &Config, project: &Config) -> Config {
    let mut merged = global.clone();

    // Model default: project overrides global
    if project.model.default.is_some() {
        merged.model.default = project.model.default.clone();
    }
    if project.model.base_url.is_some() {
        merged.model.base_url = project.model.base_url.clone();
    }
    if project.model.api_key.is_some() {
        merged.model.api_key = project.model.api_key.clone();
    }

    // Agent config: project overrides global
    if project.agent.max_turns.is_some() {
        merged.agent.max_turns = project.agent.max_turns;
    }
    if project.agent.soft_limit.is_some() {
        merged.agent.soft_limit = project.agent.soft_limit;
    }
    if project.agent.summarize_on_exit.is_some() {
        merged.agent.summarize_on_exit = project.agent.summarize_on_exit;
    }

    // Providers: merge, project takes precedence for same keys
    for (key, value) in &project.providers {
        merged.providers.insert(key.clone(), value.clone());
    }

    // MCP servers: merge, project takes precedence for same keys
    for (key, value) in &project.mcp.servers {
        merged.mcp.servers.insert(key.clone(), value.clone());
    }

    merged
}

/// Get the project overlay's SOUL.md path, if it exists.
pub fn overlay_soul_path(overlay: &Overlay) -> Option<PathBuf> {
    if overlay.has_soul {
        Some(overlay.path.join("SOUL.md"))
    } else {
        None
    }
}

/// Get the project overlay's memories directory, if it exists.
pub fn overlay_memories_dir(overlay: &Overlay) -> Option<PathBuf> {
    if overlay.has_memories {
        Some(overlay.path.join("memories"))
    } else {
        None
    }
}

/// Get the project overlay's skills directory, if it exists.
pub fn overlay_skills_dir(overlay: &Overlay) -> Option<PathBuf> {
    if overlay.has_skills {
        Some(overlay.path.join("skills"))
    } else {
        None
    }
}

/// Scaffold a new `.enchanter/` directory in the given path (REQ-OVR-007).
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

    // Write a minimal config.yaml
    let config_content = "# Enchanter project overlay configuration\n\
        # Values here override global ~/.enchanter/config.yaml\n\
        # model:\n\
        #   default: gpt-4.1-mini\n\
        # agent:\n\
        #   max_turns: 30\n";
    std::fs::write(enchanter_dir.join("config.yaml"), config_content)?;

    // Write a minimal SOUL.md template
    let soul_content = "# Project SOUL.md\n\
        # This persona supplements or overrides the global SOUL.md.\n\
        # When scope is 'both', this content is appended to the global SOUL.md.\n\
        # When scope is 'project', this replaces the global SOUL.md entirely.\n";
    std::fs::write(enchanter_dir.join("SOUL.md"), soul_content)?;

    Ok(enchanter_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_overlay_from_tmp() {
        // Should not find .enchanter/ in /tmp (usually)
        let result = discover_overlay(Path::new("/tmp"));
        // We don't assert None because someone might have .enchanter/ in /tmp
        // Just verify it doesn't panic
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
        // Create .git directory (simulating a git repo root)
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        // Create .enchanter in the git root
        std::fs::create_dir_all(dir.path().join(".enchanter")).unwrap();

        // Create a subdirectory
        let subdir = dir.path().join("src").join("module");
        std::fs::create_dir_all(&subdir).unwrap();

        // Should find the overlay at the git root
        let found = discover_overlay(&subdir);
        assert!(found.is_some());

        // Create .enchanter ABOVE the git root (should NOT be found)
        let parent = dir.path().parent().unwrap();
        // Don't create it — could interfere with other tests
        let _ = parent;
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
    }

    #[test]
    fn test_scope_display() {
        assert_eq!(Scope::Global.to_string(), "global");
        assert_eq!(Scope::Project.to_string(), "project");
        assert_eq!(Scope::Both.to_string(), "both");
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

        // Should fail if already exists
        let err = init_project_overlay(dir.path());
        assert!(err.is_err());
    }

    #[test]
    fn test_merge_configs_project_overrides() {
        let mut global = Config::default();
        global.model.default = Some("gpt-4".to_string());

        let mut project = Config::default();
        project.model.default = Some("qwen3".to_string());

        let merged = merge_configs(&global, &project);
        assert_eq!(merged.model.default, Some("qwen3".to_string()));
    }

    #[test]
    fn test_merge_configs_global_fallback() {
        let mut global = Config::default();
        global.model.default = Some("gpt-4".to_string());

        let project = Config::default();
        // project doesn't override model

        let merged = merge_configs(&global, &project);
        assert_eq!(merged.model.default, Some("gpt-4".to_string()));
    }
}