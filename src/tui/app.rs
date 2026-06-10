//! TUI application state.

use crate::agent::{AgentSession, SessionInfo};
use crate::memory::MemoryStore;
use crate::protocol::Event;
use crate::skills::SkillsIndex;
use tokio::task::JoinHandle;

use anyhow::Result;

/// Which pane is currently focused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Skills,
    Memory,
    Chat,
    Input,
}

impl Pane {
    pub fn next(self) -> Self {
        match self {
            Self::Skills => Self::Memory,
            Self::Memory => Self::Chat,
            Self::Chat => Self::Input,
            Self::Input => Self::Skills,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Skills => Self::Input,
            Self::Memory => Self::Skills,
            Self::Chat => Self::Memory,
            Self::Input => Self::Chat,
        }
    }
}

/// A single chat line for rendering.
#[derive(Clone, Debug)]
pub enum ChatLine {
    User(String),
    Assistant(String),
    ToolCall {
        name: String,
        #[allow(dead_code)]
        id: String,
    },
    ToolResult {
        #[allow(dead_code)]
        id: String,
        content: String,
    },
    System(String),
    Error(String),
}

/// State for the input bar.
#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub buffer: String,
    pub cursor: usize,
    /// Multi-line mode: Enter adds newline, Ctrl+Enter sends.
    pub multiline: bool,
}

impl InputState {
    pub fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }
}

/// The full TUI application state.
///
/// The agent is held in an `Option<AgentSession>` so it can be moved into a
/// spawned tokio task for real-time streaming. When idle, the agent is `Some`;
/// during streaming, it's `None` and the `JoinHandle` tracks the background task.
///
/// Cached display data (skills, memory) is kept separately so the sidebar
/// can be rendered even while the agent is away streaming.
pub struct App {
    pub agent: Option<AgentSession>,
    /// Handle to the spawned agent task during streaming.
    pub agent_handle: Option<JoinHandle<Result<AgentSession>>>,

    // Cached display data — always available, updated when agent is present
    pub info: SessionInfo,
    /// Cached skills for sidebar rendering.
    pub cached_skills: SkillsIndex,
    /// Cached memory for sidebar rendering.
    pub cached_memory: MemoryStore,

    // Pane state
    pub focus: Pane,
    pub skills_selected: usize,
    pub memory_scroll: usize,
    pub memory_selected: usize,
    pub chat_scroll: usize,
    pub chat_lines: Vec<ChatLine>,
    pub input: InputState,

    // Streaming state
    pub streaming: bool,
    pub current_stream_text: String,

    // Status
    pub error_message: Option<String>,
    pub turn: usize,
    /// Estimated context tokens (system prompt + conversation history).
    pub context_tokens: u64,
    /// Context window size for the current model (None if unknown).
    pub context_budget: Option<u64>,
    /// When true, chat auto-scrolls to bottom (set on new messages, cleared on manual scroll).
    pub chat_auto_scroll: bool,
}

impl App {
    pub fn new(agent: AgentSession) -> Self {
        let info = agent.info();
        let context_tokens = agent.estimated_context_tokens();
        let context_budget = crate::status_bar::model_context_size(&agent.resolved.model);
        let cached_skills = agent.skills.clone();
        let cached_memory = agent.memory.clone();
        Self {
            agent: Some(agent),
            agent_handle: None,
            info,
            cached_skills,
            cached_memory,
            focus: Pane::Input,
            skills_selected: 0,
            memory_scroll: 0,
            memory_selected: 0,
            chat_scroll: 0,
            chat_lines: Vec::new(),
            input: InputState::default(),
            streaming: false,
            current_stream_text: String::new(),
            error_message: None,
            turn: 0,
            context_tokens,
            context_budget,
            chat_auto_scroll: true,
        }
    }

    /// Cache display data from the agent. Call before moving the agent out and
    /// after putting it back.
    pub fn cache_display_data(&mut self) {
        if let Some(agent) = self.agent.as_ref() {
            self.cached_skills = agent.skills.clone();
            self.cached_memory = agent.memory.clone();
        }
    }

    /// Take the agent out, moving it into the caller's possession.
    /// Used before spawning the agent on a background task for streaming.
    pub fn take_agent(&mut self) -> AgentSession {
        // Cache display data before moving agent out
        self.cache_display_data();
        self.agent.take().expect("agent must be present when not streaming")
    }

    /// Put the agent back after recovering it from a spawned task.
    pub fn return_agent(&mut self, agent: AgentSession) {
        self.context_tokens = agent.estimated_context_tokens();
        self.context_budget = crate::status_bar::model_context_size(&agent.resolved.model);
        self.info = agent.info();
        self.cached_skills = agent.skills.clone();
        self.cached_memory = agent.memory.clone();
        self.agent = Some(agent);
    }

    /// Whether the agent is currently away (streaming on a background task).
    pub fn agent_is_away(&self) -> bool {
        self.agent.is_none()
    }

    /// Get a reference to the agent. Panics if the agent is away (streaming).
    /// Only call this when you know the agent is present (i.e., not streaming).
    pub fn get_agent(&self) -> &AgentSession {
        self.agent.as_ref().expect("agent must be present when not streaming")
    }

    /// Get a mutable reference to the agent. Panics if the agent is away (streaming).
    pub fn get_agent_mut(&mut self) -> &mut AgentSession {
        self.agent.as_mut().expect("agent must be present when not streaming")
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Content { text } => {
                self.current_stream_text.push_str(&text);
                self.chat_auto_scroll = true;
            }
            Event::ToolCall {
                id,
                name,
                arguments: _,
            } => {
                // Finalize any streaming text first
                self.finalize_stream();
                self.chat_lines.push(ChatLine::ToolCall { name, id });
                self.chat_auto_scroll = true;
            }
            Event::ToolResult { id, content } => {
                self.chat_lines.push(ChatLine::ToolResult { id, content });
                self.chat_auto_scroll = true;
            }
            Event::Compacted {
                removed_messages,
                budget_tokens,
            } => {
                self.finalize_stream();
                self.chat_lines.push(ChatLine::System(format!(
                    "Compacted {} earlier message(s) to stay within the context budget (~{} tokens).",
                    removed_messages, budget_tokens
                )));
                // context_tokens will be updated when we recover the agent
                self.chat_auto_scroll = true;
            }
            Event::Done => {
                self.finalize_stream();
                self.streaming = false;
                self.turn += 1;
                self.chat_auto_scroll = true;
            }
            Event::Error { message } => {
                self.finalize_stream();
                self.streaming = false;
                self.error_message = Some(message);
                self.chat_auto_scroll = true;
            }
            _ => {}
        }
    }

    pub fn finalize_stream(&mut self) {
        if !self.current_stream_text.is_empty() {
            self.chat_lines.push(ChatLine::Assistant(std::mem::take(
                &mut self.current_stream_text,
            )));
        }
    }

    pub fn refresh_info(&mut self) {
        if let Some(agent) = self.agent.as_ref() {
            self.info = agent.info();
        }
    }
}