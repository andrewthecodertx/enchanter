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

    let default_model = agent
        .config
        .model
        .default
        .clone()
        .unwrap_or_else(|| "gpt-4.1-mini".to_string());
    entries.push(ModelEntry {
        name: "default".to_string(),
        model: default_model.clone(),
        is_active: active_model == &default_model,
    });

    for (name, provider) in &agent.config.providers {
        let model = provider
            .model
            .clone()
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

    // ─── Focus navigation ───
    // Tab cycles through all 5 panes and wraps.
    #[test]
    fn test_focus_next_cycles() {
        assert_eq!(Focus::Models.next(), Focus::Sessions);
        assert_eq!(Focus::Sessions.next(), Focus::Skills);
        assert_eq!(Focus::Skills.next(), Focus::Chat);
        assert_eq!(Focus::Chat.next(), Focus::Input);
        assert_eq!(Focus::Input.next(), Focus::Models);
    }

    #[test]
    fn test_focus_prev_cycles() {
        assert_eq!(Focus::Models.prev(), Focus::Input);
        assert_eq!(Focus::Sessions.prev(), Focus::Models);
        assert_eq!(Focus::Skills.prev(), Focus::Sessions);
        assert_eq!(Focus::Chat.prev(), Focus::Skills);
        assert_eq!(Focus::Input.prev(), Focus::Chat);
    }

    // Spatial nav matches the visual layout:
    //   ┌──────────┬─────────────┐
    //   │ Models   │             │
    //   │ Sessions │   Chat      │
    //   │ Skills   │             │
    //   ├──────────┴─────────────┤
    //   │ Input                  │
    //   └────────────────────────┘
    #[test]
    fn test_focus_spatial_left() {
        // From the right column → nearest left pane.
        assert_eq!(Focus::Chat.move_left(), Focus::Skills);
        assert_eq!(Focus::Input.move_left(), Focus::Models);
        // Already on left column — no change.
        assert_eq!(Focus::Models.move_left(), Focus::Models);
        assert_eq!(Focus::Sessions.move_left(), Focus::Sessions);
        assert_eq!(Focus::Skills.move_left(), Focus::Skills);
    }

    #[test]
    fn test_focus_spatial_right() {
        // From the left column → Chat (right column top).
        assert_eq!(Focus::Models.move_right(), Focus::Chat);
        assert_eq!(Focus::Sessions.move_right(), Focus::Chat);
        assert_eq!(Focus::Skills.move_right(), Focus::Chat);
        // From bottom row → Chat.
        assert_eq!(Focus::Input.move_right(), Focus::Chat);
        // Already on right column — no change.
        assert_eq!(Focus::Chat.move_right(), Focus::Chat);
    }

    #[test]
    fn test_focus_spatial_up() {
        // Sidebar vertical nav.
        assert_eq!(Focus::Sessions.move_up(), Focus::Models);
        assert_eq!(Focus::Skills.move_up(), Focus::Sessions);
        // From bottom row → Skills (closest sidebar pane).
        assert_eq!(Focus::Input.move_up(), Focus::Skills);
        // Already at top — no change.
        assert_eq!(Focus::Models.move_up(), Focus::Models);
        assert_eq!(Focus::Chat.move_up(), Focus::Chat);
    }

    #[test]
    fn test_focus_spatial_down() {
        // Sidebar vertical nav.
        assert_eq!(Focus::Models.move_down(), Focus::Sessions);
        assert_eq!(Focus::Sessions.move_down(), Focus::Skills);
        assert_eq!(Focus::Skills.move_down(), Focus::Input);
        // From chat → input.
        assert_eq!(Focus::Chat.move_down(), Focus::Input);
        // Already at bottom — no change.
        assert_eq!(Focus::Input.move_down(), Focus::Input);
    }

    // ─── Chat scroll + auto_scroll state machine ───
    // TuiState without requiring AgentSession. We set only the fields
    // the scroll methods access.
    fn scroll_state(total: usize, visible: usize) -> TuiState {
        let mut s = empty_state();
        s.chat_total_rows = total;
        s.chat_visible_rows = visible;
        s
    }

    // Build a TuiState without needing a real AgentSession — just set the
    // fields the scroll/list logic touches.
    fn empty_state() -> TuiState {
        TuiState {
            focus: Focus::Input,
            models: Vec::new(),
            model_context: HashMap::new(),
            sessions: Vec::new(),
            skills: Vec::new(),
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
            tokens: 0,
            model_name: String::new(),
            session_id: String::new(),
            list_cursor: 0,
            provider_names: Vec::new(),
            pane_areas: PaneAreas::default(),
            spinner_frame: 0,
            has_first_content: false,
        }
    }

    #[test]
    fn test_scroll_up_disables_auto_scroll() {
        let mut s = scroll_state(100, 10);
        assert!(s.auto_scroll);
        s.scroll_chat_up(5);
        assert!(!s.auto_scroll);
        // Scroll position decreased.
        assert_eq!(s.chat_scroll, 0); // 0 - 5 saturates to 0
    }

    #[test]
    fn test_scroll_up_from_nonzero() {
        let mut s = scroll_state(100, 10);
        s.auto_scroll = false;
        s.chat_scroll = 20;
        s.scroll_chat_up(5);
        assert_eq!(s.chat_scroll, 15);
        assert!(!s.auto_scroll);
    }

    #[test]
    fn test_scroll_down_to_bottom_reenables_auto_scroll() {
        let mut s = scroll_state(100, 10);
        // max_scroll = 90
        s.auto_scroll = false;
        s.chat_scroll = 85;
        s.scroll_chat_down(5);
        // 85 + 5 = 90 == max → auto_scroll re-enabled
        assert_eq!(s.chat_scroll, 90);
        assert!(s.auto_scroll);
    }

    #[test]
    fn test_scroll_down_not_to_bottom_stays_manual() {
        let mut s = scroll_state(100, 10);
        s.auto_scroll = false;
        s.chat_scroll = 50;
        s.scroll_chat_down(5);
        assert_eq!(s.chat_scroll, 55);
        assert!(!s.auto_scroll);
    }

    #[test]
    fn test_scroll_down_clamped_at_max() {
        let mut s = scroll_state(100, 10);
        // max = 90. Already at 90, try to scroll more.
        s.chat_scroll = 90;
        s.scroll_chat_down(20);
        assert_eq!(s.chat_scroll, 90);
    }

    #[test]
    fn test_scroll_to_bottom() {
        let mut s = scroll_state(100, 10);
        s.auto_scroll = false;
        s.chat_scroll = 0;
        s.scroll_to_bottom();
        assert!(s.auto_scroll);
        assert_eq!(s.chat_scroll, 90);
    }

    #[test]
    fn test_scroll_when_content_fits_visible_area() {
        // total == visible → max_scroll = 0, no scrolling possible.
        let mut s = scroll_state(10, 10);
        s.scroll_chat_up(5);
        assert_eq!(s.chat_scroll, 0);
        assert!(!s.auto_scroll);
        s.scroll_to_bottom();
        assert_eq!(s.chat_scroll, 0);
    }

    #[test]
    fn test_scroll_when_less_content_than_visible() {
        // total < visible → max_scroll = 0 (saturating_sub).
        let mut s = scroll_state(5, 10);
        s.scroll_chat_up(3);
        assert_eq!(s.chat_scroll, 0);
    }

    // ─── push_chat_line + auto_scroll interaction ───

    #[test]
    fn test_push_chat_line_with_auto_scroll() {
        let mut s = scroll_state(0, 0);
        s.push_chat_line(ChatLine::User("hello".to_string()));
        // auto_scroll is true �� chat_scroll set to MAX (clamped during render).
        assert_eq!(s.chat_scroll, usize::MAX);
        assert_eq!(s.chat_lines.len(), 1);
    }

    #[test]
    fn test_push_chat_line_without_auto_scroll() {
        let mut s = scroll_state(50, 10);
        s.auto_scroll = false;
        s.chat_scroll = 20;
        s.push_chat_line(ChatLine::User("hello".to_string()));
        // auto_scroll disabled → scroll position unchanged.
        assert_eq!(s.chat_scroll, 20);
    }

    // ─── append_to_last_assistant (streaming) ───

    #[test]
    fn test_append_to_last_assistant_existing() {
        let mut s = empty_state();
        s.push_chat_line(ChatLine::Assistant("Hello".to_string()));
        s.append_to_last_assistant(" world");
        match s.chat_lines.last() {
            Some(ChatLine::Assistant(text)) => assert_eq!(text, "Hello world"),
            _ => panic!("expected Assistant line"),
        }
    }

    #[test]
    fn test_append_to_last_assistant_no_existing() {
        let mut s = empty_state();
        // No prior assistant line → creates one.
        s.append_to_last_assistant("first token");
        match s.chat_lines.last() {
            Some(ChatLine::Assistant(text)) => assert_eq!(text, "first token"),
            _ => panic!("expected Assistant line"),
        }
    }

    #[test]
    fn test_append_to_last_assistant_after_user_line() {
        let mut s = empty_state();
        s.push_chat_line(ChatLine::User("question".to_string()));
        // Last line is User, not Assistant → should create new Assistant.
        s.append_to_last_assistant("answer");
        assert_eq!(s.chat_lines.len(), 2);
        match s.chat_lines.last() {
            Some(ChatLine::Assistant(text)) => assert_eq!(text, "answer"),
            _ => panic!("expected Assistant line"),
        }
    }

    #[test]
    fn test_append_to_last_assistant_auto_scroll() {
        let mut s = empty_state();
        s.push_chat_line(ChatLine::Assistant("Hello".to_string()));
        s.chat_scroll = 0; // reset after push
        s.append_to_last_assistant(" world");
        // auto_scroll is true → scroll set to MAX.
        assert_eq!(s.chat_scroll, usize::MAX);
    }

    // ─── list navigation ───

    #[test]
    fn test_list_up_down_bounds() {
        use std::path::PathBuf;
        let mk = |name: &str| Skill {
            name: name.into(),
            description: "".into(),
            body: "".into(),
            category: Some("x".into()),
            path: PathBuf::from(format!("/{}", name)),
        };
        let mut s = empty_state();
        s.skills = vec![mk("a"), mk("b"), mk("c")];
        s.focus = Focus::Skills;
        assert_eq!(s.current_list_len(), 3);
        s.list_down(3);
        assert_eq!(s.list_cursor, 1);
        s.list_down(3);
        assert_eq!(s.list_cursor, 2);
        s.list_down(3); // at len-1, no further
        assert_eq!(s.list_cursor, 2);
        s.list_up();
        assert_eq!(s.list_cursor, 1);
        s.list_up();
        s.list_up();
        assert_eq!(s.list_cursor, 0); // clamped at 0
        s.list_up();
        assert_eq!(s.list_cursor, 0);
    }

    #[test]
    fn test_list_down_empty() {
        let mut s = empty_state();
        s.focus = Focus::Skills;
        // len=0 → list_down should be a no-op.
        s.list_down(0);
        assert_eq!(s.list_cursor, 0);
    }

    #[test]
    fn test_current_list_len_matches_focus() {
        let mut s = empty_state();
        s.models = vec![ModelEntry {
            name: "default".into(),
            model: "gpt-4".into(),
            is_active: true,
        }];
        s.sessions = vec![
            SessionEntry {
                id: "abc".into(),
                message_count: 5,
                started_at: None,
            },
            SessionEntry {
                id: "def".into(),
                message_count: 3,
                started_at: None,
            },
        ];
        s.focus = Focus::Models;
        assert_eq!(s.current_list_len(), 1);
        s.focus = Focus::Sessions;
        assert_eq!(s.current_list_len(), 2);
        s.focus = Focus::Skills;
        assert_eq!(s.current_list_len(), 0);
        s.focus = Focus::Chat;
        assert_eq!(s.current_list_len(), 0);
        s.focus = Focus::Input;
        assert_eq!(s.current_list_len(), 0);
    }

    #[test]
    fn test_selected_model_and_session() {
        let mut s = empty_state();
        s.models = vec![
            ModelEntry {
                name: "default".into(),
                model: "gpt-4".into(),
                is_active: true,
            },
            ModelEntry {
                name: "openai".into(),
                model: "o3".into(),
                is_active: false,
            },
        ];
        s.sessions = vec![SessionEntry {
            id: "s1".into(),
            message_count: 1,
            started_at: None,
        }];
        s.list_cursor = 0;
        assert_eq!(s.selected_model().map(|m| m.name.as_str()), Some("default"));
        s.list_cursor = 1;
        assert_eq!(s.selected_model().map(|m| m.name.as_str()), Some("openai"));
        s.list_cursor = 5;
        assert!(s.selected_model().is_none());

        s.list_cursor = 0;
        assert_eq!(s.selected_session().map(|s| s.id.as_str()), Some("s1"));
        s.list_cursor = 5;
        assert!(s.selected_session().is_none());
    }

    #[test]
    fn test_reset_list_cursor() {
        let mut s = empty_state();
        s.list_cursor = 7;
        s.reset_list_cursor();
        assert_eq!(s.list_cursor, 0);
    }
}
