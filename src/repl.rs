//! Crossterm-based REPL with a pinned status bar at the bottom of the terminal.
//!
//! Layout:
//! ┌──────────────────────────────────┐
//! │  chat output (scrollable)         │
//! │  ...                              │
//! │  ...                              │
//! ├──────────────────────────────────┤
//! │  ⟩ user input line               │
//! ├──────────────────────────────────┤
//! │  ⟡ model │ turn N │ STREAM 5s   │  ← status bar (1 line, non-interactive)
//! └──────────────────────────────────┘
//!
//! The status bar reads from `SharedStatus` which the REPL updates as events
//! arrive from the agent's streaming loop. Stuck-detection thresholds surface
//! ⚠ warnings when the agent appears stalled.

use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event as CrosstermEvent, KeyCode, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::AgentSession;
use crate::protocol::Event;
use crate::status_bar::{self, AgentPhase, SharedStatus};

/// Update context token estimate in the shared status from the agent's messages.
fn sync_context_tokens(agent: &AgentSession, status: &SharedStatus) {
    let tokens = agent.estimated_context_tokens();
    if let Ok(mut s) = status.lock() {
        s.context_tokens = tokens;
    }
}

// ── Input line ──

struct InputLine {
    buffer: String,
    cursor: usize,
}

impl InputLine {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }
}

// ── Chat history ──

const MAX_CHAT_LINES: usize = 10_000;

struct ChatBuffer {
    lines: Vec<String>,
}

impl ChatBuffer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() >= MAX_CHAT_LINES {
            self.lines.remove(0);
        }
        self.lines.push(line);
    }

    fn len(&self) -> usize {
        self.lines.len()
    }
}

// ── Screen layout ──

// 1 line for input prompt + 1 line for status bar
const FIXED_ROWS: u16 = 2;

fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

fn chat_rows(term_height: u16) -> u16 {
    term_height.saturating_sub(FIXED_ROWS)
}

// ── Drawing ──

fn draw_chat<W: Write>(
    w: &mut W,
    chat: &ChatBuffer,
    scroll_offset: usize,
    rows: u16,
) -> Result<()> {
    let row_count = rows as usize;
    if row_count == 0 {
        return Ok(());
    }

    let total = chat.len();
    let start = if total <= row_count {
        0
    } else if scroll_offset + row_count > total {
        total.saturating_sub(row_count)
    } else {
        scroll_offset
    };
    let end = (start + row_count).min(total);

    queue!(w, terminal::Clear(ClearType::All))?;

    for (i, row_idx) in (start..end).enumerate() {
        let line = chat.lines.get(row_idx).map(|s| s.as_str()).unwrap_or("");
        queue!(
            w,
            cursor::MoveTo(0, i as u16),
            Print(truncate_line(line, 500)),
        )?;
    }

    Ok(())
}

fn draw_input<W: Write>(w: &mut W, input: &InputLine, row: u16) -> Result<()> {
    queue!(
        w,
        cursor::MoveTo(0, row),
        terminal::Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::Cyan),
        Print("⟩ "),
        SetAttribute(Attribute::Reset),
        Print(&input.buffer),
    )?;
    queue!(w, cursor::MoveTo(2 + input.cursor as u16, row))?;
    Ok(())
}

fn draw_status_bar<W: Write>(
    w: &mut W,
    status: &status_bar::AgentStatus,
    row: u16,
    width: u16,
) -> Result<()> {
    let (text, is_warning) = status_bar::render(status, width);

    queue!(w, cursor::MoveTo(0, row))?;

    if is_warning {
        queue!(
            w,
            SetBackgroundColor(Color::Red),
            SetForegroundColor(Color::White),
            SetAttribute(Attribute::Bold),
            Print(&text),
            SetAttribute(Attribute::Reset),
        )?;
    } else {
        queue!(
            w,
            SetBackgroundColor(Color::DarkGrey),
            SetForegroundColor(Color::Grey),
            Print(&text),
            SetAttribute(Attribute::Reset),
        )?;
    }

    w.flush()?;
    Ok(())
}

fn redraw<W: Write>(
    w: &mut W,
    chat: &ChatBuffer,
    scroll_offset: usize,
    input: &InputLine,
    status: &status_bar::AgentStatus,
) -> Result<()> {
    let (width, height) = terminal_size();
    let chat_height = chat_rows(height);

    draw_chat(w, chat, scroll_offset, chat_height)?;
    draw_input(w, input, chat_height)?;

    let status_row = height.saturating_sub(1);
    draw_status_bar(w, status, status_row, width)?;

    w.flush()?;
    Ok(())
}

fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{}…", truncated)
    }
}

// ── Main REPL loop ──

pub async fn run_repl(
    agent: &mut AgentSession,
    status: SharedStatus,
) -> Result<()> {
    let mut chat = ChatBuffer::new();
    let mut input = InputLine::new();
    let mut scroll_offset: usize = 0;
    let mut auto_scroll = true;

    // Set model name and context budget in status
    {
        let mut s = status.lock().unwrap();
        s.model_short = shorten_model(&agent.resolved.model);
        s.context_budget = status_bar::model_context_size(&agent.resolved.model);
        s.context_tokens = agent.estimated_context_tokens();
    }

    // Banner
    let info = agent.info();
    let short_url = info.base_url.trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq");
    chat.push(format!("⟡ Enchanter v{}  session={}", env!("CARGO_PKG_VERSION"), &info.session_id[..8.min(info.session_id.len())]));
    chat.push(format!("  model={} | provider={} | tools={} | /help for commands", info.model, short_url, info.tool_count));
    chat.push(String::new());

    // Enter alternate screen + raw mode
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    // Initial draw
    {
        let s = status.lock().unwrap();
        redraw(&mut stdout, &chat, scroll_offset, &input, &s)?;
    }

    // Event receiver (set during streaming, None when idle)
    let mut event_rx: Option<UnboundedReceiver<Event>> = None;

    loop {
        if let Some(rx) = &mut event_rx {
            // ── Streaming: drain events, update chat & status ──
            drain_events(rx, &mut chat, &status);

            // Back to connecting after tools finish
            if event_rx.is_some() {
                let s = status.lock().unwrap();
                if matches!(s.phase, AgentPhase::Idle) {
                    event_rx = None;
                    // Update context tokens now that the turn is complete
                    drop(s);
                    sync_context_tokens(agent, &status);
                }
            }

            // Recalculate scroll
            if auto_scroll {
                scroll_offset = calc_auto_scroll(&chat);
            }

            // Redraw
            {
                let s = status.lock().unwrap();
                redraw(&mut stdout, &chat, scroll_offset, &input, &s)?;
            }

            // Check for terminal events (Ctrl+C to cancel, scroll)
            if event_rx.is_some() {
                if event::poll(Duration::from_millis(0))? {
                    if let CrosstermEvent::Key(key) = event::read()? {
                        match (key.modifiers, key.code) {
                            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                                chat.push("── cancelled ──".to_string());
                                {
                                    let mut s = status.lock().unwrap();
                                    s.phase = AgentPhase::Idle;
                                }
                                event_rx = None;
                                auto_scroll = true;
                            }
                            (KeyModifiers::NONE, KeyCode::Up) => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_sub(1);
                            }
                            (KeyModifiers::NONE, KeyCode::Down) => {
                                scroll_offset += 1;
                                let max = max_scroll(&chat);
                                if scroll_offset >= max {
                                    scroll_offset = max;
                                    auto_scroll = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else {
            // ── Idle: wait for user input ──
            {
                let s = status.lock().unwrap();
                redraw(&mut stdout, &chat, scroll_offset, &input, &s)?;
            }

            if event::poll(Duration::from_millis(200))? {
                let ev = event::read()?;
                if let CrosstermEvent::Key(key) = ev {
                    let action = handle_idle_key(key, &mut input, &mut chat, &mut scroll_offset, &mut auto_scroll, agent, &status);

                    // Recalculate scroll after any chat changes
                    if auto_scroll {
                        scroll_offset = calc_auto_scroll(&chat);
                    }

                    match action {
                        IdleAction::Continue => {
                            let s = status.lock().unwrap();
                            redraw(&mut stdout, &chat, scroll_offset, &input, &s)?;
                        }
                        IdleAction::Send(msg) => {
                            chat.push(format!("⟩ {}", msg));
                            auto_scroll = true;
                            scroll_offset = calc_auto_scroll(&chat);

                            {
                                let mut s = status.lock().unwrap();
                                s.phase = AgentPhase::Connecting;
                                s.phase_started = std::time::Instant::now();
                                s.stream_chars = 0;
                                s.tool_calls_this_turn = 0;
                            }

                            {
                                let s = status.lock().unwrap();
                                redraw(&mut stdout, &chat, scroll_offset, &input, &s)?;
                            }

                            match agent.chat_events(&msg).await {
                                Ok((_result, rx)) => {
                                    event_rx = Some(rx);
                                }
                                Err(e) => {
                                    chat.push(format!("✗ {}", e));
                                    {
                                        let mut s = status.lock().unwrap();
                                        s.phase = AgentPhase::Idle;
                                    }
                                    auto_scroll = true;
                                }
                            }
                        }
                        IdleAction::Quit => break,
                    }
                } else if let CrosstermEvent::Resize(_, _) = &ev {
                    // Redraw on resize
                }
            }
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    execute!(stdout, LeaveAlternateScreen)?;

    Ok(())
}

// ── Event draining during streaming ──

fn drain_events(rx: &mut UnboundedReceiver<Event>, chat: &mut ChatBuffer, status: &SharedStatus) {
    loop {
        match rx.try_recv() {
            Ok(event) => match event {
                Event::Content { text } => {
                    // Append streaming text
                    let is_new_line = chat.lines.last().is_none_or(|l| {
                        l.starts_with('⟩') || l.is_empty() || l.starts_with("│") || l.starts_with("──")
                    });
                    for (i, line) in text.lines().enumerate() {
                        if i == 0 && is_new_line {
                            chat.push(format!("⟨ {}", line));
                        } else if i == 0 {
                            if let Some(last) = chat.lines.last_mut() {
                                last.push_str(line);
                            } else {
                                chat.push(format!("⟨ {}", line));
                            }
                        } else {
                            chat.push(format!("  {}", line));
                        }
                    }
                    if text.ends_with('\n') {
                        chat.push(String::new());
                    }

                    let mut s = status.lock().unwrap();
                    s.stream_chars += text.len();
                    s.phase = AgentPhase::Streaming;
                }
                Event::ToolCall { name, id: _, arguments: _ } => {
                    chat.push(format!("  ⟩ {}", name));
                    let mut s = status.lock().unwrap();
                    s.phase = AgentPhase::ToolRunning { name: name.clone() };
                    s.phase_started = std::time::Instant::now();
                    s.tool_calls_this_turn += 1;
                    s.total_tool_calls += 1;
                }
                Event::ToolResult { id: _, content } => {
                    for line in content.lines().take(8) {
                        chat.push(format!("│ {}", line));
                    }
                    let total_lines = content.lines().count();
                    if total_lines > 8 {
                        chat.push(format!("│ ... ({} more)", total_lines - 8));
                    }
                    let mut s = status.lock().unwrap();
                    s.phase = AgentPhase::Connecting;
                    s.phase_started = std::time::Instant::now();
                }
                Event::Compacted { removed_messages, budget_tokens } => {
                    chat.push(format!("── compacted {} messages (~{} tokens) ──", removed_messages, budget_tokens));
                }
                Event::Done => {
                    let mut s = status.lock().unwrap();
                    s.phase = AgentPhase::Idle;
                    s.turn += 1;
                    s.stream_chars = 0;
                    return; // Done — exit drain
                }
                Event::Error { message } => {
                    chat.push(format!("✗ {}", message));
                    let mut s = status.lock().unwrap();
                    s.phase = AgentPhase::Idle;
                    return;
                }
                _ => {}
            },
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                let mut s = status.lock().unwrap();
                s.phase = AgentPhase::Idle;
                return;
            }
        }
    }
}

// ── Idle key handling ──

enum IdleAction {
    Continue,
    Send(String),
    Quit,
}

#[allow(clippy::too_many_arguments)]
fn handle_idle_key(
    key: crossterm::event::KeyEvent,
    input: &mut InputLine,
    chat: &mut ChatBuffer,
    scroll_offset: &mut usize,
    auto_scroll: &mut bool,
    agent: &mut AgentSession,
    status: &SharedStatus,
) -> IdleAction {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if !input.buffer.is_empty() {
                let msg = input.take();
                // Slash commands
                if msg.starts_with('/') {
                    if handle_slash_command(&msg, agent, chat, status) {
                        return IdleAction::Quit;
                    }
                    *auto_scroll = true;
                    return IdleAction::Continue;
                }
                IdleAction::Send(msg)
            } else {
                IdleAction::Continue
            }
        }
        (_, KeyCode::Char(c)) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            input.insert(c);
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Backspace) => {
            input.backspace();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Delete) => {
            input.delete();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Left) => {
            input.move_left();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Right) => {
            input.move_right();
            IdleAction::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            input.move_home();
            IdleAction::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            input.move_end();
            IdleAction::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            input.clear();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Home) => {
            input.move_home();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::End) => {
            input.move_end();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Esc) => {
            input.clear();
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Up) => {
            *auto_scroll = false;
            *scroll_offset = scroll_offset.saturating_sub(3);
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::Down) => {
            *scroll_offset += 3;
            let max = max_scroll(chat);
            if *scroll_offset >= max {
                *scroll_offset = max;
                *auto_scroll = true;
            }
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::PageUp) => {
            *auto_scroll = false;
            let (_, height) = terminal_size();
            let jump = chat_rows(height) as usize;
            *scroll_offset = scroll_offset.saturating_sub(jump);
            IdleAction::Continue
        }
        (KeyModifiers::NONE, KeyCode::PageDown) => {
            let (_, height) = terminal_size();
            let jump = chat_rows(height) as usize;
            *scroll_offset += jump;
            let max = max_scroll(chat);
            if *scroll_offset >= max {
                *scroll_offset = max;
                *auto_scroll = true;
            }
            IdleAction::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            input.clear();
            IdleAction::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => IdleAction::Quit,
        _ => IdleAction::Continue,
    }
}

// ── Slash commands ──

fn handle_slash_command(
    cmd: &str,
    agent: &mut AgentSession,
    chat: &mut ChatBuffer,
    status: &SharedStatus,
) -> bool {
    match cmd {
        "/exit" | "/quit" | "/bye" => return true,
        "/help" => {
            chat.push("── commands ──".to_string());
            chat.push("  /help        Show this help".to_string());
            chat.push("  /exit        Quit enchanter".to_string());
            chat.push("  /clear       Clear conversation".to_string());
            chat.push("  /model NAME  Switch provider/model".to_string());
            chat.push("  /retry       Retry last exchange".to_string());
            chat.push("  /undo        Undo last exchange".to_string());
            chat.push("  /ctx         Show context token usage".to_string());
            chat.push("  /config      Show current config".to_string());
            chat.push("  /tools       Show available tools".to_string());
            chat.push("  /log         Show recent activity log".to_string());
            chat.push("  /memory      Show memory entries".to_string());
            chat.push("  /soul        Show SOUL.md content".to_string());
            chat.push("  /skills      Show discovered skills".to_string());
        }
        "/clear" => {
            if let Err(e) = agent.clear() {
                chat.push(format!("✗ clear failed: {}", e));
            } else {
                chat.push("── conversation cleared ──".to_string());
                sync_context_tokens(agent, status);
            }
        }
        "/config" => {
            let info = agent.info();
            let tokens = agent.estimated_context_tokens();
            let budget = status_bar::model_context_size(&agent.resolved.model);
            chat.push(format!("  model:    {}", info.model));
            chat.push(format!("  base_url: {}", info.base_url));
            chat.push(format!("  api_key:  {}", if info.api_key_set { "set" } else { "none" }));
            chat.push(format!("  max:      {} (soft: {})",
                info.max_turns.map_or("unlimited".to_string(), |n| n.to_string()),
                info.soft_limit.map_or("n/a".to_string(), |n| n.to_string())
            ));
            if let Some(b) = budget {
                let pct = ((tokens as f64 / b as f64) * 100.0) as u8;
                chat.push(format!("  context:  ~{} / {} ({}%)",
                    status_bar::fmt_tokens(tokens), status_bar::fmt_tokens(b), pct));
            } else {
                chat.push(format!("  context:  ~{} tokens", status_bar::fmt_tokens(tokens)));
            }
        }
        "/memory" => {
            chat.push(agent.memory.format_for_prompt());
        }
        "/soul" => {
            chat.push(agent.soul.content.clone());
        }
        "/skills" => {
            chat.push(agent.skills.format_index_for_prompt());
        }
        "/tools" => {
            let tools = agent.tools_payload();
            let count = tools.as_ref().and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            chat.push(format!("── {} tools ──", count));
            if let Some(arr) = tools.as_ref().and_then(|v| v.as_array()) {
                for t in arr.iter().take(30) {
                    let name = t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("?");
                    chat.push(format!("  {}", name));
                }
                if arr.len() > 30 {
                    chat.push(format!("  ... ({} more)", arr.len() - 30));
                }
            }
        }
        "/ctx" | "/context" => {
            let tokens = agent.estimated_context_tokens();
            let budget = status_bar::model_context_size(&agent.resolved.model);
            if let Some(b) = budget {
                let pct = ((tokens as f64 / b as f64) * 100.0) as u8;
                chat.push(format!("── context: ~{} / {} tokens ({}%) ──",
                    status_bar::fmt_tokens(tokens),
                    status_bar::fmt_tokens(b),
                    pct));
            } else {
                chat.push(format!("── context: ~{} tokens (budget unknown for {}) ──",
                    status_bar::fmt_tokens(tokens),
                    agent.resolved.model));
            }
        }
        "/undo" => {
            if agent.undo() {
                chat.push("── undid last exchange ──".to_string());
                sync_context_tokens(agent, status);
            } else {
                chat.push("✗ nothing to undo".to_string());
            }
        }
        "/log" => {
            let log_path = crate::home::enchanter_home().join("logs/activity.jsonl");
            if !log_path.exists() {
                chat.push(format!("no activity log at {}", log_path.display()));
            } else {
                match std::fs::read_to_string(&log_path) {
                    Ok(contents) => {
                        let lines: Vec<&str> = contents.lines().rev().take(15).collect();
                        let reversed: Vec<&str> = lines.into_iter().rev().collect();
                        chat.push("── recent activity ──".to_string());
                        for line in &reversed {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("?");
                                let evt = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                                chat.push(format!("  {} {}", ts, evt));
                            }
                        }
                    }
                    Err(e) => chat.push(format!("✗ cannot read log: {}", e)),
                }
            }
        }
        "/sessions" => {
            match crate::session::Session::list_all() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        chat.push("no sessions found".to_string());
                    } else {
                        chat.push("── sessions ──".to_string());
                        for s in &sessions {
                            let short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                            chat.push(format!("  {}  {} msgs", short, s.message_count));
                        }
                    }
                }
                Err(e) => chat.push(format!("✗ {}", e)),
            }
        }
        _ => {
            if let Some(new_name) = cmd.strip_prefix("/model ") {
                let new_name = new_name.trim();
                if new_name.is_empty() {
                    chat.push("usage: /model <name>".to_string());
                } else {
                    match agent.switch_model(new_name) {
                        Ok(label) => {
                            chat.push(format!("✓ switched to {}", label));
                            let mut s = status.lock().unwrap();
                            s.model_short = shorten_model(&agent.resolved.model);
                            s.context_budget = status_bar::model_context_size(&agent.resolved.model);
                            drop(s);
                            sync_context_tokens(agent, &status);
                        }
                        Err(e) => chat.push(format!("✗ {}", e)),
                    }
                }
            } else {
                chat.push(format!("✗ unknown command: {}", cmd));
            }
        }
    }
    false
}

// ── Helpers ──

fn calc_auto_scroll(chat: &ChatBuffer) -> usize {
    let (_, height) = terminal_size();
    let visible = chat_rows(height) as usize;
    chat.len().saturating_sub(visible)
}

fn max_scroll(chat: &ChatBuffer) -> usize {
    let (_, height) = terminal_size();
    let visible = chat_rows(height) as usize;
    chat.len().saturating_sub(visible)
}

fn shorten_model(model: &str) -> String {
    let re = regex::Regex::new(r"-(20\d{2})\d{2,4}$").unwrap();
    if let Some(m) = re.find(model) {
        model[..m.start()].to_string()
    } else if model.len() > 20 {
        model[..17].to_string() + "…"
    } else {
        model.to_string()
    }
}