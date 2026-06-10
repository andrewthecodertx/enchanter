//! Rendering — all ratatui draw logic.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::app::{App, ChatLine, Pane};

/// Color palette.
mod colors {
    use ratatui::style::Color;
    pub const ACCENT: Color = Color::Cyan;
    pub const ACCENT2: Color = Color::Magenta;
    pub const DIM: Color = Color::DarkGray;
    pub const USER: Color = Color::Blue;
    pub const ASSISTANT: Color = Color::Green;
    pub const TOOL: Color = Color::Yellow;
    pub const TOOL_RESULT: Color = Color::DarkGray;
    pub const ERROR: Color = Color::Red;
    pub const HIGHLIGHT_BG: Color = Color::DarkGray;
    pub const BORDER_FOCUS: Color = Color::Cyan;
    pub const BORDER_IDLE: Color = Color::DarkGray;
    pub const HEADER_BG: Color = Color::DarkGray;
}

/// Top-level layout: header, body, footer.
pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(3),    // body
            Constraint::Length(1), // footer
        ])
        .split(size);

    draw_header(f, app, chunks[0]);
    draw_body(f, app, chunks[1]);
    draw_footer(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let short_session = if app.info.session_id.len() > 8 {
        &app.info.session_id[..8]
    } else {
        &app.info.session_id
    };

    let short_url = app
        .info
        .base_url
        .trim_end_matches('/')
        .replace("https://api.openai.com/v1", "openai")
        .replace("http://localhost:11434/v1", "ollama")
        .replace("http://127.0.0.1:11434/v1", "ollama")
        .replace("https://openrouter.ai/api/v1", "openrouter")
        .replace("https://api.groq.com/openai/v1", "groq");

    let mut spans = vec![
        Span::styled(" ⟡ ", Style::default().fg(colors::ACCENT2)),
        Span::styled(
            "Enchanter",
            Style::default()
                .fg(colors::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(&app.info.model, Style::default().fg(Color::White)),
        Span::raw(" @ "),
        Span::styled(short_url, Style::default().fg(colors::DIM)),
        Span::raw(" │ session="),
        Span::styled(short_session, Style::default().fg(colors::DIM)),
        Span::raw(" │ turn "),
        Span::styled(format!("{}", app.turn), Style::default().fg(Color::White)),
    ];

    // Context token usage
    if app.context_tokens > 0 {
        spans.push(Span::raw(" │ "));
        if let Some(budget) = app.context_budget {
            let pct = ((app.context_tokens as f64 / budget as f64) * 100.0) as u8;
            let ctx_color = if pct > 80 {
                Color::Red
            } else if pct > 60 {
                Color::Yellow
            } else {
                Color::Green
            };
            spans.push(Span::styled(
                format!("ctx:{}% {}/{}", pct,
                    crate::status_bar::fmt_tokens(app.context_tokens),
                    crate::status_bar::fmt_tokens(budget)),
                Style::default().fg(ctx_color),
            ));
        } else {
            spans.push(Span::styled(
                format!("ctx:{}", crate::status_bar::fmt_tokens(app.context_tokens)),
                Style::default().fg(Color::Green),
            ));
        }
    }

    let line = Line::from(spans);
    let header = Paragraph::new(line).style(Style::default().bg(colors::HEADER_BG));
    f.render_widget(header, area);
}

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30), // sidebar
            Constraint::Min(20),    // chat + input
        ])
        .split(area);

    draw_sidebar(f, app, body[0]);
    draw_chat_area(f, app, body[1]);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(12), // skills
            Constraint::Min(4),     // memory
        ])
        .split(area);

    draw_skills_pane(f, app, sidebar[0]);
    draw_memory_pane(f, app, sidebar[1]);
}

fn draw_skills_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Skills;
    let border_color = if focused {
        colors::BORDER_FOCUS
    } else {
        colors::BORDER_IDLE
    };

    let items: Vec<ListItem> = app
        .cached_skills
        .skills
        .iter()
        .map(|skill| {
            let cat = skill.category.as_deref().unwrap_or("other");
            let label = format!("[{}] {}", cat, skill.name);
            let style = Style::default().fg(colors::ACCENT);
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let title = format!(" SKILLS ({}) ", app.cached_skills.skills.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.skills_selected.min(items.len() - 1)));
    }

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(colors::HIGHLIGHT_BG)
            .add_modifier(Modifier::BOLD),
    );

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_memory_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Memory;
    let border_color = if focused {
        colors::BORDER_FOCUS
    } else {
        colors::BORDER_IDLE
    };

    let total = app.cached_memory.user_entries.len() + app.cached_memory.memory_entries.len();
    let title = format!(" MEMORY ({}) ", total);

    let mut lines: Vec<Line> = Vec::new();

    if !app.cached_memory.user_entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "── USER ──",
            Style::default().fg(colors::USER),
        )));
        for (i, entry) in app.cached_memory.user_entries.iter().enumerate() {
            let truncated: String = entry.chars().take(28).collect();
            let is_selected = focused && app.memory_selected == i;
            let style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(Span::styled(format!("  {}", truncated), style)));
        }
    }

    if !app.cached_memory.memory_entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "── NOTES ──",
            Style::default().fg(colors::ACCENT),
        )));
        for (i, entry) in app.cached_memory.memory_entries.iter().enumerate() {
            let truncated: String = entry.chars().take(28).collect();
            let global_idx = app.cached_memory.user_entries.len() + i;
            let is_selected = focused && app.memory_selected == global_idx;
            let style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(colors::DIM)
            };
            lines.push(Line::from(Span::styled(
                format!("[{}] {}", i + 1, truncated),
                style,
            )));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(empty)",
            Style::default().fg(colors::DIM),
        )));
    }

    // Auto-scroll memory to keep selected item visible
    let visible_lines = area.height.saturating_sub(2) as usize; // subtract borders
    let scroll_offset = if app.memory_selected >= visible_lines {
        (app.memory_selected + 1).saturating_sub(visible_lines) as u16
    } else {
        app.memory_scroll as u16
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll_offset, 0));

    f.render_widget(paragraph, area);
}

fn draw_chat_area(f: &mut Frame, app: &App, area: Rect) {
    let chat_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // chat history
            Constraint::Length(3), // input bar
        ])
        .split(area);

    draw_chat_pane(f, app, chat_area[0]);
    draw_input_bar(f, app, chat_area[1]);
}

fn draw_chat_pane(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Chat;
    let border_color = if focused {
        colors::BORDER_FOCUS
    } else {
        colors::BORDER_IDLE
    };

    let mut lines: Vec<Line> = Vec::new();

    for line in &app.chat_lines {
        match line {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "⟩ ",
                        Style::default()
                            .fg(colors::USER)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_to_width(text, (area.width as usize).saturating_sub(4)),
                        Style::default().fg(Color::White),
                    ),
                ]));
            }
            ChatLine::Assistant(text) => {
                for (i, text_line) in text.lines().enumerate() {
                    let prefix = if i == 0 { "⟨ " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(colors::ASSISTANT)),
                        Span::styled(text_line, Style::default().fg(Color::White)),
                    ]));
                }
            }
            ChatLine::ToolCall { name, id: _ } => {
                lines.push(Line::from(vec![
                    Span::styled(" ⟩ ", Style::default().fg(colors::TOOL)),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            ChatLine::ToolResult { id: _, content } => {
                for text_line in content.lines().take(5) {
                    let prefix = " │ ";
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(colors::TOOL_RESULT)),
                        Span::styled(
                            truncate_to_width(text_line, (area.width as usize).saturating_sub(6)),
                            Style::default().fg(colors::DIM),
                        ),
                    ]));
                }
                let total_lines = content.lines().count();
                if total_lines > 5 {
                    lines.push(Line::from(Span::styled(
                        format!(" │ ... ({} more lines)", total_lines - 5),
                        Style::default().fg(colors::DIM),
                    )));
                }
            }
            ChatLine::System(text) => {
                lines.push(Line::from(vec![
                    Span::styled("╌ ", Style::default().fg(colors::DIM)),
                    Span::styled(text.clone(), Style::default().fg(colors::DIM)),
                ]));
            }
            ChatLine::Error(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "✗ ",
                        Style::default()
                            .fg(colors::ERROR)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(colors::ERROR)),
                ]));
            }
        }
    }

    // Streaming text in progress
    if !app.current_stream_text.is_empty() {
        for (i, text_line) in app.current_stream_text.lines().enumerate() {
            let prefix = if i == 0 { "⟨ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(colors::ASSISTANT)),
                Span::styled(text_line, Style::default().fg(Color::White)),
                Span::styled("▌", Style::default().fg(colors::ACCENT)),
            ]));
        }
    } else if app.streaming {
        lines.push(Line::from(Span::styled(
            "  thinking▌",
            Style::default().fg(colors::DIM),
        )));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Chat will appear here. Type a message below.",
            Style::default().fg(colors::DIM),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" CHAT ");

    let line_count = lines.len() as u16;
    let visible_height = area.height.saturating_sub(2); // subtract borders
    let scroll_offset = if app.chat_auto_scroll {
        line_count.saturating_sub(visible_height)
    } else {
        app.chat_scroll as u16
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll_offset, 0))
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn draw_input_bar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Input;
    let border_color = if focused {
        colors::BORDER_FOCUS
    } else {
        colors::BORDER_IDLE
    };

    let mode_hint = if app.input.multiline { " [M]" } else { "" };
    let title = format!(" INPUT{} ", mode_hint);

    let input_text = if app.input.buffer.is_empty() && !focused {
        "Type a message... (/ for commands)".to_string()
    } else {
        app.input.buffer.clone()
    };

    let style = if app.input.buffer.is_empty() && !focused {
        Style::default().fg(colors::DIM)
    } else {
        Style::default().fg(Color::White)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    let inner = block.inner(area);

    let paragraph = Paragraph::new(input_text).style(style);
    f.render_widget(paragraph, inner);
    f.render_widget(block, area);

    // Show cursor when focused
    if focused {
        let cursor_x = inner.x + app.input.cursor as u16;
        let cursor_y = inner.y;
        f.set_cursor_position((cursor_x, cursor_y));
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let mcp_display = if app.info.mcp_tool_count > 0 {
        format!(" ({} MCP)", app.info.mcp_tool_count)
    } else {
        String::new()
    };

    let streaming_indicator = if app.streaming { " ⏳" } else { "" };
    let tools_str = format!("{}", app.info.tool_count);
    let _skills_str = format!("{}", app.info.skill_count);

    let line = Line::from(vec![
        Span::styled(" tools:", Style::default().fg(colors::DIM)),
        Span::styled(tools_str, Style::default().fg(Color::White)),
        Span::styled(&mcp_display, Style::default().fg(colors::DIM)),
        Span::raw(format!("│ skills: {}", app.info.skill_count)),
        Span::raw(streaming_indicator),
        Span::styled(" │ ", Style::default().fg(colors::DIM)),
        Span::styled("Tab", Style::default().fg(colors::ACCENT)),
        Span::styled("=focus ", Style::default().fg(colors::DIM)),
        Span::styled("Enter", Style::default().fg(colors::ACCENT)),
        Span::styled("=send ", Style::default().fg(colors::DIM)),
        Span::styled("Ctrl+Q", Style::default().fg(colors::ACCENT)),
        Span::styled("=quit", Style::default().fg(colors::DIM)),
    ]);

    let footer = Paragraph::new(line).style(Style::default().bg(colors::HEADER_BG));
    f.render_widget(footer, area);
}

/// Truncate a string to a given display width (character count, roughly).
fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        width += 1;
        if width > max_width {
            result.push_str("...");
            break;
        }
        result.push(ch);
    }
    result
}
