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
use colored::Colorize;
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::home;

/// Expand `${VAR}` references anywhere in a config value (model api_key/base_url,
/// MCP env values and headers). Supports mixed/partial strings like
/// `Bearer ${TOKEN}`. When a referenced variable is unset, warns (naming the
/// context and variable) and falls back to the literal value, so the
/// misconfiguration is visible at startup rather than surfacing later as a
/// broken request. Values with no `${…}` reference are returned unchanged.
pub fn expand_env(context: &str, val: &str) -> String {
    match shellexpand::env(val) {
        Ok(expanded) => expanded.into_owned(),
        Err(e) => {
            eprintln!(
                "{} {}: environment variable '{}' is not set; using literal value",
                "Warning:".yellow(),
                context,
                e.var_name
            );
            val.to_string()
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub security: SecurityConfig,
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

#[derive(Debug, Default, Clone, Deserialize)]
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

#[derive(Debug, Default, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub soft_limit: Option<u32>,
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
    #[serde(default)]
    pub context: ContextConfig,
}

fn default_true() -> Option<bool> {
    Some(true)
}

/// Rolling-context (conversation compaction) settings.
///
/// When the estimated token count of the live message window exceeds
/// `max_tokens`, older turns are summarized into a single synthetic message
/// while the system prompt and the most recent `keep_last_turns` user/assistant
/// turns are kept verbatim. Estimation uses the chars÷4 heuristic shared with
/// the `/prompt budget` report — approximate, intentionally conservative.
#[derive(Debug, Clone, Deserialize)]
pub struct ContextConfig {
    /// Token budget for the live window before compaction triggers.
    #[serde(default = "default_context_max_tokens")]
    pub max_tokens: u64,
    /// Number of most-recent turns to always keep verbatim (a "turn" here is a
    /// single message in history — user, assistant, or tool result).
    #[serde(default = "default_keep_last_turns")]
    pub keep_last_turns: usize,
}

fn default_context_max_tokens() -> u64 {
    96_000
}

fn default_keep_last_turns() -> usize {
    20
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens: default_context_max_tokens(),
            keep_last_turns: default_keep_last_turns(),
        }
    }
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
///
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

/// Security sandbox: paths the agent is allowed to read/write/execute within.
/// Defaults to the user's home directory. Projects can add their own paths
/// via project overlay config.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SecurityConfig {
    /// Directories the agent can access. Defaults to the user's home directory.
    /// Paths are canonicalized at resolution time.
    #[serde(default)]
    pub allowed_paths: Vec<String>,

    /// Allow `exec_command` to run without a filesystem sandbox when the kernel
    /// can't provide one (old kernel, non-Linux). Default: false (fail closed —
    /// refuse to run an unsandboxed shell). Set true as an escape hatch on
    /// platforms where Landlock is unavailable.
    #[serde(default)]
    pub allow_unsandboxed_exec: bool,
}

impl SecurityConfig {
    /// Resolve allowed_paths: if empty, defaults to the user's home directory.
    /// Expands ~ and environment variables, canonicalizes all paths.
    pub fn resolve(&self) -> Vec<PathBuf> {
        if self.allowed_paths.is_empty() {
            // Default: user's home directory
            if let Some(home) = dirs::home_dir() {
                vec![home]
            } else {
                vec![PathBuf::from("/")]
            }
        } else {
            self.allowed_paths
                .iter()
                .filter_map(|p| {
                    let expanded = shellexpand::tilde(p).to_string();
                    let path = PathBuf::from(&*expanded);
                    path.canonicalize().ok().or_else(|| Some(path.clone()))
                })
                .collect()
        }
    }
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

    /// Load config from a specific path (used by project overlay).
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let config: Config = serde_yml::from_str(&contents)
            .with_context(|| format!("parsing config YAML from {}", path.display()))?;
        Ok(config)
    }

    /// Model ID: config > ENCHANTER_MODEL > "gpt-4.1-mini".
    pub fn model_id(&self) -> String {
        self.model
            .default
            .clone()
            .or_else(|| std::env::var("ENCHANTER_MODEL").ok())
            .unwrap_or_else(|| "gpt-4.1-mini".to_string())
    }

    /// Base URL: config > ENCHANTER_BASE_URL > OPENAI_BASE_URL > OpenAI default.
    pub fn base_url(&self) -> String {
        self.model
            .base_url
            .clone()
            .or_else(|| std::env::var("ENCHANTER_BASE_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".to_string())
    }

    /// API key: config > ENCHANTER_API_KEY > OPENAI_API_KEY. None for local providers.
    pub fn api_key(&self) -> Option<String> {
        self.model
            .api_key
            .clone()
            .or_else(|| std::env::var("ENCHANTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
    }

    /// Resolve a named provider, falling back to defaults for unset fields.
    /// Returns None if no provider exists with that name.
    pub fn resolve_provider(&self, name: &str) -> Option<ResolvedModel> {
        let provider = self.providers.get(name)?;
        let model = provider
            .model
            .clone()
            .or_else(|| self.model.default.clone())
            .or_else(|| std::env::var("ENCHANTER_MODEL").ok())
            .unwrap_or_else(|| "gpt-4.1-mini".to_string());
        let base_url = provider
            .base_url
            .clone()
            .or_else(|| self.model.base_url.clone())
            .or_else(|| std::env::var("ENCHANTER_BASE_URL").ok())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".to_string());
        let api_key = provider
            .api_key
            .clone()
            .or_else(|| self.model.api_key.clone())
            .or_else(|| std::env::var("ENCHANTER_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        // Expand ${VAR} in api_key and base_url — pattern from hermes-agent's
        // mcp_servers config (hermes-agent/tools/mcp_tool.py)
        let api_key = api_key.map(|k| expand_env("model api_key", &k));
        let base_url = expand_env("model base_url", &base_url);

        Some(ResolvedModel {
            model,
            base_url,
            api_key,
        })
    }

    /// Resolve the default connection settings (top-level model config + env).
    pub fn resolve_default(&self) -> ResolvedModel {
        let model = self.model_id();
        let base_url = expand_env("model base_url", &self.base_url());
        let api_key = self.api_key().map(|k| expand_env("model api_key", &k));
        ResolvedModel {
            model,
            base_url,
            api_key,
        }
    }

    /// Hard turn limit. Defaults to 150 if not set.
    /// Returns None (unlimited) when explicitly set to 0.
    pub fn max_turns(&self) -> Option<u32> {
        match self.agent.max_turns {
            Some(0) => None, // 0 = unlimited
            Some(n) => Some(n),
            None => Some(150), // default
        }
    }

    /// Soft limit: turns remaining before the agent is nudged to wrap up.
    /// When turns_used >= (max_turns - soft_limit), a system hint is injected.
    /// Defaults to 10. Set to 0 to disable soft-limit nudges.
    /// Returns None when max_turns is unlimited.
    pub fn soft_limit(&self) -> Option<u32> {
        let max = self.max_turns()?;
        Some(self.agent.soft_limit.unwrap_or(10).min(max))
    }

    pub fn summarize_on_exit(&self) -> bool {
        self.agent.summarize_on_exit.unwrap_or(true)
    }

    pub fn memory_config(&self) -> &MemoryConfig {
        &self.agent.memory
    }

    pub fn context_config(&self) -> &ContextConfig {
        &self.agent.context
    }

    /// Resolve allowed paths for the security sandbox.
    pub fn allowed_paths(&self) -> Vec<PathBuf> {
        self.security.resolve()
    }

    /// Whether unsandboxed shell execution is permitted when no sandbox is available.
    pub fn allow_unsandboxed_exec(&self) -> bool {
        self.security.allow_unsandboxed_exec
    }
}

fn config_path() -> PathBuf {
    home::enchanter_home().join("config.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_handles_partial_whole_and_missing() {
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var("ENCHANTER_TEST_TOKEN", "secret123") };

        // Whole-string reference.
        assert_eq!(expand_env("ctx", "${ENCHANTER_TEST_TOKEN}"), "secret123");
        // Mixed/partial — the README's `Bearer ${MY_TOKEN}` header case.
        assert_eq!(
            expand_env("ctx", "Bearer ${ENCHANTER_TEST_TOKEN}"),
            "Bearer secret123"
        );
        // No reference — passthrough.
        assert_eq!(expand_env("ctx", "plain value"), "plain value");
        // Undefined variable — warns and falls back to the literal.
        assert_eq!(
            expand_env("ctx", "Bearer ${ENCHANTER_UNDEFINED_VAR_XYZ}"),
            "Bearer ${ENCHANTER_UNDEFINED_VAR_XYZ}"
        );

        unsafe { std::env::remove_var("ENCHANTER_TEST_TOKEN") };
    }

    #[test]
    fn default_config() {
        let c = Config::default();
        assert_eq!(c.model_id(), "gpt-4.1-mini");
        assert_eq!(c.max_turns(), Some(150));
        assert_eq!(c.soft_limit(), Some(10));
    }

    #[test]
    fn unlimited_turns() {
        let mut c = Config::default();
        c.agent.max_turns = Some(0);
        assert_eq!(c.max_turns(), None); // 0 = unlimited
        assert_eq!(c.soft_limit(), None); // no soft limit when unlimited
    }

    #[test]
    fn explicit_turn_limit() {
        let mut c = Config::default();
        c.agent.max_turns = Some(50);
        assert_eq!(c.max_turns(), Some(50));
        assert_eq!(c.soft_limit(), Some(10)); // default soft limit

        c.agent.soft_limit = Some(5);
        assert_eq!(c.soft_limit(), Some(5));
    }

    #[test]
    fn resolve_provider_returns_none_for_unknown() {
        let c = Config::default();
        assert!(c.resolve_provider("nonexistent").is_none());
    }

    #[test]
    fn resolve_provider_fills_defaults() {
        let mut c = Config::default();
        c.providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                model: Some("qwen3".to_string()),
                base_url: Some("http://localhost:11434/v1".to_string()),
                api_key: None,
            },
        );
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
        assert_eq!(resolved.base_url, "https://api.openai.com/v1/chat/completions");
    }
}
