//! Configuration loading from ~/.enchanter/config.yaml.
//!
//! The MCP server config format (command + args + env for stdio, url + headers for
//! HTTP, ${VAR} environment variable expansion) is adapted from hermes-agent's
//! mcp_servers config schema (hermes-agent/tools/mcp_tool.py, lines 14-48),
//! which defines the same transport types with matching field names. hermes-agent
//! additionally supports SSE transport and per-server timeout/retry settings;
//! enchanter implements stdio and Streamable HTTP.
//!
//! The provider config pattern (named presets with field-level inheritance from
//! top-level defaults, plus ENV_VAR fallback chain) follows hermes-agent's
//! multi-provider resolution (hermes-agent/hermes_cli/config.py), which uses
//! a similar config.yaml structure with named providers that inherit missing
//! fields from globals and environment variables.
//!
//! The ${VAR} expansion in api_key/base_url fields uses shellexpand, matching
//! hermes-agent's ${VAR} expansion in mcp_server env values
//! (hermes-agent/tools/mcp_tool.py).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

use crate::home;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub mcp: McpConfig,
}

/// A named provider preset: model + base_url + api_key.
/// Any field left blank inherits from the top-level model config or env vars.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
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
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default = "default_true")]
    pub summarize_on_exit: Option<bool>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_max_entries")]
    pub max_entries: u32,
    #[serde(default = "default_summarize_threshold")]
    pub summarize_threshold: u32,
}

fn default_max_entries() -> u32 {
    50
}

fn default_summarize_threshold() -> u32 {
    40
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_entries: default_max_entries(),
            summarize_threshold: default_summarize_threshold(),
        }
    }
}

// ── MCP config ──

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: std::collections::HashMap<String, McpServerConfig>,
}

/// MCP server configuration. Supports two transports:
/// - stdio: local process via `command` + `args` + `env`
/// - http: remote server via `url` + optional `headers`
///
/// Exactly one of `command` or `url` must be set.
///
/// Config schema adapted from hermes-agent's mcp_servers format
/// (hermes-agent/tools/mcp_tool.py, lines 14-48). hermes-agent defines:
///   - command, args, env for stdio transport
///   - url, headers for HTTP transport
///   - Additionally supports SSE transport, per-server timeout/retry,
///     and parallel tool call settings that enchanter omits.
/// OpenCode (opencode/packages/opencode/src/mcp/index.ts) uses the
/// @modelcontextprotocol SDK for transport; enchanter reimplements
/// the MCP protocol directly.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Command to run for stdio transport (e.g. "npx", "uvx").
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments for the stdio command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for the stdio command.
    /// Values support ${VAR} expansion from the current environment.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// URL for HTTP transport (e.g. "https://mcp.example.com/api").
    #[serde(default)]
    pub url: Option<String>,
    /// Additional HTTP headers for HTTP transport.
    /// Values support ${VAR} expansion from the current environment.
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

impl McpServerConfig {
    /// Which transport type this config specifies.
    pub fn transport_type(&self) -> McpTransportType {
        if self.url.is_some() {
            McpTransportType::Http
        } else {
            McpTransportType::Stdio
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransportType {
    Stdio,
    Http,
}

/// Resolved connection settings for a specific model+provider.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let config: Config = serde_yml::from_str(&contents)
            .with_context(|| format!("parsing config YAML from {}", path.display()))?;
        Ok(config)
    }

    /// Model ID: config > ENCHANTER_MODEL > "gpt-4.1-mini".
    pub fn model_id(&self) -> String {
        self.model.default.clone()
            .or_else(|| std::env::var("ENCHANTER_MODEL").ok())
            .unwrap_or_else(|| "gpt-4.1-mini".to_string())
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

    /// Resolve a named provider, falling back to defaults for unset fields.
    /// Returns None if no provider exists with that name.
    pub fn resolve_provider(&self, name: &str) -> Option<ResolvedModel> {
        let provider = self.providers.get(name)?;
        let model = provider.model.clone()
            .or_else(|| self.model.default.clone())
            .or_else(|| std::env::var("ENCHANTER_MODEL").ok())
            .unwrap_or_else(|| "gpt-4.1-mini".to_string());
        let base_url = provider.base_url.clone()
            .or_else(|| self.model.base_url.clone())
            .or_else(|| std::env::var("ENCHANTER_BASE_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let api_key = provider.api_key.clone()
            .or_else(|| self.model.api_key.clone())
            .or_else(|| std::env::var("ENCHANTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        // Expand ${VAR} in api_key and base_url — pattern from hermes-agent's
        // mcp_servers config (hermes-agent/tools/mcp_tool.py)
        let api_key = api_key.map(|k| shellexpand::env(&k).unwrap_or_else(|_| k.clone().into()).to_string());
        let base_url_clone = base_url.clone();
        let base_url = shellexpand::env(&base_url).unwrap_or_else(|_| base_url_clone.into()).to_string();

        Some(ResolvedModel { model, base_url, api_key })
    }

    /// Resolve the default connection settings (top-level model config + env).
    pub fn resolve_default(&self) -> ResolvedModel {
        let model = self.model_id();
        let base_url = shellexpand::env(&self.base_url())
            .unwrap_or_else(|_| self.base_url().into())
            .to_string();
        let api_key = self.api_key().map(|k| {
            shellexpand::env(&k).unwrap_or_else(|_| k.clone().into()).to_string()
        });
        ResolvedModel { model, base_url, api_key }
    }

    pub fn max_turns(&self) -> u32 {
        self.agent.max_turns.unwrap_or(60)
    }

    pub fn summarize_on_exit(&self) -> bool {
        self.agent.summarize_on_exit.unwrap_or(true)
    }

    pub fn memory_config(&self) -> &MemoryConfig {
        &self.agent.memory
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
        assert_eq!(c.model_id(), "gpt-4.1-mini");
        assert_eq!(c.max_turns(), 60);
    }

    #[test]
    fn resolve_provider_returns_none_for_unknown() {
        let c = Config::default();
        assert!(c.resolve_provider("nonexistent").is_none());
    }

    #[test]
    fn resolve_provider_fills_defaults() {
        let mut c = Config::default();
        c.providers.insert("ollama".to_string(), ProviderConfig {
            model: Some("qwen3".to_string()),
            base_url: Some("http://localhost:11434/v1".to_string()),
            api_key: None,
        });
        let resolved = c.resolve_provider("ollama").unwrap();
        assert_eq!(resolved.model, "qwen3");
        assert_eq!(resolved.base_url, "http://localhost:11434/v1");
        // api_key falls back to top-level or env — should be None in default config
        assert!(resolved.api_key.is_none());
    }

    #[test]
    fn resolve_default_uses_top_level() {
        let c = Config::default();
        let resolved = c.resolve_default();
        assert_eq!(resolved.model, "gpt-4.1-mini");
        assert_eq!(resolved.base_url, "https://api.openai.com/v1");
    }
}