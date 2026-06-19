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

use crate::tui::state::{ChatLine, Focus, PaneAreas, TuiState};

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
    // Input area grows with content (min 3 lines for border+1, up to 40% of screen).
    let input_lines = state.input_buffer.lines().count().max(1) as u16;
    let input_height = (input_lines + 2).min(area.height / 5).max(3);

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),           // sidebar + chat
            Constraint::Length(input_height), // input (grows with multi-line content)
            Constraint::Length(1),        // status bar
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

    // Store sidebar sub-pane areas for mouse hit-testing.
    // We need to re-split the sidebar to get individual pane areas.
    let sidebar_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Percentage(35),
            Constraint::Percentage(30),
        ])
        .split(sidebar_area);

    state.pane_areas = PaneAreas {
        models: sidebar_layout[0],
        sessions: sidebar_layout[1],
        skills: sidebar_layout[2],
        chat: chat_area,
        input: input_area,
    };
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
            // Strip known prefixes to get a meaningful display name.
            // For "enchanter_tui_{uuid}", show date + short UUID.
            // For bare UUIDs, show short UUID + date.
            let display_name: String = if let Some(uuid_part) = s.id.strip_prefix("enchanter_tui_") {
                let short = &uuid_part[..8.min(uuid_part.len())];
                format!("tui:{}", short)
            } else {
                s.id[..8.min(s.id.len())].to_string()
            };
            let date_str = s.started_at.as_ref()
                .and_then(|t| {
                    // Extract just the date portion from RFC3339.
                    t.split('T').next()
                })
                .map(|d| d.to_string())
                .unwrap_or_default();
            let msgs = format!("{}m", s.message_count);
            ListItem::new(Line::from(vec![
                Span::styled(display_name, Style::default().fg(if focused { Color::White } else { DIM })),
                Span::raw(" "),
                Span::styled(date_str, Style::default().fg(DIM)),
                Span::raw(" "),
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

    let title: String = if state.is_streaming {
        if state.has_first_content {
            "│ Chat ◀ streaming… │".to_string()
        } else {
            // Thinking — waiting for first token. Show animated spinner.
            let spinner = match state.spinner_frame {
                0 => "⠋",
                1 => "⠙",
                2 => "⠹",
                _ => "⠸",
            };
            format!("│ Chat ◀ {} thinking… │", spinner)
        }
    } else if focused {
        "│ Chat ◀ │".to_string()
    } else {
        "│ Chat │".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let lines = build_chat_lines(state);
    let total_rows = lines.len();
    let visible_rows = area.height.saturating_sub(2) as usize;

    // Store for scroll clamping in scroll methods.
    state.chat_total_rows = total_rows;
    state.chat_visible_rows = visible_rows;

    // Clamp scroll: if auto_scroll, snap to bottom; otherwise clamp to max.
    let scroll = if state.auto_scroll {
        total_rows.saturating_sub(visible_rows)
    } else {
        let max = total_rows.saturating_sub(visible_rows);
        state.chat_scroll.min(max)
    };
    state.chat_scroll = scroll;

    let text = Text::from(lines);

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);
}

fn build_chat_lines(state: &TuiState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in &state.chat_lines {
        match entry {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line.to_string())));
                }
                lines.push(Line::raw(""));
            }
            ChatLine::Assistant(text) => {
                lines.push(Line::from(vec![
                    Span::styled("Assistant", Style::default().fg(GREEN).add_modifier(Modifier::BOLD)),
                ]));
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line.to_string())));
                }
                lines.push(Line::raw(""));
            }
            ChatLine::ToolCall(name) => {
                lines.push(Line::from(vec![
                    Span::styled("  ⟩ ", Style::default().fg(YELLOW)),
                    Span::styled(name.clone(), Style::default().fg(YELLOW)),
                ]));
            }
            ChatLine::ToolResult(name, content) => {
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(DIM)),
                    Span::styled(name.clone(), Style::default().fg(DIM)),
                ]));
                for line in content.lines().take(5) {
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(DIM)),
                        Span::raw(line.to_string()),
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
                lines.push(Line::from(Span::styled(text.clone(), Style::default().fg(DIM))));
            }
            ChatLine::Compacted(text) => {
                lines.push(Line::from(Span::styled(
                    text.clone(),
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

    let prompt_symbol = if state.is_streaming {
        if state.has_first_content { "…" } else { "⠋" }
    } else { "⟩" };
    let title = if focused {
        format!("│ {} Input ◀ │", prompt_symbol)
    } else {
        format!("│ {} Input │", prompt_symbol)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    // Build multi-line input text with prompt symbol on first line.
    let prompt_span = Span::styled(format!("{} ", prompt_symbol), Style::default().fg(ACCENT));

    // Split input buffer into lines for rendering. The first line gets the
    // prompt prefix; subsequent lines get a 2-space indent to align.
    let mut lines: Vec<Line> = Vec::new();
    let buffer_lines: Vec<&str> = state.input_buffer.lines().collect();

    if buffer_lines.is_empty() {
        // Empty input — single line with just the prompt.
        lines.push(Line::from(vec![prompt_span]));
    } else {
        for (i, line_text) in buffer_lines.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![prompt_span.clone(), Span::raw(*line_text)]));
            } else {
                // Indent continuation lines to align under the text (2 spaces = prompt width).
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(*line_text),
                ]));
            }
        }
        // If the buffer ends with \n, add an empty line so the cursor can sit there.
        if state.input_buffer.ends_with('\n') {
            lines.push(Line::from(vec![Span::raw("  ")]));
        }
    }

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(paragraph, area);

    // Place the cursor at the right position for multi-line input.
    if focused {
        // Calculate which line and column the cursor is on.
        let (cursor_row, cursor_col) = cursor_row_col(&state.input_buffer, state.input_cursor);

        // Cursor x = border(1) + prefix(2) + column within line.
        // Every line has a 2-cell prefix: "⟩ " on the first line, "  " on continuations.
        let cursor_x = area.x + 1 + 2 + cursor_col as u16;
        let cursor_y = area.y + 1 + cursor_row as u16;

        // Clamp cursor to visible area.
        let max_y = area.y + area.height.saturating_sub(1);
        let clamped_y = cursor_y.min(max_y);

        frame.set_cursor_position((cursor_x, clamped_y));
    }
}

/// Given a buffer and a byte offset cursor position, return (row, col)
/// where row is the line index (0-based) and col is the character offset
/// within that line (0-based).
pub fn cursor_row_col(buffer: &str, cursor: usize) -> (usize, usize) {
    let mut row = 0;
    let mut col = 0;
    let mut byte_idx = 0;

    for ch in buffer.chars() {
        if byte_idx >= cursor {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
        byte_idx += ch.len_utf8();
    }

    (row, col)
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

    let short_id = if let Some(uuid_part) = state.session_id.strip_prefix("enchanter_tui_") {
        &uuid_part[..8.min(uuid_part.len())]
    } else {
        &state.session_id[..8.min(state.session_id.len())]
    };
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

    // While streaming but before first content, show a thinking spinner.
    if state.is_streaming && !state.has_first_content {
        let spinner = match state.spinner_frame {
            0 => "⠋",
            1 => "⠙",
            2 => "⠹",
            _ => "⠸",
        };
        let bar = Line::from(vec![
            Span::styled(format!(" {} thinking… ", spinner), Style::default().fg(YELLOW)),
            Span::styled("│ ", Style::default().fg(DIM)),
            Span::styled(&short_model, Style::default()),
            Span::styled(" │ ", Style::default().fg(DIM)),
            Span::styled(short_id, Style::default().fg(DIM)),
            Span::styled(" │ ", Style::default().fg(DIM)),
            Span::styled(format!("[{}]", focus_label), Style::default().fg(ACCENT)),
            Span::raw(" "),
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