//! TUI state — all mutable state for the pane-based UI.
//!
//! Sidebar has three stacked panes: Models, Sessions, Skills.
//! The main area has Chat (conversation output) and Input (text entry).
//! A status bar runs across the bottom.

use std::collections::HashMap;

use crate::agent::AgentSession;
use crate::session::SessionMeta;
use crate::skills::Skill;

/// Which pane is focused for keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Models,
    Sessions,
    Skills,
    Chat,
    Input,
}

impl Focus {
    /// Cycle focus: Models → Sessions → Skills → Chat → Input → Models.
    pub fn next(self) -> Self {
        match self {
            Focus::Models => Focus::Sessions,
            Focus::Sessions => Focus::Skills,
            Focus::Skills => Focus::Chat,
            Focus::Chat => Focus::Input,
            Focus::Input => Focus::Models,
        }
    }

    /// Reverse cycle.
    pub fn prev(self) -> Self {
        match self {
            Focus::Models => Focus::Input,
            Focus::Sessions => Focus::Models,
            Focus::Skills => Focus::Sessions,
            Focus::Chat => Focus::Skills,
            Focus::Input => Focus::Chat,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Focus::Models => "models",
            Focus::Sessions => "sessions",
            Focus::Skills => "skills",
            Focus::Chat => "chat",
            Focus::Input => "input",
        }
    }
}

/// Model entry for the models pane.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub name: String,
    pub model: String,
    pub is_active: bool,
}

/// Chat line for the conversation view.
#[derive(Debug, Clone)]
pub enum ChatLine {
    User(String),
    Assistant(String),
    ToolCall(String),
    ToolResult(String, String),
    System(String),
    Compacted(String),
    Error(String),
}

/// Session entry for the sessions pane.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub message_count: usize,
    pub started_at: Option<String>,
}

/// Context info queried from the provider's /models endpoint.
#[derive(Debug, Clone, Default)]
pub struct ModelContextInfo {
    pub context_size: Option<u64>,
    pub source: ContextSource,
}

#[derive(Debug, Clone, Default)]
pub enum ContextSource {
    #[default]
    Hardcoded,
    ApiQuery,
    Config,
}

/// All state for the TUI.
pub struct TuiState {
    pub focus: Focus,
    pub models: Vec<ModelEntry>,
    pub model_context: HashMap<String, ModelContextInfo>,
    pub sessions: Vec<SessionEntry>,
    pub skills: Vec<Skill>,
    pub chat_lines: Vec<ChatLine>,
    pub chat_scroll: usize,
    pub input_buffer: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub status_message: String,
    pub is_streaming: bool,
    pub tokens: u64,
    pub model_name: String,
    pub session_id: String,
    /// Cursor position within the currently focused sidebar list.
    pub list_cursor: usize,
    /// Provider names from config (for model switching).
    pub provider_names: Vec<String>,
}

impl TuiState {
    pub fn new(agent: &AgentSession) -> Self {
        let models = build_model_list(agent);
        let sessions = build_session_list();
        let skills = agent.skills.skills.clone();
        let model_name = agent.resolved.model.clone();
        let session_id = agent.session.id().to_string();
        let tokens = agent.estimated_context_tokens();
        let provider_names: Vec<String> = agent.config.providers.keys().cloned().collect();

        Self {
            focus: Focus::Input,
            models,
            model_context: HashMap::new(),
            sessions,
            skills,
            chat_lines: Vec::new(),
            chat_scroll: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            status_message: String::new(),
            is_streaming: false,
            tokens,
            model_name,
            session_id,
            list_cursor: 0,
            provider_names,
        }
    }

    /// Get the context budget for the current model.
    pub fn context_budget(&self) -> Option<u64> {
        if let Some(info) = self.model_context.get(&self.model_name) {
            if let Some(size) = info.context_size {
                return Some(size);
            }
        }
        crate::status_bar::model_context_size(&self.model_name)
    }

    /// Scroll chat up by n lines.
    pub fn scroll_chat_up(&mut self, n: usize) {
        self.chat_scroll = self.chat_scroll.saturating_add(n);
    }

    /// Scroll chat down by n lines.
    pub fn scroll_chat_down(&mut self, n: usize) {
        self.chat_scroll = self.chat_scroll.saturating_sub(n);
    }

    /// Update token count after a turn.
    pub fn update_tokens(&mut self, agent: &AgentSession) {
        self.tokens = agent.estimated_context_tokens();
    }

    /// Push a chat line and auto-scroll to bottom.
    pub fn push_chat_line(&mut self, line: ChatLine) {
        self.chat_lines.push(line);
        self.chat_scroll = 0;
    }

    /// Append text to the last assistant line (for streaming).
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(ChatLine::Assistant(content)) = self.chat_lines.last_mut() {
            content.push_str(text);
        } else {
            self.push_chat_line(ChatLine::Assistant(text.to_string()));
        }
    }

    /// Move the sidebar list cursor up.
    pub fn list_up(&mut self) {
        if self.list_cursor > 0 {
            self.list_cursor -= 1;
        }
    }

    /// Move the sidebar list cursor down.
    pub fn list_down(&mut self, len: usize) {
        if len > 0 && self.list_cursor < len - 1 {
            self.list_cursor += 1;
        }
    }

    /// Reset the list cursor when switching panes.
    pub fn reset_list_cursor(&mut self) {
        self.list_cursor = 0;
    }

    /// Get the selected model name based on the cursor.
    pub fn selected_model(&self) -> Option<&ModelEntry> {
        self.models.get(self.list_cursor)
    }

    /// Get the selected session based on the cursor.
    pub fn selected_session(&self) -> Option<&SessionEntry> {
        self.sessions.get(self.list_cursor)
    }

    /// Get the selected skill based on the cursor.
    pub fn selected_skill(&self) -> Option<&Skill> {
        self.skills.get(self.list_cursor)
    }

    /// Current list length based on focus.
    pub fn current_list_len(&self) -> usize {
        match self.focus {
            Focus::Models => self.models.len(),
            Focus::Sessions => self.sessions.len(),
            Focus::Skills => self.skills.len(),
            _ => 0,
        }
    }
}

/// Build the model list from config providers + default.
pub fn build_model_list(agent: &AgentSession) -> Vec<ModelEntry> {
    let active_model = &agent.resolved.model;
    let mut entries = Vec::new();

    let default_model = agent.config.model.default.clone()
        .unwrap_or_else(|| "gpt-4.1-mini".to_string());
    entries.push(ModelEntry {
        name: "default".to_string(),
        model: default_model.clone(),
        is_active: active_model == &default_model,
    });

    for (name, provider) in &agent.config.providers {
        let model = provider.model.clone()
            .unwrap_or_else(|| default_model.clone());
        let is_active = active_model == &model || active_model == name;
        entries.push(ModelEntry {
            name: name.clone(),
            model,
            is_active,
        });
    }

    entries
}

/// Build the session list from the sessions directory.
pub fn build_session_list() -> Vec<SessionEntry> {
    match crate::session::Session::list_all() {
        Ok(sessions) => sessions
            .into_iter()
            .map(|s: SessionMeta| SessionEntry {
                id: s.id,
                message_count: s.message_count,
                started_at: s.started_at,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}