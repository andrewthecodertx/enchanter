//! TUI state — all mutable state for the pane-based UI.
//!
//! Sidebar has three stacked panes: Models, Sessions, Skills.
//! The main area has Chat (conversation output) and Input (text entry).
//! A status bar runs across the bottom.

use std::collections::HashMap;

use ratatui::layout::Rect;

use crate::agent::AgentSession;
use crate::session::SessionMeta;
use crate::skills::Skill;

/// Rectangular areas for each pane, stored during render so mouse clicks
/// can hit-test against them.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaneAreas {
    pub models: Rect,
    pub sessions: Rect,
    pub skills: Rect,
    pub chat: Rect,
    pub input: Rect,
}

impl PaneAreas {
    /// Find which pane contains the given (column, row) coordinate.
    /// Returns None if the click is outside all panes (e.g. on the status bar).
    pub fn hit_test(&self, col: u16, row: u16) -> Option<Focus> {
        let point = (col, row);
        if self.models.contains_point(point) {
            Some(Focus::Models)
        } else if self.sessions.contains_point(point) {
            Some(Focus::Sessions)
        } else if self.skills.contains_point(point) {
            Some(Focus::Skills)
        } else if self.chat.contains_point(point) {
            Some(Focus::Chat)
        } else if self.input.contains_point(point) {
            Some(Focus::Input)
        } else {
            None
        }
    }

    /// Compute the list item index for a click within a sidebar pane.
    /// Returns None if the click is on the border or outside the list area.
    pub fn list_index_for_click(&self, focus: Focus, row: u16) -> Option<usize> {
        let pane = match focus {
            Focus::Models => self.models,
            Focus::Sessions => self.sessions,
            Focus::Skills => self.skills,
            _ => return None,
        };
        // List items start 1 row below the pane top (inside the border).
        // The bottom border is the last row of the pane.
        if row <= pane.y || row >= pane.y + pane.height {
            return None;
        }
        let index = (row - pane.y - 1) as usize;
        Some(index)
    }
}

/// Extension trait — ratatui's Rect doesn't have contains_point.
trait RectContains {
    fn contains_point(&self, point: (u16, u16)) -> bool;
}

impl RectContains for Rect {
    fn contains_point(&self, (col, row): (u16, u16)) -> bool {
        col >= self.x && col < self.x + self.width && row >= self.y && row < self.y + self.height
    }
}

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

    /// Spatial navigation matching the visual layout:
    ///
    /// ```text
    /// ┌──────────┬─────────────┐
    /// │ Models   │             │
    /// │ Sessions │   Chat      │
    /// │ Skills   │             │
    /// ├──────────┴─────────────┤
    /// │ Input                  │
    /// └────────────────────────┘
    /// ```
    pub fn move_left(self) -> Self {
        match self {
            Focus::Chat => Focus::Skills,
            Focus::Input => Focus::Models,
            other => other,
        }
    }

    pub fn move_right(self) -> Self {
        match self {
            Focus::Models | Focus::Sessions | Focus::Skills => Focus::Chat,
            Focus::Input => Focus::Chat,
            other => other,
        }
    }

    pub fn move_up(self) -> Self {
        match self {
            Focus::Sessions => Focus::Models,
            Focus::Skills => Focus::Sessions,
            Focus::Input => Focus::Skills,
            other => other,
        }
    }

    pub fn move_down(self) -> Self {
        match self {
            Focus::Models => Focus::Sessions,
            Focus::Sessions => Focus::Skills,
            Focus::Skills => Focus::Input,
            Focus::Chat => Focus::Input,
            other => other,
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
    /// Scroll offset (visual rows from the top of chat content).
    pub chat_scroll: usize,
    /// When true, chat auto-scrolls to show newest content. Disabled on manual scroll,
    /// re-enabled when new content arrives or user presses End.
    pub auto_scroll: bool,
    /// Total rendered rows in the chat (set during render for scroll clamping).
    pub chat_total_rows: usize,
    /// Visible rows in the chat area (set during render for scroll clamping).
    pub chat_visible_rows: usize,
    pub input_buffer: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub status_message: String,
    pub is_streaming: bool,
    /// When true, the user requested quit while streaming — break after agent recovers.
    pub pending_quit: bool,
    pub tokens: u64,
    pub model_name: String,
    pub session_id: String,
    /// Cursor position within the currently focused sidebar list.
    pub list_cursor: usize,
    /// Provider names from config (for model switching).
    pub provider_names: Vec<String>,
    /// Pane areas stored during render for mouse hit-testing.
    pub pane_areas: PaneAreas,
    /// Animation frame for the thinking spinner (0-3, cycles).
    pub spinner_frame: u8,
    /// True once we receive the first content/tool event during streaming.
    /// Used to distinguish "thinking" (waiting for first token) from "streaming" (actively receiving).
    pub has_first_content: bool,
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
            auto_scroll: true,
            chat_total_rows: 0,
            chat_visible_rows: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            status_message: String::new(),
            is_streaming: false,
            pending_quit: false,
            tokens,
            model_name,
            session_id,
            list_cursor: 0,
            provider_names,
            pane_areas: PaneAreas::default(),
            spinner_frame: 0,
            has_first_content: false,
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

    /// Scroll chat toward older messages (content moves down).
    pub fn scroll_chat_up(&mut self, n: usize) {
        self.auto_scroll = false;
        self.chat_scroll = self.chat_scroll.saturating_sub(n);
    }

    /// Scroll chat toward newer messages (content moves up).
    pub fn scroll_chat_down(&mut self, n: usize) {
        let max = self.max_scroll();
        self.chat_scroll = (self.chat_scroll + n).min(max);
        // Re-enable auto-scroll if we hit the bottom.
        if self.chat_scroll >= max {
            self.auto_scroll = true;
        }
    }

    /// Jump to bottom (newest content).
    pub fn scroll_to_bottom(&mut self) {
        self.auto_scroll = true;
        self.chat_scroll = self.max_scroll();
    }

    /// Maximum scroll offset — content rows minus visible rows (clamped to 0).
    fn max_scroll(&self) -> usize {
        self.chat_total_rows.saturating_sub(self.chat_visible_rows)
    }

    /// Update token count after a turn.
    pub fn update_tokens(&mut self, agent: &AgentSession) {
        self.tokens = agent.estimated_context_tokens();
    }

    /// Push a chat line. If auto_scroll is enabled, jumps to bottom.
    pub fn push_chat_line(&mut self, line: ChatLine) {
        self.chat_lines.push(line);
        if self.auto_scroll {
            // chat_scroll will be clamped to max during render after row count updates.
            // Set it high so render clamps it to bottom.
            self.chat_scroll = usize::MAX;
        }
    }

    /// Append text to the last assistant line (for streaming).
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(ChatLine::Assistant(content)) = self.chat_lines.last_mut() {
            content.push_str(text);
            if self.auto_scroll {
                self.chat_scroll = usize::MAX;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn areas() -> PaneAreas {
        PaneAreas {
            models: Rect::new(0, 0, 24, 10),
            sessions: Rect::new(0, 10, 24, 10),
            skills: Rect::new(0, 20, 24, 10),
            chat: Rect::new(24, 0, 60, 30),
            input: Rect::new(0, 30, 84, 5),
        }
    }

    #[test]
    fn test_hit_test_models() {
        let a = areas();
        assert_eq!(a.hit_test(1, 1), Some(Focus::Models));
        assert_eq!(a.hit_test(23, 9), Some(Focus::Models));
    }

    #[test]
    fn test_hit_test_sessions() {
        let a = areas();
        assert_eq!(a.hit_test(1, 11), Some(Focus::Sessions));
        assert_eq!(a.hit_test(1, 19), Some(Focus::Sessions));
    }

    #[test]
    fn test_hit_test_skills() {
        let a = areas();
        assert_eq!(a.hit_test(1, 21), Some(Focus::Skills));
    }

    #[test]
    fn test_hit_test_chat() {
        let a = areas();
        assert_eq!(a.hit_test(25, 5), Some(Focus::Chat));
        assert_eq!(a.hit_test(83, 29), Some(Focus::Chat));
    }

    #[test]
    fn test_hit_test_input() {
        let a = areas();
        assert_eq!(a.hit_test(0, 31), Some(Focus::Input));
    }

    #[test]
    fn test_hit_test_outside() {
        let a = areas();
        // Status bar area (below input)
        assert_eq!(a.hit_test(0, 36), None);
        // Outside all panes
        assert_eq!(a.hit_test(100, 100), None);
    }

    #[test]
    fn test_list_index_for_click() {
        let a = areas();
        // First item in models pane (row 1 = first item, row 0 = border)
        assert_eq!(a.list_index_for_click(Focus::Models, 1), Some(0));
        assert_eq!(a.list_index_for_click(Focus::Models, 5), Some(4));
        // On the top border (row 0)
        assert_eq!(a.list_index_for_click(Focus::Models, 0), None);
        // On the bottom border (row 9 = models.y + models.height - 1)
        assert_eq!(a.list_index_for_click(Focus::Models, 10), None);
    }

    #[test]
    fn test_list_index_for_click_sessions() {
        let a = areas();
        assert_eq!(a.list_index_for_click(Focus::Sessions, 11), Some(0));
        assert_eq!(a.list_index_for_click(Focus::Sessions, 13), Some(2));
    }

    #[test]
    fn test_list_index_for_click_non_sidebar() {
        let a = areas();
        assert_eq!(a.list_index_for_click(Focus::Chat, 5), None);
        assert_eq!(a.list_index_for_click(Focus::Input, 5), None);
    }
}