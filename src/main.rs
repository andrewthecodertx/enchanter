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

mod api;
mod cli;
mod config;
mod home;
mod mcp;
mod memory;
mod prompt;
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