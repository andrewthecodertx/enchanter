//! Real-time agent status bar for the REPL.
//!
//! Provides shared state (`AgentStatus`) that the agent loop updates
//! and the REPL status bar reads. Stuck-detection thresholds surface
//! warnings when the agent appears stalled.

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

    let bar = format!(
        " ⟡ {} │ turn {} │ {} │ tools:{}",
        if status.model_short.is_empty() {
            "?"
        } else {
            &status.model_short
        },
        status.turn,
        phase_label,
        status.total_tool_calls
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
}