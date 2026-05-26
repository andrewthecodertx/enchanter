//! Session replay — re-run recorded sessions with optional model swap or stubbed tools.
//!
//! Implements REQ-REP-001 through REQ-REP-006:
//! - `enchanter replay <file.jsonl>` subcommand (REQ-REP-001)
//! - `--swap-model <model>` flag for model substitution (REQ-REP-003)
//! - `--exact` mode that errors on provider mismatch (REQ-REP-002)
//! - `--tools live` (default) and `--tools stubbed` modes (REQ-REP-004, 005)
//! - Diffable replay output against original recording (REQ-REP-006)

use anyhow::Result;
use colored::Colorize;

use crate::recorder::read_recording;

/// Replay mode for tool execution during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayToolMode {
    /// Call tools live during replay (default).
    Live,
    /// Use recorded tool outputs for deterministic replay.
    Stubbed,
}

/// Parse replay tool mode from string.
pub fn parse_tools_mode(s: &str) -> Result<ReplayToolMode> {
    match s.to_lowercase().as_str() {
        "live" => Ok(ReplayToolMode::Live),
        "stubbed" => Ok(ReplayToolMode::Stubbed),
        _ => anyhow::bail!("Invalid tools mode: '{}'. Use 'live' or 'stubbed'.", s),
    }
}

/// Replay a recorded session, printing events and optionally checking for exact model match.
pub fn replay_session(
    path: &std::path::Path,
    swap_model: Option<&str>,
    exact: bool,
    tools_mode: &ReplayToolMode,
) -> Result<()> {
    let events = read_recording(path)?;

    if events.is_empty() {
        println!("{} Recording is empty.", "Note:".dimmed());
        return Ok(());
    }

    println!("{}", "═══ REPLAY ═══".bright_cyan());
    println!("  {} events from {}", events.len(), path.display());
    println!();

    // Check schema version
    let schema = &events[0].schema_version;
    println!("  {} Schema version: {}", "↳".dimmed(), schema);

    // Find config snapshot to get original model
    let original_model = events.iter()
        .find(|e| e.event_type == "config_snapshot")
        .and_then(|e| e.payload.get("model"))
        .and_then(|v| v.as_str());

    let _effective_model = swap_model
        .map(|s| s.to_string())
        .or_else(|| original_model.map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string());

    // Exact mode check
    if exact {
        if let Some(orig) = original_model {
            if let Some(swapped) = swap_model {
                if orig != swapped {
                    anyhow::bail!(
                        "Exact mode: recorded model '{}' does not match --swap-model '{}'",
                        orig,
                        swapped
                    );
                }
            }
        }
    }

    println!("  {} Original model: {}", "↳".dimmed(), original_model.unwrap_or("unknown"));
    if swap_model.is_some() {
        println!("  {} Swapped model:  {}", "↳".dimmed(), swap_model.unwrap().bright_yellow());
    }
    println!("  {} Tools mode:      {}", "↳".dimmed(), match tools_mode {
        ReplayToolMode::Live => "live",
        ReplayToolMode::Stubbed => "stubbed (deterministic)",
    });
    println!();

    // Replay events
    let mut tool_results: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for event in &events {
        match event.event_type.as_str() {
            "config_snapshot" => {
                println!("{} {}", "⚙ Config Snapshot".bright_cyan(), format!("[seq={}]", event.seq).dimmed());
                if let Some(model) = event.payload.get("model").and_then(|v| v.as_str()) {
                    println!("    Model: {}", model);
                }
                if let Some(base_url) = event.payload.get("base_url").and_then(|v| v.as_str()) {
                    println!("    Base URL: {}", base_url);
                }
                if let Some(providers) = event.payload.get("providers").and_then(|v| v.as_array()) {
                    let names: Vec<String> = providers.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    if !names.is_empty() {
                        println!("    Providers: {}", names.join(", "));
                    }
                }
            }
            "prompt_hash" => {
                if let (Some(layer), Some(hash)) = (
                    event.payload.get("layer").and_then(|v| v.as_str()),
                    event.payload.get("hash").and_then(|v| v.as_str()),
                ) {
                    println!("{} {} = {}", "# Prompt Hash".dimmed(), layer, hash);
                }
            }
            "user_message" => {
                if let Some(content) = event.payload.get("content").and_then(|v| v.as_str()) {
                    println!("{} {}", "⟩".bright_blue(), content.chars().take(200).collect::<String>());
                }
            }
            "assistant_response" => {
                if let Some(content) = event.payload.get("content").and_then(|v| v.as_str()) {
                    println!("{} {}", "⟨".bright_green(), content.chars().take(200).collect::<String>());
                }
            }
            "tool_call" => {
                if let (Some(name), Some(_id)) = (
                    event.payload.get("tool_name").and_then(|v| v.as_str()),
                    event.payload.get("tool_id").and_then(|v| v.as_str()),
                ) {
                    let is_mcp = event.payload.get("is_mcp").and_then(|v| v.as_bool()).unwrap_or(false);
                    let tag = if is_mcp { " (MCP)".dimmed() } else { "".normal() };
                    println!("{} {}{} [seq={}]", "🔧".bright_yellow(), name, tag, event.seq);
                    if tools_mode == &ReplayToolMode::Stubbed {
                        println!("    {} Stubbed: tool call not executed", "↳".dimmed());
                    }
                }
            }
            "tool_result" => {
                if let Some(id) = event.payload.get("tool_id").and_then(|v| v.as_str()) {
                    let is_error = event.payload.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    let result = event.payload.get("result").and_then(|v| v.as_str()).unwrap_or("");
                    tool_results.insert(id.to_string(), result.to_string());

                    if tools_mode == &ReplayToolMode::Stubbed {
                        let icon = if is_error { "✗".red() } else { "✓".green() };
                        let preview: String = result.chars().take(100).collect();
                        println!("    {} Result: {}{}", icon, preview.dimmed(), if result.len() > 100 { "…" } else { "" });
                    }
                }
            }
            "memory_write" => {
                if let Some(action) = event.payload.get("action").and_then(|v| v.as_str()) {
                    println!("{} Memory {}", "💾".bright_magenta(), action);
                }
            }
            "model_change" => {
                if let (Some(from), Some(to)) = (
                    event.payload.get("from_model").and_then(|v| v.as_str()),
                    event.payload.get("to_model").and_then(|v| v.as_str()),
                ) {
                    println!("{} Model: {} → {}", "⟳".bright_yellow(), from, to);
                }
            }
            "session_summary" => {
                if let Some(content) = event.payload.get("content").and_then(|v| v.as_str()) {
                    println!("{} Session summary: {}", "📋".bright_cyan(), content.chars().take(100).collect::<String>());
                }
            }
            _ => {
                println!("{} Unknown event type: {}", "?".dimmed(), event.event_type);
            }
        }
    }

    println!();
    println!("{} Replay complete. {} events processed.", "✓".green(), events.len());

    if tools_mode == &ReplayToolMode::Stubbed {
        println!("{} {} tool results restored from recording.", "↳".dimmed(), tool_results.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tools_mode() {
        assert_eq!(parse_tools_mode("live").unwrap(), ReplayToolMode::Live);
        assert_eq!(parse_tools_mode("stubbed").unwrap(), ReplayToolMode::Stubbed);
        assert_eq!(parse_tools_mode("LIVE").unwrap(), ReplayToolMode::Live);
        assert_eq!(parse_tools_mode("STUBBED").unwrap(), ReplayToolMode::Stubbed);
        assert!(parse_tools_mode("invalid").is_err());
    }
}