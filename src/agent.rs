//! Agent session — extracted conversation state and logic.
//!
//! Separates the core agent loop from CLI display concerns so it can be
//! used by both the REPL and the daemon (Phase 2).

use anyhow::Result;
use colored::Colorize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::api::{LlmClient, Message};
use crate::config::{Config, ResolvedModel};
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::prompt;
use crate::protocol::Event;
use crate::session::Session;
use crate::skills::SkillsIndex;
use crate::soul::Soul;
use crate::tools;

/// Running agent session — holds all state for one conversation.
pub struct AgentSession {
    pub config: Config,
    pub soul: Soul,
    pub memory: MemoryStore,
    pub skills: SkillsIndex,
    pub mcp: McpManager,
    pub messages: Vec<Message>,
    pub resolved: ResolvedModel,
    pub client: LlmClient,
    pub session: Session,
    pub no_stream: bool,
    pub no_tools: bool,
    pub system_override: Option<String>,
}

/// Result of a single chat turn.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ChatResult {
    pub response: Option<String>,
    pub tool_calls: usize,
}

/// Info about the session, for display and status reporting.
#[allow(dead_code)]
pub struct SessionInfo {
    pub model: String,
    pub base_url: String,
    pub api_key_set: bool,
    pub max_turns: u32,
    pub soft_limit: u32,
    pub tool_count: usize,
    pub mcp_tool_count: usize,
    pub mcp_servers: Vec<String>,
    pub skill_count: usize,
    pub session_id: String,
}

impl AgentSession {
    /// Create a new agent session from config, loading all state.
    pub fn new(config: Config, soul: Soul, memory: MemoryStore, skills: SkillsIndex, resolved: ResolvedModel, no_stream: bool, no_tools: bool, system_override: Option<String>) -> Result<Self> {
        let client = LlmClient::new(&resolved.base_url, resolved.api_key.as_deref(), &resolved.model);
        let mcp = McpManager::new();
        if !no_tools && !config.mcp.servers.is_empty() {
            // Note: MCP startup is async, must be called from an async context
            // This is handled by `start_mcp()` below
        }
        let session = Session::new(&resolved.model)?;

        let system_prompt = match &system_override {
            Some(s) => s.clone(),
            None => prompt::build_system_prompt_with_model(&soul, &memory, &skills, &config, &resolved.model),
        };
        let messages = vec![Message::system(&system_prompt)];

        // TODO: session.append for initial system message — handled by caller for now

        Ok(Self {
            config,
            soul,
            memory,
            skills,
            mcp,
            messages,
            resolved,
            client,
            session,
            no_stream,
            no_tools,
            system_override,
        })
    }

    /// Start MCP servers (async — must be called from a tokio runtime).
    pub async fn start_mcp(&mut self) {
        if !self.no_tools && !self.config.mcp.servers.is_empty() {
            self.mcp.start_all(&self.config.mcp.servers).await;
        }
    }

    /// Shut down MCP servers.
    pub async fn shutdown_mcp(&mut self) {
        self.mcp.shutdown_all().await;
    }

    /// Build the combined tools payload (built-in + MCP).
    pub fn tools_payload(&self) -> Option<Value> {
        if self.no_tools {
            return None;
        }
        let mut all_tools = tools::tools_json();
        all_tools.extend(self.mcp.all_tools_json());
        Some(Value::Array(all_tools))
    }

    /// Get session info for display.
    pub fn info(&self) -> SessionInfo {
        let tools_payload = self.tools_payload();
        let tool_count = tools_payload
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        SessionInfo {
            model: self.resolved.model.clone(),
            base_url: self.resolved.base_url.clone(),
            api_key_set: self.resolved.api_key.is_some(),
            max_turns: self.config.max_turns(),
            soft_limit: self.config.soft_limit(),
            tool_count,
            mcp_tool_count: self.mcp.total_tool_count(),
            mcp_servers: self.mcp.server_names().into_iter().map(String::from).collect(),
            skill_count: self.skills.skills.len(),
            session_id: self.session.id().to_string(),
        }
    }

    /// Run one agent loop: call model, handle tool_calls, repeat until done or max_turns.
    /// Returns the final text response and the number of tool calls made.
    pub async fn chat(&mut self, user_prompt: &str) -> Result<ChatResult> {
        let user_msg = Message::user(user_prompt);
        self.session.append(&user_msg)?;
        self.messages.push(user_msg);

        let result = self.run_agent_loop().await?;
        Ok(result)
    }

    /// Run one agent loop, emitting events through a channel.
    /// Returns the final ChatResult and the receiving end of the event channel.
    /// The caller should read events from the receiver and handle them (print,
    /// send over socket, etc.). Event::Done signals the end.
    pub async fn chat_events(&mut self, user_prompt: &str) -> Result<(ChatResult, mpsc::UnboundedReceiver<Event>)> {
        let user_msg = Message::user(user_prompt);
        self.session.append(&user_msg)?;
        self.messages.push(user_msg);

        let (tx, rx) = mpsc::unbounded_channel();
        let result = self.run_agent_loop_events(&tx).await?;
        Ok((result, rx))
    }

    /// Retry the last message, emitting events through a channel.
    #[allow(dead_code)]
    pub async fn retry_events(&mut self) -> Result<(ChatResult, mpsc::UnboundedReceiver<Event>)> {
        let last_user_idx = self.messages.iter().rposition(|m| m.role == "user");
        if let Some(idx) = last_user_idx {
            if idx + 1 < self.messages.len() {
                self.messages.truncate(idx + 1);
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let result = self.run_agent_loop_events(&tx).await?;
        Ok((result, rx))
    }

    /// Continue from existing messages (e.g., /retry).
    pub async fn retry(&mut self) -> Result<ChatResult> {
        // Remove everything after last user message, then re-run
        let last_user_idx = self.messages.iter().rposition(|m| m.role == "user");
        if let Some(idx) = last_user_idx {
            if idx + 1 < self.messages.len() {
                self.messages.truncate(idx + 1);
            }
        }
        self.run_agent_loop().await
    }

    /// Undo the last exchange (remove everything after and including last user message).
    pub fn undo(&mut self) -> bool {
        let last_user_idx = self.messages.iter().rposition(|m| m.role == "user");
        if let Some(idx) = last_user_idx {
            self.messages.truncate(idx);
            true
        } else {
            false
        }
    }

    /// Clear conversation and start a new session.
    pub fn clear(&mut self) -> Result<()> {
        let fresh_prompt = match &self.system_override {
            Some(s) => s.clone(),
            None => prompt::build_system_prompt_with_model(
                &self.soul, &self.memory, &self.skills, &self.config, &self.resolved.model,
            ),
        };
        self.messages = vec![Message::system(&fresh_prompt)];
        match Session::new(&self.resolved.model) {
            Ok(mut new_session) => {
                if let Err(e) = new_session.append(&self.messages[0]) {
                    eprintln!("{} Could not create session: {}", "Warning:".yellow(), e);
                }
                self.session = new_session;
            }
            Err(e) => eprintln!("{} Could not create session: {}", "Warning:".yellow(), e),
        }
        Ok(())
    }

    /// Switch model/provider. Returns a description of the new model.
    pub fn switch_model(&mut self, name: &str) -> Result<String> {
        let new_resolved = self.config.resolve_provider(name)
            .unwrap_or_else(|| {
                let default = self.config.resolve_default();
                ResolvedModel {
                    model: name.to_string(),
                    base_url: default.base_url,
                    api_key: default.api_key,
                }
            });

        let provider_label = if self.config.providers.contains_key(name) {
            format!("{} (provider: {})", new_resolved.model, name)
        } else {
            new_resolved.model.clone()
        };

        self.client = LlmClient::new(&new_resolved.base_url, new_resolved.api_key.as_deref(), &new_resolved.model);
        self.resolved = new_resolved;

        // Refresh system prompt to update the Model: line
        if self.system_override.is_none() {
            let refreshed = prompt::build_system_prompt_with_model(
                &self.soul, &self.memory, &self.skills, &self.config, &self.resolved.model,
            );
            if let Some(sys_msg) = self.messages.first_mut() {
                sys_msg.content = Some(refreshed);
            }
        }

        Ok(provider_label)
    }

    /// Run the agent loop: call model, handle tool_calls, with soft-limit nudging
    /// and a hard turn limit as a safety net.
    async fn run_agent_loop(&mut self) -> Result<ChatResult> {
        let max_turns = self.config.max_turns();
        let soft_limit = self.config.soft_limit();
        let tools_payload = self.tools_payload();
        let mut total_tool_calls = 0;
        let mut soft_limit_triggered = false;

        for turn in 0..max_turns {
            let turns_remaining = max_turns.saturating_sub(turn);

            // Soft limit: inject a nudge when we're running low on turns
            if !soft_limit_triggered && soft_limit > 0 && turns_remaining <= soft_limit {
                soft_limit_triggered = true;
                let nudge = format!(
                    "[System] You have {} turns remaining before the hard limit. Wrap up now: produce a final text response without any tool calls.",
                    turns_remaining,
                );
                self.messages.push(Message::user(&nudge));
            }

            let result = if self.no_stream {
                self.client.chat(self.messages.clone(), tools_payload.clone()).await?
            } else {
                self.client.chat_stream(self.messages.clone(), tools_payload.clone()).await?
            };

            if result.has_tool_calls() {
                let tool_calls = result.tool_calls.unwrap();
                let assistant_msg = Message::assistant_with_tools(tool_calls.clone(), result.content);
                self.session.append(&assistant_msg)?;
                self.messages.push(assistant_msg);

                for tc in &tool_calls {
                    let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Null);
                    let output = dispatch_tool(&tc.function.name, &tc_args, &mut self.memory, &mut self.mcp).await;
                    let tool_msg = Message::tool_result(&tc.id, output);
                    self.session.append(&tool_msg)?;
                    self.messages.push(tool_msg);
                    total_tool_calls += 1;
                }
            } else {
                let content = result.content;
                if let Some(ref text) = content {
                    self.session.append(&Message::assistant(text))?;
                }
                return Ok(ChatResult {
                    response: content,
                    tool_calls: total_tool_calls,
                });
            }
        }

        anyhow::bail!("Max agent turns reached ({}). The agent exceeded its turn limit without producing a final response.", max_turns);
    }

    /// Event-yielding version of the agent loop.
    /// Sends structured events through the channel instead of printing to stdout.
    /// Supports both streaming and non-streaming modes.
    async fn run_agent_loop_events(&mut self, tx: &mpsc::UnboundedSender<Event>) -> Result<ChatResult> {
        let max_turns = self.config.max_turns();
        let soft_limit = self.config.soft_limit();
        let tools_payload = self.tools_payload();
        let mut total_tool_calls = 0;
        let mut soft_limit_triggered = false;

        for turn in 0..max_turns {
            let turns_remaining = max_turns.saturating_sub(turn);

            if !soft_limit_triggered && soft_limit > 0 && turns_remaining <= soft_limit {
                soft_limit_triggered = true;
                let nudge = format!(
                    "[System] You have {} turns remaining before the hard limit. Wrap up now: produce a final text response without any tool calls.",
                    turns_remaining,
                );
                self.messages.push(Message::user(&nudge));
            }

            let result = if self.no_stream {
                self.client.chat(self.messages.clone(), tools_payload.clone()).await?
            } else {
                // Streaming: send each content token as an event
                self.client.chat_stream_with(
                    self.messages.clone(),
                    tools_payload.clone(),
                    |token| {
                        let _ = tx.send(Event::Content { text: token.to_string() });
                    },
                ).await?
            };

            if result.has_tool_calls() {
                let tool_calls = result.tool_calls.unwrap();
                // If there was content alongside the tool calls, emit it
                if let Some(ref content) = result.content {
                    if !content.is_empty() {
                        let _ = tx.send(Event::Content { text: content.clone() });
                    }
                }
                for tc in &tool_calls {
                    let _ = tx.send(Event::ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    });
                }

                let assistant_msg = Message::assistant_with_tools(tool_calls.clone(), result.content);
                self.session.append(&assistant_msg)?;
                self.messages.push(assistant_msg);

                for tc in &tool_calls {
                    let tc_args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Null);
                    let output = dispatch_tool(&tc.function.name, &tc_args, &mut self.memory, &mut self.mcp).await;
                    let _ = tx.send(Event::ToolResult {
                        id: tc.id.clone(),
                        content: output.clone(),
                    });
                    let tool_msg = Message::tool_result(&tc.id, output);
                    self.session.append(&tool_msg)?;
                    self.messages.push(tool_msg);
                    total_tool_calls += 1;
                }
            } else {
                let content = result.content;
                // Non-streaming mode: send the full content at once
                if self.no_stream {
                    if let Some(ref text) = content {
                        let _ = tx.send(Event::Content { text: text.clone() });
                    }
                }
                if let Some(ref text) = content {
                    self.session.append(&Message::assistant(text))?;
                }
                let _ = tx.send(Event::Done);
                return Ok(ChatResult {
                    response: content,
                    tool_calls: total_tool_calls,
                });
            }
        }

        let _ = tx.send(Event::Error {
            message: format!("Max agent turns reached ({}).", max_turns),
        });
        anyhow::bail!("Max agent turns reached ({}). The agent exceeded its turn limit without producing a final response.", max_turns);
    }
}

/// Dispatch a tool call — built-in tools first, then MCP.
async fn dispatch_tool(
    name: &str,
    args: &Value,
    memory: &mut MemoryStore,
    mcp: &mut McpManager,
) -> String {
    let built_in_names = ["exec_command", "read_file", "write_file", "edit_file",
                          "search_files", "list_directory", "memory"];
    if built_in_names.contains(&name) {
        tools::dispatch(name, args, memory)
    } else if name.contains(':') {
        match mcp.dispatch(name, args).await {
            Some(Ok(result)) => result,
            Some(Err(e)) => format!("MCP error: {}", e),
            None => format!("Unknown tool: {}", name),
        }
    } else {
        format!("Unknown tool: {}", name)
    }
}