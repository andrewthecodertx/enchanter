//! SOUL.md loader — persona definition from ~/.enchanter/SOUL.md.
//!
//! The SOUL.md convention (user-editable markdown file in the home directory
//! that defines the agent's persona and behavioral directives) is adapted from
//! hermes-agent (hermes-agent/agent/prompt_builder.py, load_soul_md()),
//! which loads SOUL.md from ~/.hermes/ and injects it as the identity slot
//! in the system prompt. hermes-agent also scans for injection attempts in
//! SOUL.md content; enchanter loads it verbatim with a fallback persona.
//!
//! The fallback pattern (hardcoded identity string when SOUL.md is missing)
//! mirrors hermes-agent's DEFAULT_AGENT_IDENTITY constant
//! (hermes-agent/agent/prompt_builder.py:134).

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::home;

#[derive(Debug, Clone)]
pub struct Soul {
    pub content: String,
    pub source: PathBuf,
}

impl Soul {
    pub fn load() -> Result<Option<Self>> {
        let path = soul_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading SOUL from {}", path.display()))?;
        Ok(Some(Self { content, source: path }))
    }

    /// Load SOUL.md, or return a fallback persona.
    pub fn load_or_fallback() -> Result<Self> {
        match Self::load()? {
            Some(soul) => Ok(soul),
            None => Ok(Self {
                content: String::from(
                    "You are Enchanter, a focused AI agent harness. \
                     You are concise, helpful, and direct.",
                ),
                source: PathBuf::from("<fallback>"),
            }),
        }
    }
}

fn soul_path() -> PathBuf {
    home::enchanter_home().join("SOUL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_has_name() {
        let fallback = Soul {
            content: String::from(
                "You are Enchanter, a focused AI agent harness. \
                 You are concise, helpful, and direct.",
            ),
            source: PathBuf::from("<fallback>"),
        };
        assert!(fallback.content.contains("Enchanter"));
    }
}