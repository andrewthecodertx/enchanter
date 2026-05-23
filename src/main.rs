//! Enchanter — a focused AI agent harness.

mod api;
mod cli;
mod config;
mod home;
mod memory;
mod prompt;
mod skills;
mod soul;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Args::parse();
    cli::run(args).await
}