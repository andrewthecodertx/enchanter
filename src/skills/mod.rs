//! Skill discovery — scans ~/.enchanter/skills/ for SKILL.md files.
//!
//! The SKILL.md discovery pattern (walking directories for SKILL.md files,
//! parsing YAML frontmatter for name/description, inferring category from
//! directory path) is adapted from two sources:
//!
//! - hermes-agent's skill system (hermes-agent/agent/skill_utils.py):
//!   Walks skill directories, parses YAML frontmatter using ConfigMarkdown,
//!   extracts name/description/version, and builds a skills index for the
//!   prompt. hermes-agent also has skill conditions, platform filtering,
//!   and skill bundles; enchanter implements only the discovery and
//!   frontmatter parsing subset.
//!
//! - OpenCode's skill system (opencode/packages/opencode/src/skill/skill.ts):
//!   Scans .claude/skills/, .agents/skills/, and .opencode/skill/
//!   directories for SKILL.md files with frontmatter, builds a name→Info
//!   map, and formats the index for the system prompt. OpenCode uses
//!   ConfigMarkdown for parsing; enchanter uses a lightweight YAML
//!   frontmatter parser with serde_yml.
//!
//! The category-from-path convention (directory name under skills/
//! becomes the category tag) follows hermes-agent's convention where
//! skills/<category>/<name>/SKILL.md determines category membership.

use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::home;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[allow(dead_code)]
    pub body: String,
    pub category: Option<String>,
    #[allow(dead_code)]
    pub path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SkillsIndex {
    pub skills: Vec<Skill>,
}

impl SkillsIndex {
    pub fn discover() -> Result<Self> {
        let skills_dir = home::enchanter_home().join("skills");
        if !skills_dir.exists() {
            return Ok(Self::default());
        }

        let mut skills = Vec::new();

        for entry in WalkDir::new(&skills_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == "SKILL.md"
                && let Some(skill) = parse_skill_file(entry.path(), &skills_dir)
            {
                skills.push(skill);
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self { skills })
    }

    #[allow(dead_code)]
    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|s| s.name == name)
            .or_else(|| self.skills.iter().find(|s| s.name.contains(name)))
    }

    pub fn format_index_for_prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut lines = vec!["Available skills:".to_string()];
        for skill in &self.skills {
            let cat = skill
                .category
                .as_deref()
                .map(|c| format!("[{}] ", c))
                .unwrap_or_default();
            lines.push(format!(
                "  {}{}{}",
                cat,
                skill.name,
                if skill.description.is_empty() {
                    String::new()
                } else {
                    format!(": {}", skill.description)
                }
            ));
        }
        lines.join("\n")
    }
}

fn parse_skill_file(path: &Path, skills_dir: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = parse_frontmatter(&content);

    // Category from first path segment under skills_dir.
    let relative = path.strip_prefix(skills_dir).ok()?;
    let category = relative
        .iter()
        .next()
        .and_then(|c| c.to_str())
        .map(|s| s.to_string())
        .filter(|s| s != "SKILL.md");

    // Name: frontmatter > parent directory name.
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let name = frontmatter.name.unwrap_or_else(|| dir_name.clone());
    let description = frontmatter.description.unwrap_or_default();

    Some(Skill {
        name,
        description,
        body,
        category,
        path: path.to_path_buf(),
    })
}

fn parse_frontmatter(content: &str) -> (SkillFrontmatter, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (SkillFrontmatter::default(), content.to_string());
    }

    let after_open = &trimmed[3..];
    if let Some(end_idx) = after_open.find("\n---") {
        let yaml_str = &after_open[..end_idx];
        let body = after_open[end_idx + 4..].trim_start().to_string();
        let frontmatter: SkillFrontmatter = serde_yml::from_str(yaml_str).unwrap_or_default();
        return (frontmatter, body);
    }

    (SkillFrontmatter::default(), content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parsing() {
        let content = "---\nname: my-skill\ndescription: Does stuff\n---\n\n# My Skill\n\nDo the thing.";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name, Some("my-skill".to_string()));
        assert_eq!(fm.description, Some("Does stuff".to_string()));
        assert!(body.contains("# My Skill"));
    }

    #[test]
    fn no_frontmatter() {
        let content = "# Just a heading\n\nSome body text.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert_eq!(body, content);
    }
}