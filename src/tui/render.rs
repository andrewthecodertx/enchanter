//! Rendering — convert TUI state into ratatui widgets.
//!
//! Layout:
//!   ┌──────────────┬────────────────────────┐
//!   │ Models       │ Chat                    │
//!   │ Sessions     │                         │
//!   │ Skills       │                         │
//!   ├──────────────┴────────────────────────┤
//!   │ Input                                  │
//!   ├────────────────────────────────────────┤
//!   │ Status bar                             │
//!   └────────────────────────────────────────┘

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::tui::state::{ChatLine, Focus, TuiState};

/// Theme colors — dark terminal aesthetic matching the existing status bar.
const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const GREEN: Color = Color::Green;
const YELLOW: Color = Color::Yellow;
const RED: Color = Color::Red;

/// Render the full TUI.
pub fn render(frame: &mut Frame, state: &mut TuiState) {
    let area = frame.area();

    // Layout: sidebar | main, then input, then status bar at bottom.
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),      // sidebar + chat
            Constraint::Length(3),    // input
            Constraint::Length(1),    // status bar
        ])
        .split(area);

    let top_area = main_layout[0];
    let input_area = main_layout[1];
    let status_area = main_layout[2];

    // Split top into sidebar and chat.
    let top_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Min(10),
        ])
        .split(top_area);

    let sidebar_area = top_split[0];
    let chat_area = top_split[1];

    render_sidebar(frame, state, sidebar_area);
    render_chat(frame, state, chat_area);
    render_input(frame, state, input_area);
    render_status_bar(frame, state, status_area);
}

fn render_sidebar(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    // Split sidebar into three panes.
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Percentage(35),
            Constraint::Percentage(30),
        ])
        .split(area);

    render_models_pane(frame, state, layout[0]);
    render_sessions_pane(frame, state, layout[1]);
    render_skills_pane(frame, state, layout[2]);
}

fn render_models_pane(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let focused = state.focus == Focus::Models;

    let border_style = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let title = if focused { "│ Models ◀ │" } else { "│ Models │" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let items: Vec<ListItem> = state
        .models
        .iter()
        .map(|m| {
            let marker = if m.is_active { "▸ " } else { "  " };
            let style = if m.is_active {
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let line = Line::from(vec![
                Span::styled(marker, style),
                Span::styled(&m.name, style),
                Span::raw("  "),
                Span::styled(&m.model, Style::default().fg(DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if focused {
        list_state.select(Some(state.list_cursor.min(state.models.len().saturating_sub(1))));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_sessions_pane(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let focused = state.focus == Focus::Sessions;

    let border_style = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let title = if focused { "│ Sessions ◀ │" } else { "│ Sessions │" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let items: Vec<ListItem> = state
        .sessions
        .iter()
        .take(20)
        .map(|s| {
            let short_id = &s.id[..8.min(s.id.len())];
            let msgs = format!("{}m", s.message_count);
            ListItem::new(Line::from(vec![
                Span::styled(short_id, Style::default().fg(if focused { Color::White } else { DIM })),
                Span::raw("  "),
                Span::styled(msgs, Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if focused {
        list_state.select(Some(state.list_cursor.min(state.sessions.len().saturating_sub(1))));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_skills_pane(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let focused = state.focus == Focus::Skills;

    let border_style = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let title = if focused { "│ Skills ◀ │" } else { "│ Skills │" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let items: Vec<ListItem> = state
        .skills
        .iter()
        .map(|s| {
            let cat = s.category.as_deref().unwrap_or("general");
            let short_cat = &cat[..cat.len().min(10)];
            ListItem::new(Line::from(vec![
                Span::styled(&s.name, Style::default()),
                Span::raw("  "),
                Span::styled(short_cat, Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if focused {
        list_state.select(Some(state.list_cursor.min(state.skills.len().saturating_sub(1))));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_chat(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let focused = state.focus == Focus::Chat;

    let border_style = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let title = if state.is_streaming {
        "│ Chat ◀ streaming… │"
    } else if focused {
        "│ Chat ◀ │"
    } else {
        "│ Chat │"
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let lines = build_chat_lines(state);
    let text = Text::from(lines);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.chat_scroll as u16, 0));

    frame.render_widget(paragraph, area);
}

fn build_chat_lines(state: &TuiState) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();

    for entry in &state.chat_lines {
        match entry {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line)));
                }
                lines.push(Line::raw(""));
            }
            ChatLine::Assistant(text) => {
                lines.push(Line::from(vec![
                    Span::styled("Assistant", Style::default().fg(GREEN).add_modifier(Modifier::BOLD)),
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line)));
                }
                lines.push(Line::raw(""));
            }
            ChatLine::ToolCall(name) => {
                lines.push(Line::from(vec![
                    Span::styled("  ⟩ ", Style::default().fg(YELLOW)),
                    Span::styled(name, Style::default().fg(YELLOW)),
                ]));
            }
            ChatLine::ToolResult(name, content) => {
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(DIM)),
                    Span::styled(name, Style::default().fg(DIM)),
                ]));
                for line in content.lines().take(5) {
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(DIM)),
                        Span::raw(line),
                    ]));
                }
                let total = content.lines().count();
                if total > 5 {
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(DIM)),
                        Span::styled(format!("… ({} more)", total - 5), Style::default().fg(DIM)),
                    ]));
                }
            }
            ChatLine::System(text) => {
                lines.push(Line::from(Span::styled(text, Style::default().fg(DIM))));
            }
            ChatLine::Compacted(text) => {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(YELLOW),
                )));
            }
            ChatLine::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("✗ {}", text),
                    Style::default().fg(RED),
                )));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No conversation yet. Type a message below.",
            Style::default().fg(DIM),
        )));
    }

    lines
}

fn render_input(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let focused = state.focus == Focus::Input;

    let border_style = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let prompt_symbol = if state.is_streaming { "…" } else { "⟩" };
    let title = if focused {
        format!("│ {} Input ◀ │", prompt_symbol)
    } else {
        format!("│ {} Input │", prompt_symbol)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    // Build the input line with a visible cursor.
    let input_style = Style::default();
    let prompt_span = Span::styled(format!("{} ", prompt_symbol), Style::default().fg(ACCENT));
    let content_span = Span::styled(&state.input_buffer, input_style);

    let line = Line::from(vec![prompt_span, content_span]);

    let paragraph = Paragraph::new(line)
        .block(block);

    frame.render_widget(paragraph, area);

    // Place the cursor at the right position.
    if focused {
        // Cursor x = border (1) + prompt symbol (2) + cursor position
        let cursor_x = area.x + 1 + 2 + state.input_cursor as u16;
        let cursor_y = area.y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_status_bar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let budget = state.context_budget();
    let token_str = match budget {
        Some(b) => {
            let pct = if b > 0 {
                ((state.tokens as f64 / b as f64) * 100.0).round() as u8
            } else {
                0
            };
            format!(
                "{} / {} ({}%)",
                crate::status_bar::fmt_tokens(state.tokens),
                crate::status_bar::fmt_tokens(b),
                pct
            )
        }
        None => format!("{} tokens", crate::status_bar::fmt_tokens(state.tokens)),
    };

    let short_id = &state.session_id[..8.min(state.session_id.len())];
    let short_model = state.model_name.rsplit_once('/')
        .map(|(_, m)| m.to_string())
        .unwrap_or_else(|| state.model_name.clone());

    let focus_label = state.focus.label();

    // Show status message if present, otherwise show the normal bar.
    if !state.status_message.is_empty() && !state.is_streaming {
        let bar = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(&state.status_message, Style::default().fg(YELLOW)),
        ]);
        let paragraph = Paragraph::new(bar).style(Style::default().bg(Color::Black));
        frame.render_widget(paragraph, area);
        return;
    }

    let token_color = if state.tokens > budget.unwrap_or(0) / 4 * 3 {
        YELLOW
    } else {
        Color::White
    };

    let bar = Line::from(vec![
        Span::styled(" Context: ", Style::default().fg(DIM)),
        Span::styled(token_str, Style::default().fg(token_color)),
        Span::styled(" │ ", Style::default().fg(DIM)),
        Span::styled(&short_model, Style::default()),
        Span::styled(" │ ", Style::default().fg(DIM)),
        Span::styled(short_id, Style::default().fg(DIM)),
        Span::styled(" │ ", Style::default().fg(DIM)),
        Span::styled(format!("[{}]", focus_label), Style::default().fg(ACCENT)),
        Span::raw(" "),
    ]);

    let paragraph = Paragraph::new(bar).style(Style::default().bg(Color::Black));
    frame.render_widget(paragraph, area);
}