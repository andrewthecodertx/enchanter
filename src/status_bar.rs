//! Real-time agent status bar for the REPL.
//!
//! Provides shared state (`AgentStatus`) that the agent loop updates
//! and the REPL status bar reads. Stuck-detection thresholds surface
//! warnings when the agent appears stalled. Context token tracking
//! shows estimated context window usage so users can self-regulate.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Current phase of the agent loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum AgentPhase {
    #[default]
    Idle,
    Connecting,
    #[allow(dead_code)]
    Streaming,
    ToolRunning { name: String },
    #[allow(dead_code)]
    Compacting,
}

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

/// Shared agent status, updated by the agent loop and read by the status bar.
#[derive(Debug)]
pub struct AgentStatus {
    pub phase: AgentPhase,
    pub turn: u32,
    pub model_short: String,
    pub phase_started: Instant,
    pub tool_calls_this_turn: usize,
    pub total_tool_calls: usize,
    pub stream_chars: usize,
    /// Estimated tokens consumed by system prompt + conversation history.
    pub context_tokens: u64,
    /// Context window size for the current model (None if unknown).
    pub context_budget: Option<u64>,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self {
            phase: AgentPhase::Idle,
            turn: 0,
            model_short: String::new(),
            phase_started: Instant::now(),
            tool_calls_this_turn: 0,
            total_tool_calls: 0,
            stream_chars: 0,
            context_tokens: 0,
            context_budget: None,
        }
    }
}

pub type SharedStatus = Arc<Mutex<AgentStatus>>;

pub fn new_shared_status() -> SharedStatus {
    Arc::new(Mutex::new(AgentStatus::default()))
}

// ── Stuck detection thresholds ──

/// Warn if API call takes longer than this.
const API_STUCK_SECS: u64 = 30;
/// Warn if a tool takes longer than this.
const TOOL_STUCK_SECS: u64 = 60;
/// Warn if streaming takes longer than this with fewer chars.
const STREAM_STUCK_SECS: u64 = 120;
const STREAM_STUCK_MIN_CHARS: usize = 50;

/// Render the status bar as a single line. Returns (text, is_warning).
pub fn render(status: &AgentStatus, width: u16) -> (String, bool) {
    let elapsed = status.phase_started.elapsed();
    let elapsed_secs = elapsed.as_secs();

    let (phase_label, is_warning) = match &status.phase {
        AgentPhase::Idle => ("IDLE".to_string(), false),
        AgentPhase::Connecting => {
            let warn = elapsed_secs > API_STUCK_SECS;
            (
                format!(
                    "API {}{}",
                    fmt_dur(elapsed),
                    if warn { " ⚠SLOW" } else { "" }
                ),
                warn,
            )
        }
        AgentPhase::Streaming => {
            let warn = elapsed_secs > STREAM_STUCK_SECS
                && status.stream_chars < STREAM_STUCK_MIN_CHARS;
            (
                format!(
                    "STREAM {} · {}c{}",
                    fmt_dur(elapsed),
                    status.stream_chars,
                    if warn { " ⚠STALL" } else { "" }
                ),
                warn,
            )
        }
        AgentPhase::ToolRunning { name } => {
            let warn = elapsed_secs > TOOL_STUCK_SECS;
            let display_name = if name.len() > 18 {
                format!("{}…", &name[..15])
            } else {
                name.clone()
            };
            (
                format!(
                    "{} {}{}",
                    display_name,
                    fmt_dur(elapsed),
                    if warn { " ⚠SLOW" } else { "" }
                ),
                warn,
            )
        }
        AgentPhase::Compacting => (format!("COMPACT {}", fmt_dur(elapsed)), false),
    };

    // Context token display
    let ctx_display = match (status.context_tokens, status.context_budget) {
        (0, _) => String::new(), // Don't show until we have data
        (used, Some(budget)) => {
            let pct = ((used as f64 / budget as f64) * 100.0) as u8;
            let warn = pct > 80;
            if warn {
                format!(" │ ctx:{} {}/{} ⚠", pct, fmt_tokens(used), fmt_tokens(budget))
            } else {
                format!(" │ ctx:{} {}/{}", pct, fmt_tokens(used), fmt_tokens(budget))
            }
        }
        (used, None) => format!(" │ ctx:{}", fmt_tokens(used)),
    };

    let bar = format!(
        " ⟡ {} │ turn {} │ {} │ tools:{}{}",
        if status.model_short.is_empty() {
            "?"
        } else {
            &status.model_short
        },
        status.turn,
        phase_label,
        status.total_tool_calls,
        ctx_display
    );

    (pad_or_truncate(&bar, width as usize), is_warning)
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{}s", s)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

fn pad_or_truncate(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= width {
        chars[..width].iter().collect()
    } else {
        format!("{}{}", s, " ".repeat(width - chars.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status() {
        let status = AgentStatus::default();
        let (text, warn) = render(&status, 80);
        assert!(text.contains("IDLE"));
        assert!(!warn);
    }

    #[test]
    fn streaming_status() {
        let mut status = AgentStatus::default();
        status.phase = AgentPhase::Streaming;
        status.phase_started = Instant::now() - Duration::from_secs(5);
        status.stream_chars = 500;
        let (text, warn) = render(&status, 80);
        assert!(text.contains("STREAM"));
        assert!(text.contains("500c"));
        assert!(!warn);
    }

    #[test]
    fn tool_status() {
        let mut status = AgentStatus::default();
        status.phase = AgentPhase::ToolRunning {
            name: "exec_command".into(),
        };
        status.phase_started = Instant::now() - Duration::from_secs(3);
        let (text, warn) = render(&status, 80);
        assert!(text.contains("exec_command"));
        assert!(!warn);
    }

    #[test]
    fn stuck_api_warning() {
        let mut status = AgentStatus::default();
        status.phase = AgentPhase::Connecting;
        status.phase_started = Instant::now() - Duration::from_secs(45);
        let (text, warn) = render(&status, 80);
        assert!(text.contains("⚠SLOW"));
        assert!(warn);
    }

    #[test]
    fn truncate_long_tool_name() {
        let mut status = AgentStatus::default();
        status.phase = AgentPhase::ToolRunning {
            name: "fal-blog-images:generate_blog_image".into(),
        };
        status.phase_started = Instant::now();
        let (text, _) = render(&status, 80);
        assert!(text.contains("…"));
    }

    #[test]
    fn context_display_with_budget() {
        let mut status = AgentStatus::default();
        status.context_tokens = 45_000;
        status.context_budget = Some(128_000);
        let (text, _) = render(&status, 120);
        assert!(text.contains("ctx:35")); // 45k/128k = ~35%
        assert!(text.contains("45k"));
        assert!(text.contains("128k"));
    }

    #[test]
    fn context_display_without_budget() {
        let mut status = AgentStatus::default();
        status.context_tokens = 45_000;
        status.context_budget = None;
        let (text, _) = render(&status, 120);
        assert!(text.contains("ctx:45k"));
        assert!(!text.contains("/"));
    }

    #[test]
    fn context_display_zero_tokens_hidden() {
        let status = AgentStatus::default();
        let (text, _) = render(&status, 120);
        assert!(!text.contains("ctx:"));
    }

    #[test]
    fn context_display_high_usage_warning() {
        let mut status = AgentStatus::default();
        status.context_tokens = 110_000;
        status.context_budget = Some(128_000);
        let (text, _) = render(&status, 120);
        assert!(text.contains("⚠")); // 85% usage
    }

    #[test]
    fn model_context_size_lookup() {
        assert_eq!(model_context_size("gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(model_context_size("gpt-4.1"), Some(1_047_576));
        assert_eq!(model_context_size("glm-5.1:cloud"), Some(128_000));
        assert_eq!(model_context_size("sonar"), Some(128_000));
        assert_eq!(model_context_size("unknown-model"), None);
    }

    #[test]
    fn fmt_tokens_formatting() {
        assert_eq!(fmt_tokens(500), "500");
        assert_eq!(fmt_tokens(45_000), "45k");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
        assert_eq!(fmt_tokens(128_000), "128k");
    }
}