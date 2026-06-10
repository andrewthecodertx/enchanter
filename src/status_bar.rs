//! Model context size lookup and token formatting utilities.
//!
//! Previously housed a full status bar renderer for the crossterm TUI;
//! that was removed when the REPL switched to line-oriented mode.

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
}