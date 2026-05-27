//! Enchanter — a focused AI agent harness.
//!
//! Architecture influenced by:
//! - hermes-agent (github.com/NousResearch/hermes-agent) — SOUL.md file convention,
//!   §-delimited memory store, MCP client config schema, home-directory bootstrap,
//!   REPL slash commands, prompt tier assembly, session summarization
//! - OpenCode (github.com/nicepkg/opencode) — SSE streaming with [DONE] sentinel,
//!   SKILL.md filesystem discovery, system prompt section structure
//!   (opencode/packages/opencode/src/session/system.ts),
//!   edit tool old/new string replacement (opencode/packages/opencode/src/tool/edit.ts)
//! - Claude Code (github.com/anthropics/claude-code) — built-in tool set naming
//!   (Bash→exec_command, Read→read_file, Write→write_file, Edit→edit_file,
//!   Grep→search_files, Memory→memory), edit_file uniqueness constraint and
//!   replace_all semantics (claude-code/src/tools/FileEditTool/),
//!   memory add/remove/replace/list operations (claude-code/src/memdir/)

mod agent;
mod api;
mod cli;
mod config;
#[cfg(unix)]
mod daemon;
#[cfg(not(unix))]
mod daemon {
    //! Stub — daemon mode requires Unix sockets (not available on Windows).
    use anyhow::{bail, Result};

    pub fn socket_path() -> std::path::PathBuf {
        unreachable!()
    }

    pub fn pid_path() -> std::path::PathBuf {
        unreachable!()
    }

    pub async fn is_running() -> bool {
        false
    }

    pub fn spawn_daemon(_idle_timeout_mins: Option<u64>) -> Result<u32> {
        bail!("Daemon mode is not supported on this platform (requires Unix sockets)")
    }

    pub async fn wait_for_socket(_timeout_secs: u64) -> Result<()> {
        bail!("Daemon mode is not supported on this platform")
    }

    pub async fn chat_via_daemon(
        _prompt: &str,
        _model: Option<String>,
        _system: Option<String>,
        _no_stream: bool,
        _no_tools: bool,
    ) -> Result<Option<String>> {
        bail!("Daemon mode is not supported on this platform")
    }

    pub async fn print_status() -> Result<()> {
        bail!("Daemon mode is not supported on this platform")
    }

    pub async fn stop_daemon() -> Result<()> {
        bail!("Daemon mode is not supported on this platform")
    }
}
mod home;
mod mcp;
mod memory;
mod prompt;
mod protocol;
mod recorder;
mod replay;
mod session;
mod skills;
mod soul;
mod summary;
mod tools;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Args::parse();
    cli::run(args).await
}