//! Inline status bar — printed as a regular line above the input prompt.
//!
//! Uses ANSI reverse-video styling (dim text, faint separators) to make the
//! bar visually distinct but unobtrusive. No absolute cursor positioning,
//! no alternate screen — just prints a line before the prompt.

/// Render the status bar content for one row.
///
/// Format: ` Context: 45k/128k (35%) │ gpt-4o │ a1b2c3d4 `
///
/// Uses dim text (ANSI faint/bright-black) so it's visible but unobtrusive.
/// Pads to fill the full terminal width.
pub fn render_bar(model: &str, tokens: u64, budget: Option<u64>, session_id: &str, width: u16) -> String {
    // Token portion — e.g. "45k/128k (35%)" or "45k"
    let token_str = match budget {
        Some(b) => {
            let pct = ((tokens as f64 / b as f64) * 100.0).round() as u8;
            format!("{}k/{} ({}%)", tokens / 1000, fmt_tokens(b), pct)
        }
        None => format!("{}k", tokens / 1000),
    };

    let short_id = &session_id[..8.min(session_id.len())];
    let short_model = model
        .rsplit_once('/')
        .map(|(_, m)| m)
        .unwrap_or(model);

    // Build candidate strings (longest to shortest)
    // Style: "Context:" in bright-black (dim), separators in faint,
    // values in default foreground.
    let full = format!(
        " \x1b[90mContext:\x1b[0m {} \x1b[2m│\x1b[22m {} \x1b[2m│\x1b[22m {} ",
        token_str, short_model, short_id
    );
    let without_session = format!(
        " \x1b[90mContext:\x1b[0m {} \x1b[2m│\x1b[22m {} ",
        token_str, short_model
    );
    let minimal = format!(
        " \x1b[90mContext:\x1b[0m {} ",
        token_str
    );

    // Pick the longest that fits
    let content = if strip_ansi_len(&full) <= width as usize {
        full
    } else if strip_ansi_len(&without_session) <= width as usize {
        without_session
    } else {
        minimal
    };

    // Pad to fill width
    let visible_len = strip_ansi_len(&content);
    let pad = if (width as usize) > visible_len {
        " ".repeat(width as usize - visible_len)
    } else {
        String::new()
    };

    format!("{}{}\x1b[0m", content, pad)
}

/// Print the status bar as a line above the prompt.
/// Uses reverse video to make it visually distinct.
pub fn print_bar(model: &str, tokens: u64, budget: Option<u64>, session_id: &str) {
    let (rows, cols) = terminal_size();
    if rows == 0 || cols == 0 {
        return;
    }

    let bar = render_bar(model, tokens, budget, session_id, cols);
    // Print with reverse video background: \x1b[7m = reverse, \x1b[0m = reset
    println!("\x1b[7m{}\x1b[0m", bar);
}

/// Get terminal size (rows, cols). Falls back to (24, 80) if unavailable.
pub fn terminal_size() -> (u16, u16) {
    #[cfg(unix)]
    {
        use libc::{ioctl, isatty, winsize, TIOCGWINSZ, STDOUT_FILENO};
        unsafe {
            if isatty(STDOUT_FILENO) == 0 {
                return (24, 80);
            }
            let mut ws: winsize = std::mem::zeroed();
            if ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut ws) == 0 {
                (ws.ws_row, ws.ws_col)
            } else {
                (24, 80)
            }
        }
    }
    #[cfg(not(unix))]
    {
        (24, 80)
    }
}

/// Measure the visible (non-ANSI) width of a string, in terminal columns.
fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

// ── Token formatting ──

/// Known model context window sizes (in tokens). Used to display budget
/// percentage. Models not in this list show raw token count only.
const MODEL_CONTEXT_SIZES: &[(&str, u64)] = &[
    // OpenAI
    ("gpt-4.1", 1_047_576),
    ("gpt-4.1-mini", 1_047_576),
    ("gpt-4.1-nano", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4o-mini", 128_000),
    ("o3", 200_000),
    ("o3-mini", 200_000),
    ("o4-mini", 200_000),
    // Anthropic (via proxy)
    ("claude-3.5-sonnet", 200_000),
    ("claude-3-opus", 200_000),
    ("claude-3-haiku", 200_000),
    ("claude-sonnet-4", 200_000),
    // Google
    ("gemini-2.5-pro", 1_048_576),
    ("gemini-2.5-flash", 1_048_576),
    ("gemini-2.0-flash", 1_048_576),
    // Meta
    ("llama-3.3-70b", 128_000),
    ("llama-3.1-405b", 128_000),
    ("llama-3.1-70b", 128_000),
    // DeepSeek
    ("deepseek-r1", 128_000),
    ("deepseek-v3", 128_000),
    // Perplexity
    ("sonar", 128_000),
    // GLM
    ("glm-5.1", 128_000),
    // Qwen
    ("qwen3-235b", 128_000),
    ("qwen3-30b", 128_000),
];

/// Look up the context window size for a model name.
/// Tries prefix matching for partial names (e.g., "gpt-4.1" matches "gpt-4.1-mini").
pub fn model_context_size(model: &str) -> Option<u64> {
    let lower = model.to_lowercase();
    for (prefix, size) in MODEL_CONTEXT_SIZES {
        if lower.starts_with(prefix) {
            return Some(*size);
        }
    }
    None
}

/// Format a token count for display: "45k" for thousands, "1.2M" for millions.
pub fn fmt_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1000 {
        format!("{}k", tokens / 1000)
    } else {
        format!("{}", tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_context_lookup() {
        assert_eq!(model_context_size("gpt-4o"), Some(128_000));
        assert_eq!(model_context_size("gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(model_context_size("unknown-model"), None);
    }

    #[test]
    fn fmt_tokens_display() {
        assert_eq!(fmt_tokens(500), "500");
        assert_eq!(fmt_tokens(45000), "45k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn strip_ansi_len_plain() {
        assert_eq!(strip_ansi_len("hello"), 5);
    }

    #[test]
    fn strip_ansi_len_with_escapes() {
        assert_eq!(strip_ansi_len("\x1b[90m│\x1b[0m hello"), 7);
    }

    #[test]
    fn render_bar_fits_width() {
        let bar = render_bar("gpt-4o", 45000, Some(128000), "abc12345", 80);
        assert!(bar.contains("Context"));
        assert!(bar.contains("45k"));
    }

    #[test]
    fn render_bar_narrow_terminal() {
        // Should still render something on a 40-col terminal
        let bar = render_bar("gpt-4o", 45000, Some(128000), "abc12345", 40);
        assert!(bar.contains("Context"));
    }
}