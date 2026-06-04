//! Agent session — extracted conversation state and logic.
//!
//! Separates the core agent loop from CLI display concerns so it can be
//! used by both the REPL and the daemon (Phase 2).

use anyhow::Result;
use colored::Colorize;
use serde_json::Value;
use std::path::PathBuf;
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

use crate::prompt::inspect::PromptLayers;

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
    /// Previous prompt layers for diff between turns (REQ-INS-001).
    pub previous_prompt_layers: Option<PromptLayers>,
}

/// Runtime options for an agent session, separate from the loaded core state.
#[derive(Debug, Default)]
pub struct SessionOptions {
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
    pub fn new(
        config: Config,
        soul: Soul,
        memory: MemoryStore,
        skills: SkillsIndex,
        resolved: ResolvedModel,
        options: SessionOptions,
    ) -> Result<Self> {
        let SessionOptions {
            no_stream,
            no_tools,
            system_override,
        } = options;
        let client = LlmClient::new(
            &resolved.base_url,
            resolved.api_key.as_deref(),
            &resolved.model,
        );
        let mcp = McpManager::new();
        if !no_tools && !config.mcp.servers.is_empty() {
            // MCP startup is async, handled by `start_mcp()` later.
        }
        let session = Session::new(&resolved.model)?;

        let system_prompt = match &system_override {
            Some(s) => s.clone(),
            None => prompt::build_system_prompt_with_model(
                &soul,
                &memory,
                &skills,
                &config,
                &resolved.model,
            ),
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
            previous_prompt_layers: None,
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
            mcp_servers: self
                .mcp
                .server_names()
                .into_iter()
                .map(String::from)
                .collect(),
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
    pub async fn chat_events(
        &mut self,
        user_prompt: &str,
    ) -> Result<(ChatResult, mpsc::UnboundedReceiver<Event>)> {
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
        if let Some(idx) = last_user_idx
            && idx + 1 < self.messages.len()
        {
            self.messages.truncate(idx + 1);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let result = self.run_agent_loop_events(&tx).await?;
        Ok((result, rx))
    }

    /// Continue from existing messages (e.g., /retry).
    pub async fn retry(&mut self) -> Result<ChatResult> {
        // Remove everything after last user message, then re-run
        let last_user_idx = self.messages.iter().rposition(|m| m.role == "user");
        if let Some(idx) = last_user_idx
            && idx + 1 < self.messages.len()
        {
            self.messages.truncate(idx + 1);
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
                &self.soul,
                &self.memory,
                &self.skills,
                &self.config,
                &self.resolved.model,
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
        let new_resolved = self.config.resolve_provider(name).unwrap_or_else(|| {
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

        self.client = LlmClient::new(
            &new_resolved.base_url,
            new_resolved.api_key.as_deref(),
            &new_resolved.model,
        );
        self.resolved = new_resolved;

        // Refresh system prompt to update the Model: line
        if self.system_override.is_none() {
            let refreshed = prompt::build_system_prompt_with_model(
                &self.soul,
                &self.memory,
                &self.skills,
                &self.config,
                &self.resolved.model,
            );
            if let Some(sys_msg) = self.messages.first_mut() {
                sys_msg.content = Some(refreshed);
            }
        }

        Ok(provider_label)
    }

    /// Run one agent loop, returning the result directly (no events).
    async fn run_agent_loop(&mut self) -> Result<ChatResult> {
        self.run_loop(EventSink::Silent).await
    }

    /// Run one agent loop, emitting events through a channel.
    async fn run_agent_loop_events(
        &mut self,
        tx: &mpsc::UnboundedSender<Event>,
    ) -> Result<ChatResult> {
        self.run_loop(EventSink::Channel(tx.clone())).await
    }

    /// Compact the live message window if its estimated token count exceeds the
    /// configured budget. Keeps the system prompt and the most recent turns
    /// verbatim, replacing the older span with a single synthetic summary
    /// message. The full conversation is still preserved on disk (session JSONL)
    /// — this only trims what is re-sent to the model each turn.
    ///
    /// On summarizer failure the window is left untouched (never drops history
    /// uncondensed), so a flaky summary call degrades to a longer prompt rather
    /// than lost context.
    async fn compact_if_needed(&mut self, sink: &EventSink) {
        let cfg = self.config.context_config();
        let max_tokens = cfg.max_tokens;
        let keep_last = cfg.keep_last_turns;

        // max_tokens == 0 disables compaction entirely.
        if max_tokens == 0 || estimate_messages_tokens(&self.messages) <= max_tokens {
            return;
        }

        let Some(split) = compaction_split(&self.messages, keep_last) else {
            return;
        };

        // Clone the span to summarize so we don't hold a borrow across the await.
        let head: Vec<Message> = self.messages[1..split].to_vec();
        let summary = match crate::summary::summarize_for_compaction(&self.client, &head).await {
            Ok(s) if !s.trim().is_empty() => s,
            _ => return,
        };

        let removed = split - 1;
        self.messages = apply_compaction(&self.messages, split, &summary);

        // Surface it the same way streaming output is surfaced: as an event for
        // channel consumers (daemon, TUI), and as a direct stderr notice for the
        // REPL/inline path, which prints rather than consuming events.
        match sink {
            EventSink::Channel(_) => sink.send(Event::Compacted {
                removed_messages: removed,
                budget_tokens: max_tokens,
            }),
            EventSink::Silent => {
                let note = format!(
                    "Compacted {} earlier message(s) into a summary to stay within the context budget (~{} tokens).",
                    removed, max_tokens
                );
                eprintln!("{} {}", "⟡".dimmed(), note.dimmed());
            }
        }
    }

    /// Core agent loop: call model, handle tool_calls, with soft-limit nudging
    /// and a hard turn limit as a safety net.
    async fn run_loop(&mut self, sink: EventSink) -> Result<ChatResult> {
        let max_turns = self.config.max_turns();
        let soft_limit = self.config.soft_limit();
        let tools_payload = self.tools_payload();
        let mut total_tool_calls = 0;
        let mut soft_limit_triggered = false;

        for turn in 0..max_turns {
            let turns_remaining = max_turns.saturating_sub(turn);

            // Roll up older turns into a summary if the live window is over budget.
            self.compact_if_needed(&sink).await;

            if !soft_limit_triggered && soft_limit > 0 && turns_remaining <= soft_limit {
                soft_limit_triggered = true;
                let nudge = format!(
                    "[System] You have {} turns remaining before the hard limit. Wrap up now: produce a final text response without any tool calls.",
                    turns_remaining,
                );
                self.messages.push(Message::user(&nudge));
            }

            let result = if self.no_stream {
                self.client
                    .chat(&self.messages, tools_payload.as_ref())
                    .await?
            } else {
                match &sink {
                    EventSink::Channel(tx) => {
                        self.client
                            .chat_stream_with(&self.messages, tools_payload.as_ref(), |token| {
                                let _ = tx.send(Event::Content {
                                    text: token.to_string(),
                                });
                            })
                            .await?
                    }
                    EventSink::Silent => {
                        self.client
                            .chat_stream(&self.messages, tools_payload.as_ref())
                            .await?
                    }
                }
            };

            if result.has_tool_calls() {
                let tool_calls = result.tool_calls.unwrap();

                // Emit content that came alongside tool calls
                if let Some(ref content) = result.content
                    && !content.is_empty()
                {
                    sink.send(Event::Content {
                        text: content.clone(),
                    });
                }

                for tc in &tool_calls {
                    sink.send(Event::ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    });
                }

                let assistant_msg =
                    Message::assistant_with_tools(tool_calls.clone(), result.content);
                self.session.append(&assistant_msg)?;
                self.messages.push(assistant_msg);

                for tc in &tool_calls {
                    let tc_args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let output = dispatch_tool(
                        &tc.function.name,
                        &tc_args,
                        &mut self.memory,
                        &mut self.mcp,
                        &self.config.allowed_paths(),
                        self.config.allow_unsandboxed_exec(),
                    )
                    .await;
                    sink.send(Event::ToolResult {
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
                if self.no_stream
                    && let Some(ref text) = content
                {
                    sink.send(Event::Content { text: text.clone() });
                }
                if let Some(ref text) = content {
                    self.session.append(&Message::assistant(text))?;
                }
                sink.send(Event::Done);
                return Ok(ChatResult {
                    response: content,
                    tool_calls: total_tool_calls,
                });
            }
        }

        sink.send(Event::Error {
            message: format!("Max agent turns reached ({}).", max_turns),
        });
        anyhow::bail!(
            "Max agent turns reached ({}). The agent exceeded its turn limit without producing a final response.",
            max_turns
        );
    }
}

/// Event sink for the agent loop — either sends events through a channel or drops them.
enum EventSink {
    Channel(mpsc::UnboundedSender<Event>),
    Silent,
}

impl EventSink {
    fn send(&self, event: Event) {
        match self {
            EventSink::Channel(tx) => {
                let _ = tx.send(event);
            }
            EventSink::Silent => {}
        }
    }
}

/// Dispatch a tool call — built-in tools first, then MCP.
async fn dispatch_tool(
    name: &str,
    args: &Value,
    memory: &mut MemoryStore,
    mcp: &mut McpManager,
    allowed_paths: &[PathBuf],
    allow_unsandboxed_exec: bool,
) -> String {
    let built_in_names = [
        "exec_command",
        "read_file",
        "write_file",
        "edit_file",
        "search_files",
        "list_directory",
        "memory",
    ];
    if built_in_names.contains(&name) {
        tools::dispatch(name, args, memory, allowed_paths, allow_unsandboxed_exec)
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

// ── Rolling-context compaction helpers (pure, unit-tested) ──────────

/// Estimate the token footprint of a single message: content plus any tool-call
/// names and arguments. Uses the shared chars÷4 heuristic.
fn estimate_message_tokens(msg: &Message) -> u64 {
    use crate::prompt::inspect::estimate_tokens;
    let mut total = msg.content.as_deref().map(estimate_tokens).unwrap_or(0);
    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            total += estimate_tokens(&tc.function.name);
            total += estimate_tokens(&tc.function.arguments);
        }
    }
    total
}

/// Estimate the total token footprint of a message window.
fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Decide where to split history for compaction. Returns the index at which the
/// kept (verbatim) tail begins: messages `[1..split]` are summarized and
/// `[split..]` are kept, with `messages[0]` (the system prompt) always retained.
///
/// The split is chosen at a clean turn boundary — the first `user` message at or
/// after `len - keep_last_turns` — so a kept `tool` result is never separated
/// from the assistant tool-call message it answers (which OpenAI-compatible APIs
/// reject). Returns `None` when there is nothing safe to compact (too short, or
/// no clean boundary leaves at least one message to summarize).
fn compaction_split(messages: &[Message], keep_last_turns: usize) -> Option<usize> {
    let n = messages.len();
    // Need at least system + 2 messages to have anything worth compacting.
    if n < 3 {
        return None;
    }
    let naive = n.saturating_sub(keep_last_turns).max(1);
    // Walk forward to the first clean user-turn boundary.
    (naive..n)
        .find(|&i| messages[i].role == "user")
        // Need at least one non-system message before the split to summarize.
        .filter(|&i| i >= 2)
}

/// Build the compacted message window: system prompt, a synthetic summary
/// message standing in for `[1..split]`, then the kept tail `[split..]`.
fn apply_compaction(messages: &[Message], split: usize, summary: &str) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages.len() - split + 2);
    out.push(messages[0].clone());
    out.push(Message::user(format!(
        "[Summary of earlier conversation, condensed to save context]\n{}",
        summary
    )));
    out.extend_from_slice(&messages[split..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ToolCall, ToolCallFunction};

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "exec_command".to_string(),
                arguments: "{\"command\":\"ls\"}".to_string(),
            },
        }
    }

    /// system + two tool-using turns + a final plain turn (11 messages).
    fn sample_history() -> Vec<Message> {
        vec![
            Message::system("SYSTEM"),                                 // 0
            Message::user("q1"),                                       // 1
            Message::assistant_with_tools(vec![tool_call("a")], None), // 2
            Message::tool_result("a", "result a"),                     // 3
            Message::assistant("answer1"),                             // 4
            Message::user("q2"),                                       // 5
            Message::assistant_with_tools(vec![tool_call("b")], None), // 6
            Message::tool_result("b", "result b"),                     // 7
            Message::assistant("answer2"),                             // 8
            Message::user("q3"),                                       // 9
            Message::assistant("answer3"),                             // 10
        ]
    }

    #[test]
    fn estimate_tokens_sums_content_and_tool_calls() {
        let msgs = vec![
            Message::user("hello world"), // 11 chars → 3 tokens
            Message::assistant_with_tools(vec![tool_call("a")], None),
        ];
        // Non-zero and equals the sum of the parts.
        let total = estimate_messages_tokens(&msgs);
        assert_eq!(
            total,
            estimate_message_tokens(&msgs[0]) + estimate_message_tokens(&msgs[1])
        );
        assert!(estimate_message_tokens(&msgs[1]) > 0); // tool name+args counted
    }

    #[test]
    fn split_picks_clean_user_boundary() {
        let h = sample_history();
        // keep_last_turns=3 → naive=8 (assistant answer2); next user is index 9.
        let split = compaction_split(&h, 3).unwrap();
        assert_eq!(split, 9);
        assert_eq!(h[split].role, "user");
    }

    #[test]
    fn split_never_orphans_a_tool_result() {
        let h = sample_history();
        // keep_last_turns=4 → naive=7 (a tool result). Must skip forward to the
        // user boundary at 9, never leaving a tool message at the tail head.
        let split = compaction_split(&h, 4).unwrap();
        assert_eq!(h[split].role, "user");
        assert_ne!(h[split].role, "tool");
    }

    #[test]
    fn split_returns_none_when_nothing_to_compact() {
        let h = sample_history();
        // Keeping more than the whole history → no compaction.
        assert!(compaction_split(&h, 100).is_none());
        // keep_last_turns=0 → naive past the end → no boundary.
        assert!(compaction_split(&h, 0).is_none());
        // Too short to bother.
        assert!(compaction_split(&[Message::system("s"), Message::user("u")], 1).is_none());
    }

    #[test]
    fn apply_compaction_preserves_system_and_tail() {
        let h = sample_history();
        let split = compaction_split(&h, 3).unwrap();
        let out = apply_compaction(&h, split, "SUMMARY TEXT");

        // system, summary, then the kept tail (q3, answer3).
        assert_eq!(out.len(), 2 + (h.len() - split));
        assert_eq!(out[0].role, "system");
        assert_eq!(out[0].content.as_deref(), Some("SYSTEM"));
        assert!(out[1].content.as_deref().unwrap().contains("SUMMARY TEXT"));
        // Tail starts at a clean user boundary — no orphaned tool result.
        assert_eq!(out[2].role, "user");
        assert_eq!(out[2].content.as_deref(), Some("q3"));
        assert_eq!(out.last().unwrap().content.as_deref(), Some("answer3"));
    }
}
