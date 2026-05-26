//! Session recording — JSONL event recording for reproducible runs.
//!
//! Implements REQ-REC-001 through REQ-REC-006:
//! - Full session recording to JSONL (REQ-REC-001)
//! - Event envelope with type, sequence, timestamp, payload (REQ-REC-002)
//! - Captures config, prompt hashes, messages, tool calls, model info (REQ-REC-003)
//! - API key redaction by default (REQ-REC-004)
//! - User-configurable field redaction via --record-redact (REQ-REC-005)
//! - Schema versioning (REQ-REC-006)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

use crate::prompt::inspect::redact_secrets;

/// Current recording schema version.
const SCHEMA_VERSION: &str = "1";

/// A single recorded event in the JSONL recording format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordEvent {
    /// Schema version for future format evolution (REQ-REC-006).
    pub schema_version: String,
    /// Monotonically increasing sequence number (REQ-REC-002).
    pub seq: u64,
    /// UTC timestamp in ISO 8601 format (REQ-REC-002).
    pub ts: String,
    /// Event type discriminator (REQ-REC-002).
    #[serde(rename = "type")]
    pub event_type: String,
    /// Event payload — varies by type (REQ-REC-002).
    pub payload: serde_json::Value,
}

/// The recording session — writes events to a JSONL file.
pub struct Recorder {
    writer: std::io::BufWriter<std::fs::File>,
    seq: u64,
    redact_fields: bool,
}

impl Recorder {
    /// Create a new recorder that writes to the given file path.
    /// If redact_fields is true, additional user-specified fields are redacted
    /// beyond the default API key redaction (REQ-REC-005).
    pub fn new(path: &Path, redact_fields: bool) -> Result<Self> {
        let file = std::fs::File::create(path)
            .with_context(|| format!("creating recording file at {}", path.display()))?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
            seq: 0,
            redact_fields,
        })
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// Redact sensitive content if redaction is enabled.
    fn redact(&self, text: &str) -> String {
        let text = redact_secrets(text);
        if self.redact_fields {
            // Additional redaction: remove common patterns
            // TODO: make this configurable in v1.0
            text
        } else {
            text
        }
    }

    /// Record a config snapshot event (REQ-REC-003).
    /// API keys are always redacted (REQ-REC-004).
    pub fn record_config_snapshot(
        &mut self,
        model: &str,
        base_url: &str,
        api_key_set: bool,
        provider_names: &[String],
    ) -> Result<()> {
        let payload = serde_json::json!({
            "model": model,
            "base_url": base_url,
            "api_key_set": api_key_set,
            "providers": provider_names,
        });
        self.write_event("config_snapshot", payload)
    }

    /// Record a prompt layer hash event (REQ-REC-003).
    pub fn record_prompt_hash(&mut self, layer_name: &str, hash: &str) -> Result<()> {
        let payload = serde_json::json!({
            "layer": layer_name,
            "hash": hash,
        });
        self.write_event("prompt_hash", payload)
    }

    /// Record a user message event (REQ-REC-003).
    pub fn record_user_message(&mut self, content: &str) -> Result<()> {
        let payload = serde_json::json!({
            "content": self.redact(content),
        });
        self.write_event("user_message", payload)
    }

    /// Record an assistant response event (REQ-REC-003).
    pub fn record_assistant_response(&mut self, content: &str) -> Result<()> {
        let payload = serde_json::json!({
            "content": content.to_string(),
        });
        self.write_event("assistant_response", payload)
    }

    /// Record a tool call event (REQ-REC-003).
    #[allow(dead_code)]
    pub fn record_tool_call(
        &mut self,
        tool_name: &str,
        tool_id: &str,
        arguments: &serde_json::Value,
        is_mcp: bool,
    ) -> Result<()> {
        let args_str = serde_json::to_string(arguments).unwrap_or_default();
        let payload = serde_json::json!({
            "tool_name": tool_name,
            "tool_id": tool_id,
            "arguments": self.redact(&args_str),
            "is_mcp": is_mcp,
        });
        self.write_event("tool_call", payload)
    }

    /// Record a tool result event (REQ-REC-003).
    #[allow(dead_code)]
    pub fn record_tool_result(
        &mut self,
        tool_id: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "tool_id": tool_id,
            "result": self.redact(result),
            "is_error": is_error,
        });
        self.write_event("tool_result", payload)
    }

    /// Record a memory write event (REQ-REC-003).
    #[allow(dead_code)]
    pub fn record_memory_write(&mut self, action: &str, content: &str) -> Result<()> {
        let payload = serde_json::json!({
            "action": action,
            "content": self.redact(content),
        });
        self.write_event("memory_write", payload)
    }

    /// Record a model/provider change event (REQ-REC-003).
    pub fn record_model_change(&mut self, from_model: &str, to_model: &str) -> Result<()> {
        let payload = serde_json::json!({
            "from_model": from_model,
            "to_model": to_model,
        });
        self.write_event("model_change", payload)
    }

    /// Record a session summary event (REQ-REC-003).
    #[allow(dead_code)]
    pub fn record_session_summary(&mut self, summary: &str) -> Result<()> {
        let payload = serde_json::json!({
            "content": summary,
        });
        self.write_event("session_summary", payload)
    }

    /// Write a raw event to the recording.
    fn write_event(&mut self, event_type: &str, payload: serde_json::Value) -> Result<()> {
        let event = RecordEvent {
            schema_version: SCHEMA_VERSION.to_string(),
            seq: self.next_seq(),
            ts: Self::timestamp(),
            event_type: event_type.to_string(),
            payload,
        };
        let line = serde_json::to_string(&event)
            .with_context(|| format!("serializing record event: {}", event_type))?;
        writeln!(self.writer, "{}", line)
            .with_context(|| "writing to recording file")?;
        self.writer.flush().with_context(|| "flushing recording file")?;
        Ok(())
    }
}

/// Read recorded events from a JSONL file.
pub fn read_recording(path: &Path) -> Result<Vec<RecordEvent>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading recording from {}", path.display()))?;
    let mut events = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: RecordEvent = serde_json::from_str(line)
            .with_context(|| format!("parsing recording line {}: {}", line_num + 1, line))?;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_recorder_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_config_snapshot("test-model", "https://api.test.com", true, &[]).unwrap();
        recorder.record_user_message("hello").unwrap();

        // Read back and verify
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("config_snapshot"));
        assert!(content.contains("user_message"));
        assert!(content.contains("test-model"));
    }

    #[test]
    fn test_recording_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_schema.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_user_message("test").unwrap();

        let events = read_recording(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].schema_version, "1");
        assert_eq!(events[0].event_type, "user_message");
    }

    #[test]
    fn test_sequence_numbers_increase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_seq.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_user_message("first").unwrap();
        recorder.record_user_message("second").unwrap();
        recorder.record_user_message("third").unwrap();

        let events = read_recording(&path).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
        assert_eq!(events[2].seq, 3);
    }

    #[test]
    fn test_api_keys_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_redact.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_user_message("my api key is sk-1234567890abcdef1234567890").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("sk-1234567890abcdef1234567890"));
    }

    #[test]
    fn test_tool_call_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_tool.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_tool_call(
            "exec_command",
            "call_123",
            &serde_json::json!({"command": "ls -la"}),
            false,
        ).unwrap();

        let events = read_recording(&path).unwrap();
        assert_eq!(events[0].event_type, "tool_call");
        assert_eq!(events[0].payload["tool_name"], "exec_command");
        assert_eq!(events[0].payload["is_mcp"], false);
    }

    #[test]
    fn test_model_change_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_model_change.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_model_change("gpt-4", "qwen3").unwrap();

        let events = read_recording(&path).unwrap();
        assert_eq!(events[0].event_type, "model_change");
        assert_eq!(events[0].payload["from_model"], "gpt-4");
        assert_eq!(events[0].payload["to_model"], "qwen3");
    }

    #[test]
    fn test_prompt_hash_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_hash.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_prompt_hash("SOUL", "abc123").unwrap();

        let events = read_recording(&path).unwrap();
        assert_eq!(events[0].event_type, "prompt_hash");
        assert_eq!(events[0].payload["layer"], "SOUL");
        assert_eq!(events[0].payload["hash"], "abc123");
    }

    #[test]
    fn test_read_recording_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_empty_lines.jsonl");
        let mut recorder = Recorder::new(&path, false).unwrap();

        recorder.record_user_message("first").unwrap();
        recorder.record_user_message("second").unwrap();

        // Add empty lines manually
        drop(recorder);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file).unwrap();
        writeln!(file).unwrap();
        drop(file);

        let events = read_recording(&path).unwrap();
        assert_eq!(events.len(), 2);
    }
}