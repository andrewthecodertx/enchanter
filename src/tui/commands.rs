//! Slash command handling for TUI — mirrors REPL commands.

use super::app::App;

/// Result of executing a slash command.
pub enum CommandResult {
    /// Command handled, continue running.
    Done,
    /// Command produced a message to send to the LLM.
    SendMessage(String),
    /// Quit requested.
    Quit,
}

/// Parse and execute a slash command. Returns CommandResult indicating what to do next.
pub fn handle_command(app: &mut App, line: &str) -> CommandResult {
    match line.trim() {
        "/exit" | "/quit" | "/bye" => CommandResult::Quit,
        "/clear" => {
            if let Err(e) = app.agent.clear() {
                app.chat_lines
                    .push(crate::tui::app::ChatLine::Error(format!(
                        "Failed to clear: {}",
                        e
                    )));
            } else {
                app.chat_lines.clear();
                app.chat_lines.push(crate::tui::app::ChatLine::System(
                    "Conversation cleared.".into(),
                ));
                app.turn = 0;
            }
            CommandResult::Done
        }
        "/help" => {
            app.chat_lines.push(crate::tui::app::ChatLine::System(
                "Commands: /help /clear /soul /memory /skills /tools /config /prompt /prompt diff /prompt budget /model <name> /retry /undo /sessions /exit".into(),
            ));
            CommandResult::Done
        }
        "/soul" => {
            app.chat_lines
                .push(crate::tui::app::ChatLine::System(format!(
                    "SOUL.md:\n{}",
                    app.agent.soul.content
                )));
            CommandResult::Done
        }
        "/memory" => {
            app.chat_lines
                .push(crate::tui::app::ChatLine::System(format!(
                    "Memory:\n{}",
                    app.agent.memory.format_for_prompt()
                )));
            CommandResult::Done
        }
        "/skills" => {
            app.chat_lines
                .push(crate::tui::app::ChatLine::System(format!(
                    "Skills:\n{}",
                    app.agent.skills.format_index_for_prompt()
                )));
            CommandResult::Done
        }
        "/tools" => {
            let builtins: Vec<String> = crate::tools::tool_definitions()
                .iter()
                .map(|t| {
                    format!(
                        "  {} — {}",
                        t.name,
                        t.description.lines().next().unwrap_or("")
                    )
                })
                .collect();
            let mut msg = format!(
                "Built-in tools ({}):\n{}",
                builtins.len(),
                builtins.join("\n")
            );
            let servers = app.agent.mcp.server_names();
            if !servers.is_empty() {
                msg.push_str("\n\nMCP tools:");
                for server_name in &servers {
                    msg.push_str(&format!("\n  [{}]", server_name));
                }
            }
            let total = crate::tools::tool_definitions().len() + app.agent.mcp.total_tool_count();
            msg.push_str(&format!("\n\nTotal: {} tools", total));
            app.chat_lines.push(crate::tui::app::ChatLine::System(msg));
            CommandResult::Done
        }
        "/config" => {
            let info = app.agent.info();
            let key_status = if info.api_key_set {
                "configured"
            } else {
                "not set"
            };
            app.chat_lines.push(crate::tui::app::ChatLine::System(
                format!("Config:\n  Model: {}\n  Base URL: {}\n  API key: {}\n  Max turns: {} (soft: {})\n  Tools: {} ({} MCP)\n  Skills: {}",
                    info.model, info.base_url, key_status,
                    info.max_turns.map_or("unlimited".to_string(), |n| n.to_string()),
                    info.soft_limit.map_or("n/a".to_string(), |n| n.to_string()),
                    info.tool_count, info.mcp_tool_count, info.skill_count),
            ));
            CommandResult::Done
        }
        "/prompt" => {
            let prompt_text = app
                .agent
                .messages
                .first()
                .map(|m| m.content.as_deref().unwrap_or(""))
                .unwrap_or("(no system prompt)")
                .to_string();
            app.chat_lines
                .push(crate::tui::app::ChatLine::System(format!(
                    "System prompt:\n{}",
                    prompt_text
                )));
            CommandResult::Done
        }
        "/sessions" => {
            match crate::session::Session::list_all() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        app.chat_lines.push(crate::tui::app::ChatLine::System(
                            "No sessions found.".into(),
                        ));
                    } else {
                        let lines: Vec<String> = sessions
                            .iter()
                            .map(|s| {
                                format!(
                                    "  {}... — {} msgs",
                                    &s.id[..8.min(s.id.len())],
                                    s.message_count
                                )
                            })
                            .collect();
                        app.chat_lines
                            .push(crate::tui::app::ChatLine::System(format!(
                                "Sessions:\n{}",
                                lines.join("\n")
                            )));
                    }
                }
                Err(e) => {
                    app.chat_lines
                        .push(crate::tui::app::ChatLine::Error(format!(
                            "Could not list sessions: {}",
                            e
                        )));
                }
            }
            CommandResult::Done
        }
        s if s.starts_with("/model ") => {
            let name = s.trim_start_matches("/model ").trim().to_string();
            if name.is_empty() {
                app.chat_lines.push(crate::tui::app::ChatLine::Error(
                    "Usage: /model <name>".into(),
                ));
            } else {
                match app.agent.switch_model(&name) {
                    Ok(label) => {
                        app.chat_lines
                            .push(crate::tui::app::ChatLine::System(format!(
                                "Switched to {}",
                                label
                            )));
                        app.refresh_info();
                    }
                    Err(e) => {
                        app.chat_lines
                            .push(crate::tui::app::ChatLine::Error(format!(
                                "Failed to switch model: {}",
                                e
                            )));
                    }
                }
            }
            CommandResult::Done
        }
        s if s.starts_with("/prompt diff") => {
            let layers = crate::prompt::build_prompt_layers(
                &app.agent.soul,
                &app.agent.memory,
                &app.agent.kstore,
                &app.agent.skills,
                &app.agent.config,
                &app.agent.resolved.model,
            );
            if let Some(prev) = &app.agent.previous_prompt_layers {
                let diff = layers.diff(prev);
                app.chat_lines
                    .push(crate::tui::app::ChatLine::System(format!(
                        "Prompt diff:\n{}",
                        diff.render()
                    )));
            } else {
                let names: Vec<String> = layers
                    .layers
                    .iter()
                    .map(|l| format!("  ● {}", l.name))
                    .collect();
                app.chat_lines
                    .push(crate::tui::app::ChatLine::System(format!(
                        "Prompt layers (no previous to diff):\n{}",
                        names.join("\n")
                    )));
            }
            app.agent.previous_prompt_layers = Some(layers);
            CommandResult::Done
        }
        s if s.starts_with("/prompt budget") => {
            let layers = crate::prompt::build_prompt_layers(
                &app.agent.soul,
                &app.agent.memory,
                &app.agent.kstore,
                &app.agent.skills,
                &app.agent.config,
                &app.agent.resolved.model,
            );
            let report = layers.budget();
            app.chat_lines
                .push(crate::tui::app::ChatLine::System(format!(
                    "Prompt budget:\n{}",
                    report.render(4000)
                )));
            CommandResult::Done
        }
        "/retry" => {
            // Retry needs async, handled in the main loop
            CommandResult::SendMessage("/retry".into())
        }
        "/undo" => {
            if app.agent.undo() {
                app.chat_lines.push(crate::tui::app::ChatLine::System(
                    "Undid last exchange.".into(),
                ));
            } else {
                app.chat_lines
                    .push(crate::tui::app::ChatLine::Error("Nothing to undo".into()));
            }
            CommandResult::Done
        }
        other => {
            app.chat_lines
                .push(crate::tui::app::ChatLine::Error(format!(
                    "Unknown command: {}",
                    other
                )));
            CommandResult::Done
        }
    }
}
