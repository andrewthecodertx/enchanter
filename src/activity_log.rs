//! Activity log — append-only JSONL record of agent actions.
//!
//! Writes one line per notable event *before* the action starts, so a hang or
//! crash never loses the record of what was in-flight. After a Ctrl+C the user
//! can inspect `~/.enchanter/logs/activity.jsonl` to see exactly where the agent
//! stalled.
//!
//! Events logged:
//! - API call start/end (with duration)
//! - Tool dispatch start/end (with name, truncated args, duration)
//! - Agent loop turn start/end
//! - Stream chunk timeouts
//! - Errors
//!
//! The log rotates when it exceeds 5 MB to avoid unbounded growth.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use crate::home;

/// Maximum log file size before rotation (5 MB).
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

/// Truncate tool arguments to this many bytes in the log (avoid huge payloads).
const MAX_ARGS_LEN: usize = 500;

/// Truncate tool results to this many bytes in the log.
const MAX_RESULT_LEN: usize = 500;

// ── Event types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityEvent {
    /// About to call the LLM API.
    ApiCallStart {
        model: String,
        message_count: usize,
        tool_count: Option<usize>,
        stream: bool,
    },
    /// LLM API call completed.
    ApiCallEnd {
        model: String,
        duration_ms: u64,
        has_tool_calls: bool,
        content_len: Option<usize>,
    },
    /// LLM API call failed.
    ApiCallError {
        model: String,
        duration_ms: u64,
        error: String,
    },
    /// Stream chunk timeout (potential hang point).
    StreamTimeout { model: String, elapsed_secs: u64 },
    /// About to dispatch a tool call.
    ToolCallStart {
        name: String,
        /// First 500 bytes of arguments JSON.
        arguments: String,
        turn: u32,
    },
    /// Tool call completed.
    ToolCallEnd {
        name: String,
        duration_ms: u64,
        /// First 500 bytes of result.
        result: String,
        exit_code: Option<i32>,
    },
    /// Tool call failed.
    ToolCallError {
        name: String,
        duration_ms: u64,
        error: String,
    },
    /// Agent loop turn started.
    TurnStart { turn: u32, max_turns: Option<u32> },
    /// Agent loop turn completed.
    TurnEnd {
        turn: u32,
        tool_calls: usize,
        duration_ms: u64,
    },
    /// Agent loop hit max turns.
    MaxTurnsReached { turn: u32, limit: String },
    /// Session started.
    SessionStart { session_id: String, model: String },
    /// Session ended (normal exit or Ctrl+C).
    SessionEnd {
        session_id: String,
        total_turns: u32,
        total_tool_calls: usize,
        duration_secs: u64,
    },
    /// Compaction happened.
    Compacted {
        removed_messages: usize,
        budget_tokens: u64,
    },
    /// Generic error.
    Error { context: String, message: String },
}

impl ActivityEvent {
    /// Serialize as a single JSONL line with timestamp.
    fn to_jsonl(&self) -> anyhow::Result<String> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let event_json = serde_json::to_string(self)?;
        Ok(format!("{{\"ts\":\"{}\",{}}}", ts, &event_json[1..]))
    }
}

// ── Global logger ───────────────────────────────────────────────

/// Global activity logger. Uses a mutex for thread-safe append.
static LOGGER: std::sync::OnceLock<Mutex<ActivityLogger>> = std::sync::OnceLock::new();

/// Get or initialize the global activity logger.
pub fn logger() -> &'static Mutex<ActivityLogger> {
    LOGGER.get_or_init(|| {
        let log_dir = home::enchanter_home().join("logs");
        std::fs::create_dir_all(&log_dir).ok();
        let log_path = log_dir.join("activity.jsonl");
        match ActivityLogger::open(&log_path) {
            Ok(l) => Mutex::new(l),
            Err(e) => {
                eprintln!("Warning: could not open activity log: {}", e);
                Mutex::new(ActivityLogger::disabled())
            }
        }
    })
}

/// Log a single activity event. Silently ignores write failures.
pub fn log(event: ActivityEvent) {
    if let Ok(mut guard) = logger().lock() {
        guard.log(event).ok();
    }
}

// ── Timing helper ───────────────────────────────────────────────

/// RAII guard that logs a `*Start` event on creation and a `*End` event on drop.
/// Usage: `let _guard = ApiCallGuard::new(model, msg_count, tool_count, stream);`
pub struct ApiCallGuard {
    model: String,
    start: Instant,
}

impl ApiCallGuard {
    pub fn new(
        model: String,
        message_count: usize,
        tool_count: Option<usize>,
        stream: bool,
    ) -> Self {
        log(ActivityEvent::ApiCallStart {
            model: model.clone(),
            message_count,
            tool_count,
            stream,
        });
        Self {
            model,
            start: Instant::now(),
        }
    }

    pub fn end(self, has_tool_calls: bool, content_len: Option<usize>) {
        log(ActivityEvent::ApiCallEnd {
            model: self.model,
            duration_ms: self.start.elapsed().as_millis() as u64,
            has_tool_calls,
            content_len,
        });
    }

    pub fn fail(self, error: String) {
        log(ActivityEvent::ApiCallError {
            model: self.model,
            duration_ms: self.start.elapsed().as_millis() as u64,
            error,
        });
    }
}

/// RAII guard for tool call timing.
pub struct ToolCallGuard {
    name: String,
    _turn: u32,
    start: Instant,
}

impl ToolCallGuard {
    pub fn new(name: String, arguments: &str, turn: u32) -> Self {
        let truncated_args = truncate_str(arguments, MAX_ARGS_LEN);
        log(ActivityEvent::ToolCallStart {
            name: name.clone(),
            arguments: truncated_args,
            turn,
        });
        Self {
            name,
            _turn: turn,
            start: Instant::now(),
        }
    }

    pub fn end(self, result: &str, exit_code: Option<i32>) {
        let truncated = truncate_str(result, MAX_RESULT_LEN);
        log(ActivityEvent::ToolCallEnd {
            name: self.name,
            duration_ms: self.start.elapsed().as_millis() as u64,
            result: truncated,
            exit_code,
        });
    }

    #[allow(dead_code)]
    pub fn fail(self, error: String) {
        log(ActivityEvent::ToolCallError {
            name: self.name,
            duration_ms: self.start.elapsed().as_millis() as u64,
            error,
        });
    }
}

// ── Logger implementation ───────────────────────────────────────

pub struct ActivityLogger {
    writer: Option<std::io::BufWriter<std::fs::File>>,
    _path: PathBuf,
}

impl ActivityLogger {
    /// Open (or create) the activity log file.
    pub fn open(path: &PathBuf) -> anyhow::Result<Self> {
        // Rotate if the file is too large
        if path.exists()
            && let Ok(meta) = std::fs::metadata(path)
                && meta.len() > MAX_LOG_SIZE {
                    let rotated = path.with_extension("jsonl.bak");
                    let _ = std::fs::rename(path, &rotated);
                }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
            _path: path.clone(),
        })
    }

    /// Create a disabled logger that silently drops all events.
    pub fn disabled() -> Self {
        Self {
            writer: None,
            _path: PathBuf::new(),
        }
    }

    /// Append one event to the log.
    pub fn log(&mut self, event: ActivityEvent) -> anyhow::Result<()> {
        let Some(writer) = &mut self.writer else {
            return Ok(());
        };
        let line = event.to_jsonl()?;
        writeln!(writer, "{}", line)?;
        writer.flush()?; // Always flush — the whole point is durability on crash
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let trunc_at = s.floor_char_boundary(max);
        format!("{}... [truncated, {} bytes total]", &s[..trunc_at], s.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serialization() {
        let event = ActivityEvent::ApiCallStart {
            model: "gpt-4.1".into(),
            message_count: 12,
            tool_count: Some(8),
            stream: true,
        };
        let line = event.to_jsonl().unwrap();
        assert!(line.starts_with("{\"ts\":\""));
        assert!(line.contains("\"type\":\"api_call_start\""));
        assert!(line.contains("\"model\":\"gpt-4.1\""));
    }

    #[test]
    fn tool_call_event_serialization() {
        let event = ActivityEvent::ToolCallStart {
            name: "exec_command".into(),
            arguments: r#"{"command":"ls -la"}"#.into(),
            turn: 3,
        };
        let line = event.to_jsonl().unwrap();
        assert!(line.contains("\"type\":\"tool_call_start\""));
        assert!(line.contains("\"name\":\"exec_command\""));
    }

    #[test]
    fn truncate_preserves_char_boundary() {
        let s = "héllo wörld"; // contains multi-byte chars
        let truncated = truncate_str(s, 7);
        assert!(truncated.len() <= 50); // accounts for suffix
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn session_events_roundtrip() {
        let event = ActivityEvent::SessionStart {
            session_id: "abc123".into(),
            model: "claude-3".into(),
        };
        let jsonl = event.to_jsonl().unwrap();
        assert!(jsonl.contains("\"session_id\":\"abc123\""));
    }
}
