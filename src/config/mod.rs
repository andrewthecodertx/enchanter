//! Configuration loading from ~/.enchanter/config.yaml.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

use crate::home;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct ModelConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub personalities: std::collections::HashMap<String, String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let config: Config = serde_yaml::from_str(&contents)
            .with_context(|| format!("parsing config YAML from {}", path.display()))?;
        Ok(config)
    }

    /// Model ID: config > ENCHANTER_MODEL > "gpt-4o".
    pub fn model_id(&self) -> String {
        self.model.default.clone()
            .or_else(|| std::env::var("ENCHANTER_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o".to_string())
    }

    /// Base URL: config > ENCHANTER_BASE_URL > OPENAI_BASE_URL > OpenAI default.
    pub fn base_url(&self) -> String {
        self.model.base_url.clone()
            .or_else(|| std::env::var("ENCHANTER_BASE_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    /// API key: config > ENCHANTER_API_KEY > OPENAI_API_KEY. None for local providers.
    pub fn api_key(&self) -> Option<String> {
        self.model.api_key.clone()
            .or_else(|| std::env::var("ENCHANTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
    }

    pub fn max_turns(&self) -> u32 {
        self.agent.max_turns.unwrap_or(30)
    }
}

fn config_path() -> PathBuf {
    home::enchanter_home().join("config.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let c = Config::default();
        assert_eq!(c.model_id(), "gpt-4o");
        assert_eq!(c.max_turns(), 30);
    }
}