//! TUI application state.

use crate::agent::{AgentSession, SessionInfo};
use crate::protocol::Event;

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
pub struct App {
    pub agent: AgentSession,
    pub info: SessionInfo,

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
    /// When true, chat auto-scrolls to bottom (set on new messages, cleared on manual scroll).
    pub chat_auto_scroll: bool,
}

impl App {
    pub fn new(agent: AgentSession) -> Self {
        let info = agent.info();
        Self {
            agent,
            info,
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
            chat_auto_scroll: true,
        }
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
        self.info = self.agent.info();
    }
}
